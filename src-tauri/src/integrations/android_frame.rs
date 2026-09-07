//! Android 投屏适配器：参考 scrcpy 的宿主/设备端分层。
//!
//! 链路：
//!
//! adb push + adb reverse/forward
//!          └─ scrcpy-server（运行在 Android 设备上，MediaCodec + Surface）
//!                 ├─ video socket：原始 H264 → ffmpeg → RGBA → FrameSink
//!                 └─ control socket：scrcpy 二进制控制协议
//!
//! 快投屏不启动 scrcpy 的原生窗口。Rust 只负责会话生命周期、socket 和解码，
//! React 模拟器窗口只接收 RGBA 帧并绘制到 canvas。ADB 的 shell input 保留在
//! adb.rs 作为诊断/兼容工具，但不是连续输入主链路。

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::frame_bridge::{FrameSink, RgbaFrame};
use super::process::hidden_command;
use super::AdapterError;

const SCRCPY_SERVER_VERSION: &str = "4.0";
const SCRCPY_REMOTE_SERVER: &str = "/data/local/tmp/phonebridge-scrcpy-server.jar";
/// scrcpy 的 `max_size` 是最长边，不是宽度。保留 1080 的真实像素级别，
/// 避免先压成约 221x480 再在无边框窗口里放大造成糊图。
const MAX_FRAME_DIMENSION: u32 = 1080;
const SERVER_MAX_FPS: u32 = 30;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);
/// scrcpy control_msg.h 的 SC_POINTER_ID_GENERIC_FINGER（UINT64_C(-2)）。
const SCRCPY_POINTER_ID_GENERIC_FINGER: u64 = u64::MAX - 1;

static NEXT_SCID: AtomicU32 = AtomicU32::new(0x1357_9bdf);

/// FFmpeg 可执行文件定位：应用内置 sidecar 优先，其次环境变量与常见位置/PATH。
pub fn locate_ffmpeg(resources: &Path) -> Option<PathBuf> {
    super::adb::resolve_tool(resources, "ffmpeg", "PHONEBRIDGE_FFMPEG_PATH").ok()
}

/// 定位与 scrcpy-server 匹配的 server jar。
///
/// Homebrew 的 scrcpy 安装把 jar 放在 share/scrcpy/scrcpy-server，而 Tauri
/// 发布包把它放入 resources/binaries/。两种布局都支持；不自动下载，避免把
/// 未审计的第三方构建物带进应用。
pub fn locate_scrcpy_server(resources: &Path) -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PHONEBRIDGE_SCRCPY_SERVER_PATH") {
        let path = PathBuf::from(path.trim());
        if path.is_file() {
            return Some(path);
        }
    }

    let mut candidates = vec![
        resources.join("binaries/scrcpy-server"),
        resources.join("binaries/scrcpy-server.jar"),
        resources.join("scrcpy-server"),
        resources.join("scrcpy-server.jar"),
        PathBuf::from("/opt/homebrew/opt/scrcpy/share/scrcpy/scrcpy-server"),
        PathBuf::from("/opt/homebrew/share/scrcpy/scrcpy-server"),
        PathBuf::from("/usr/local/opt/scrcpy/share/scrcpy/scrcpy-server"),
        PathBuf::from("/usr/share/scrcpy/scrcpy-server"),
    ];

    if let Ok(scrcpy) = super::adb::resolve_tool(resources, "scrcpy", "PHONEBRIDGE_SCRCPY_PATH") {
        if let Some(parent) = scrcpy.parent() {
            candidates.push(parent.join("scrcpy-server"));
            candidates.push(parent.join("../share/scrcpy/scrcpy-server"));
            candidates.push(parent.join("../../share/scrcpy/scrcpy-server"));
        }
    }

    candidates.dedup();
    candidates.into_iter().find(|path| path.is_file())
}

pub fn scrcpy_server_version() -> &'static str {
    SCRCPY_SERVER_VERSION
}

fn next_scid() -> String {
    let value = NEXT_SCID.fetch_add(1, Ordering::Relaxed) & 0x7fff_ffff;
    let value = if value == 0 { 1 } else { value };
    format!("{value:08x}")
}

fn scaled_dimensions(width: u32, height: u32) -> (u32, u32) {
    let width = width.max(2) as u64;
    let height = height.max(2) as u64;
    let longest = width.max(height);
    let limit = MAX_FRAME_DIMENSION.max(2) as u64;

    let (scaled_width, scaled_height) = if longest > limit {
        // scrcpy 会按最长边缩放；使用 floor 后再取偶数，保证 H264/解码器
        // 的尺寸稳定且不超过 max_size。
        (width * limit / longest, height * limit / longest)
    } else {
        (width, height)
    };

    fn even_at_least_two(value: u64) -> u32 {
        ((value.max(2).min(u32::MAX as u64) as u32) & !1).max(2)
    }

    (
        even_at_least_two(scaled_width),
        even_at_least_two(scaled_height),
    )
}

fn sanitized_child_error(operation: &str, status: &std::process::ExitStatus) -> AdapterError {
    AdapterError::Failed(format!("{operation}失败（进程状态 {status}）"))
}

fn push_server(adb: &Path, serial: &str, server: &Path) -> Result<(), AdapterError> {
    let output = hidden_command(adb)
        .arg("-s")
        .arg(serial)
        .arg("push")
        .arg(server)
        .arg(SCRCPY_REMOTE_SERVER)
        .output()
        .map_err(|e| AdapterError::Failed(format!("执行 adb push scrcpy-server 失败：{e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(sanitized_child_error("推送 scrcpy-server", &output.status))
    }
}

fn setup_reverse(
    adb: &Path,
    serial: &str,
    socket_name: &str,
    port: u16,
) -> Result<(), AdapterError> {
    let output = hidden_command(adb)
        .arg("-s")
        .arg(serial)
        .arg("reverse")
        .arg(format!("localabstract:{socket_name}"))
        .arg(format!("tcp:{port}"))
        .output()
        .map_err(|e| AdapterError::Failed(format!("建立 adb reverse 失败：{e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(sanitized_child_error("建立 adb reverse", &output.status))
    }
}

fn setup_forward(
    adb: &Path,
    serial: &str,
    socket_name: &str,
    port: u16,
) -> Result<(), AdapterError> {
    let output = hidden_command(adb)
        .arg("-s")
        .arg(serial)
        .arg("forward")
        .arg(format!("tcp:{port}"))
        .arg(format!("localabstract:{socket_name}"))
        .output()
        .map_err(|e| AdapterError::Failed(format!("建立 adb forward 失败：{e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(sanitized_child_error("建立 adb forward", &output.status))
    }
}

fn remove_reverse(adb: &Path, serial: &str, socket_name: &str) {
    let _ = hidden_command(adb)
        .arg("-s")
        .arg(serial)
        .arg("reverse")
        .arg("--remove")
        .arg(format!("localabstract:{socket_name}"))
        .output();
}

fn remove_forward(adb: &Path, serial: &str, port: u16) {
    let _ = hidden_command(adb)
        .arg("-s")
        .arg(serial)
        .arg("forward")
        .arg("--remove")
        .arg(format!("tcp:{port}"))
        .output();
}

struct ScrcpyCleanup {
    adb: PathBuf,
    serial: String,
    socket_name: String,
    port: u16,
    tunnel_forward: bool,
}

fn cleanup_scrcpy(cleanup: &ScrcpyCleanup) {
    if cleanup.tunnel_forward {
        remove_forward(&cleanup.adb, &cleanup.serial, cleanup.port);
    } else {
        remove_reverse(&cleanup.adb, &cleanup.serial, &cleanup.socket_name);
    }
    let _ = hidden_command(&cleanup.adb)
        .arg("-s")
        .arg(&cleanup.serial)
        .args(["shell", "rm", "-f", SCRCPY_REMOTE_SERVER])
        .output();
}

fn start_server(
    adb: &Path,
    serial: &str,
    scid: &str,
    tunnel_forward: bool,
) -> Result<Child, AdapterError> {
    let args = [
        "-s".to_string(),
        serial.to_string(),
        "shell".to_string(),
        format!("CLASSPATH={SCRCPY_REMOTE_SERVER}"),
        "app_process".to_string(),
        "/".to_string(),
        "com.genymobile.scrcpy.Server".to_string(),
        SCRCPY_SERVER_VERSION.to_string(),
        format!("scid={scid}"),
        format!("tunnel_forward={tunnel_forward}"),
        "audio=false".to_string(),
        "control=true".to_string(),
        "cleanup=true".to_string(),
        "raw_stream=true".to_string(),
        format!("max_size={MAX_FRAME_DIMENSION}"),
        format!("max_fps={SERVER_MAX_FPS}"),
    ];
    hidden_command(adb)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| AdapterError::Failed(format!("启动 scrcpy-server 失败：{e}")))
}

fn spawn_log_reader<R: Read + Send + 'static>(
    reader: R,
    prefix: &'static str,
    log: Option<Arc<dyn Fn(String) + Send + Sync>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Some(callback) = &log {
                        let text = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                        if !text.is_empty() {
                            callback(format!("{prefix}: {text}"));
                        }
                    }
                }
            }
        }
    })
}

fn accept_until(
    listener: &TcpListener,
    running: &AtomicBool,
    deadline: Instant,
) -> std::io::Result<Option<TcpStream>> {
    loop {
        if !running.load(Ordering::SeqCst) {
            return Ok(None);
        }
        match listener.accept() {
            Ok((stream, _)) => return Ok(Some(stream)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
}

enum ScrcpyTransport {
    Reverse(TcpListener),
    Forward(u16),
}

fn connect_until(
    port: u16,
    running: &AtomicBool,
    deadline: Instant,
) -> std::io::Result<Option<TcpStream>> {
    loop {
        if !running.load(Ordering::SeqCst) {
            return Ok(None);
        }
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => return Ok(Some(stream)),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                // adb forward 建立后，设备端 LocalServerSocket 可能还没开始
                // accept；按 scrcpy 客户端的方式短暂重试，而不是误报黑屏。
                if !matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::NotFound
                ) {
                    return Err(error);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn next_connection(
    transport: &ScrcpyTransport,
    running: &AtomicBool,
    deadline: Instant,
) -> std::io::Result<Option<TcpStream>> {
    match transport {
        ScrcpyTransport::Reverse(listener) => accept_until(listener, running, deadline),
        ScrcpyTransport::Forward(port) => connect_until(*port, running, deadline),
    }
}

// ---------------------------------------------------------------------------
// scrcpy control channel
// ---------------------------------------------------------------------------

fn keycode_packet(action: u8, keycode: u32, repeat: u32, metastate: u32) -> Vec<u8> {
    let mut packet = Vec::with_capacity(14);
    packet.push(0);
    packet.push(action);
    packet.extend_from_slice(&keycode.to_be_bytes());
    packet.extend_from_slice(&repeat.to_be_bytes());
    packet.extend_from_slice(&metastate.to_be_bytes());
    packet
}

fn text_packet(text: &str) -> Result<Vec<u8>, AdapterError> {
    let bytes = text.as_bytes();
    if bytes.len() > u32::MAX as usize {
        return Err(AdapterError::Failed("scrcpy 文本过长".into()));
    }
    let mut packet = Vec::with_capacity(5 + bytes.len());
    packet.push(1);
    packet.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    packet.extend_from_slice(bytes);
    Ok(packet)
}

fn touch_packet(
    action: u8,
    pointer_id: u64,
    x: u32,
    y: u32,
    screen_width: u32,
    screen_height: u32,
    pressure: f32,
    action_button: u32,
    buttons: u32,
) -> Vec<u8> {
    let mut packet = Vec::with_capacity(32);
    packet.push(2);
    packet.push(action);
    packet.extend_from_slice(&pointer_id.to_be_bytes());
    packet.extend_from_slice(&x.to_be_bytes());
    packet.extend_from_slice(&y.to_be_bytes());
    packet.extend_from_slice(&(screen_width.min(u16::MAX as u32) as u16).to_be_bytes());
    packet.extend_from_slice(&(screen_height.min(u16::MAX as u32) as u16).to_be_bytes());
    let pressure = (pressure.clamp(0.0, 1.0) * u16::MAX as f32).round() as u16;
    packet.extend_from_slice(&pressure.to_be_bytes());
    packet.extend_from_slice(&action_button.to_be_bytes());
    packet.extend_from_slice(&buttons.to_be_bytes());
    packet
}

fn scroll_axis(value: f32) -> i16 {
    value.round().clamp(-16.0, 16.0) as i16
}

fn scroll_packet(
    x: u32,
    y: u32,
    screen_width: u32,
    screen_height: u32,
    hscroll: f32,
    vscroll: f32,
    buttons: u32,
) -> Vec<u8> {
    let mut packet = Vec::with_capacity(21);
    packet.push(3);
    packet.extend_from_slice(&x.to_be_bytes());
    packet.extend_from_slice(&y.to_be_bytes());
    packet.extend_from_slice(&(screen_width.min(u16::MAX as u32) as u16).to_be_bytes());
    packet.extend_from_slice(&(screen_height.min(u16::MAX as u32) as u16).to_be_bytes());
    packet.extend_from_slice(&scroll_axis(hscroll).to_be_bytes());
    packet.extend_from_slice(&scroll_axis(vscroll).to_be_bytes());
    packet.extend_from_slice(&buttons.to_be_bytes());
    packet
}

/// 对 scrcpy ControlMessage 的小型、可测试封装。
///
/// 每个 packet 在同一把 mutex 下完整写出，避免多个输入事件交错；连接断开后
/// 不会回退到未经确认的 ADB 文本命令，而是把错误返回给会话层。
#[derive(Clone)]
pub struct ScrcpyControl {
    stream: Arc<Mutex<Option<TcpStream>>>,
}

impl ScrcpyControl {
    fn new(stream: Arc<Mutex<Option<TcpStream>>>) -> Self {
        Self { stream }
    }

    pub fn is_connected(&self) -> bool {
        self.stream.lock().unwrap().is_some()
    }

    fn send(&self, packet: &[u8]) -> Result<(), AdapterError> {
        let mut guard = self.stream.lock().unwrap();
        let stream = guard
            .as_mut()
            .ok_or_else(|| AdapterError::ControlInactive("scrcpy 控制通道未连接".into()))?;
        if let Err(error) = stream.write_all(packet) {
            // 不保留一个已经写坏的 socket；否则前端下一次点击只会继续
            // 静默失败，看起来像“控制没有反应”。
            *guard = None;
            return Err(AdapterError::Failed(format!(
                "写入 scrcpy 控制通道失败：{error}"
            )));
        }
        Ok(())
    }

    /// scrcpy control message type 0：keycode。
    pub fn keycode(
        &self,
        action: u8,
        keycode: u32,
        repeat: u32,
        metastate: u32,
    ) -> Result<(), AdapterError> {
        self.send(&keycode_packet(action, keycode, repeat, metastate))
    }

    /// scrcpy control message type 1：UTF-8 文本。
    pub fn text(&self, text: &str) -> Result<(), AdapterError> {
        self.send(&text_packet(text)?)
    }

    /// scrcpy control message type 2：触摸事件。
    pub fn touch(
        &self,
        action: u8,
        pointer_id: u64,
        x: u32,
        y: u32,
        screen_width: u32,
        screen_height: u32,
        pressure: f32,
        action_button: u32,
        buttons: u32,
    ) -> Result<(), AdapterError> {
        self.send(&touch_packet(
            action,
            pointer_id,
            x,
            y,
            screen_width,
            screen_height,
            pressure,
            action_button,
            buttons,
        ))
    }

    /// scrcpy control message type 3：滚轮。
    pub fn scroll(
        &self,
        x: u32,
        y: u32,
        screen_width: u32,
        screen_height: u32,
        hscroll: f32,
        vscroll: f32,
        buttons: u32,
    ) -> Result<(), AdapterError> {
        self.send(&scroll_packet(
            x,
            y,
            screen_width,
            screen_height,
            hscroll,
            vscroll,
            buttons,
        ))
    }

    pub fn touch_down(&self, x: u32, y: u32, width: u32, height: u32) -> Result<(), AdapterError> {
        self.touch(
            0,
            SCRCPY_POINTER_ID_GENERIC_FINGER,
            x,
            y,
            width,
            height,
            1.0,
            0,
            0,
        )
    }

    pub fn touch_move(&self, x: u32, y: u32, width: u32, height: u32) -> Result<(), AdapterError> {
        self.touch(
            2,
            SCRCPY_POINTER_ID_GENERIC_FINGER,
            x,
            y,
            width,
            height,
            1.0,
            0,
            0,
        )
    }

    pub fn touch_up(&self, x: u32, y: u32, width: u32, height: u32) -> Result<(), AdapterError> {
        self.touch(
            1,
            SCRCPY_POINTER_ID_GENERIC_FINGER,
            x,
            y,
            width,
            height,
            0.0,
            0,
            0,
        )
    }
}

// ---------------------------------------------------------------------------
// 视频源
// ---------------------------------------------------------------------------

/// Android 投屏会话：设备端 scrcpy-server + 宿主 H264 解码器。
pub struct AndroidScreenSource {
    running: Arc<AtomicBool>,
    adb_child: Mutex<Option<Child>>,
    decoder: Mutex<Option<Child>>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    sink: Arc<Mutex<Option<Arc<dyn FrameSink>>>>,
    control_stream: Arc<Mutex<Option<TcpStream>>>,
    on_log: Mutex<Option<Arc<dyn Fn(String) + Send + Sync>>>,
    on_first_frame: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    on_control_ready: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    on_failure: Mutex<Option<Arc<dyn Fn(String) + Send + Sync>>>,
    first_frame: Arc<AtomicBool>,
    cleanup: Mutex<Option<ScrcpyCleanup>>,
    width: u32,
    height: u32,
}

impl AndroidScreenSource {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            adb_child: Mutex::new(None),
            decoder: Mutex::new(None),
            workers: Mutex::new(Vec::new()),
            sink: Arc::new(Mutex::new(None)),
            control_stream: Arc::new(Mutex::new(None)),
            on_log: Mutex::new(None),
            on_first_frame: Mutex::new(None),
            on_control_ready: Mutex::new(None),
            on_failure: Mutex::new(None),
            first_frame: Arc::new(AtomicBool::new(false)),
            cleanup: Mutex::new(None),
            width: width.max(2),
            height: height.max(2),
        }
    }

    /// 注册日志回调（不转发帧、输入内容或设备身份）。
    pub fn set_log(&self, f: Arc<dyn Fn(String) + Send + Sync>) {
        *self.on_log.lock().unwrap() = Some(f);
    }

    pub fn set_on_first_frame(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *self.on_first_frame.lock().unwrap() = Some(f);
    }

    pub fn set_on_control_ready(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *self.on_control_ready.lock().unwrap() = Some(f);
    }

    pub fn set_on_failure(&self, f: Arc<dyn Fn(String) + Send + Sync>) {
        *self.on_failure.lock().unwrap() = Some(f);
    }

    pub fn attach_sink(&self, sink: Arc<dyn FrameSink>) {
        let (width, height) = scaled_dimensions(self.width, self.height);
        sink.on_size(width, height);
        *self.sink.lock().unwrap() = Some(sink);
    }

    pub fn control_handle(&self) -> ScrcpyControl {
        ScrcpyControl::new(self.control_stream.clone())
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// scrcpy 视频 socket 的尺寸，也是控制协议 position.screen_size 必须使用的尺寸。
    /// 它与设备物理/逻辑屏幕尺寸不同：server 可能因 max_size 缩放视频。
    pub fn video_dimensions(&self) -> (u32, u32) {
        scaled_dimensions(self.width, self.height)
    }

    fn log(&self, message: String) {
        if let Some(callback) = self.on_log.lock().unwrap().clone() {
            callback(message);
        }
    }

    fn fail_from_worker(
        running: &AtomicBool,
        on_log: &Option<Arc<dyn Fn(String) + Send + Sync>>,
        on_failure: &Option<Arc<dyn Fn(String) + Send + Sync>>,
        message: String,
    ) {
        if running.swap(false, Ordering::SeqCst) {
            if let Some(callback) = on_log {
                callback(message.clone());
            }
            if let Some(callback) = on_failure {
                callback(message);
            }
        }
    }

    /// 启动 scrcpy-server、ADB 隧道、视频解码与控制通道。
    ///
    /// 启动函数只负责资源建立；已连接必须由真正解码出第一帧的回调确认。
    pub fn start(&self, resources: &Path, adb: &Path, serial: &str) -> Result<(), AdapterError> {
        if self.running.load(Ordering::SeqCst) {
            return Err(AdapterError::Busy("Android 投屏已在运行".into()));
        }
        if self.adb_child.lock().unwrap().is_some()
            || self.decoder.lock().unwrap().is_some()
            || self.cleanup.lock().unwrap().is_some()
        {
            self.stop();
        }

        let ffmpeg = locate_ffmpeg(resources).ok_or_else(|| {
            AdapterError::DependencyMissing(
                "未找到 ffmpeg（应用内置或 PATH）。scrcpy H264 解码依赖 ffmpeg，请先运行 pnpm sidecars".into(),
            )
        })?;
        let server = locate_scrcpy_server(resources).ok_or_else(|| {
            AdapterError::DependencyMissing(
                "未找到 scrcpy-server（需与客户端版本匹配）。请运行 pnpm sidecars，或设置 PHONEBRIDGE_SCRCPY_SERVER_PATH".into(),
            )
        })?;
        if !adb.exists() {
            return Err(AdapterError::DependencyMissing(format!(
                "未找到 adb（{}）",
                adb.display()
            )));
        }

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| AdapterError::Failed(format!("创建 scrcpy 本地通道失败：{e}")))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| AdapterError::Failed(format!("配置 scrcpy 本地通道失败：{e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| AdapterError::Failed(format!("读取 scrcpy 本地端口失败：{e}")))?
            .port();
        let scid = next_scid();
        let socket_name = format!("scrcpy_{scid}");
        let mut cleanup = ScrcpyCleanup {
            adb: adb.to_path_buf(),
            serial: serial.to_string(),
            socket_name: socket_name.clone(),
            port,
            tunnel_forward: false,
        };

        if let Err(error) = push_server(adb, serial, &server) {
            cleanup_scrcpy(&cleanup);
            return Err(error);
        }

        // scrcpy 的宿主端先尝试 reverse；部分 Android/ADB 组合不支持 reverse
        // 或被策略禁用时，回退到 forward，并让设备端 server 使用相反的隧道方向。
        let transport = match setup_reverse(adb, serial, &socket_name, port) {
            Ok(()) => ScrcpyTransport::Reverse(listener),
            Err(reverse_error) => {
                remove_reverse(adb, serial, &socket_name);
                drop(listener);
                if let Err(forward_error) = setup_forward(adb, serial, &socket_name, port) {
                    cleanup_scrcpy(&cleanup);
                    return Err(AdapterError::Failed(format!(
                        "建立 adb reverse 失败（{reverse_error}）；adb forward 回退失败（{forward_error}）"
                    )));
                }
                cleanup.tunnel_forward = true;
                ScrcpyTransport::Forward(port)
            }
        };
        let tunnel_mode = if cleanup.tunnel_forward {
            "adb forward"
        } else {
            "adb reverse"
        };

        let mut server_child = match start_server(adb, serial, &scid, cleanup.tunnel_forward) {
            Ok(child) => child,
            Err(error) => {
                cleanup_scrcpy(&cleanup);
                return Err(error);
            }
        };
        let server_stdout = server_child.stdout.take().expect("scrcpy stdout 已 piped");
        let server_stderr = server_child.stderr.take().expect("scrcpy stderr 已 piped");

        let (target_w, target_h) = scaled_dimensions(self.width, self.height);
        let filter = format!("scale={target_w}:{target_h}:flags=fast_bilinear");
        let mut decoder = match hidden_command(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-flags",
                "low_delay",
                // scrcpy 是持续输出的裸 H264 流；缩小探测窗口，避免 ffmpeg
                // 为等待更多 SPS/时间戳而缓存数秒，首帧和后续输入才能接近实时。
                "-probesize",
                "32",
                "-analyzeduration",
                "0",
                "-f",
                "h264",
                "-i",
                "pipe:0",
                "-an",
                "-sn",
                "-dn",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "-vf",
                &filter,
                "-flush_packets",
                "1",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let _ = server_child.kill();
                let _ = server_child.wait();
                cleanup_scrcpy(&cleanup);
                return Err(AdapterError::Failed(format!("启动 ffmpeg 失败：{error}")));
            }
        };
        let decoder_stdin = decoder.stdin.take().expect("ffmpeg stdin 已 piped");
        let decoder_stdout = decoder.stdout.take().expect("ffmpeg stdout 已 piped");
        let decoder_stderr = decoder.stderr.take().expect("ffmpeg stderr 已 piped");

        *self.control_stream.lock().unwrap() = None;
        self.first_frame.store(false, Ordering::SeqCst);
        self.running.store(true, Ordering::SeqCst);
        *self.cleanup.lock().unwrap() = Some(cleanup);
        *self.adb_child.lock().unwrap() = Some(server_child);
        *self.decoder.lock().unwrap() = Some(decoder);

        let log = self.on_log.lock().unwrap().clone();
        let on_failure = self.on_failure.lock().unwrap().clone();
        let mut workers = self.workers.lock().unwrap();
        workers.push(spawn_log_reader(
            server_stdout,
            "scrcpy-server",
            log.clone(),
        ));
        workers.push(spawn_log_reader(
            server_stderr,
            "scrcpy-server",
            log.clone(),
        ));
        workers.push(spawn_log_reader(decoder_stderr, "ffmpeg", log.clone()));

        let running = self.running.clone();
        let control_stream = self.control_stream.clone();
        let on_control_ready = self.on_control_ready.lock().unwrap().clone();
        let connection_log = log.clone();
        let connection_failure = on_failure.clone();
        let (video_tx, video_rx) = channel::<TcpStream>();
        let connection = std::thread::spawn(move || {
            let deadline = Instant::now() + CONNECTION_TIMEOUT;
            let video = match next_connection(&transport, &running, deadline) {
                Ok(Some(stream)) => stream,
                Ok(None) => {
                    Self::fail_from_worker(
                        &running,
                        &connection_log,
                        &connection_failure,
                        "等待 scrcpy 视频通道超时".into(),
                    );
                    return;
                }
                Err(error) => {
                    Self::fail_from_worker(
                        &running,
                        &connection_log,
                        &connection_failure,
                        format!("接受 scrcpy 视频通道失败：{error}"),
                    );
                    return;
                }
            };
            let _ = video.set_nonblocking(false);
            let _ = video.set_nodelay(true);
            if let Some(callback) = &connection_log {
                callback("scrcpy 视频 socket 已连接".into());
            }
            if video_tx.send(video).is_err() {
                return;
            }

            // scrcpy 的 DesktopConnection 按顺序建立 video → audio（已关闭）→ control。
            // 只有这个线程消费隧道，避免把 control socket 误当成 video socket。
            match next_connection(&transport, &running, deadline) {
                Ok(Some(control)) => {
                    let _ = control.set_nodelay(true);
                    *control_stream.lock().unwrap() = Some(control);
                    if let Some(callback) = &connection_log {
                        callback("scrcpy 控制 socket 已连接".into());
                    }
                    if let Some(callback) = on_control_ready {
                        callback();
                    }
                }
                Ok(None) => {
                    if let Some(callback) = &connection_log {
                        callback("scrcpy 控制通道尚未连接；画面仍可继续显示".into());
                    }
                }
                Err(error) => {
                    if let Some(callback) = &connection_log {
                        callback(format!("接受 scrcpy 控制通道失败：{error}"));
                    }
                }
            }
        });
        workers.push(connection);

        let running = self.running.clone();
        let video_pump_log = log.clone();
        let video_pump_failure = on_failure.clone();
        let video_pump = std::thread::spawn(move || {
            let mut input = match video_rx.recv_timeout(CONNECTION_TIMEOUT) {
                Ok(stream) => stream,
                Err(RecvTimeoutError::Timeout) => {
                    Self::fail_from_worker(
                        &running,
                        &video_pump_log,
                        &video_pump_failure,
                        "scrcpy 视频通道未建立".into(),
                    );
                    return;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let mut output = decoder_stdin;
            let mut buf = [0u8; 32 * 1024];
            while running.load(Ordering::SeqCst) {
                match input.read(&mut buf) {
                    Ok(0) => {
                        Self::fail_from_worker(
                            &running,
                            &video_pump_log,
                            &video_pump_failure,
                            "scrcpy 视频通道已断开".into(),
                        );
                        break;
                    }
                    Ok(n) => {
                        if output.write_all(&buf[..n]).is_err() {
                            Self::fail_from_worker(
                                &running,
                                &video_pump_log,
                                &video_pump_failure,
                                "ffmpeg H264 输入管道已断开".into(),
                            );
                            break;
                        }
                    }
                    Err(error) => {
                        Self::fail_from_worker(
                            &running,
                            &video_pump_log,
                            &video_pump_failure,
                            format!("读取 scrcpy 视频通道失败：{error}"),
                        );
                        break;
                    }
                }
            }
        });
        workers.push(video_pump);

        let running = self.running.clone();
        let sink = self.sink.clone();
        let first_frame = self.first_frame.clone();
        let on_first_frame = self.on_first_frame.lock().unwrap().clone();
        let frame_log = log.clone();
        let frame_failure = on_failure;
        let frame_len = (target_w * target_h * 4) as usize;
        let reader = std::thread::spawn(move || {
            let mut output = decoder_stdout;
            let mut frame = vec![0u8; frame_len];
            while running.load(Ordering::SeqCst) {
                match output.read_exact(&mut frame) {
                    Ok(()) => {
                        if !first_frame.swap(true, Ordering::SeqCst) {
                            if let Some(callback) = &on_first_frame {
                                callback();
                            }
                        }
                        if let Some(frame_sink) = sink.lock().unwrap().clone() {
                            frame_sink.push(&RgbaFrame {
                                width: target_w,
                                height: target_h,
                                rgba: Arc::from(frame.clone()),
                            });
                        }
                    }
                    Err(error) => {
                        if running.load(Ordering::SeqCst) {
                            Self::fail_from_worker(
                                &running,
                                &frame_log,
                                &frame_failure,
                                format!("读取 ffmpeg RGBA 帧失败：{error}"),
                            );
                        }
                        break;
                    }
                }
            }
        });
        workers.push(reader);
        drop(workers);

        self.log(format!(
            "scrcpy-server {} 已启动（{tunnel_mode}，video/control 独立通道，输出 {target_w}x{target_h}）",
            SCRCPY_SERVER_VERSION,
        ));
        Ok(())
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(stream) = self.control_stream.lock().unwrap().take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(mut child) = self.decoder.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut child) = self.adb_child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for worker in self.workers.lock().unwrap().drain(..) {
            let _ = worker.join();
        }
        if let Some(cleanup) = self.cleanup.lock().unwrap().take() {
            cleanup_scrcpy(&cleanup);
        }
        self.first_frame.store(false, Ordering::SeqCst);
    }
}

impl Drop for AndroidScreenSource {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_dimensions_are_even_and_bounded() {
        assert_eq!(scaled_dimensions(1080, 2340), (498, 1080));
        assert_eq!(scaled_dimensions(2340, 1080), (1080, 498));
        assert_eq!(scaled_dimensions(360, 640), (360, 640));
        let (_, height) = scaled_dimensions(1, 1);
        assert_eq!(height % 2, 0);
    }

    #[test]
    fn server_socket_id_is_hex_and_31_bit() {
        let id = next_scid();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(u32::from_str_radix(&id, 16).unwrap() <= 0x7fff_ffff);
    }

    #[test]
    fn start_without_dependencies_reports_missing() {
        let source = AndroidScreenSource::new(320, 640);
        let result = source.start(
            Path::new("/nonexistent-resources"),
            Path::new("/nonexistent/adb"),
            "SERIAL",
        );
        assert!(result.is_err());
    }

    #[test]
    fn scrcpy_control_packets_have_protocol_sizes() {
        assert_eq!(keycode_packet(0, 29, 0, 0).len(), 14);
        assert_eq!(text_packet("你好").unwrap().len(), 5 + "你好".len());
        assert_eq!(touch_packet(0, 0, 1, 2, 1080, 2340, 1.0, 0, 0).len(), 32);
        assert_eq!(scroll_packet(1, 2, 1080, 2340, 0.0, -1.0, 0).len(), 21);
    }

    #[test]
    fn scroll_axis_is_limited_to_scrcpy_range() {
        assert_eq!(scroll_axis(100.0), 16);
        assert_eq!(scroll_axis(-100.0), -16);
        assert_eq!(scroll_axis(1.4), 1);
    }

    #[test]
    #[ignore = "需要已授权的真实 Android 设备；用 cargo test real_scrcpy_server -- --ignored --nocapture"]
    fn real_scrcpy_server_produces_rgba_frame() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let adb = root.join("binaries/adb");
        let serial = super::super::adb::first_authorized_serial(&adb)
            .expect("adb devices 执行失败")
            .expect("没有已授权 Android 设备");
        let dimensions =
            super::super::adb::query_wm_size(&adb, Some(&serial)).unwrap_or((1080, 2340));
        let source = AndroidScreenSource::new(dimensions.0, dimensions.1);
        source.set_log(Arc::new(|message| eprintln!("scrcpy-test: {message}")));
        let frame_count = Arc::new(AtomicU32::new(0));
        let frame_count_sink = frame_count.clone();
        struct TestSink {
            frames: Arc<AtomicU32>,
        }
        impl FrameSink for TestSink {
            fn push(&self, frame: &RgbaFrame) {
                assert_eq!(frame.rgba.len(), (frame.width * frame.height * 4) as usize);
                self.frames.fetch_add(1, Ordering::SeqCst);
            }
            fn on_size(&self, _width: u32, _height: u32) {}
        }
        source.attach_sink(Arc::new(TestSink {
            frames: frame_count_sink,
        }));
        source
            .start(&root, &adb, &serial)
            .expect("scrcpy-server 启动失败");
        let deadline = Instant::now() + Duration::from_secs(15);
        while frame_count.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            source.control_handle().is_connected(),
            "scrcpy 控制 socket 未建立"
        );
        source.stop();
        assert!(
            frame_count.load(Ordering::SeqCst) > 0,
            "15 秒内未收到 RGBA 首帧"
        );
    }
}
