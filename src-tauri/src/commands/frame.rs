//! 帧桥接命令：附加帧通道、测试帧源开关、帧状态。

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::commands::CmdResult;
use crate::session::manager::SessionRegistry as SessionManager;

/// 帧通道由前端在会话建立后调用（字节通道，非 JSON 帧）。
#[tauri::command]
pub fn attach_frame_channel(
    app: AppHandle,
    id: String,
    channel: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
) -> CmdResult<()> {
    let sink = std::sync::Arc::new(crate::session::frame_events::ChannelSink::new(channel));
    app.state::<SessionManager>()
        .attach_frame_sink(&id, sink)
        .map_err(crate::commands::CommandErrorPayload::from)
}

/// 前端确认收到一帧；用于给 Tauri 大帧 Channel 提供有界背压。
#[tauri::command]
pub fn acknowledge_frame(app: AppHandle, id: String, sequence: u64) -> CmdResult<()> {
    app.state::<SessionManager>()
        .acknowledge_frame(&id, sequence);
    Ok(())
}

/// 测试帧模式：验证帧管线端到端（受控生成帧，不伪造真实画面）。
#[tauri::command]
pub fn frame_test_mode(app: AppHandle, id: String, enabled: bool) -> CmdResult<()> {
    app.state::<SessionManager>()
        .frame_test_mode(&id, enabled)
        .map_err(crate::commands::CommandErrorPayload::from)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameStatus {
    pub has_sink: bool,
    pub test_mode: bool,
    pub width: u32,
    pub height: u32,
}

#[tauri::command]
pub fn frame_status(app: AppHandle, id: String) -> FrameStatus {
    let mgr = app.state::<SessionManager>();
    let (has_sink, test, w, h) = mgr
        .get(&id)
        .map(|s| s.frame_status())
        .unwrap_or((false, false, 0, 0));
    FrameStatus {
        has_sink,
        test_mode: test,
        width: w,
        height: h,
    }
}
