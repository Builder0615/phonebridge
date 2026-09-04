//! check_capabilities：能力报告（FR-DIAG-001 的前置检查）。

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::commands::CmdResult;
use crate::integrations::diagnostics::{
    meets_windows_minimum, os_build_number, os_version_string, probe_mdns, DependencyState,
    MIN_WINDOWS_BUILD,
};
use crate::integrations::hid_adapter::probe_ble_capabilities;
use crate::integrations::mirror_adapter::uxplay_package_state;
use crate::session::manager::SessionRegistry as SessionManager;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkCapability {
    pub available: bool,
    pub interface_count: usize,
    pub primary_ip_masked: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BluetoothCapability {
    pub supported: bool,
    pub peripheral_role: bool,
    pub adapter_name: Option<String>,
    pub adapter_address_masked: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorCapability {
    pub status: String,
    pub uxplay_present: bool,
    pub uxplay_version: Option<String>,
    pub expected_path: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsTargetInfo {
    pub min_build: u32,
    pub current_build: u32,
    pub meets_minimum: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidCapability {
    pub adb_available: bool,
    pub ffmpeg_available: bool,
    pub scrcpy_server_available: bool,
    /// 已授权设备（脱敏序列号:状态）
    pub authorized_devices: Vec<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityReport {
    pub platform: String,
    pub os_version: String,
    pub os_build: Option<u32>,
    pub app_version: String,
    pub tauri_version: String,
    pub network: NetworkCapability,
    pub mdns: DependencyState,
    pub bluetooth: BluetoothCapability,
    pub mirror: MirrorCapability,
    pub android: AndroidCapability,
    pub clipboard: bool,
    pub windows_target: Option<WindowsTargetInfo>,
}

#[tauri::command]
pub fn check_capabilities(app: AppHandle) -> CmdResult<CapabilityReport> {
    let mgr = app.state::<SessionManager>();
    let resources = mgr.resources_dir().to_path_buf();
    let port: u16 = 6000; // UxPlay 端口段基址（多会话时递增）

    let build = os_build_number();
    let (ble_role, ble_addr, ble_note) = probe_ble_capabilities();
    let (uxplay_present, uxplay_version, uxplay_detail) = uxplay_package_state(&resources);

    let interface_count = if_addrs::get_if_addrs().map(|a| a.len()).unwrap_or(0);

    let primary_ip = local_ip_address::local_ip().ok();
    let network = NetworkCapability {
        available: primary_ip.is_some(),
        interface_count,
        primary_ip_masked: primary_ip
            .map(|ip| crate::integrations::diagnostics::mask_ipv4(&ip.to_string())),
        error: if primary_ip.is_none() {
            Some("未探测到主网络接口".into())
        } else {
            None
        },
    };

    let bluetooth = BluetoothCapability {
        supported: ble_role,
        peripheral_role: ble_role,
        adapter_name: None,
        adapter_address_masked: ble_addr,
        error: ble_note,
    };

    let mirror_status = if uxplay_present { "ok" } else { "missing" };
    let mirror = MirrorCapability {
        status: mirror_status.into(),
        uxplay_present,
        uxplay_version,
        expected_path: resources.join("binaries").display().to_string(),
        detail: if uxplay_present {
            format!("UxPlay 就位（端口 {port}）")
        } else {
            uxplay_detail
        },
    };

    let (adb_available, ffmpeg_available, scrcpy_server_available, authorized, android_detail) =
        match crate::integrations::adb::resolve_tool(&resources, "adb", "PHONEBRIDGE_ADB_PATH") {
            Ok(adb) => {
                let devices = crate::integrations::adb::list_devices(&adb).unwrap_or_default();
                let authorized: Vec<String> = devices
                    .iter()
                    .filter(|d| d.status == "device")
                    .map(|d| format!("{}:{}", d.serial_masked, d.status))
                    .collect();
                let ff = crate::integrations::android_frame::locate_ffmpeg(&resources).is_some();
                let scrcpy_server =
                    crate::integrations::android_frame::locate_scrcpy_server(&resources).is_some();
                let detail = if ff && scrcpy_server {
                    "adb、scrcpy-server 与 ffmpeg 就绪".into()
                } else if !scrcpy_server {
                    "adb 就绪，但未找到匹配的 scrcpy-server".into()
                } else {
                    "adb 就绪，但未找到 ffmpeg（投屏解码依赖）".into()
                };
                (true, ff, scrcpy_server, authorized, detail)
            }
            Err(e) => (false, false, false, Vec::new(), e),
        };
    let android = AndroidCapability {
        adb_available,
        ffmpeg_available,
        scrcpy_server_available,
        authorized_devices: authorized,
        detail: android_detail,
    };

    let windows_target = std::env::consts::OS
        .eq_ignore_ascii_case("windows")
        .then(|| {
            let current = build.unwrap_or(0);
            WindowsTargetInfo {
                min_build: MIN_WINDOWS_BUILD,
                current_build: current,
                meets_minimum: meets_windows_minimum(Some(current)),
            }
        });

    Ok(CapabilityReport {
        platform: std::env::consts::OS.into(),
        os_version: os_version_string(),
        os_build: build,
        app_version: app.package_info().version.to_string(),
        tauri_version: tauri::VERSION.to_string(),
        network,
        mdns: probe_mdns(),
        bluetooth,
        mirror,
        android,
        clipboard: true,
        windows_target,
    })
}
