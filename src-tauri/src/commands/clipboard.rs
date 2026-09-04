//! 粘贴命令（FR-CLP-001/002）：读取纯文本 → 校验 → 编码 → 发送。

use serde::Deserialize;
use tauri::{AppHandle, Manager};

use crate::integrations::clipboard_adapter::PasteResult;
use crate::session::manager::SessionRegistry as SessionManager;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PastePrefs {
    pub max_paste_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteOptions {
    #[serde(default)]
    pub prefs: Option<PastePrefs>,
}

#[tauri::command]
pub async fn paste_plain_text(app: AppHandle, id: String, options: PasteOptions) -> PasteResult {
    let mgr = app.state::<SessionManager>();
    match mgr.get(&id) {
        Some(s) => {
            let max_bytes = options.prefs.map(|p| p.max_paste_bytes);
            match tauri::async_runtime::spawn_blocking(move || s.paste(max_bytes)).await {
                Ok(result) => result,
                Err(error) => PasteResult {
                    ok: false,
                    char_count: 0,
                    byte_count: 0,
                    truncated: false,
                    unencodable_positions: Vec::new(),
                    error: Some(crate::integrations::clipboard_adapter::PasteError {
                        code: "paste_task_failed".into(),
                        message: format!("粘贴任务失败：{error}"),
                    }),
                },
            }
        }
        None => PasteResult {
            ok: false,
            char_count: 0,
            byte_count: 0,
            truncated: false,
            unencodable_positions: Vec::new(),
            error: Some(crate::integrations::clipboard_adapter::PasteError {
                code: "no_session".into(),
                message: "会话不存在".into(),
            }),
        },
    }
}
