//! 集成层：MirrorAdapter（UxPlay）、HidAdapter（BLE HID）、ClipboardAdapter、Diagnostics。
//!
//! 适配器之间通过本模块的公共错误类型与结构化事件通信，业务层不直接接触
//! 子进程输出、WinRT GATT 细节或剪贴板格式。

pub mod adb;
pub mod android_frame;
pub mod clipboard_adapter;
pub mod diagnostics;
pub mod ffmpeg_bridge;
pub mod frame_bridge;
pub mod hid_adapter;
pub mod hid_report;
pub mod ios_usb_control;
pub mod ios_wda_setup;
#[cfg(target_os = "macos")]
pub mod macos_hid;
pub mod mirror_adapter;
pub(crate) mod process;
pub mod text_encoder;
pub mod usb_devices;

/// 适配器公共错误。每个变体都会映射为前端可读的 `{ code, message }`。
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("依赖缺失：{0}")]
    DependencyMissing(String),
    #[error("当前平台不支持：{0}")]
    UnsupportedPlatform(String),
    #[error("该能力尚未在真实设备验证：{0}")]
    PendingRealDeviceValidation(String),
    #[error("蓝牙适配器不支持 LE Peripheral 角色")]
    PeripheralUnsupported,
    #[error("iOS USB/WDA 控制不可用：{0}")]
    IosUsbUnavailable(String),
    #[error("操作失败：{0}")]
    Failed(String),
    #[error("控制未激活：{0}")]
    ControlInactive(String),
    #[error("会话正忙或有未完成操作：{0}")]
    Busy(String),
}

impl AdapterError {
    /// 稳定的错误码（供前端与诊断使用，不随文案变化）。
    pub fn code(&self) -> &'static str {
        match self {
            AdapterError::DependencyMissing(_) => "dependency_missing",
            AdapterError::UnsupportedPlatform(_) => "unsupported_platform",
            AdapterError::PendingRealDeviceValidation(_) => "pending_real_device_validation",
            AdapterError::PeripheralUnsupported => "peripheral_unsupported",
            AdapterError::IosUsbUnavailable(_) => "ios_usb_unavailable",
            AdapterError::Failed(_) => "adapter_failed",
            AdapterError::ControlInactive(_) => "control_inactive",
            AdapterError::Busy(_) => "session_busy",
        }
    }
}
