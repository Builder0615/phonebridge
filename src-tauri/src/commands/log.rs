//! 日志命令：拉取历史日志（重启后前端可恢复日志面板内容）。

use tauri::{AppHandle, Manager};

use crate::integrations::diagnostics::LogEntry;
use crate::session::manager::SessionRegistry;

#[tauri::command]
pub fn get_app_logs(app: AppHandle) -> Vec<LogEntry> {
    app.state::<SessionRegistry>().aggregate_error_log()
}

/// 前端页面错误上报（写入日志，消息为脱敏代码/类型，不包含内容）。
#[tauri::command]
pub fn report_frontend_error(app: AppHandle, message: String) -> Result<(), String> {
    let reg = app.state::<SessionRegistry>();
    let entry = LogEntry {
        at: chrono::Utc::now().to_rfc3339(),
        level: "warn".into(),
        code: "frontend_error".into(),
        message: message.chars().take(500).collect(),
    };
    reg.record_frontend_log(entry);
    Ok(())
}
