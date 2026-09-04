//! USB 设备自动识别命令（iPhone + Android）。

use serde::Serialize;
use tauri::{AppHandle, Manager};

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
