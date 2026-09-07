//! USB 设备自动识别命令（iPhone + Android）。

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::commands::CmdResult;
use crate::integrations::ios_wda_setup::IosWdaSetupReport;
use crate::integrations::usb_devices::{list_usb_devices, UsbDevice};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsbDevicesReport {
    pub devices: Vec<UsbDeviceView>,
    pub error_per_source: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsbDeviceView {
    pub kind: String, // "iphone" | "android"
    pub name: String,
    pub id_masked: String,
    pub state: String,
    pub authorized: bool,
}

/// 枚举当前通过 USB 连接的 iPhone 与 Android 设备。
#[tauri::command]
pub fn list_usb_devices_cmd(app: AppHandle) -> UsbDevicesReport {
    let mgr = app.state::<crate::session::manager::SessionRegistry>();
    let resources = mgr.resources_dir().to_path_buf();
    let (devices, notes) = list_usb_devices(&resources);
    let views: Vec<UsbDeviceView> = devices.iter().map(to_view).collect();
    UsbDevicesReport {
        devices: views,
        error_per_source: notes,
    }
}

/// 检查可选 iOS USB/WDA 精确控制链路的宿主侧准备状态。
#[tauri::command]
pub fn get_ios_control_capability(
    app: AppHandle,
) -> crate::integrations::ios_usb_control::IosControlCapability {
    app.state::<crate::session::manager::SessionRegistry>()
        .ios_control_capability()
}

/// 只读检查 iOS WDA 自动准备流程当前卡在哪一步。
#[tauri::command]
pub async fn inspect_ios_wda(
    app: AppHandle,
    device_id_masked: Option<String>,
) -> CmdResult<IosWdaSetupReport> {
    let registry = app.state::<crate::session::manager::SessionRegistry>();
    let resources = registry.resources_dir().to_path_buf();
    let app_data = app.path().app_data_dir().map_err(|error| {
        crate::commands::CommandErrorPayload::new(
            "app_data_dir",
            format!("无法定位应用数据目录：{error}"),
        )
    })?;
    tauri::async_runtime::spawn_blocking(move || {
        crate::integrations::ios_wda_setup::inspect(
            &resources,
            &app_data,
            device_id_masked.as_deref(),
        )
    })
    .await
    .map_err(|error| {
        crate::commands::CommandErrorPayload::new("ios_wda_inspect", error.to_string())
    })
}

/// 用户明确点击“一键准备 WDA”后执行：优先安装发布包内的签名 IPA，使用 go-ios
/// 启动 XCTest/WDA 并等待设备上的 /status 成功；没有 IPA 的 macOS 开发包才回退到
/// Xcode 源码流程。
#[tauri::command]
pub async fn prepare_ios_wda(
    app: AppHandle,
    device_id_masked: Option<String>,
) -> CmdResult<IosWdaSetupReport> {
    let registry = app.state::<crate::session::manager::SessionRegistry>();
    let resources = registry.resources_dir().to_path_buf();
    let app_data = app.path().app_data_dir().map_err(|error| {
        crate::commands::CommandErrorPayload::new(
            "app_data_dir",
            format!("无法定位应用数据目录：{error}"),
        )
    })?;
    tauri::async_runtime::spawn_blocking(move || {
        crate::integrations::ios_wda_setup::prepare(
            &resources,
            &app_data,
            device_id_masked.as_deref(),
        )
    })
    .await
    .map_err(|error| {
        crate::commands::CommandErrorPayload::new("ios_wda_prepare", error.to_string())
    })
}

fn to_view(d: &UsbDevice) -> UsbDeviceView {
    match d.kind {
        crate::integrations::usb_devices::UsbDeviceKind::Iphone => UsbDeviceView {
            kind: "iphone".into(),
            name: d.name.clone(),
            id_masked: d.id_masked.clone(),
            state: d.state.clone(),
            authorized: d.state == "connected",
        },
        crate::integrations::usb_devices::UsbDeviceKind::Android => UsbDeviceView {
            kind: "android".into(),
            name: d.name.clone(),
            id_masked: d.id_masked.clone(),
            state: d.state.clone(),
            authorized: d.state == "device",
        },
    }
}
