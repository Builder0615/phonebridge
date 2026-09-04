//! Diagnostics：环境检查、脱敏报告与导出包（FR-DIAG-001/002）。
//!
//! 隐私规则：
//! - IP/MAC/地址一律脱敏（掩码）后才进入报告；
//! - 报告不包含画面、音频、剪贴板文本、键盘原文或认证材料；
//! - 导出由用户显式触发，导出前前端展示包含项清单。

use std::time::Duration;

use serde::Serialize;

use crate::integrations::mirror_adapter::uxplay_package_state;
use crate::integrations::AdapterError;

use super::hid_adapter::probe_ble_capabilities;

/// 依赖状态（与前端 DependencyState 对应）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyState {
    pub status: String, // ok | missing | unsupported | unknown
    pub detail: String,
}

impl DependencyState {
    pub fn ok(detail: impl Into<String>) -> Self {
        Self {
            status: "ok".into(),
            detail: detail.into(),
        }
    }
    pub fn missing(detail: impl Into<String>) -> Self {
        Self {
            status: "missing".into(),
            detail: detail.into(),
        }
    }
    pub fn unsupported(detail: impl Into<String>) -> Self {
        Self {
            status: "unsupported".into(),
            detail: detail.into(),
        }
    }
    pub fn unknown(detail: impl Into<String>) -> Self {
        Self {
            status: "unknown".into(),
            detail: detail.into(),
        }
    }
}

/// 网络接口（脱敏）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkInterfaceInfo {
    pub name: String,
    pub ipv4_masked: Option<String>,
    pub mac_masked: Option<String>,
    pub status: String,
}

/// 防火墙检查（MVP 不在应用内改系统设置）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirewallCheck {
    pub checked: bool,
    pub allowed: Option<bool>,
    pub detail: String,
}

/// 诊断日志条目（脱敏，不包含输入内容）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub at: String,
    pub level: String, // info | warn | error
    pub code: String,
    pub message: String,
}

/// 完整诊断报告（对应前端 DiagnosticsReport）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsReport {
    pub generated_at: String,
    pub source: String,
    pub app: AppInfo,
    pub os: OsInfo,
    pub network: NetworkDiagnostic,
    pub bluetooth: BluetoothDiagnostic,
    pub mirror: MirrorDiagnostic,
    pub security: SecurityDiagnostic,
    pub recent_errors: Vec<LogEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub tauri_version: String,
    pub rust_version: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OsInfo {
    pub family: String,
    pub version: String,
    pub build: Option<u32>,
    pub meets_minimum: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkDiagnostic {
    pub interfaces: Vec<NetworkInterfaceInfo>,
    pub mdns: DependencyState,
    pub firewall: FirewallCheck,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BluetoothDiagnostic {
    pub peripheral_role: bool,
    pub adapter_name: Option<String>,
    pub adapter_address_masked: Option<String>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorDiagnostic {
    pub uxplay: DependencyState,
    pub gstreamer: DependencyState,
    pub port: u16,
    pub last_exit_code: Option<i32>,
    pub android: AndroidDiagnostic,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidDiagnostic {
    pub adb: DependencyState,
    pub ffmpeg: DependencyState,
    pub scrcpy_server: DependencyState,
    /// 已授权设备（脱敏序列号:状态）
    pub authorized_devices: Vec<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityDiagnostic {
    pub csp: String,
    pub capabilities: Vec<String>,
    pub clipboard_policy: String,
}

// ---------------------------------------------------------------------------
// 脱敏
// ---------------------------------------------------------------------------

/// IPv4 脱敏：保留前两段（192.168.*.*）。
pub fn mask_ipv4(ip: &str) -> String {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() == 4 {
        format!("{}.{}.*.*", parts[0], parts[1])
    } else {
        "*.*.*.*".into()
    }
}

/// MAC 脱敏：保留厂商段（前 3 字节），后 3 字节掩码（AA:BB:CC:xx:xx:xx）。
pub fn mask_mac(mac: &str) -> String {
    let parts: Vec<&str> = mac
        .split(|c| c == ':' || c == '-')
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() == 6 {
        format!("{}:{}:{}:xx:xx:xx", parts[0], parts[1], parts[2]).to_uppercase()
    } else {
        "XX:XX:XX:XX:XX:XX".into()
    }
}

// ---------------------------------------------------------------------------
// 探测
// ---------------------------------------------------------------------------

/// mDNS 探测：尝试解析 .local 域名（2 秒超时）。
pub fn probe_mdns() -> DependencyState {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // 使用固定探测名，不涉及真实主机。
        let r: std::io::Result<std::vec::IntoIter<std::net::SocketAddr>> =
            std::net::ToSocketAddrs::to_socket_addrs(&("phonebridge-mdns-probe.local", 0u16));
        let _ = tx.send(r.is_ok());
    });
    match rx.recv_timeout(Duration::from_millis(2000)) {
        Ok(true) => DependencyState::ok("系统可解析 .local 域名（mDNS/Bonjour 可用）"),
        Ok(false) => {
            DependencyState::missing("无法解析 .local 域名；mDNS/Bonjour 可能未安装或未运行")
        }
        Err(_) => DependencyState::unknown("探测超时，mDNS 状态待确认"),
    }
}

fn list_interfaces() -> Vec<NetworkInterfaceInfo> {
    match if_addrs::get_if_addrs() {
        Ok(addrs) => addrs
            .iter()
            .filter(|a| a.ip().is_ipv4())
            .map(|a| NetworkInterfaceInfo {
                name: a.name.clone(),
                ipv4_masked: Some(mask_ipv4(&a.ip().to_string())),
                mac_masked: None,
                status: "UP".into(),
            })
            .collect(),
        Err(e) => vec![NetworkInterfaceInfo {
            name: "（枚举失败）".into(),
            ipv4_masked: None,
            mac_masked: None,
            status: format!("{e}"),
        }],
    }
}

pub fn primary_ip_masked() -> Option<String> {
    local_ip_address::local_ip()
        .ok()
        .map(|ip| mask_ipv4(&ip.to_string()))
}

/// Windows 构建号（WinRT/注册表）；其它平台返回 None。
pub fn os_build_number() -> Option<u32> {
    #[cfg(target_os = "windows")]
    {
        unsafe {
            use windows_sys::Win32::System::SystemInformation::{
                RtlGetVersion, RTL_OSVERSIONINFOW,
            };
            let mut info: RTL_OSVERSIONINFOW = std::mem::zeroed();
            info.dwOSVersionInfoSize = std::mem::size_of::<RTL_OSVERSIONINFOW>() as u32;
            let status = RtlGetVersion(&mut info);
            if status >= 0 {
                Some(info.dwBuildNumber)
            } else {
                None
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

pub fn os_version_string() -> String {
    let info = os_info::get();
    format!(
        "{} {} ({})",
        info.os_type(),
        info.version(),
        info.architecture().unwrap_or("unknown")
    )
}

pub const MIN_WINDOWS_BUILD: u32 = 19041; // Windows 10 2004

pub fn meets_windows_minimum(build: Option<u32>) -> bool {
    build.map(|b| b >= MIN_WINDOWS_BUILD).unwrap_or(false)
}

/// 收集完整诊断报告。`error_log` 为会话层维护的最近错误。
pub fn collect_diagnostics(
    app_version: &str,
    tauri_version: &str,
    rust_version: &str,
    _app_data_supported: bool,
    error_log: &[LogEntry],
    uxplay_port: u16,
    capabilities: Vec<String>,
    app_resources_dir: &std::path::Path,
) -> DiagnosticsReport {
    let build = os_build_number();
    let (ble_supported, ble_addr, ble_note) = probe_ble_capabilities();
    let (uxplay_present, uxplay_version, uxplay_detail) = uxplay_package_state(app_resources_dir);
    let (android, _authorized) = android_diagnostic(app_resources_dir);

    DiagnosticsReport {
        generated_at: chrono::Utc::now().to_rfc3339(),
        source: "live".into(),
        app: AppInfo {
            version: app_version.into(),
            tauri_version: tauri_version.into(),
            rust_version: rust_version.into(),
        },
        os: OsInfo {
            family: std::env::consts::OS.into(),
            version: os_version_string(),
            build,
            meets_minimum: meets_windows_minimum(build),
        },
        network: NetworkDiagnostic {
            interfaces: list_interfaces(),
            mdns: probe_mdns(),
            firewall: FirewallCheck {
                checked: false,
                allowed: None,
                detail: "MVP 不在应用内修改系统设置；请在真实 Windows 环境按文档确认 UDP 6000-6010 入站规则允许 UxPlay".into(),
            },
        },
        bluetooth: BluetoothDiagnostic {
            peripheral_role: ble_supported,
            adapter_name: None,
            adapter_address_masked: ble_addr,
            note: ble_note.unwrap_or_else(|| {
                "Peripheral 角色支持取决于适配器；不支持时「启用控制」会被禁用".into()
            }),
        },
        mirror: MirrorDiagnostic {
            uxplay: if uxplay_present {
                DependencyState::ok(format!(
                    "已就位：版本 {}",
                    uxplay_version.as_deref().unwrap_or("未登记")
                ))
            } else {
                DependencyState::missing(uxplay_detail)
            },
            gstreamer: DependencyState::unknown(
                "GStreamer 随 UxPlay 构建打包；发布前需生成 SBOM 与第三方声明（Spec §7）".to_string(),
            ),
            port: uxplay_port,
            last_exit_code: None,
            android,
        },
        security: SecurityDiagnostic {
            csp: "default-src 'self'; connect-src ipc: http://ipc.localhost".into(),
            capabilities,
            clipboard_policy: "仅在用户触发时读取纯文本；不落盘、不上传、不写日志".into(),
        },
        recent_errors: error_log.to_vec(),
    }
}

/// Android 链路诊断（adb + scrcpy-server + ffmpeg）。
fn android_diagnostic(app_resources_dir: &std::path::Path) -> (AndroidDiagnostic, Vec<String>) {
    let ffmpeg_state = match crate::integrations::android_frame::locate_ffmpeg(app_resources_dir) {
        Some(p) => DependencyState::ok(format!("ffmpeg 就位：{}", p.display())),
        None => {
            DependencyState::missing("未找到 ffmpeg（PATH/Homebrew）；scrcpy H264 解码依赖 ffmpeg")
        }
    };
    let scrcpy_server_state =
        match crate::integrations::android_frame::locate_scrcpy_server(app_resources_dir) {
            Some(p) => DependencyState::ok(format!(
                "scrcpy-server {} 就位：{}",
                crate::integrations::android_frame::scrcpy_server_version(),
                p.display()
            )),
            None => DependencyState::missing(format!(
                "未找到 scrcpy-server {}；请运行 pnpm sidecars",
                crate::integrations::android_frame::scrcpy_server_version()
            )),
        };
    match crate::integrations::adb::resolve_tool(app_resources_dir, "adb", "PHONEBRIDGE_ADB_PATH") {
        Ok(adb) => {
            let devices = crate::integrations::adb::list_devices(&adb);
            let (authorized, detail) = match devices {
                Ok(list) => {
                    let authorized: Vec<String> = list
                        .iter()
                        .filter(|d| d.status == "device")
                        .map(|d| format!("{}:{}", d.serial_masked, d.status))
                        .collect();
                    let detail = format!(
                        "{} 台设备（{}）",
                        list.len(),
                        list.iter()
                            .map(|d| format!("{}:{}", d.serial_masked, d.status))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    (authorized, detail)
                }
                Err(e) => (Vec::new(), format!("设备枚举失败：{e}")),
            };
            (
                AndroidDiagnostic {
                    adb: DependencyState::ok("adb 可用"),
                    ffmpeg: ffmpeg_state,
                    scrcpy_server: scrcpy_server_state,
                    authorized_devices: authorized.clone(),
                    detail,
                },
                authorized,
            )
        }
        Err(e) => (
            AndroidDiagnostic {
                adb: DependencyState::missing(e),
                ffmpeg: ffmpeg_state,
                scrcpy_server: scrcpy_server_state,
                authorized_devices: Vec::new(),
                detail: "adb 未就绪".into(),
            },
            Vec::new(),
        ),
    }
}

/// 导出诊断包：写入应用数据目录，返回路径与包含项清单。
pub fn export_diagnostics(
    report: &DiagnosticsReport,
    app_data_dir: &std::path::Path,
) -> Result<(String, Vec<String>), AdapterError> {
    let dir = app_data_dir.join("diagnostics");
    std::fs::create_dir_all(&dir)
        .map_err(|e| AdapterError::Failed(format!("创建诊断目录失败: {e}")))?;
    let file = dir.join(format!(
        "phonebridge-diagnostics-{}.json",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    ));
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| AdapterError::Failed(format!("序列化诊断报告失败: {e}")))?;
    std::fs::write(&file, json)
        .map_err(|e| AdapterError::Failed(format!("写入诊断包失败: {e}")))?;

    let items = vec![
        "应用与系统版本".into(),
        "网络接口（IP/MAC 已脱敏）".into(),
        "mDNS / Bonjour 状态".into(),
        "防火墙检查状态（非系统修改）".into(),
        "蓝牙 LE Peripheral 能力".into(),
        "UxPlay / GStreamer 依赖状态".into(),
        "安全策略（CSP / capabilities / 剪贴板策略）".into(),
        "最近错误（已脱敏，不含输入内容）".into(),
    ];
    Ok((file.display().to_string(), items))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_ipv4() {
        assert_eq!(mask_ipv4("192.168.1.23"), "192.168.*.*");
        assert_eq!(mask_ipv4("10.0.0.7"), "10.0.*.*");
        assert_eq!(mask_ipv4("garbage"), "*.*.*.*");
    }

    #[test]
    fn masks_mac() {
        assert_eq!(mask_mac("AA:BB:CC:DD:EE:FF"), "AA:BB:CC:XX:XX:XX");
        assert_eq!(mask_mac("11-22-33-44-55-66"), "11:22:33:XX:XX:XX");
        assert_eq!(mask_mac("not-a-mac"), "XX:XX:XX:XX:XX:XX");
    }

    #[test]
    fn windows_minimum_rule() {
        assert!(!meets_windows_minimum(None));
        assert!(!meets_windows_minimum(Some(19041 - 1)));
        assert!(meets_windows_minimum(Some(19041)));
        assert!(meets_windows_minimum(Some(26100)));
    }

    #[test]
    fn dependency_state_shapes() {
        let ok = DependencyState::ok("fine");
        assert_eq!(ok.status, "ok");
        assert_eq!(DependencyState::missing("x").status, "missing");
        assert_eq!(DependencyState::unsupported("x").status, "unsupported");
        assert_eq!(DependencyState::unknown("x").status, "unknown");
    }
}
