//! Tauri 事件出口：把会话状态广播到前端（事件名见 Spec §5.4，均携带 sessionId）。

use tauri::{AppHandle, Emitter};

use super::manager::{EventSink, LogEntry, SessionStateView};
use crate::integrations::hid_adapter::HidStatus;

pub struct TauriEventSink {
    app: AppHandle,
}

impl TauriEventSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl EventSink for TauriEventSink {
    fn emit_session(&self, session_id: &str, view: &SessionStateView) {
        let _ = self.app.emit(
            "session://state",
            serde_json::json!({ "sessionId": session_id, "state": view }),
        );
    }

    fn emit_hid(&self, session_id: &str, status: &HidStatus) {
        let _ = self.app.emit(
            "hid://status",
            serde_json::json!({ "sessionId": session_id, "status": status }),
        );
    }

    fn emit_mirror_metadata(&self, session_id: &str, width: u32, height: u32) {
        let _ = self.app.emit(
            "mirror://metadata",
            serde_json::json!({
                "sessionId": session_id,
                "metadata": crate::commands::mirror::MirrorMetadataPayload {
                    width, height, rotation: 0, codec: None,
                },
            }),
        );
    }

    fn emit_diagnostic_error(&self, session_id: &str, entry: &LogEntry) {
        let _ = self.app.emit(
            "diagnostic://error",
            serde_json::json!({ "sessionId": session_id, "entry": entry }),
        );
    }

    fn emit_active_changed(&self, session_id: Option<&str>) {
        let _ = self.app.emit(
            "session://active",
            serde_json::json!({ "sessionId": session_id }),
        );
    }

    fn emit_log(&self, session_id: &str, entry: &LogEntry) {
        let _ = self.app.emit(
            "log://entry",
            serde_json::json!({ "sessionId": session_id, "entry": entry }),
        );
    }
}
