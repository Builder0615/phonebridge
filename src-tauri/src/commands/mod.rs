//! 受控 Tauri commands（React ↔ Rust 的唯一业务边界，Spec §5.4）。
//!
//! 命令名与事件名 snake_case；错误统一转为 `{ code, message }` 结构化负载，
//! 不把原生错误字符串直接暴露给 UI。

pub mod capabilities;
pub mod clipboard;
pub mod diagnostics;
pub mod frame;
pub mod hid;
pub mod log;
pub mod mirror;
pub mod session;
pub mod usb;
pub mod windows;

use serde::Serialize;

/// 命令错误负载（前端 CommandError）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandErrorPayload {
    pub code: String,
    pub message: String,
}

impl CommandErrorPayload {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<crate::integrations::AdapterError> for CommandErrorPayload {
    fn from(e: crate::integrations::AdapterError) -> Self {
        Self::new(e.code(), e.to_string())
    }
}

pub type CmdResult<T> = Result<T, CommandErrorPayload>;
