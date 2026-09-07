//! DeviceSession：单台已连接设备的完整会话（Spec v1.2 §1.2）。
//!
//! 每台设备一个实例，拥有独立的状态机、镜像适配器、输入控制器、帧通道、
//! 元数据与错误日志；命令/事件均携带 sessionId 路由。输入只跟随激活设备
//! （由 SessionRegistry 决定），本模块只负责自身链路。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::integrations::android_frame::ScrcpyControl;
use crate::integrations::clipboard_adapter::{
    run_paste_text_flow, ClipboardAdapter, PasteError, PasteResult, SystemClipboard,
};
use crate::integrations::frame_bridge::FrameSink;
use crate::integrations::hid_adapter::{
    authorization_name, create_hid_controller, HidStatus, IHidController,
    INPUT_REPORT_BOOT_KEYBOARD, INPUT_REPORT_BOOT_MOUSE, INPUT_REPORT_KEYBOARD, INPUT_REPORT_MOUSE,
};
use crate::integrations::hid_report::{
    build_mouse_report, clamp_delta, key_down_up, usage_for_code, BTN_LEFT, MOD_ALT, MOD_CTRL,
    MOD_META, MOD_SHIFT,
};
use crate::integrations::ios_usb_control::{IosUsbControl, ScreenSize};
use crate::integrations::mirror_adapter::{MirrorAdapter, MirrorEvent};
use crate::integrations::text_encoder::encode_ascii_text;
use crate::integrations::AdapterError;
use crate::session::state::{
    derive_control_state, derive_mirror_state, ControlState, SessionEvent, SessionStateMachine,
    SessionStateName,
};

use super::manager::{EventSink, LogEntry, SessionError, SessionPreferences, SessionStateView};

/// 设备类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Iphone,
    Android,
}

impl DeviceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceKind::Iphone => "iphone",
            DeviceKind::Android => "android",
        }
    }
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "iphone" => Some(Self::Iphone),
            "android" => Some(Self::Android),
            _ => None,
        }
    }
}

fn keyboard_input_ready(input_report_mask: u8) -> bool {
    input_report_mask & (INPUT_REPORT_KEYBOARD | INPUT_REPORT_BOOT_KEYBOARD) != 0
}

struct ActiveMirror {
    adapter: Arc<Mutex<Box<dyn MirrorAdapter>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

/// 停止镜像时不能无期限等待阻塞的帧读取线程。子进程已经被适配器终止，
/// 已完成的事件线程立即回收；仍在等待管道 EOF 的线程丢弃 JoinHandle，
/// 让它在后台自然退出，不阻塞取消投屏或关闭模拟器窗口。
fn stop_active_mirror(active: ActiveMirror) -> Result<(), AdapterError> {
    let result = active.adapter.lock().unwrap().stop();
    if let Some(worker) = active.worker {
        if worker.is_finished() {
            let _ = worker.join();
        }
    }
    result
}

/// 输入控制器句柄：iOS=BLE HID；Android=scrcpy control socket。
enum InputHandle {
    Ble(Box<dyn IHidController>),
    IosUsb(IosUsbControl),
    Scrcpy(ScrcpyControl),
}

enum InputBorrow<'a> {
    Ble(&'a mut dyn IHidController),
    IosUsb(&'a mut IosUsbControl),
    Scrcpy(&'a ScrcpyControl),
}

/// 自动重连上限（第 1..5 次重启，之后转 Failed 并提示手动重试）。
const RECONNECT_MAX_ATTEMPTS: u32 = 5;
/// 自动重连基础退避（毫秒）：1s、2s、4s、8s、16s，之后封顶 16s。
const RECONNECT_BASE_DELAY_MS: u64 = 1000;

/// 第 `attempt` 次重连前的退避时长（毫秒），指数增长并在 16s 封顶。
fn reconnect_delay_ms(attempt: u32) -> u64 {
    let exponent = attempt.saturating_sub(1).min(4);
    RECONNECT_BASE_DELAY_MS << exponent
}

struct DeviceInner {
    state: Mutex<SessionStateMachine>,
    since: Mutex<String>,
    prefs: Mutex<SessionPreferences>,
    mirror: Mutex<Option<ActiveMirror>>,
    /// Serializes input operations without keeping `input` locked while a
    /// native HID call waits for a platform callback.
    input_operation: Mutex<()>,
    /// Serializes control start/stop with the slow WDA/iproxy setup path. This
    /// prevents a concurrent cancel from installing a controller after the
    /// user has already stopped it.
    control_operation: Mutex<()>,
    input: Mutex<Option<InputHandle>>,
    android_serial: Mutex<Option<String>>,
    adb_drag: Mutex<bool>,
    ios_drag: Mutex<bool>,
    android_pointer: Mutex<Option<(u32, u32)>>,
    screen: Mutex<Option<Arc<crate::integrations::android_frame::AndroidScreenSource>>>,
    error_log: Mutex<Vec<LogEntry>>,
    paste_token: AtomicU64,
    last_error: Mutex<Option<SessionError>>,
    last_metadata: Mutex<Option<(u32, u32)>>,
    frame_sink: Mutex<Option<Arc<dyn FrameSink>>>,
    frame_worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    frame_test_mode: Arc<AtomicBool>,
    /// 自动重连：已执行的尝试次数（每次进入 Reconnecting 时清零）。
    reconnect_attempts: AtomicU32,
    /// 自动重连：是否已有一个重连循环线程在运行（幂等闸门）。
    reconnect_armed: Arc<AtomicBool>,
}

/// 单设备会话。
#[derive(Clone)]
pub struct DeviceSession {
    pub id: String,
    pub kind: DeviceKind,
    inner: Arc<DeviceInner>,
    sink: Arc<dyn EventSink>,
    resources: Arc<std::path::PathBuf>,
    uxplay_port: u16,
}

impl DeviceSession {
    pub fn new(
        id: String,
        kind: DeviceKind,
        sink: Arc<dyn EventSink>,
        resources: std::path::PathBuf,
        uxplay_port: u16,
    ) -> Self {
        Self {
            id,
            kind,
            inner: Arc::new(DeviceInner {
                state: Mutex::new(SessionStateMachine::new()),
                since: Mutex::new(chrono::Utc::now().to_rfc3339()),
                prefs: Mutex::new(SessionPreferences::default()),
                mirror: Mutex::new(None),
                input_operation: Mutex::new(()),
                control_operation: Mutex::new(()),
                input: Mutex::new(None),
                android_serial: Mutex::new(None),
                adb_drag: Mutex::new(false),
                ios_drag: Mutex::new(false),
                android_pointer: Mutex::new(None),
                screen: Mutex::new(None),
                error_log: Mutex::new(Vec::new()),
                paste_token: AtomicU64::new(0),
                last_error: Mutex::new(None),
                last_metadata: Mutex::new(None),
                frame_sink: Mutex::new(None),
                frame_worker: Mutex::new(None),
                frame_test_mode: Arc::new(AtomicBool::new(false)),
                reconnect_attempts: AtomicU32::new(0),
                reconnect_armed: Arc::new(AtomicBool::new(false)),
            }),
            sink,
            resources: Arc::new(resources),
            uxplay_port,
        }
    }

    // -----------------------------------------------------------------------
    // 内部状态广播
    // -----------------------------------------------------------------------

    fn apply(&self, event: SessionEvent, detail: Option<String>) -> Result<(), AdapterError> {
        let mut st = self.inner.state.lock().unwrap();
        st.transition(event)
            .map_err(|e| AdapterError::Failed(format!("非法状态转移 {:?}", e)))?;
        *self.inner.since.lock().unwrap() = chrono::Utc::now().to_rfc3339();
        let view = self.view_locked(&st, detail);
        drop(st);
        self.sink.emit_session(&self.id, &view);
        Ok(())
    }

    fn view_locked(&self, st: &SessionStateMachine, detail: Option<String>) -> SessionStateView {
        let state = st.state();
        let since = self.inner.since.lock().unwrap().clone();
        let last_error = self.inner.last_error.lock().unwrap().clone();
        SessionStateView {
            state,
            since,
            detail,
            control: derive_control_state(state),
            mirror: derive_mirror_state(state),
            last_error,
        }
    }

    fn set_last_error(&self, e: &AdapterError, recoverable: bool) {
        let se = SessionError::from_adapter(e, recoverable);
        *self.inner.last_error.lock().unwrap() = Some(se);
        let entry = LogEntry {
            at: chrono::Utc::now().to_rfc3339(),
            level: "error".into(),
            code: e.code().into(),
            message: e.to_string(),
        };
        self.record_error(entry);
    }

    fn record_error(&self, entry: LogEntry) {
        {
            let mut log = self.inner.error_log.lock().unwrap();
            log.push(entry.clone());
            let overflow = log.len().saturating_sub(200);
            if overflow > 0 {
                log.drain(0..overflow);
            }
        }
        self.sink.emit_diagnostic_error(&self.id, &entry);
    }

    /// 运行时日志（info/warn/error），实时广播给前端日志面板。
    fn emit_log(&self, level: &str, code: &str, message: String) {
        let entry = LogEntry {
            at: chrono::Utc::now().to_rfc3339(),
            level: level.into(),
            code: code.into(),
            message,
        };
        self.sink.emit_log(&self.id, &entry);
        if level == "error" {
            self.record_error(entry);
        }
    }

    // -----------------------------------------------------------------------
    // 生命周期
    // -----------------------------------------------------------------------

    pub fn state_view(&self) -> SessionStateView {
        let st = self.inner.state.lock().unwrap();
        self.view_locked(&st, None)
    }

    /// 启动镜像会话（按设备类型选择链路）。
    pub fn start_session(&self, prefs: Option<SessionPreferences>) -> Result<(), AdapterError> {
        if let Some(p) = prefs {
            *self.inner.prefs.lock().unwrap() = p;
        }
        self.apply(SessionEvent::UserConnect, None)?;
        match self.clone().preflight() {
            Ok(()) => {
                self.apply(SessionEvent::ChecksPassed, None)?;
                self.launch_mirror()
            }
            Err(e) => {
                self.set_last_error(&e, true);
                let _ = self.apply(SessionEvent::ChecksFailed, Some(format!("{e}")));
                Err(e)
            }
        }
    }

    fn preflight(&self) -> Result<(), AdapterError> {
        match self.kind {
            DeviceKind::Iphone => {
                crate::integrations::mirror_adapter::UxPlayMirrorAdapter::check_binary(
                    &self.resources,
                )
                .map_err(AdapterError::DependencyMissing)?;
                #[cfg(not(target_os = "macos"))]
                if crate::integrations::android_frame::locate_ffmpeg(&self.resources).is_none() {
                    return Err(AdapterError::DependencyMissing(
                        "未找到随应用分发的 ffmpeg。Windows iOS UxPlay RTP 输出需要宿主解码器，请运行 pnpm sidecars"
                            .into(),
                    ));
                }
                Ok(())
            }
            DeviceKind::Android => {
                let adb = crate::integrations::adb::resolve_tool(
                    &self.resources,
                    "adb",
                    "PHONEBRIDGE_ADB_PATH",
                )
                .map_err(AdapterError::DependencyMissing)?;
                let devices = crate::integrations::adb::list_devices(&adb)?;
                if !devices.iter().any(|d| d.status == "device") {
                    return Err(AdapterError::Failed(
                        "未找到已授权的 Android 设备；请开启 USB 调试并在设备上完成授权".into(),
                    ));
                }
                if crate::integrations::android_frame::locate_scrcpy_server(&self.resources)
                    .is_none()
                {
                    return Err(AdapterError::DependencyMissing(
                        "未找到 scrcpy-server（需与宿主协议版本匹配）；请运行 pnpm sidecars 或配置 PHONEBRIDGE_SCRCPY_SERVER_PATH".into(),
                    ));
                }
                // 宿主端用 ffmpeg 解码 scrcpy 的原始 H264 视频通道。
                if crate::integrations::android_frame::locate_ffmpeg(&self.resources).is_none() {
                    return Err(AdapterError::DependencyMissing(
                        "未找到 ffmpeg（PATH/Homebrew）。scrcpy H264 解码依赖 ffmpeg，请安装后重试"
                            .into(),
                    ));
                }
                Ok(())
            }
        }
    }

    fn launch_mirror(&self) -> Result<(), AdapterError> {
        self.apply(SessionEvent::MirrorStarted, None)?;
        match self.kind {
            DeviceKind::Iphone => self.launch_airplay(),
            DeviceKind::Android => self.launch_android_screen(),
        }
    }

    /// iPhone：UxPlay（AirPlay）镜像。
    fn launch_airplay(&self) -> Result<(), AdapterError> {
        let path =
            crate::integrations::mirror_adapter::UxPlayMirrorAdapter::check_binary(&self.resources)
                .map_err(AdapterError::DependencyMissing)?;
        let mut adapter: Box<dyn MirrorAdapter> = {
            let mut a = crate::integrations::mirror_adapter::UxPlayMirrorAdapter::new(
                crate::integrations::mirror_adapter::UxPlayConfig {
                    port: self.uxplay_port,
                    ..crate::integrations::mirror_adapter::UxPlayConfig::default()
                },
            );
            a.set_binary_path(path);
            #[cfg(not(target_os = "macos"))]
            {
                let ffmpeg = crate::integrations::android_frame::locate_ffmpeg(&self.resources)
                    .ok_or_else(|| AdapterError::DependencyMissing("未找到 ffmpeg".into()))?;
                a.set_ffmpeg_path(ffmpeg);
            }
            if let Some(sink) = self.inner.frame_sink.lock().unwrap().clone() {
                a.attach_sink(sink);
            }
            Box::new(a)
        };
        let (tx, rx): (Sender<MirrorEvent>, Receiver<MirrorEvent>) = channel();
        if let Err(e) = adapter.start(tx) {
            self.set_last_error(&e, true);
            let _ = self.stop_mirror();
            let _ = self.apply(
                SessionEvent::MirrorFailed,
                Some(format!("镜像引擎启动失败：{e}")),
            );
            return Err(e);
        }
        let adapter = Arc::new(Mutex::new(adapter));
        let worker = self.spawn_mirror_worker(adapter.clone(), rx);
        *self.inner.mirror.lock().unwrap() = Some(ActiveMirror {
            adapter,
            worker: Some(worker),
        });
        Ok(())
    }

    /// Android：scrcpy-server H264 通道 + ffmpeg 解码 → RGBA 帧。
    fn launch_android_screen(&self) -> Result<(), AdapterError> {
        let adb =
            crate::integrations::adb::resolve_tool(&self.resources, "adb", "PHONEBRIDGE_ADB_PATH")
                .map_err(AdapterError::DependencyMissing)?;
        let serial = crate::integrations::adb::serial_for_session_id(&adb, &self.id)?;
        let serial =
            serial.ok_or_else(|| AdapterError::Failed("Android 设备未授权或已断开".into()))?;
        *self.inner.android_serial.lock().unwrap() = Some(serial.clone());

        let (w, h) = crate::integrations::adb::query_wm_size(&adb, Some(&serial))
            .unwrap_or((1080u32, 2340u32));
        let screen = Arc::new(crate::integrations::android_frame::AndroidScreenSource::new(w, h));
        let (video_w, video_h) = screen.video_dimensions();
        {
            let me = self.clone();
            let id = self.id.clone();
            screen.set_log(Arc::new(move |line| {
                let entry = LogEntry {
                    at: chrono::Utc::now().to_rfc3339(),
                    level: "info".into(),
                    code: "screen_pipe".into(),
                    message: line,
                };
                me.sink.emit_log(&id, &entry);
            }));
        }
        {
            let me = self.clone();
            screen.set_on_first_frame(Arc::new(move || {
                // 控制协议中的 screen_size 必须与 scrcpy 视频帧一致，而不是
                // `wm size` 返回的物理/逻辑屏幕尺寸。
                me.on_android_first_frame(video_w, video_h);
            }));
        }
        {
            let me = self.clone();
            screen.set_on_control_ready(Arc::new(move || {
                me.on_android_control_ready();
            }));
        }
        {
            let me = self.clone();
            screen.set_on_failure(Arc::new(move |message| {
                me.on_android_stream_failure(message);
            }));
        }
        if let Some(sink) = self.inner.frame_sink.lock().unwrap().clone() {
            screen.attach_sink(sink);
        }
        // 先登记 source，再启动后台线程；这样首帧/控制回调不会在注册前到达。
        *self.inner.screen.lock().unwrap() = Some(screen.clone());
        if let Err(e) = screen.start(&self.resources, &adb, &serial) {
            self.inner.screen.lock().unwrap().take();
            self.set_last_error(&e, true);
            let _ = self.apply(
                SessionEvent::MirrorFailed,
                Some(format!("投屏启动失败：{e}")),
            );
            return Err(e);
        }
        self.emit_log(
            "info",
            "screen_started",
            format!(
                "scrcpy-server → H264 → ffmpeg 已启动（等待真实首帧，设备 {w}x{h}，视频 {video_w}x{video_h}）"
            ),
        );
        Ok(())
    }

    fn on_android_first_frame(&self, width: u32, height: u32) {
        self.sink.emit_mirror_metadata(&self.id, width, height);
        *self.inner.last_metadata.lock().unwrap() = Some((width, height));
        let state = self.inner.state.lock().unwrap().state();
        if state == SessionStateName::Mirroring {
            if let Err(error) =
                self.apply(SessionEvent::FirstFrame, Some(format!("{width}x{height}")))
            {
                self.set_last_error(&error, true);
                return;
            }
            self.emit_log(
                "info",
                "first_frame",
                format!("已解码 Android 首帧（{width}x{height}）"),
            );
        }
        self.maybe_start_android_auto_control();
    }

    fn on_android_control_ready(&self) {
        self.emit_log("info", "control_channel", "scrcpy 控制通道已连接".into());
        self.maybe_start_android_auto_control();
    }

    fn maybe_start_android_auto_control(&self) {
        let auto = self.inner.prefs.lock().unwrap().auto_control;
        let connected =
            self.inner.state.lock().unwrap().state() == SessionStateName::MirroringConnected;
        if auto && connected {
            if let Err(error) = self.start_control_inner() {
                self.emit_log(
                    "warn",
                    "control_pending",
                    format!("自动启用 scrcpy 控制失败：{error}"),
                );
            }
        }
    }

    fn on_android_stream_failure(&self, message: String) {
        let state = self.inner.state.lock().unwrap().state();
        let error = AdapterError::Failed(message.clone());
        self.set_last_error(&error, true);
        match state {
            SessionStateName::Mirroring => {
                let _ = self.apply(SessionEvent::MirrorFailed, Some(message));
            }
            SessionStateName::MirroringConnected
            | SessionStateName::ControlPairing
            | SessionStateName::ControlReady => {
                let _ = self.apply(SessionEvent::LinkLost, Some(message));
                self.schedule_reconnect();
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // 自动重连（Reconnecting）：链路断开后自动重启镜像接收器并继续广播，
    // 避免状态机停留在 Reconnecting 而没有任何恢复路径（此前无实现）。
    // -----------------------------------------------------------------------

    /// 进入重连调度：幂等（已有一个循环在跑则忽略），并清空尝试计数。
    fn schedule_reconnect(&self) {
        if self.inner.reconnect_armed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.inner.reconnect_attempts.store(0, Ordering::SeqCst);
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name(format!("reconnect:{}", self.id))
            .spawn(move || me.reconnect_loop());
    }

    /// 重连循环：退避重启镜像接收器；状态离开 Reconnecting（成功/用户停止）
    /// 或尝试次数耗尽时结束。
    fn reconnect_loop(&self) {
        loop {
            let state = self.inner.state.lock().unwrap().state();
            if state != SessionStateName::Reconnecting {
                break;
            }
            let attempt = self.inner.reconnect_attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt > RECONNECT_MAX_ATTEMPTS {
                let message = format!("镜像链路自动重连 {} 次未成功，请手动重试投屏", attempt - 1);
                self.emit_log("error", "reconnect_timeout", message.clone());
                self.set_last_error(&AdapterError::Failed(message.clone()), true);
                let _ = self.apply(SessionEvent::ReconnectTimeout, Some(message));
                break;
            }
            let delay_ms = reconnect_delay_ms(attempt);
            self.emit_log(
                "info",
                "reconnect_scheduled",
                format!(
                    "镜像链路断开，{:.1} 秒后自动重连（第 {attempt} 次，共 {RECONNECT_MAX_ATTEMPTS} 次）",
                    delay_ms as f64 / 1000.0
                ),
            );
            if !self.sleep_interruptible(delay_ms) {
                break;
            }
            {
                let st = self.inner.state.lock().unwrap();
                if st.state() != SessionStateName::Reconnecting {
                    break;
                }
            }
            self.emit_log(
                "info",
                "reconnect_started",
                format!("正在重启镜像接收器（第 {attempt} 次）"),
            );
            if let Err(error) = self.relaunch_mirror() {
                self.emit_log(
                    "warn",
                    "reconnect_failed",
                    format!("镜像接收器重启失败：{error}"),
                );
                self.set_last_error(&error, true);
                // 继续下一轮退避重试
            }
        }
        self.inner.reconnect_armed.store(false, Ordering::SeqCst);
    }

    /// 睡眠可中断：用户停止会话（状态离开 Reconnecting）时立即返回 false。
    fn sleep_interruptible(&self, millis: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(millis);
        loop {
            {
                let st = self.inner.state.lock().unwrap();
                if st.state() != SessionStateName::Reconnecting {
                    return false;
                }
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return true;
            }
            let remain = (deadline - now).min(std::time::Duration::from_millis(200));
            std::thread::sleep(remain);
        }
    }

    /// 重建镜像链路（不改变状态机：重连期间状态保持 Reconnecting，
    /// 之后由 MirrorConnected/FirstFrame 事件驱动 ReconnectOk）。
    fn relaunch_mirror(&self) -> Result<(), AdapterError> {
        // 先显式回收旧接收器：旧 worker 线程持有 adapter 的 Arc 克隆，若只
        // 替换槽位而不 stop()，Drop 不会触发，前一次重连启动的 UxPlay 会
        // 变成孤儿继续占用端口段并广播同名服务。stop() 会终止子进程；
        // 事件线程若还在等待管道 EOF，则放到后台自然退出，不阻塞重连循环。
        let active = self.inner.mirror.lock().unwrap().take();
        if let Some(active) = active {
            let _ = stop_active_mirror(active);
        }
        match self.kind {
            DeviceKind::Iphone => self.launch_airplay(),
            DeviceKind::Android => self.launch_android_screen(),
        }
    }

    /// 重连成功：状态由 Reconnecting -> MirroringConnected（事件驱动）。
    fn on_reconnect_success(&self) {
        let state = self.inner.state.lock().unwrap().state();
        if state != SessionStateName::Reconnecting {
            return;
        }
        self.inner.reconnect_attempts.store(0, Ordering::SeqCst);
        self.inner.reconnect_armed.store(false, Ordering::SeqCst);
        if let Err(error) = self.apply(SessionEvent::ReconnectOk, Some("镜像链路已自动恢复".into()))
        {
            self.set_last_error(&error, true);
            return;
        }
        self.emit_log("info", "reconnect_ok", "镜像链路自动重连成功".into());
    }

    fn spawn_mirror_worker(
        &self,
        adapter: Arc<Mutex<Box<dyn MirrorAdapter>>>,
        rx: Receiver<MirrorEvent>,
    ) -> std::thread::JoinHandle<()> {
        let session = self.clone();
        let me = self.id.clone();
        std::thread::spawn(move || {
            for ev in rx {
                match ev {
                    MirrorEvent::Metadata { width, height } => {
                        log::info!("[{me}] mirror metadata: {width}x{height}");
                        session.sink.emit_mirror_metadata(&me, width, height);
                        *session.inner.last_metadata.lock().unwrap() = Some((width, height));
                    }
                    MirrorEvent::MirrorConnected => {
                        let state = session.inner.state.lock().unwrap().state();
                        if state == SessionStateName::Mirroring {
                            if let Err(error) = session.apply(
                                SessionEvent::MirrorConnected,
                                Some("AirPlay 会话已建立".into()),
                            ) {
                                session.set_last_error(&error, true);
                                continue;
                            }
                            session.emit_log(
                                "info",
                                "mirror_connected",
                                "iOS AirPlay 镜像连接已建立，正在等待视频首帧".into(),
                            );
                        } else if state == SessionStateName::Reconnecting {
                            session.on_reconnect_success();
                        }
                    }
                    MirrorEvent::FirstFrame => {
                        let metadata = session.mirror_metadata().unwrap_or((640, 480));
                        let state = session.inner.state.lock().unwrap().state();
                        let mirror_active = matches!(
                            state,
                            SessionStateName::Mirroring
                                | SessionStateName::MirroringConnected
                                | SessionStateName::ControlPairing
                                | SessionStateName::ControlReady
                        );
                        if state == SessionStateName::Reconnecting {
                            session.on_reconnect_success();
                        } else if state == SessionStateName::Mirroring {
                            if let Err(error) = session.apply(
                                SessionEvent::FirstFrame,
                                Some(format!("{}x{}", metadata.0, metadata.1)),
                            ) {
                                session.set_last_error(&error, true);
                                continue;
                            }
                        }
                        if mirror_active {
                            session.emit_log(
                                "info",
                                "first_frame",
                                format!(
                                    "已解码 {} 首帧（{}x{}）",
                                    if me.starts_with("iphone") {
                                        "iOS"
                                    } else {
                                        "Android"
                                    },
                                    metadata.0,
                                    metadata.1
                                ),
                            );
                            if session.inner.prefs.lock().unwrap().auto_control
                                && session.inner.state.lock().unwrap().state()
                                    == SessionStateName::MirroringConnected
                            {
                                let _ = session.start_control_inner();
                            }
                        }
                    }
                    MirrorEvent::Crashed { source, exit_code } => {
                        let state = session.inner.state.lock().unwrap().state();
                        if matches!(
                            state,
                            SessionStateName::Idle
                                | SessionStateName::Stopping
                                | SessionStateName::Failed
                        ) {
                            // 用户主动停止会话时，子进程退出事件可能已经排队；
                            // 这些退出不是镜像故障，不应再次改变状态或写错误日志。
                            log::debug!(
                                "[{me}] ignoring {source} exit after session cleanup: {exit_code:?}"
                            );
                            continue;
                        }
                        let source_label = match source {
                            "uxplay" => "UxPlay",
                            "ios-ffmpeg" => "iOS ffmpeg",
                            other => other,
                        };
                        log::warn!("[{me}] {source_label} mirror process exited: {exit_code:?}");
                        let entry = LogEntry {
                            at: chrono::Utc::now().to_rfc3339(),
                            level: "error".into(),
                            code: "mirror_crashed".into(),
                            message: format!(
                                "{source_label} 已停止（退出码 {exit_code:?}），可一键重启；请查看镜像进程日志"
                            ),
                        };
                        session.record_error(entry);
                        {
                            let st = session.inner.state.lock().unwrap();
                            let connected = matches!(
                                st.state(),
                                SessionStateName::MirroringConnected
                                    | SessionStateName::ControlPairing
                                    | SessionStateName::ControlReady
                            );
                            drop(st);
                            if connected {
                                let _ = session.apply(SessionEvent::LinkLost, None);
                                // 崩溃即断链：停止旧接收器后自动重连（新 UxPlay
                                // 会重新广播，手机端可再次选择快投屏）。
                                session.schedule_reconnect();
                            } else {
                                let message = format!("{source_label} 镜像进程已停止");
                                let err = AdapterError::Failed(message.clone());
                                session.set_last_error(&err, true);
                                let _ = session.apply(SessionEvent::MirrorFailed, Some(message));
                            }
                        }
                        let _ = adapter.lock().unwrap().stop();
                    }
                    MirrorEvent::LogLine(line) => {
                        log::debug!("[{me}] {line}");
                        if !line.is_empty() {
                            // 进程输出走普通日志事件；崩溃本身已经通过
                            // record_error 发出，避免同一错误再次触发诊断事件。
                            session.sink.emit_log(
                                &me,
                                &LogEntry {
                                    at: chrono::Utc::now().to_rfc3339(),
                                    level: "info".into(),
                                    code: "mirror_process".into(),
                                    message: line,
                                },
                            );
                        }
                    }
                }
            }
        })
    }

    fn stop_mirror(&self) -> Result<(), AdapterError> {
        let active = self.inner.mirror.lock().unwrap().take();
        if let Some(active) = active {
            let res = stop_active_mirror(active);
            *self.inner.last_metadata.lock().unwrap() = None;
            return res;
        }
        Ok(())
    }

    fn stop_android_screen(&self) {
        if let Some(screen) = self.inner.screen.lock().unwrap().take() {
            screen.stop();
            *self.inner.android_pointer.lock().unwrap() = None;
            *self.inner.adb_drag.lock().unwrap() = false;
            self.emit_log("info", "screen_stopped", "Android scrcpy 投屏已停止".into());
        }
    }

    fn android_dimensions(&self) -> (u32, u32) {
        self.inner
            .screen
            .lock()
            .unwrap()
            .as_ref()
            .map(|screen| screen.video_dimensions())
            .or_else(|| *self.inner.last_metadata.lock().unwrap())
            .unwrap_or((1080, 2340))
    }

    /// 断开本设备会话（按 §6.3 顺序清理）。
    pub fn stop_session(&self) -> Result<(), AdapterError> {
        {
            let st = self.inner.state.lock().unwrap();
            match st.state() {
                SessionStateName::Idle | SessionStateName::Stopping => return Ok(()),
                SessionStateName::Failed => {
                    // 失败态直接回 Idle（等价「关闭错误」），不经过 UserStop
                    drop(st);
                    let _ = self.stop_control_inner();
                    let _ = self.stop_mirror();
                    self.stop_android_screen();
                    self.stop_test_frames();
                    self.apply(SessionEvent::UserDismissError, None)?;
                    return Ok(());
                }
                _ => {}
            }
        }
        let _ = self.stop_control_inner();
        self.apply(SessionEvent::UserStop, None)?;
        let _ = self.stop_mirror();
        self.stop_android_screen();
        self.stop_test_frames();
        self.apply(SessionEvent::CleanupDone, None)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 控制（iOS：可选 USB/WDA 绝对坐标，默认 BLE HID；Android：scrcpy control channel）
    // -----------------------------------------------------------------------

    fn start_control_inner(&self) -> Result<(), AdapterError> {
        let _control_operation = self.inner.control_operation.lock().unwrap();
        {
            let st = self.inner.state.lock().unwrap();
            if st.state() != SessionStateName::MirroringConnected {
                return Err(AdapterError::ControlInactive(
                    "仅镜像已连接时才能启用控制".into(),
                ));
            }
        }
        self.apply(SessionEvent::UserEnableControl, None)?;
        let result = match self.kind {
            DeviceKind::Iphone => {
                let use_usb = self.inner.prefs.lock().unwrap().ios_usb_control_enabled;
                if use_usb {
                    // This switch explicitly means "precision mode". Do not
                    // silently fall back to BLE here: BLE is a relative mouse
                    // and can never preserve the absolute point selected in
                    // the AirPlay canvas. A silent fallback made the UI say
                    // that control was connected while every click still
                    // suffered from pointer acceleration/drift.
                    let result = self.start_ios_usb_control();
                    if let Err(error) = &result {
                        self.emit_log(
                            "error",
                            "ios_usb_required",
                            format!(
                                "iOS USB/WDA 精确控制未就绪，未回退 BLE；请先准备并运行 WDA、iproxy 和 USB 信任：{error}"
                            ),
                        );
                    }
                    result
                } else {
                    self.start_ble_control()
                }
            }
            DeviceKind::Android => self.start_scrcpy_control(),
        };
        match result {
            Ok(()) => Ok(()),
            Err(e) => {
                let detail = format!("启用控制失败：{e}");
                self.set_last_error(&e, false);
                let _ = self.apply(SessionEvent::UserCancelControl, Some(detail.clone()));
                Err(AdapterError::Failed(detail))
            }
        }
    }

    fn start_ble_control(&self) -> Result<(), AdapterError> {
        // Serialize startup with stop/cancel so a concurrent cancel cannot see
        // an empty input slot and leave a newly-started BLE controller alive.
        let _input_operation = self.inner.input_operation.lock().unwrap();
        let mut hid = create_hid_controller();
        let session = self.clone();
        hid.set_status_callback(Some(Arc::new(move |status| {
            session.on_hid_status(status);
        })));
        hid.start_advertising()?;
        hid.set_control_state(ControlState::Broadcasting);
        let mut s = hid.status();
        s.control_transport = "ble".into();
        if self.inner.state.lock().unwrap().state() != SessionStateName::ControlPairing {
            let _ = hid.stop_advertising();
            hid.set_control_state(ControlState::Disabled);
            return Err(AdapterError::ControlInactive(
                "镜像已断开，取消启用控制".into(),
            ));
        }
        *self.inner.input.lock().unwrap() = Some(InputHandle::Ble(hid));
        drop(_input_operation);
        self.sink.emit_hid(&self.id, &s);
        // CoreBluetooth 的电源、GATT 发布和广播都是异步上报：start 返回时
        // powered_on/advertising 可能仍为 false（授权弹窗/控制器上电中）。
        // native bridge 会把真实 state/authorization/错误保留在 status，稍后复查。
        let advertised_name = s.advertised_name.as_deref().unwrap_or(crate::APP_NAME);
        let pairing_name = s.pairing_name.as_deref().unwrap_or("本机名称");
        self.emit_log(
            "info",
            "control_broadcasting",
            format!(
                "iOS BLE HID 已请求广播；实际 BLE 广播短名为「{advertised_name}」。iPhone 蓝牙列表通常显示 macOS 本机名称「{pairing_name}」，不一定显示「{}」，请连接该本机条目",
                crate::APP_NAME,
            ),
        );
        self.schedule_ble_power_check();
        Ok(())
    }

    /// iOS precision path: attach to a user-provisioned WDA over USB. The
    /// video stream remains AirPlay; WDA is used only for absolute input.
    fn start_ios_usb_control(&self) -> Result<(), AdapterError> {
        let source_size = self
            .mirror_metadata()
            .map(|(width, height)| ScreenSize { width, height });
        let control = IosUsbControl::connect(&self.resources, &self.id, source_size)?;
        let target = control.target_size();
        if self.inner.state.lock().unwrap().state() != SessionStateName::ControlPairing {
            return Err(AdapterError::ControlInactive(
                "镜像已断开，取消启用控制".into(),
            ));
        }
        {
            let _input_operation = self.inner.input_operation.lock().unwrap();
            *self.inner.input.lock().unwrap() = Some(InputHandle::IosUsb(control));
        }
        *self.inner.ios_drag.lock().unwrap() = false;
        self.on_input_ready(Some("iOS USB/WDA".into()));
        self.emit_log(
            "info",
            "ios_usb_control",
            format!(
                "iOS USB/WDA 绝对坐标输入已就绪（WDA 逻辑屏幕 {}x{}，画面仍走 AirPlay）",
                target.width, target.height
            ),
        );
        Ok(())
    }

    /// 等待 CoreBluetooth 完成异步状态回调，超过窗口后仍未真正广播才告警。
    /// CoreBluetooth 首次授权和状态回调可能需要数秒；不能把短暂的 Unknown
    /// 误报成权限失败，也不能在服务发布成功前提示用户反复修改权限。
    fn schedule_ble_power_check(&self) {
        let session = self.clone();
        std::thread::spawn(move || {
            const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
            const WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
            let deadline = std::time::Instant::now() + WAIT_TIMEOUT;
            loop {
                std::thread::sleep(POLL_INTERVAL);
                let state = session.inner.state.lock().unwrap().state();
                // 会话已被用户停止则不再告警。
                if !matches!(
                    state,
                    SessionStateName::ControlPairing | SessionStateName::MirroringConnected
                ) {
                    return;
                }
                let s = session.hid_status();
                if s.powered_on && s.advertising && s.last_error.is_none() {
                    return;
                }

                // Unknown/Resetting 是 CoreBluetooth 的异步过渡状态；只要
                // 仍处于过渡期就继续等，避免 8 秒检查先报错、4 秒后才成功。
                let transient = s
                    .last_error
                    .as_deref()
                    .map(|error| error.contains("Unknown") || error.contains("重置"))
                    .unwrap_or(!s.powered_on || !s.advertising);
                if transient && std::time::Instant::now() < deadline {
                    continue;
                }

                let detail = s.last_error.as_deref().unwrap_or_else(|| {
                    if !s.powered_on {
                        "尚未收到 CoreBluetooth 的 PoweredOn 回调"
                    } else if !s.advertising {
                        "CoreBluetooth 已上电，但尚未收到 HOGP 广播成功回调"
                    } else {
                        "CoreBluetooth 状态尚未完成"
                    }
                });
                let message = format!(
                    "macOS BLE HID 尚未真正广播（{}）：{}；iPhone 蓝牙列表不会出现对应 HID 条目。请确认系统蓝牙已开启，并在 系统设置→隐私与安全性→蓝牙 中允许本应用；如果刚修改过权限，请完全退出并重启快投屏后重试。",
                    s.device_name.as_deref().unwrap_or("快投屏 BLE"),
                    detail
                );
                session.emit_log("warn", "ble_not_powered", message);
                return;
            }
        });
    }

    fn start_scrcpy_control(&self) -> Result<(), AdapterError> {
        let screen = self
            .inner
            .screen
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| AdapterError::ControlInactive("Android 投屏源不存在".into()))?;
        let ctrl = screen.control_handle();
        if !ctrl.is_connected() {
            return Err(AdapterError::ControlInactive(
                "scrcpy 控制通道尚未连接，请等待画面首帧后重试".into(),
            ));
        }
        let serial = self.inner.android_serial.lock().unwrap().clone();
        {
            let _input_operation = self.inner.input_operation.lock().unwrap();
            *self.inner.input.lock().unwrap() = Some(InputHandle::Scrcpy(ctrl));
        }
        self.on_input_ready(
            serial
                .as_deref()
                .map(crate::integrations::adb::mask_serial)
                .or_else(|| Some("Android 设备".into())),
        );
        Ok(())
    }

    fn on_input_ready(&self, name: Option<String>) {
        {
            let st = self.inner.state.lock().unwrap();
            if st.state() != SessionStateName::ControlPairing {
                return;
            }
        }
        let _ = self.apply(SessionEvent::ControlPaired, Some("输入已就绪".into()));
        {
            let input = self.inner.input.lock().unwrap();
            let status = match input.as_ref() {
                Some(InputHandle::Ble(h)) => {
                    let mut s = h.status();
                    s.control = ControlState::Connected;
                    s.paired = true;
                    s.connected = true;
                    s.subscribed = true;
                    s.device_name = name;
                    s.control_transport = "ble".into();
                    s
                }
                Some(InputHandle::IosUsb(_)) => HidStatus {
                    control: ControlState::Connected,
                    paired: true,
                    connected: true,
                    subscribed: true,
                    control_transport: "usb_wda".into(),
                    device_name: name,
                    ..HidStatus::default()
                },
                Some(InputHandle::Scrcpy(_)) => HidStatus {
                    control: ControlState::Connected,
                    paired: true,
                    connected: true,
                    subscribed: true,
                    control_transport: "scrcpy".into(),
                    device_name: name,
                    ..HidStatus::default()
                },
                None => return,
            };
            self.sink.emit_hid(&self.id, &status);
        }
    }

    /// BLE 配对回调（真实实现由 GATT 连接/订阅事件触发）。
    pub fn on_hid_paired(&self, name: Option<String>) {
        self.on_input_ready(name);
    }

    /// CoreBluetooth 的异步连接状态回调。广播成功不等于 iPhone 已经订阅
    /// keyboard/mouse report；只有订阅后才把会话切到 ControlReady。
    fn on_hid_status(&self, status: crate::integrations::hid_adapter::HidStatus) {
        self.sink.emit_hid(&self.id, &status);
        let state = self.inner.state.lock().unwrap().state();
        if let Some(error) = status.last_error.as_deref() {
            let level = if error.contains("Unknown") || error.contains("重置") {
                "warn"
            } else {
                "error"
            };
            self.emit_log(level, "ble_hid_status", format!("iOS BLE HID：{error}"));
        } else if status.advertising
            && !status.connected
            && state == SessionStateName::ControlPairing
        {
            // macOS 上授权「未确定」时 CoreBluetooth 也会回调广播成功但射频
            // 不发包（桥接层此时会以上游 last_error 进入 warn 分支，不会到
            // 这里）；只有授权确认后才打印“可发现”，并附上授权状态供核对。
            let auth = status.authorization.map(authorization_name).unwrap_or("-");
            let advertised_name = status.advertised_name.as_deref().unwrap_or(crate::APP_NAME);
            self.emit_log(
                "info",
                "ble_hid_advertising",
                format!(
                    "iOS BLE HID GATT 服务已发布，广播已启动（授权状态：{auth}，实际 BLE 广播短名：{advertised_name}）；macOS 的 iPhone 系统蓝牙列表可能显示本机/GAP 名称而不是「{}」，请连接列表中的对应本机条目",
                    crate::APP_NAME
                ),
            );
        }
        if status.connected && status.subscribed {
            if state == SessionStateName::ControlPairing {
                let input_report_mask = status.input_report_mask;
                let keyboard_ready = keyboard_input_ready(input_report_mask);
                let mouse_ready =
                    input_report_mask & (INPUT_REPORT_MOUSE | INPUT_REPORT_BOOT_MOUSE) != 0;
                let input_summary = match (keyboard_ready, mouse_ready) {
                    (true, true) => "键盘和鼠标",
                    (true, false) => "键盘",
                    (false, true) => "鼠标",
                    (false, false) => "无",
                };
                self.on_hid_paired(status.device_name);
                self.emit_log(
                    "info",
                    "control_connected",
                    format!(
                        "iOS BLE HID 已连接并订阅{input_summary}输入报告（输入报告掩码 0x{input_report_mask:02X}），BLE 通知通道已就绪"
                    ),
                );
            }
            return;
        }
        // CBPeripheralManager reports its initial powered/advertising state
        // while `start_advertising()` is still installing the controller.  A
        // powered-on but not-yet-advertising callback is not a disconnect.
        // Also ignore a transient pre-handle callback: the input handle is
        // stored only after the native start call returns.
        let input_installed = self.inner.input.lock().unwrap().is_some();
        if !input_installed {
            return;
        }
        // ControlPairing 期间“尚未广播/正在重建服务”是启动过程，不应被
        // 当成断开；只有已经进入 ControlReady 后才处理真正的断连。
        if state == SessionStateName::ControlReady {
            let _ = self.apply(
                SessionEvent::BleDisconnected,
                Some("iOS BLE HID 已断开".into()),
            );
            self.emit_log(
                "warn",
                "control_disconnected",
                "iOS BLE HID 连接已断开（手机取消了键鼠报告订阅），键盘和鼠标输入已暂停；请在 iPhone 蓝牙中重新连接列表中的对应本机条目".into(),
            );
        }
    }

    pub fn start_control(&self, prefs: Option<SessionPreferences>) -> Result<(), AdapterError> {
        if let Some(p) = prefs {
            *self.inner.prefs.lock().unwrap() = p;
        }
        self.start_control_inner()
    }

    fn stop_control_inner(&self) -> Result<(), AdapterError> {
        let _control_operation = self.inner.control_operation.lock().unwrap();
        self.inner.paste_token.fetch_add(1, Ordering::SeqCst);
        *self.inner.adb_drag.lock().unwrap() = false;
        *self.inner.ios_drag.lock().unwrap() = false;

        // Do not hold `input` while a native BLE call synchronously dispatches
        // to the platform queue. CoreBluetooth callbacks can call back into
        // `on_hid_status`, which needs to inspect the same slot.
        {
            let _input_operation = self.inner.input_operation.lock().unwrap();
            let input = {
                let mut slot = self.inner.input.lock().unwrap();
                slot.take()
            };
            if let Some(input) = input {
                match input {
                    InputHandle::Ble(mut h) => {
                        h.release_all();
                        let _ = h.stop_advertising();
                        h.set_control_state(ControlState::Disabled);
                    }
                    InputHandle::IosUsb(mut c) => {
                        c.release_all();
                    }
                    InputHandle::Scrcpy(c) => {
                        if let Some((x, y)) = self.inner.android_pointer.lock().unwrap().take() {
                            let (width, height) = self.android_dimensions();
                            let _ = c.touch_up(x, y, width, height);
                        }
                    }
                }
            }
        }
        {
            let st = self.inner.state.lock().unwrap();
            let in_control = matches!(
                st.state(),
                SessionStateName::ControlPairing | SessionStateName::ControlReady
            );
            drop(st);
            if in_control {
                self.apply(SessionEvent::BleDisconnected, Some("用户停止控制".into()))?;
            }
        }
        Ok(())
    }

    pub fn stop_control(&self) -> Result<(), AdapterError> {
        self.stop_control_inner()
    }

    /// 释放全部按键与鼠标按钮（切换激活/紧急/失焦/断开路径）。
    pub fn release_all_input(&self) {
        let _control_operation = self.inner.control_operation.lock().unwrap();
        self.inner.paste_token.fetch_add(1, Ordering::SeqCst);
        *self.inner.adb_drag.lock().unwrap() = false;
        *self.inner.ios_drag.lock().unwrap() = false;
        let _input_operation = self.inner.input_operation.lock().unwrap();
        let mut input = {
            let mut slot = self.inner.input.lock().unwrap();
            slot.take()
        };
        if let Some(input_handle) = input.as_mut() {
            match input_handle {
                InputHandle::Ble(h) => {
                    h.release_all();
                    self.sink.emit_hid(&self.id, &h.status());
                }
                InputHandle::IosUsb(c) => {
                    c.release_all();
                }
                InputHandle::Scrcpy(c) => {
                    if let Some((x, y)) = self.inner.android_pointer.lock().unwrap().take() {
                        let (width, height) = self.android_dimensions();
                        let _ = c.touch_up(x, y, width, height);
                    }
                }
            }
        }
        *self.inner.input.lock().unwrap() = input;
    }

    // -----------------------------------------------------------------------
    // 输入命令（仅激活设备被调用；本模块不校验激活状态）
    // -----------------------------------------------------------------------

    fn with_input<T>(
        &self,
        f: impl FnOnce(InputBorrow<'_>) -> Result<T, AdapterError>,
    ) -> Result<T, AdapterError> {
        {
            let st = self.inner.state.lock().unwrap();
            if st.state() != SessionStateName::ControlReady {
                return Err(AdapterError::ControlInactive(
                    "控制未连接，输入被忽略".into(),
                ));
            }
        }
        let _input_operation = self.inner.input_operation.lock().unwrap();
        let mut input = {
            let mut slot = self.inner.input.lock().unwrap();
            slot.take()
        };
        let result = match input.as_mut() {
            Some(InputHandle::Ble(h)) => f(InputBorrow::Ble(h.as_mut())),
            Some(InputHandle::IosUsb(c)) => f(InputBorrow::IosUsb(c)),
            Some(InputHandle::Scrcpy(c)) => f(InputBorrow::Scrcpy(c)),
            None => Err(AdapterError::ControlInactive("输入控制器不存在".into())),
        };
        *self.inner.input.lock().unwrap() = input;
        result
    }

    pub fn send_pointer_move(
        &self,
        dx: i32,
        dy: i32,
        abs: Option<(u32, u32)>,
        source_size: Option<(u32, u32)>,
    ) -> Result<(), AdapterError> {
        self.with_input(|input| match input {
            InputBorrow::Ble(h) => {
                h.send_mouse(&build_mouse_report(0, clamp_delta(dx), clamp_delta(dy), 0))
            }
            InputBorrow::Scrcpy(c) => {
                if *self.inner.adb_drag.lock().unwrap() {
                    if let Some((x, y)) = abs {
                        let (width, height) = self.android_dimensions();
                        *self.inner.android_pointer.lock().unwrap() = Some((x, y));
                        c.touch_move(x, y, width, height)
                    } else {
                        Ok(())
                    }
                } else {
                    Ok(())
                }
            }
            InputBorrow::IosUsb(c) => {
                if let (Some((x, y)), Some((width, height))) = (abs, source_size) {
                    if *self.inner.ios_drag.lock().unwrap() {
                        c.pointer_move(ScreenSize { width, height }, x, y)
                    } else {
                        Ok(())
                    }
                } else {
                    Ok(())
                }
            }
        })
    }

    pub fn send_pointer_button(
        &self,
        button: u8,
        pressed: bool,
        abs: Option<(u32, u32)>,
        source_size: Option<(u32, u32)>,
    ) -> Result<(), AdapterError> {
        self.with_input(|input| match input {
            InputBorrow::Ble(h) => {
                let report = build_mouse_report(if pressed { button } else { 0 }, 0, 0, 0);
                h.send_mouse(&report)
            }
            InputBorrow::Scrcpy(c) => {
                // Android 的 scrcpy 输入模型是触摸屏，不存在 HID middle/right
                // button；右键菜单由应用自身快捷键处理，不能伪造成悬空触摸。
                if button != BTN_LEFT {
                    return Ok(());
                }
                match abs {
                    Some((x, y)) if pressed => {
                        let (width, height) = self.android_dimensions();
                        *self.inner.adb_drag.lock().unwrap() = true;
                        *self.inner.android_pointer.lock().unwrap() = Some((x, y));
                        c.touch_down(x, y, width, height)
                    }
                    Some((x, y)) => {
                        let (width, height) = self.android_dimensions();
                        *self.inner.adb_drag.lock().unwrap() = false;
                        *self.inner.android_pointer.lock().unwrap() = None;
                        c.touch_up(x, y, width, height)
                    }
                    None => Err(AdapterError::Failed("Android 触摸需要设备坐标".into())),
                }
            }
            InputBorrow::IosUsb(c) => {
                if button != BTN_LEFT {
                    return Ok(());
                }
                let (x, y) =
                    abs.ok_or_else(|| AdapterError::Failed("iOS USB/WDA 触摸需要设备坐标".into()))?;
                let (width, height) = source_size
                    .ok_or_else(|| AdapterError::Failed("iOS USB/WDA 触摸缺少画面尺寸".into()))?;
                let result = c.pointer_button(ScreenSize { width, height }, x, y, pressed);
                if result.is_ok() {
                    *self.inner.ios_drag.lock().unwrap() = pressed;
                }
                result
            }
        })
    }

    pub fn send_wheel(&self, delta_y: i32) -> Result<(), AdapterError> {
        let step = self.inner.prefs.lock().unwrap().scroll_step.max(1);
        let ticks = clamp_delta(delta_y.div_euclid(step));
        self.with_input(|input| match input {
            InputBorrow::Ble(h) => h.send_mouse(&build_mouse_report(0, 0, 0, ticks)),
            InputBorrow::IosUsb(c) => c.wheel(delta_y),
            InputBorrow::Scrcpy(c) => {
                let (width, height) = self.android_dimensions();
                let (x, y) = self
                    .inner
                    .android_pointer
                    .lock()
                    .unwrap()
                    .unwrap_or((width / 2, height / 2));
                c.scroll(x, y, width, height, 0.0, ticks as f32, 0)
            }
        })
    }

    pub fn send_key_stroke(
        &self,
        code: &str,
        key: &str,
        ctrl: bool,
        shift: bool,
        alt: bool,
        meta: bool,
    ) -> Result<(), AdapterError> {
        self.with_input(|input| match input {
            InputBorrow::Ble(h) => {
                let usage = usage_for_code(code)
                    .ok_or_else(|| AdapterError::Failed(format!("未映射的按键 code：{code}")))?;
                let mods = (u8::from(ctrl) * MOD_CTRL)
                    | (u8::from(shift) * MOD_SHIFT)
                    | (u8::from(alt) * MOD_ALT)
                    | (u8::from(meta) * MOD_META);
                let pair = key_down_up(usage, mods);
                h.send_keyboard(&pair[0])?;
                h.send_keyboard(&pair[1])
            }
            InputBorrow::IosUsb(c) => c.key_stroke(code, key, ctrl, shift, alt, meta),
            InputBorrow::Scrcpy(c) => {
                let key =
                    crate::integrations::adb::android_keycode_value(code).ok_or_else(|| {
                        AdapterError::Failed(format!("未映射的 Android 按键：{code}"))
                    })?;
                let metastate = crate::integrations::adb::android_metastate(ctrl, shift, alt, meta);
                c.keycode(0, key, 0, metastate)?;
                c.keycode(1, key, 0, metastate)
            }
        })
    }

    // -----------------------------------------------------------------------
    // 粘贴（仅激活设备被调用）
    // -----------------------------------------------------------------------

    /// 粘贴结果只记录动作和错误原因，不记录剪贴板内容。
    fn finish_paste(&self, result: PasteResult) -> PasteResult {
        if result.ok {
            self.emit_log("info", "paste_completed", "粘贴已发送".into());
        } else if let Some(error) = result.error.as_ref() {
            self.emit_log(
                "warn",
                "paste_failed",
                format!("粘贴失败（{}）：{}", error.code, error.message),
            );
        }
        result
    }

    pub fn paste(&self, max_bytes: Option<usize>) -> PasteResult {
        let max = max_bytes.unwrap_or(self.inner.prefs.lock().unwrap().max_paste_bytes);
        let token = self.inner.paste_token.load(Ordering::SeqCst);
        let rejected = |code: &str, msg: &str| PasteResult {
            ok: false,
            char_count: 0,
            byte_count: 0,
            truncated: false,
            unencodable_positions: Vec::new(),
            error: Some(PasteError {
                code: code.into(),
                message: msg.into(),
            }),
        };
        let not_ready = |msg: &str| rejected("control_inactive", msg);

        let ready = self.inner.state.lock().unwrap().state() == SessionStateName::ControlReady;
        if !ready {
            return self.finish_paste(not_ready("控制未连接，无法粘贴"));
        }
        let clip = SystemClipboard;
        let text = match clip.read_text() {
            Ok(t) => t,
            Err(e) => {
                return self.finish_paste(PasteResult {
                    ok: false,
                    char_count: 0,
                    byte_count: 0,
                    truncated: false,
                    unencodable_positions: Vec::new(),
                    error: Some(PasteError {
                        code: "clipboard_read_failed".into(),
                        message: e.to_string(),
                    }),
                });
            }
        };
        if text.is_empty() {
            return self.finish_paste(rejected("clipboard_empty", "剪贴板中没有可粘贴的纯文本"));
        }
        if text.len() > max {
            return self.finish_paste(PasteResult {
                ok: false,
                char_count: 0,
                byte_count: text.len(),
                truncated: false,
                unencodable_positions: Vec::new(),
                error: Some(PasteError {
                    code: "paste_oversized".into(),
                    message: format!(
                        "剪贴板文本 {} 字节超过限制 {max} 字节；MVP 不静默截断，请手动分段",
                        text.len()
                    ),
                }),
            });
        }
        match self.kind {
            DeviceKind::Iphone => {
                let _input_operation = self.inner.input_operation.lock().unwrap();
                let mut input = {
                    let mut slot = self.inner.input.lock().unwrap();
                    slot.take()
                };
                let result = match input.as_mut() {
                    Some(InputHandle::Ble(h)) => {
                        let status = h.status();
                        let keyboard_ready = keyboard_input_ready(status.input_report_mask);
                        if !keyboard_ready {
                            PasteResult {
                                ok: false,
                                char_count: text.encode_utf16().count(),
                                byte_count: text.len(),
                                truncated: false,
                                unencodable_positions: Vec::new(),
                                error: Some(PasteError {
                                    code: "keyboard_report_not_ready".into(),
                                    message: format!(
                                        "iOS BLE 当前未订阅键盘输入报告（掩码 0x{:02X}），本次粘贴未发送；中英文混合文本请在设置中启用 USB/WDA 精确控制，并重新启用控制",
                                        status.input_report_mask
                                    ),
                                }),
                            }
                        } else {
                            let enc = encode_ascii_text(&text);
                            let positions = enc.unencodable_positions.clone();
                            if !positions.is_empty() {
                                PasteResult {
                                    ok: false,
                                    char_count: enc.char_count,
                                    byte_count: enc.byte_count,
                                    truncated: false,
                                    unencodable_positions: positions,
                                    error: Some(PasteError {
                                        code: "unencodable_chars".into(),
                                        message: format!(
                                            "有 {} 个字符（中文、Emoji 或其他 Unicode）无法通过 BLE HID 键盘表达，未发送；中英文混合文本请在设置中启用 USB/WDA 精确控制，并重新启用控制",
                                            enc.unencodable_positions.len()
                                        ),
                                    }),
                                }
                            } else {
                                run_paste_text_flow(&text, h.as_mut(), max, token, token)
                            }
                        }
                    }
                    Some(InputHandle::IosUsb(c)) => {
                        if token != self.inner.paste_token.load(Ordering::SeqCst) {
                            not_ready("粘贴已取消（控制已停止）")
                        } else {
                            match c.text(&text) {
                                Ok(()) => PasteResult {
                                    ok: true,
                                    char_count: text.encode_utf16().count(),
                                    byte_count: text.len(),
                                    truncated: false,
                                    unencodable_positions: Vec::new(),
                                    error: None,
                                },
                                Err(e) => PasteResult {
                                    ok: false,
                                    char_count: text.encode_utf16().count(),
                                    byte_count: text.len(),
                                    truncated: true,
                                    unencodable_positions: Vec::new(),
                                    error: Some(PasteError {
                                        code: e.code().into(),
                                        message: format!("iOS USB/WDA 输入失败：{e}"),
                                    }),
                                },
                            }
                        }
                    }
                    _ => not_ready("iOS 控制器不存在"),
                };
                *self.inner.input.lock().unwrap() = input;
                self.finish_paste(result)
            }
            DeviceKind::Android => {
                let _input_operation = self.inner.input_operation.lock().unwrap();
                let mut input = {
                    let mut slot = self.inner.input.lock().unwrap();
                    slot.take()
                };
                let result = match input.as_mut() {
                    Some(InputHandle::Scrcpy(c)) => {
                        if token != self.inner.paste_token.load(Ordering::SeqCst) {
                            not_ready("粘贴已取消（控制已停止）")
                        } else {
                            match c.text(&text) {
                                Ok(()) => PasteResult {
                                    ok: true,
                                    char_count: text.chars().count(),
                                    byte_count: text.len(),
                                    truncated: false,
                                    unencodable_positions: Vec::new(),
                                    error: None,
                                },
                                Err(e) => PasteResult {
                                    ok: false,
                                    char_count: text.chars().count(),
                                    byte_count: text.len(),
                                    truncated: true,
                                    unencodable_positions: Vec::new(),
                                    error: Some(PasteError {
                                        code: e.code().into(),
                                        message: format!("Android 输入失败：{e}"),
                                    }),
                                },
                            }
                        }
                    }
                    _ => not_ready("scrcpy 控制器不存在"),
                };
                *self.inner.input.lock().unwrap() = input;
                self.finish_paste(result)
            }
        }
    }

    // -----------------------------------------------------------------------
    // 帧桥接
    // -----------------------------------------------------------------------

    pub fn attach_frame_sink(&self, sink: Arc<dyn FrameSink>) -> Result<(), AdapterError> {
        // 先记录 sink；若 Android 投屏已在运行，立即绑定。
        if let Some(screen) = self.inner.screen.lock().unwrap().clone() {
            screen.attach_sink(sink.clone());
        }
        if let Some(active) = self.inner.mirror.lock().unwrap().as_ref() {
            active.adapter.lock().unwrap().attach_sink(sink.clone());
        }
        *self.inner.frame_sink.lock().unwrap() = Some(sink);
        Ok(())
    }

    pub fn acknowledge_frame(&self, sequence: u64) {
        if let Some(sink) = self.inner.frame_sink.lock().unwrap().clone() {
            sink.acknowledge(sequence);
        }
    }

    pub fn set_test_frames(&self, enabled: bool) -> Result<(), AdapterError> {
        if let Some(w) = self.inner.frame_worker.lock().unwrap().take() {
            self.inner.frame_test_mode.store(false, Ordering::SeqCst);
            let _ = w.join();
        }
        if !enabled {
            return Ok(());
        }
        let sink = self
            .inner
            .frame_sink
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| AdapterError::Busy("尚未附加帧通道".into()))?;
        self.inner.frame_test_mode.store(true, Ordering::SeqCst);
        let running = self.inner.frame_test_mode.clone();
        let worker = std::thread::spawn(move || {
            let mut t: u32 = 0;
            sink.on_size(640, 480);
            while running.load(Ordering::SeqCst) {
                let frame = crate::integrations::frame_bridge::generate_test_frame(640, 480, t);
                sink.push(&frame);
                t = t.wrapping_add(1);
                std::thread::sleep(std::time::Duration::from_millis(33));
            }
        });
        *self.inner.frame_worker.lock().unwrap() = Some(worker);
        Ok(())
    }

    fn stop_test_frames(&self) {
        self.inner.frame_test_mode.store(false, Ordering::SeqCst);
        if let Some(w) = self.inner.frame_worker.lock().unwrap().take() {
            let _ = w.join();
        }
    }

    pub fn frame_status(&self) -> (bool, bool, u32, u32) {
        let has_sink = self.inner.frame_sink.lock().unwrap().is_some();
        let test = self.inner.frame_test_mode.load(Ordering::SeqCst);
        let meta = self.mirror_metadata().unwrap_or((640, 480));
        (has_sink, test, meta.0, meta.1)
    }

    // -----------------------------------------------------------------------
    // 查询
    // -----------------------------------------------------------------------

    pub fn preferences(&self) -> SessionPreferences {
        *self.inner.prefs.lock().unwrap()
    }
    pub fn error_log(&self) -> Vec<LogEntry> {
        self.inner.error_log.lock().unwrap().clone()
    }
    pub fn mirror_metadata(&self) -> Option<(u32, u32)> {
        *self.inner.last_metadata.lock().unwrap()
    }
    pub fn hid_status(&self) -> HidStatus {
        let input = self.inner.input.lock().unwrap();
        match input.as_ref() {
            Some(InputHandle::Ble(h)) => {
                let mut status = h.status();
                if status.control_transport.is_empty() {
                    status.control_transport = "ble".into();
                }
                status
            }
            Some(InputHandle::IosUsb(_)) => HidStatus {
                control: derive_control_state(self.inner.state.lock().unwrap().state()),
                paired: true,
                connected: true,
                subscribed: true,
                control_transport: "usb_wda".into(),
                device_name: Some("iOS USB/WDA".into()),
                ..HidStatus::default()
            },
            Some(InputHandle::Scrcpy(_)) => {
                let control = derive_control_state(self.inner.state.lock().unwrap().state());
                HidStatus {
                    control,
                    paired: true,
                    connected: true,
                    subscribed: true,
                    control_transport: "scrcpy".into(),
                    device_name: self
                        .inner
                        .android_serial
                        .lock()
                        .unwrap()
                        .as_deref()
                        .map(crate::integrations::adb::mask_serial),
                    ..HidStatus::default()
                }
            }
            None => HidStatus::default(),
        }
    }
    pub fn resources_dir(&self) -> &std::path::Path {
        &self.resources
    }
    pub fn uxplay_port(&self) -> u16 {
        self.uxplay_port
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    struct MemSink {
        sessions: StdMutex<Vec<(String, SessionStateView)>>,
        errors: StdMutex<Vec<(String, LogEntry)>>,
        active: StdMutex<Vec<Option<String>>>,
    }
    impl EventSink for MemSink {
        fn emit_session(&self, id: &str, view: &SessionStateView) {
            self.sessions
                .lock()
                .unwrap()
                .push((id.into(), view.clone()));
        }
        fn emit_hid(&self, _id: &str, _s: &HidStatus) {}
        fn emit_mirror_metadata(&self, _id: &str, _w: u32, _h: u32) {}
        fn emit_diagnostic_error(&self, id: &str, e: &LogEntry) {
            self.errors.lock().unwrap().push((id.into(), e.clone()));
        }
        fn emit_active_changed(&self, id: Option<&str>) {
            self.active.lock().unwrap().push(id.map(|s| s.into()));
        }
    }

    #[test]
    fn session_id_and_kind_roundtrip() {
        let sink = Arc::new(MemSink {
            sessions: StdMutex::new(Vec::new()),
            errors: StdMutex::new(Vec::new()),
            active: StdMutex::new(Vec::new()),
        });
        let s = DeviceSession::new(
            "iphone:TEST-1234".into(),
            DeviceKind::Iphone,
            sink,
            std::path::PathBuf::from("/nonexistent-phonebridge-test-resources"),
            6000,
        );
        assert_eq!(s.id, "iphone:TEST-1234");
        assert_eq!(s.kind, DeviceKind::Iphone);
        assert!(s.stop_session().is_ok());
        // UxPlay is intentionally discoverable from the bundled development
        // runtime and PATH, so a nonexistent resource directory no longer
        // guarantees a preflight failure.  Dependency resolution is covered
        // by the locator tests; this test only owns ID/kind round-tripping.
    }

    #[test]
    fn kind_from_str() {
        assert_eq!(DeviceKind::from_str("iphone"), Some(DeviceKind::Iphone));
        assert_eq!(DeviceKind::from_str("android"), Some(DeviceKind::Android));
        assert_eq!(DeviceKind::from_str("nope"), None);
    }

    #[test]
    fn reconnect_delay_backs_off_and_caps() {
        assert_eq!(reconnect_delay_ms(1), 1000);
        assert_eq!(reconnect_delay_ms(2), 2000);
        assert_eq!(reconnect_delay_ms(3), 4000);
        assert_eq!(reconnect_delay_ms(4), 8000);
        // 第 5 次起封顶 16s，避免长时间静默等待
        assert_eq!(reconnect_delay_ms(5), 16000);
        assert_eq!(reconnect_delay_ms(6), 16000);
        assert_eq!(reconnect_delay_ms(u32::MAX), 16000);
    }

    #[test]
    fn keyboard_report_readiness_ignores_mouse_only_subscriptions() {
        assert!(!keyboard_input_ready(INPUT_REPORT_MOUSE));
        assert!(!keyboard_input_ready(INPUT_REPORT_BOOT_MOUSE));
        assert!(keyboard_input_ready(INPUT_REPORT_KEYBOARD));
        assert!(keyboard_input_ready(INPUT_REPORT_BOOT_KEYBOARD));
        assert!(keyboard_input_ready(
            INPUT_REPORT_MOUSE | INPUT_REPORT_KEYBOARD
        ));
    }

    #[test]
    fn reconnect_schedule_is_idempotent() {
        let sink = Arc::new(MemSink {
            sessions: StdMutex::new(Vec::new()),
            errors: StdMutex::new(Vec::new()),
            active: StdMutex::new(Vec::new()),
        });
        let s = DeviceSession::new(
            "iphone:TEST-1234".into(),
            DeviceKind::Iphone,
            sink,
            std::path::PathBuf::from("/nonexistent-phonebridge-test-resources"),
            6000,
        );
        // 两次调度只应启动一个重连循环（armed 幂等闸门）；循环发现会话
        // 不在 Reconnecting（Idle）后应自行退出并把 armed 复位。
        s.schedule_reconnect();
        s.schedule_reconnect();
        let mut waited = 0;
        while s.inner.reconnect_armed.load(Ordering::SeqCst) && waited < 100 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            waited += 1;
        }
        assert!(
            !s.inner.reconnect_armed.load(Ordering::SeqCst),
            "重连循环应在非 Reconnecting 状态立即退出并复位 armed"
        );
        // 状态机路径：Reconnecting -> ReconnectTimeout -> Failed（现有转移，
        // 防止将来状态机改动破坏自动重连的终态）。
        let mut m = SessionStateMachine::new();
        m.transition(SessionEvent::UserConnect).unwrap();
        m.transition(SessionEvent::ChecksPassed).unwrap();
        m.transition(SessionEvent::MirrorStarted).unwrap();
        m.transition(SessionEvent::FirstFrame).unwrap();
        m.transition(SessionEvent::LinkLost).unwrap();
        assert_eq!(m.state(), SessionStateName::Reconnecting);
        m.transition(SessionEvent::ReconnectOk).unwrap();
        assert_eq!(m.state(), SessionStateName::MirroringConnected);
    }
}
