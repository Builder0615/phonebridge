//! SessionRegistry：多设备会话注册表（Spec v1.2 §1.2、§5.2）。
//!
//! - 每台已连接设备一个 `DeviceSession`（device.rs），拥有独立状态机/链路/帧；
//! - 本模块管理会话集合、激活会话（输入/粘贴/聚焦目标）、事件广播；
//! - 命令/事件按 sessionId 路由；激活切换时先 Release All 旧设备。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::integrations::frame_bridge::FrameSink;
use crate::integrations::hid_adapter::HidStatus;
use crate::integrations::AdapterError;
use crate::session::device::{DeviceKind, DeviceSession};
use crate::session::state::{ControlState, MirrorState, SessionStateName};

/// 会话选项（前端 SessionPreferences，serde camelCase）。
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPreferences {
    pub auto_control: bool,
    pub paste_shortcut_enabled: bool,
    pub scroll_step: i32,
    pub max_paste_bytes: usize,
    /// iOS BLE 相对鼠标的宿主侧速度倍率；Android 不使用。
    #[serde(default = "default_ios_pointer_scale")]
    pub ios_pointer_scale: f32,
    /// 启用实验性的 USB + WDA 绝对坐标输入；不可用时拒绝启动，避免
    /// 用户以为正在使用绝对坐标、实际却被静默切回 BLE 相对鼠标。
    #[serde(default)]
    pub ios_usb_control_enabled: bool,
}

fn default_ios_pointer_scale() -> f32 {
    1.0
}

impl Default for SessionPreferences {
    fn default() -> Self {
        Self {
            auto_control: true,
            paste_shortcut_enabled: true,
            scroll_step: 80,
            max_paste_bytes: 32 * 1024,
            ios_pointer_scale: default_ios_pointer_scale(),
            ios_usb_control_enabled: false,
        }
    }
}

/// 会话错误（前端 SessionError）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionError {
    pub code: String,
    pub message: String,
    pub recoverable: bool,
}

impl SessionError {
    pub fn from_adapter(e: &AdapterError, recoverable: bool) -> Self {
        Self {
            code: e.code().to_string(),
            message: e.to_string(),
            recoverable,
        }
    }
}

/// 会话状态视图（前端 SessionStateView）。`since` 为 ISO-8601。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStateView {
    pub state: SessionStateName,
    pub since: String,
    pub detail: Option<String>,
    pub control: ControlState,
    pub mirror: MirrorState,
    pub last_error: Option<SessionError>,
}

/// 诊断日志条目（复用 integrations::diagnostics::LogEntry）。
pub type LogEntry = crate::integrations::diagnostics::LogEntry;

/// 事件出口：为前端广播状态/元数据/错误（均携带 sessionId）。测试可用内存实现。
pub trait EventSink: Send + Sync {
    fn emit_session(&self, session_id: &str, view: &SessionStateView);
    fn emit_hid(&self, session_id: &str, status: &HidStatus);
    fn emit_mirror_metadata(&self, session_id: &str, width: u32, height: u32);
    fn emit_diagnostic_error(&self, session_id: &str, entry: &LogEntry);
    fn emit_active_changed(&self, session_id: Option<&str>);
    /// 运行时日志（info/warn/error），默认不广播；需要日志面板时由实现覆盖。
    fn emit_log(&self, session_id: &str, entry: &LogEntry) {
        let _ = (session_id, entry);
    }
}

/// 会话注册表（全局单例，Tauri manage）。
#[derive(Clone)]
pub struct SessionRegistry {
    sink: Arc<dyn EventSink>,
    resources: Arc<std::path::PathBuf>,
    sessions: Arc<Mutex<HashMap<String, Arc<DeviceSession>>>>,
    active: Arc<Mutex<Option<String>>>,
    runtime_log: Arc<Mutex<Vec<LogEntry>>>,
    next_port: Arc<AtomicU16>,
}

impl SessionRegistry {
    pub fn new(sink: Arc<dyn EventSink>, resources: std::path::PathBuf) -> Self {
        Self {
            sink,
            resources: Arc::new(resources),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            active: Arc::new(Mutex::new(None)),
            runtime_log: Arc::new(Mutex::new(Vec::new())),
            next_port: Arc::new(AtomicU16::new(6000)),
        }
    }

    /// 建立指定设备的会话并启动镜像。id 形如 `iphone:<udid>` / `android:<serial>`。
    /// start 失败（依赖缺失等）时会话仍保留（状态 Failed），便于面板显示与断开。
    pub fn start_session(
        &self,
        kind: DeviceKind,
        id: String,
        prefs: Option<SessionPreferences>,
    ) -> Result<(), AdapterError> {
        {
            let sessions = self.sessions.lock().unwrap();
            if sessions.contains_key(&id) {
                return Err(AdapterError::Busy(format!("设备 {id} 已在会话中")));
            }
        }
        // UxPlay 的 `-p n` 会占用完整的端口段：TCP n/n+1 和 UDP
        // n/n+1/n+2。每个实例必须跨过整个段，否则第二台 iPhone 会与
        // 第一台的镜像数据端口或 RTSP 端口冲突，表现为连接成功但没有画面。
        let port = self.next_port.fetch_add(3, Ordering::SeqCst);
        let session = DeviceSession::new(
            id.clone(),
            kind,
            self.sink.clone(),
            (*self.resources).clone(),
            port,
        );
        let result = session.start_session(prefs);
        self.sessions
            .lock()
            .unwrap()
            .insert(id.clone(), Arc::new(session));
        // 首台设备自动成为激活会话
        if self.active.lock().unwrap().is_none() {
            self.set_active(Some(id));
        }
        result
    }

    /// 断开指定设备会话（清理链路、关闭帧）。
    pub fn stop_session(&self, id: &str) -> Result<(), AdapterError> {
        let session = self.sessions.lock().unwrap().remove(id);
        if let Some(s) = session {
            let result = s.stop_session();
            if self.active.lock().unwrap().as_deref() == Some(id) {
                let next = self.sessions.lock().unwrap().keys().next().cloned();
                self.set_active(next);
            }
            result
        } else {
            Ok(())
        }
    }

    /// 全部断开。
    pub fn stop_all(&self) {
        let ids: Vec<String> = self.sessions.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let _ = self.stop_session(&id);
        }
        self.set_active(None);
    }

    /// 切换激活会话：先释放旧设备全部输入，再切换（输入/粘贴/聚焦目标）。
    pub fn activate(&self, id: &str) -> Result<(), AdapterError> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| AdapterError::Failed(format!("会话不存在：{id}")))?;
        if let Some(old) = self.active.lock().unwrap().clone() {
            if old != id {
                if let Some(old_session) = self.sessions.lock().unwrap().get(&old).cloned() {
                    old_session.release_all_input();
                }
            }
        }
        self.set_active(Some(id.to_string()));
        Ok(())
    }

    fn set_active(&self, id: Option<String>) {
        *self.active.lock().unwrap() = id.clone();
        self.sink.emit_active_changed(id.as_deref());
    }

    pub fn active(&self) -> Option<String> {
        self.active.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<Arc<DeviceSession>> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    /// 会话列表（id, kind, 状态视图）。按 id 排序保持稳定。
    pub fn list(&self) -> Vec<(String, DeviceKind, SessionStateView)> {
        let sessions = self.sessions.lock().unwrap();
        let mut out: Vec<(String, DeviceKind, SessionStateView)> = sessions
            .iter()
            .map(|(id, s)| (id.clone(), s.kind, s.state_view()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn attach_frame_sink(
        &self,
        id: &str,
        sink: Arc<dyn FrameSink>,
    ) -> Result<(), AdapterError> {
        self.get(id)
            .ok_or_else(|| AdapterError::Failed(format!("会话不存在：{id}")))?
            .attach_frame_sink(sink)
    }

    pub fn acknowledge_frame(&self, id: &str, sequence: u64) {
        if let Some(session) = self.get(id) {
            session.acknowledge_frame(sequence);
        }
    }

    pub fn frame_test_mode(&self, id: &str, enabled: bool) -> Result<(), AdapterError> {
        self.get(id)
            .ok_or_else(|| AdapterError::Failed(format!("会话不存在：{id}")))?
            .set_test_frames(enabled)
    }

    /// 全局诊断用：聚合运行日志与全部会话错误日志（脱敏）。
    pub fn aggregate_error_log(&self) -> Vec<LogEntry> {
        let sessions = self.sessions.lock().unwrap();
        let mut out = self.runtime_log.lock().unwrap().clone();
        for s in sessions.values() {
            out.extend(s.error_log());
        }
        out.sort_by(|a, b| a.at.cmp(&b.at));
        out.truncate(500);
        out
    }

    /// 记录前端上报的错误到全局日志（脱敏消息，仅限代码级信息）。
    pub fn record_frontend_log(&self, entry: LogEntry) {
        // 广播给日志面板（无会话归属）
        self.sink.emit_log("frontend", &entry);
        {
            let mut log = self.runtime_log.lock().unwrap();
            log.push(entry);
            let overflow = log.len().saturating_sub(500);
            if overflow > 0 {
                log.drain(0..overflow);
            }
        }
    }

    pub fn resources_dir(&self) -> &std::path::Path {
        &self.resources
    }

    pub fn ios_control_capability(
        &self,
    ) -> crate::integrations::ios_usb_control::IosControlCapability {
        crate::integrations::ios_usb_control::inspect_capability(&self.resources)
    }

    pub fn count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }
}

impl Drop for SessionRegistry {
    fn drop(&mut self) {
        // Tests and non-Tauri callers may drop the registry without going
        // through the app ExitRequested handler. Stop every session here so
        // UxPlay/scrcpy workers cannot retain a session cycle or become
        // orphaned sidecars.
        self.stop_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    struct MemSink {
        sessions: StdMutex<Vec<(String, SessionStateView)>>,
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
        fn emit_diagnostic_error(&self, _id: &str, _e: &LogEntry) {}
        fn emit_active_changed(&self, id: Option<&str>) {
            self.active.lock().unwrap().push(id.map(|s| s.into()));
        }
    }

    fn registry() -> (SessionRegistry, Arc<MemSink>) {
        let sink = Arc::new(MemSink {
            sessions: StdMutex::new(Vec::new()),
            active: StdMutex::new(Vec::new()),
        });
        (
            SessionRegistry::new(
                sink.clone(),
                std::path::PathBuf::from("/nonexistent-phonebridge-test-resources"),
            ),
            sink,
        )
    }

    #[test]
    fn multiple_sessions_coexist() {
        let (reg, _) = registry();
        // 依赖缺失时 start 返回 Err，但会话保留（Failed）便于展示
        let _ = reg.start_session(DeviceKind::Iphone, "iphone:A".into(), None);
        let _ = reg.start_session(DeviceKind::Android, "android:SER".into(), None);
        assert_eq!(reg.count(), 2);
        assert_eq!(reg.list().len(), 2);
        // 首台自动激活
        assert_eq!(reg.active().as_deref(), Some("iphone:A"));
    }

    #[test]
    fn uxplay_instances_receive_non_overlapping_port_ranges() {
        let (reg, _) = registry();
        let _ = reg.start_session(DeviceKind::Iphone, "iphone:A".into(), None);
        let _ = reg.start_session(DeviceKind::Iphone, "iphone:B".into(), None);

        assert_eq!(reg.get("iphone:A").unwrap().uxplay_port(), 6000);
        assert_eq!(reg.get("iphone:B").unwrap().uxplay_port(), 6003);
    }

    #[test]
    fn activate_releases_old_and_switches() {
        let (reg, _) = registry();
        let _ = reg.start_session(DeviceKind::Iphone, "iphone:A".into(), None);
        let _ = reg.start_session(DeviceKind::Android, "android:SER".into(), None);
        reg.activate("android:SER").unwrap();
        assert_eq!(reg.active().as_deref(), Some("android:SER"));
        assert!(reg.activate("iphone:none").is_err());
    }

    #[test]
    fn stop_session_promotes_next_active() {
        let (reg, _) = registry();
        let _ = reg.start_session(DeviceKind::Iphone, "iphone:A".into(), None);
        let _ = reg.start_session(DeviceKind::Iphone, "iphone:B".into(), None);
        assert_eq!(reg.active().as_deref(), Some("iphone:A"));
        reg.stop_session("iphone:A").unwrap();
        assert!(reg.active().is_some());
        assert_ne!(reg.active().as_deref(), Some("iphone:A"));
        assert_eq!(reg.count(), 1);
    }

    #[test]
    fn stop_all_clears_everything() {
        let (reg, sink) = registry();
        let _ = reg.start_session(DeviceKind::Iphone, "iphone:A".into(), None);
        let _ = reg.start_session(DeviceKind::Android, "android:SER".into(), None);
        reg.stop_all();
        assert_eq!(reg.count(), 0);
        assert_eq!(reg.active(), None);
        assert!(sink.active.lock().unwrap().contains(&None));
    }

    #[test]
    fn duplicate_session_rejected() {
        let (reg, _) = registry();
        let _ = reg.start_session(DeviceKind::Iphone, "iphone:A".into(), None);
        assert!(matches!(
            reg.start_session(DeviceKind::Iphone, "iphone:A".into(), None),
            Err(AdapterError::Busy(_))
        ));
    }
}
