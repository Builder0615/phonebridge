//! HID 输入命令：鼠标、滚轮、键盘（仅控制已连接时生效）。

use serde::Deserialize;
use tauri::{AppHandle, Manager};

use crate::commands::CmdResult;
use crate::session::manager::SessionRegistry as SessionManager;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerMoveEvent {
    pub dx: i32,
    pub dy: i32,
    /// Android 绝对坐标（设备像素，可选）
    #[serde(default)]
    pub abs_x: Option<u32>,
    #[serde(default)]
    pub abs_y: Option<u32>,
    /// 绝对坐标所属的解码帧尺寸（iOS USB/WDA 使用，Android 忽略）
    #[serde(default)]
    pub source_width: Option<u32>,
    #[serde(default)]
    pub source_height: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerButtonEvent {
    pub button: String, // "left" | "middle" | "right"
    pub pressed: bool,
    /// Android 绝对坐标（设备像素，可选）
    #[serde(default)]
    pub x: Option<u32>,
    #[serde(default)]
    pub y: Option<u32>,
    /// 绝对坐标所属的解码帧尺寸（iOS USB/WDA 使用，Android 忽略）
    #[serde(default)]
    pub source_width: Option<u32>,
    #[serde(default)]
    pub source_height: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WheelPayload {
    pub delta_y: i32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HidModifiers {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub meta: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HidKeyStroke {
    pub code: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub modifiers: HidModifiers,
    #[serde(default)]
    pub repeat: bool,
}

fn button_bit(button: &str) -> Option<u8> {
    match button {
        "left" => Some(crate::integrations::hid_report::BTN_LEFT),
        "middle" => Some(crate::integrations::hid_report::BTN_MIDDLE),
        "right" => Some(crate::integrations::hid_report::BTN_RIGHT),
        _ => None,
    }
}

#[tauri::command]
pub fn hid_pointer_move(app: AppHandle, id: String, event: PointerMoveEvent) -> CmdResult<()> {
    let abs = match (event.abs_x, event.abs_y) {
        (Some(x), Some(y)) => Some((x, y)),
        _ => None,
    };
    let source_size = match (event.source_width, event.source_height) {
        (Some(width), Some(height)) => Some((width, height)),
        _ => None,
    };
    app.state::<SessionManager>()
        .get(&id)
        .ok_or_else(|| crate::commands::CommandErrorPayload::new("no_session", "会话不存在"))
        .and_then(|s| {
            s.send_pointer_move(event.dx, event.dy, abs, source_size)
                .map_err(Into::into)
        })
}

#[tauri::command]
pub fn hid_pointer_button(app: AppHandle, id: String, event: PointerButtonEvent) -> CmdResult<()> {
    let bit = button_bit(&event.button).ok_or_else(|| {
        crate::commands::CommandErrorPayload::new("invalid_button", "未知鼠标按键")
    })?;
    let abs = match (event.x, event.y) {
        (Some(x), Some(y)) => Some((x, y)),
        _ => None,
    };
    let source_size = match (event.source_width, event.source_height) {
        (Some(width), Some(height)) => Some((width, height)),
        _ => None,
    };
    app.state::<SessionManager>()
        .get(&id)
        .ok_or_else(|| crate::commands::CommandErrorPayload::new("no_session", "会话不存在"))
        .and_then(|s| {
            s.send_pointer_button(bit, event.pressed, abs, source_size)
                .map_err(Into::into)
        })
}

#[tauri::command]
pub fn hid_wheel(app: AppHandle, id: String, event: WheelPayload) -> CmdResult<()> {
    app.state::<SessionManager>()
        .get(&id)
        .ok_or_else(|| crate::commands::CommandErrorPayload::new("no_session", "会话不存在"))
        .and_then(|s| s.send_wheel(event.delta_y).map_err(Into::into))
}

#[tauri::command]
pub fn hid_key_stroke(app: AppHandle, id: String, stroke: HidKeyStroke) -> CmdResult<()> {
    // 每个键事件产生完整 KeyDown/KeyUp 对（含 repeat），保证按键始终配对；
    // 连发语义由前端节流（框架层限制为重复按下，产生等价于键盘连发的字符）。
    app.state::<SessionManager>()
        .get(&id)
        .ok_or_else(|| crate::commands::CommandErrorPayload::new("no_session", "会话不存在"))
        .and_then(|s| {
            s.send_key_stroke(
                &stroke.code,
                &stroke.key,
                stroke.modifiers.ctrl,
                stroke.modifiers.shift,
                stroke.modifiers.alt,
                stroke.modifiers.meta,
            )
            .map_err(Into::into)
        })
}
