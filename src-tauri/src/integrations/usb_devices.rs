//! USB 设备自动识别（Spec v1.1：iPhone 与 Android 通过 USB 连接后自动列出）。
//!
//! 约束：
//! - 结构化命令枚举，不读取设备文件、剪贴板或认证材料；
//! - 序列号/标识只做脱敏展示（保留末 4 位）或内部使用；
//! - 依赖缺失（adb / xcrun / go-ios / idevice）时返回可理解的
//!   DependencyMissing，不隐式成功。

use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use super::{process::hidden_command, AdapterError};

/// 设备来源：USB（有线）发现。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbDeviceKind {
    Iphone,
    Android,
}

/// 一台 USB 连接的设备（脱敏展示 + 内部标识）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsbDevice {
    pub kind: UsbDeviceKind,
    /// 展示名（iPhone 型号 / Android 厂商名占位）
    pub name: String,
    /// 脱敏后的序列号/UDID 尾部（***0123）
    pub id_masked: String,
    /// 内部识别符（仅 Rust 内部使用；序列号/UDID 不进日志）
    pub raw_id: String,
    /// "connected" | "unauthorized" | "unavailable" 等
    pub state: String,
}

// ---------------------------------------------------------------------------
// Android：adb devices
// ---------------------------------------------------------------------------

/// 解析 `adb devices` 输出为原始 (serial, state) 列表。
fn parse_adb_raw(output: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in output.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() || line.starts_with("List of") {
            continue;
        }
        let mut parts = line.split_whitespace();
        if let (Some(serial), Some(state)) = (parts.next(), parts.next()) {
            out.push((serial.to_string(), state.to_string()));
        }
    }
    out
}

/// `adb devices -l` 额外解析厂商名（可选）。
fn adb_vendor(output: &str, serial: &str) -> Option<String> {
    for line in output.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let first = parts.next()?;
        if first != serial {
            continue;
        }
        for tok in parts {
            if let Some(model) = tok.strip_prefix("model:") {
                return Some(model.to_string());
            }
        }
    }
    None
}

/// 通过 adb 枚举 Android 设备。
pub fn list_android(adb: &Path) -> Result<Vec<UsbDevice>, AdapterError> {
    if !adb.exists() {
        return Err(AdapterError::DependencyMissing(format!(
            "未找到 adb（{}）。请安装 Android SDK Platform-Tools 或设置 PHONEBRIDGE_ADB_PATH",
            adb.display()
        )));
    }
    let basic = hidden_command(adb)
        .arg("devices")
        .output()
        .map_err(|e| AdapterError::Failed(format!("执行 adb devices 失败: {e}")))?;
    if !basic.status.success() {
        let detail = String::from_utf8_lossy(&basic.stderr)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(3)
            .collect::<Vec<_>>()
            .join("；");
        return Err(if detail.is_empty() {
            AdapterError::Failed(format!("adb devices 返回 {}", basic.status))
        } else {
            AdapterError::Failed(format!("adb devices 返回 {}：{detail}", basic.status))
        });
    }
    let basic_text = String::from_utf8_lossy(&basic.stdout).to_string();
    let detail = hidden_command(adb)
        .args(["devices", "-l"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let devices: Vec<UsbDevice> = parse_adb_raw(&basic_text)
        .into_iter()
        .map(|(serial, state)| {
            let vendor = adb_vendor(&detail, &serial).unwrap_or_else(|| "Android 设备".into());
            UsbDevice {
                kind: UsbDeviceKind::Android,
                name: vendor,
                id_masked: mask_id(&serial),
                raw_id: serial,
                state,
            }
        })
        .collect();
    Ok(devices)
}

// ---------------------------------------------------------------------------
// iPhone：xcrun devicectl list devices（macOS 自带）
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn xcrun_command() -> Command {
    // 从 Finder 启动的 Tauri 应用通常没有 shell 的 PATH；xcrun 在 macOS
    // 系统路径中是稳定的，优先使用绝对路径，只有开发者工具被安装到
    // 非标准位置时才回退到 PATH 查找。
    if Path::new("/usr/bin/xcrun").is_file() {
        hidden_command("/usr/bin/xcrun")
    } else {
        hidden_command("xcrun")
    }
}

#[cfg(target_os = "macos")]
fn system_profiler_command() -> Command {
    // 与 xcrun 一样，GUI 进程可能没有继承 `/usr/sbin`；这里不依赖 PATH。
    if Path::new("/usr/sbin/system_profiler").is_file() {
        hidden_command("/usr/sbin/system_profiler")
    } else {
        hidden_command("system_profiler")
    }
}

/// 解析 `xcrun devicectl list devices --json-output <file>` 的设备列表。
///
/// Xcode 版本间字段位置发生过变化：旧版本把 `name`、`model` 和
/// `connectionType` 放在设备对象顶层；当前版本把它们放进
/// `deviceProperties`、`hardwareProperties` 和 `connectionProperties`。
/// 这里同时兼容两种结构，并只返回在线的实体 iPhone，不把模拟器/网络设备
/// 当成 USB 设备。
fn parse_devicectl_json(raw: &str) -> Vec<UsbDevice> {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let devices = value
        .get("result")
        .and_then(|r| r.get("devices"))
        .and_then(|d| d.as_array());
    let Some(devices) = devices else {
        return Vec::new();
    };
    devices
        .iter()
        .filter_map(|d| {
            let device_props = d.get("deviceProperties");
            let hardware_props = d.get("hardwareProperties");
            let connection_props = d.get("connectionProperties");

            let name = device_props
                .and_then(|v| v.get("name"))
                .or_else(|| d.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("iPhone")
                .to_string();
            let model = hardware_props
                .and_then(|v| v.get("marketingName"))
                .or_else(|| d.get("model"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // AirPlay/HID 会话使用真实 UDID；`identifier` 是 CoreDevice
            // 标识，只有旧格式没有 hardwareProperties.udid 时才回退使用它。
            let id = hardware_props
                .and_then(|v| v.get("udid"))
                .or_else(|| d.get("identifier"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                return None;
            }

            let reality = hardware_props
                .and_then(|v| v.get("reality"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            if reality == "simulator" || reality == "virtual" {
                return None;
            }
            // `devicectl list devices` also includes the Mac itself and other
            // CoreDevice providers.  Only accept iOS/iPadOS physical records;
            // older Xcode output may omit these fields, so an empty value stays
            // compatible with the legacy branch below.
            let platform = hardware_props
                .and_then(|v| v.get("platform"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let device_type = hardware_props
                .and_then(|v| v.get("deviceType"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            if (!platform.is_empty()
                && !platform.contains("ios")
                && !platform.contains("iphone")
                && !platform.contains("ipad"))
                || (!device_type.is_empty()
                    && !device_type.contains("iphone")
                    && !device_type.contains("ipad"))
            {
                return None;
            }

            let transport = connection_props
                .and_then(|v| {
                    v.get("transportType")
                        .or_else(|| v.get("transport"))
                        .or_else(|| v.get("interface"))
                })
                .or_else(|| d.get("connectionType"))
                .or_else(|| d.get("interface"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let legacy_state = d
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let boot_state = device_props
                .and_then(|v| v.get("bootState"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let pairing = connection_props
                .and_then(|v| v.get("pairingState"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let is_unavailable = legacy_state.contains("unavailable")
                || legacy_state.contains("offline")
                || legacy_state.contains("disconnected");
            let is_usb = transport.contains("wired")
                || transport.contains("usb")
                || transport.contains("cable")
                // 旧 devicectl 格式没有 reality/transportType，保留其
                // `state=connected` 的兼容行为。
                || (transport.is_empty()
                    && (legacy_state == "connected" || boot_state == "booted"));
            // Unpaired phones often have no legacy `state` or `bootState` yet,
            // but devicectl still reports `transportType=wired`.  Keep them in
            // the list as `unauthorized` so the UI can tell the user to unlock
            // and trust this Mac.  A booted physical device with a missing
            // transport field is also a connected device on older Xcode JSON.
            let is_online = !is_unavailable
                && (is_usb
                    || legacy_state.contains("connected")
                    || legacy_state.contains("available")
                    || legacy_state.contains("booted")
                    || boot_state == "booted");
            if !is_online || !is_usb {
                return None;
            }

            let state = if pairing == "paired" || legacy_state == "connected" {
                "connected".to_string()
            } else if !pairing.is_empty() {
                "unauthorized".to_string()
            } else if !legacy_state.is_empty() {
                legacy_state
            } else {
                "connected".to_string()
            };
            Some(UsbDevice {
                kind: UsbDeviceKind::Iphone,
                name: if !model.is_empty() {
                    format!("{name} · {model}")
                } else {
                    name
                },
                id_masked: mask_id(&id),
                raw_id: id,
                state,
            })
        })
        .collect()
}

/// 解析旧版 Xcode `xcdevice list` 的 JSON。`devicectl` 在部分 macOS/Xcode
/// 组合中不存在或不会返回已通过 USB 连接的设备，`xcdevice` 是同一套
/// CoreDevice 工具链提供的兼容入口；这里只接受实体 iOS USB 设备。
fn parse_xcdevice_json(raw: &str) -> Vec<UsbDevice> {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            if d.get("simulator").and_then(|v| v.as_bool()) == Some(true)
                || d.get("ignored").and_then(|v| v.as_bool()) == Some(true)
                || d.get("available").and_then(|v| v.as_bool()) == Some(false)
            {
                return None;
            }
            let platform = d
                .get("platform")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            if !(platform.contains("iphoneos") || platform.contains("ipad")) {
                return None;
            }
            let interface = d
                .get("interface")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            if !interface.contains("usb") {
                return None;
            }
            let id = d.get("identifier").and_then(|v| v.as_str()).unwrap_or("");
            if id.is_empty() {
                return None;
            }
            let name = d
                .get("name")
                .or_else(|| d.get("modelName"))
                .and_then(|v| v.as_str())
                .unwrap_or("iPhone")
                .to_string();
            Some(UsbDevice {
                kind: UsbDeviceKind::Iphone,
                name,
                id_masked: mask_id(id),
                raw_id: id.to_string(),
                state: "connected".into(),
            })
        })
        .collect()
}

/// 通过 xcrun devicectl 枚举 iPhone（macOS）；其它平台返回 Unsupported 错误。
pub fn list_ios_gs_devicectl() -> Result<Vec<UsbDevice>, AdapterError> {
    #[cfg(target_os = "macos")]
    {
        static NEXT_JSON_FILE: AtomicU64 = AtomicU64::new(0);
        let json_path = std::env::temp_dir().join(format!(
            "phonebridge-devicectl-{}-{}.json",
            std::process::id(),
            NEXT_JSON_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let out = xcrun_command()
            // devicectl 明确规定脚本只能消费文件形式的 JSON；`-` 会把表格和
            // JSON 混在 stdout，解析必然失败。临时文件只保存本次结果，读完即删。
            .args([
                "devicectl",
                "list",
                "devices",
                "--quiet",
                // devicectl enforces a minimum overall timeout of 5 seconds;
                // smaller values make the command fail with exit code 64
                // before it even writes its JSON result.
                "--timeout",
                "5",
                "--json-output",
                json_path.to_string_lossy().as_ref(),
            ])
            .output()
            .map_err(|e| AdapterError::Failed(format!("执行 xcrun devicectl 失败: {e}")))?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&json_path);
            return Err(AdapterError::Failed(format!(
                "xcrun devicectl 失败（{}）：{}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let raw = std::fs::read_to_string(&json_path).map_err(|e| {
            let _ = std::fs::remove_file(&json_path);
            AdapterError::Failed(format!("读取 devicectl JSON 结果失败：{e}"))
        })?;
        let _ = std::fs::remove_file(&json_path);
        let devices = parse_devicectl_json(&raw);
        Ok(devices)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(AdapterError::UnsupportedPlatform(
            "xcrun devicectl 仅 macOS 可用；Windows 上 iOS USB 识别待接入相应工具".into(),
        ))
    }
}

/// 通过旧版 Xcode `xcdevice list` 枚举 iPhone（macOS 兼容兜底）。
fn list_ios_xcdevice() -> Result<Vec<UsbDevice>, AdapterError> {
    #[cfg(target_os = "macos")]
    {
        let out = xcrun_command()
            .args(["xcdevice", "list", "--timeout=1"])
            .output()
            .map_err(|e| AdapterError::Failed(format!("执行 xcrun xcdevice 失败: {e}")))?;
        if !out.status.success() {
            return Err(AdapterError::Failed(format!(
                "xcrun xcdevice 失败（{}）：{}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(parse_xcdevice_json(&String::from_utf8_lossy(&out.stdout)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(AdapterError::UnsupportedPlatform(
            "xcrun xcdevice 仅 macOS 可用".into(),
        ))
    }
}

/// 解析 adb 路径（委托 adb::resolve_tool，查找顺序：应用内置 binaries/ 优先 →
/// PHONEBRIDGE_ADB_PATH → Android SDK/Homebrew 常见位置 → PATH → command -v 兜底）。
pub fn resolve_adb(resources: &Path) -> Result<PathBuf, String> {
    super::adb::resolve_tool(resources, "adb", "PHONEBRIDGE_ADB_PATH")
}

/// system_profiler 兜底：SPUSBDataType 列出 USB 设备（macOS 自带），
/// 解析含 iPhone/iPad 名称的条目取序列号（宽松匹配，容错解析）。
#[cfg(target_os = "macos")]
fn list_ios_system_profiler() -> Vec<UsbDevice> {
    let out = system_profiler_command()
        .args(["SPUSBDataType", "-json"])
        .output();
    let Ok(out) = out else { return Vec::new() };
    let Ok(text) = String::from_utf8(out.stdout) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    // 遍历所有嵌套对象：匹配含 serial_number 且名称像 iPhone/iPad 的项
    fn walk(v: &serde_json::Value, out: &mut Vec<UsbDevice>) {
        match v {
            serde_json::Value::Array(arr) => arr.iter().for_each(|x| walk(x, out)),
            serde_json::Value::Object(map) => {
                let name = map
                    .get("_name")
                    .or_else(|| map.get("device"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                let serial = map
                    .get("serial_number")
                    .or_else(|| map.get("serial_num"))
                    .or_else(|| map.get("serialNumber"))
                    .or_else(|| map.get("device_serial_number"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                let product = map
                    .get("_name")
                    .or_else(|| map.get("device"))
                    .or_else(|| map.get("name"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                if !serial.is_empty()
                    && (name.contains("iPhone")
                        || name.contains("iPad")
                        || product.contains("iPhone")
                        || product.contains("iPad"))
                {
                    out.push(UsbDevice {
                        kind: UsbDeviceKind::Iphone,
                        name: if !name.is_empty() {
                            name.trim().to_string()
                        } else {
                            product.trim().to_string()
                        },
                        id_masked: mask_id(serial),
                        raw_id: serial.to_string(),
                        state: "connected".into(),
                    });
                }
                map.values().for_each(|x| walk(x, out));
            }
            _ => {}
        }
    }
    let mut devices = Vec::new();
    walk(&value, &mut devices);
    devices
}

#[cfg(not(target_os = "macos"))]
fn list_ios_system_profiler() -> Vec<UsbDevice> {
    Vec::new()
}

// ---------------------------------------------------------------------------
// Portable iOS USB fallback (libimobiledevice)
// ---------------------------------------------------------------------------

/// Locate an optional libimobiledevice executable without assuming a
/// platform-specific developer environment. A release can place the audited
/// binary in `resources/binaries`; environment variables and PATH are only
/// fallbacks for development or an enterprise deployment.
fn resolve_ios_tool(resources: &Path, name: &str, env_var: &str) -> Option<PathBuf> {
    let executable = if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let mut candidates = vec![
        resources.join("binaries").join(&executable),
        resources.join("binaries").join("ios-usb").join(&executable),
        resources.join(&executable),
        resources.join("../MacOS").join(&executable),
    ];
    if let Ok(configured) = std::env::var(env_var) {
        let configured = PathBuf::from(configured);
        candidates.push(if configured.is_dir() {
            configured.join(&executable)
        } else {
            configured
        });
    }
    #[cfg(target_os = "macos")]
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin").join(&executable),
        PathBuf::from("/usr/local/bin").join(&executable),
    ]);
    #[cfg(target_os = "windows")]
    {
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            candidates.push(
                PathBuf::from(program_files)
                    .join("libimobiledevice")
                    .join(&executable),
            );
        }
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(local_app_data)
                    .join("libimobiledevice")
                    .join(&executable),
            );
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(&executable)));
    }
    candidates.into_iter().find(|path| path.is_file())
}

fn parse_idevice_id_output(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .filter(|id| {
            id.len() <= 128
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
        .map(ToOwned::to_owned)
        .collect()
}

/// 解析 `go-ios list` 的默认 JSON 输出。
///
/// 当前 go-ios 输出形如 `{"deviceList":["<udid>"]}`。这里同时容忍
/// `devices`、嵌套 `properties.serialNumber` 和直接对象字段，避免 go-ios
/// 在不同版本调整 JSON 包装层后让 Windows 设备列表再次消失。
fn parse_go_ios_list_output(output: &str) -> Vec<String> {
    fn valid_id(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    }

    fn collect(value: &serde_json::Value, ids: &mut Vec<String>) {
        match value {
            serde_json::Value::Array(values) => {
                for value in values {
                    collect(value, ids);
                }
            }
            serde_json::Value::String(value) if valid_id(value) => {
                if !ids.iter().any(|id| id == value) {
                    ids.push(value.clone());
                }
            }
            serde_json::Value::Object(object) => {
                for key in ["udid", "UDID", "serialNumber", "serial_number"] {
                    if let Some(serde_json::Value::String(value)) = object.get(key) {
                        if valid_id(value) && !ids.iter().any(|id| id == value) {
                            ids.push(value.clone());
                        }
                    }
                }
                for key in ["deviceList", "devices", "DeviceList"] {
                    if let Some(value) = object.get(key) {
                        collect(value, ids);
                    }
                }
                if let Some(properties) = object.get("properties") {
                    collect(properties, ids);
                }
            }
            _ => {}
        }
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    collect(&value, &mut ids);
    ids
}

/// Enumerate iOS devices through the cross-platform go-ios CLI.  This is the
/// Windows WDA package's primary discovery fallback because `idevice_id.exe`
/// is optional there while `ios.exe` is already required for WDA startup.
fn list_ios_go_ios(resources: &Path) -> Result<Vec<UsbDevice>, AdapterError> {
    let tool = resolve_ios_tool(resources, "ios", "PHONEBRIDGE_GO_IOS_PATH").ok_or_else(|| {
        AdapterError::DependencyMissing("未找到 go-ios（ios.exe），无法枚举 Windows iPhone".into())
    })?;
    let output = hidden_command(&tool)
        .arg("list")
        .output()
        .map_err(|e| AdapterError::Failed(format!("执行 go-ios list 失败：{e}")))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            AdapterError::Failed(format!("go-ios list 失败（{}）", output.status))
        } else {
            AdapterError::Failed(format!("go-ios list 失败（{}）：{detail}", output.status))
        });
    }
    Ok(
        parse_go_ios_list_output(&String::from_utf8_lossy(&output.stdout))
            .into_iter()
            .map(|raw_id| UsbDevice {
                kind: UsbDeviceKind::Iphone,
                name: "iPhone".into(),
                id_masked: mask_id(&raw_id),
                raw_id,
                state: "connected".into(),
            })
            .collect(),
    )
}

/// Enumerate iOS devices through the cross-platform libimobiledevice utility.
/// On Windows this requires the Apple Mobile Device/libusbmuxd transport and
/// an audited `idevice_id.exe`; go-ios is tried separately when this optional
/// fallback is unavailable.
fn list_ios_idevice_id(resources: &Path) -> Result<Vec<UsbDevice>, AdapterError> {
    let tool = resolve_ios_tool(resources, "idevice_id", "PHONEBRIDGE_IDEVICE_ID_PATH")
        .ok_or_else(|| {
            AdapterError::DependencyMissing(
                "未找到 idevice_id（可选的跨平台 iOS USB 识别工具）".into(),
            )
        })?;
    let output = hidden_command(&tool)
        .arg("-l")
        .output()
        .map_err(|e| AdapterError::Failed(format!("执行 idevice_id 失败：{e}")))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(3)
            .collect::<Vec<_>>()
            .join("；");
        return Err(if detail.is_empty() {
            AdapterError::Failed(format!("idevice_id 失败（{}）", output.status))
        } else {
            AdapterError::Failed(format!("idevice_id 失败（{}）：{detail}", output.status))
        });
    }
    Ok(
        parse_idevice_id_output(&String::from_utf8_lossy(&output.stdout))
            .into_iter()
            .map(|raw_id| UsbDevice {
                kind: UsbDeviceKind::Iphone,
                name: "iPhone".into(),
                id_masked: mask_id(&raw_id),
                raw_id,
                state: "connected".into(),
            })
            .collect(),
    )
}

/// 统一枚举 USB 设备（iOS + Android）。
///
/// 返回 `(已识别设备, 各来源的未就绪提示)`：依赖缺失/工具不可用不再伪装成
/// 「识别不可用」的假设备行，而是收进错误提示列表，由前端以中性引导展示，
/// 避免「一堆环境缺失报错」的观感（依赖缺失 = 未就绪，不是错误）。
pub fn list_usb_devices(resources: &Path) -> (Vec<UsbDevice>, Vec<String>) {
    let mut devices = Vec::new();
    let mut notes = Vec::new();
    let mut devicectl_error = None;
    let mut xcdevice_error = None;
    let mut portable_ios_error = None;
    let mut go_ios_error = None;
    match list_ios_gs_devicectl() {
        Ok(ios) => devices.extend(ios),
        Err(error) => devicectl_error = Some(error),
    }

    // `system_profiler` can still see a trusted USB iPhone while CoreDevice is
    // refreshing its registry (and can also expose a phone before pairing).
    // Merge it on every refresh, not only when devicectl failed, so a second
    // physical iPhone is not hidden by the first successful source.
    for candidate in list_ios_system_profiler() {
        merge_device(&mut devices, candidate);
    }

    // Some Xcode/macOS combinations return success with an empty devicectl
    // list.  In that case xcdevice is the useful compatibility path; previously
    // it was called only after a process error, which made connected iPhones
    // disappear silently.
    if !devices.iter().any(|d| d.kind == UsbDeviceKind::Iphone) {
        match list_ios_xcdevice() {
            Ok(ios) => {
                for candidate in ios {
                    merge_device(&mut devices, candidate);
                }
            }
            Err(error) => xcdevice_error = Some(error),
        }
    }
    // libimobiledevice/idevice_id is the cross-platform fallback. It is
    // especially important on Windows, where xcrun is intentionally absent;
    // a release may bundle the audited executable and its Apple USB transport.
    if !devices.iter().any(|d| d.kind == UsbDeviceKind::Iphone) {
        match list_ios_idevice_id(resources) {
            Ok(ios) => {
                for candidate in ios {
                    merge_device(&mut devices, candidate);
                }
            }
            Err(error) => portable_ios_error = Some(error),
        }
    }
    // The Windows package carries go-ios even in BLE-compatible builds. Use its
    // read-only `list` command as a second portable discovery path so a package
    // does not require the separately optional idevice_id.exe just to show an
    // iPhone.
    if !devices.iter().any(|d| d.kind == UsbDeviceKind::Iphone) {
        match list_ios_go_ios(resources) {
            Ok(ios) => {
                for candidate in ios {
                    merge_device(&mut devices, candidate);
                }
            }
            Err(error) => go_ios_error = Some(error),
        }
    }
    if !devices.iter().any(|d| d.kind == UsbDeviceKind::Iphone) {
        let mut sources = Vec::new();
        if let Some(error) = devicectl_error {
            sources.push(format!("devicectl：{error}"));
        }
        if let Some(error) = xcdevice_error {
            sources.push(format!("xcdevice：{error}"));
        }
        if let Some(error) = portable_ios_error {
            sources.push(format!("idevice_id：{error}"));
        }
        if let Some(error) = go_ios_error {
            sources.push(format!("go-ios：{error}"));
        }
        if !sources.is_empty() {
            notes.push(format!("iPhone（USB）：{}", sources.join("；")));
        }
    }
    match resolve_adb(resources) {
        Ok(adb) => match list_android(&adb) {
            Ok(and) => devices.extend(and),
            Err(e) => notes.push(format!("Android（adb 设备枚举）：{e}")),
        },
        Err(e) => notes.push(format!("Android（adb 定位）：{e}")),
    }
    (devices, notes)
}

/// Resolve the raw UDID behind the masked session id. Raw identifiers stay in
/// the Rust process and are never returned to the frontend or written to logs.
pub fn resolve_ios_raw_id(resources: &Path, masked_id: &str) -> Result<String, AdapterError> {
    let (devices, _) = list_usb_devices(resources);
    let matches: Vec<&UsbDevice> = devices
        .iter()
        .filter(|device| device.kind == UsbDeviceKind::Iphone && device.id_masked == masked_id)
        .collect();
    match matches.as_slice() {
        [device] => Ok(device.raw_id.clone()),
        [] => Err(AdapterError::IosUsbUnavailable(format!(
            "未找到与 {masked_id} 对应的已连接 iPhone UDID；请保持 USB 连接并解锁/信任此电脑"
        ))),
        _ => Err(AdapterError::IosUsbUnavailable(format!(
            "多个 iPhone 使用相同的脱敏标识 {masked_id}，请使用 PHONEBRIDGE_IOS_UDID 指定目标"
        ))),
    }
}

/// 合并多套 macOS 设备发现结果，按真实标识去重并保留更可靠的状态/名称。
fn merge_device(devices: &mut Vec<UsbDevice>, candidate: UsbDevice) {
    if let Some(existing) = devices
        .iter_mut()
        .find(|device| device.kind == candidate.kind && device.raw_id == candidate.raw_id)
    {
        if existing.state != "connected" && candidate.state == "connected" {
            existing.state = candidate.state.clone();
        }
        if (existing.name == "iPhone" || existing.name == "Android 设备")
            && !candidate.name.is_empty()
        {
            existing.name = candidate.name.clone();
        }
        return;
    }
    devices.push(candidate);
}

/// 脱敏：保留末 4 位，其余掩码（`abcd...****01234` 形式）。
pub fn mask_id(id: &str) -> String {
    if id.is_empty() {
        return "****".into();
    }
    let n = id.len();
    if n <= 4 {
        return "****".into();
    }
    format!("***{}", &id[n - 4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_adb_devices_table() {
        let out = "List of devices attached\n0123456789ABCDEF\tdevice\nGHIJKLMN\tunauthorized\n";
        let raw = parse_adb_raw(out);
        assert_eq!(raw.len(), 2);
        assert_eq!(
            raw[0],
            ("0123456789ABCDEF".to_string(), "device".to_string())
        );
        assert_eq!(raw[1], ("GHIJKLMN".to_string(), "unauthorized".to_string()));
    }

    #[test]
    fn adb_vendor_extraction() {
        let out = "List of devices attached\n0123456789ABCDEF       device product:model model:SM_G998B device:SM_G998B\n";
        assert_eq!(adb_vendor(out, "0123456789ABCDEF"), Some("SM_G998B".into()));
    }

    #[test]
    fn parses_devicectl_json() {
        let raw = r#"{
            "result": {
                "devices": [
                    { "name": "iPhone 15", "identifier": "ABC-123", "connectionType": "USB", "state": "connected", "model": "iPhone 15 Pro" },
                    { "name": "iPhone 16 Sim", "identifier": "DEF-456", "connectionType": "Network", "state": "booted" },
                    { "name": "iPhone 17", "identifier": "GHI-789", "connectionType": "USB", "state": "connected", "model": "iPhone 17" }
                ]
            }
        }"#;
        let devs = parse_devicectl_json(raw);
        // 只保留 USB 连接的两台，排除 Network 模拟器
        assert_eq!(devs.len(), 2);
        assert!(devs.iter().all(|d| d.kind == UsbDeviceKind::Iphone));
        assert!(devs[0].name.contains("iPhone 15 Pro"));
        assert_eq!(devs[0].raw_id, "ABC-123");
    }

    #[test]
    fn missing_fields_are_skipped() {
        let raw = r#"{"result":{"devices":[{"name":"x"}]}}"#;
        assert!(parse_devicectl_json(raw).is_empty());
    }

    #[test]
    fn parses_current_nested_devicectl_json() {
        let raw = r#"{
            "result": {
                "devices": [
                    {
                        "identifier": "CORE-1",
                        "deviceProperties": {"name": "My iPhone", "bootState": "booted"},
                        "hardwareProperties": {"marketingName": "iPhone 16", "reality": "physical", "udid": "UDID-1"},
                        "connectionProperties": {"transportType": "wired", "pairingState": "paired"}
                    },
                    {
                        "identifier": "CORE-2",
                        "deviceProperties": {"name": "Network iPhone", "bootState": "booted"},
                        "hardwareProperties": {"marketingName": "iPhone 16", "reality": "physical", "udid": "UDID-2"},
                        "connectionProperties": {"transportType": "wifi", "pairingState": "paired"}
                    },
                    {
                        "identifier": "CORE-3",
                        "deviceProperties": {"name": "Simulator", "bootState": "booted"},
                        "hardwareProperties": {"marketingName": "iPhone 16", "reality": "simulator", "udid": "UDID-3"},
                        "connectionProperties": {"transportType": "wired", "pairingState": "paired"}
                    }
                ]
            }
        }"#;
        let devs = parse_devicectl_json(raw);
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].raw_id, "UDID-1");
        assert_eq!(devs[0].state, "connected");
        assert!(devs[0].name.contains("iPhone 16"));
    }

    #[test]
    fn keeps_wired_unpaired_device_for_trust_prompt() {
        let raw = r#"{
            "result": {
                "devices": [{
                    "identifier": "CORE-UNPAIRED",
                    "deviceProperties": {"name": "New iPhone"},
                    "hardwareProperties": {
                        "reality": "physical",
                        "udid": "UDID-UNPAIRED"
                    },
                    "connectionProperties": {
                        "transportType": "wired",
                        "pairingState": "unpaired"
                    }
                }]
            }
        }"#;
        let devs = parse_devicectl_json(raw);
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].state, "unauthorized");
    }

    #[test]
    fn keeps_booted_physical_device_when_transport_is_missing() {
        let raw = r#"{
            "result": {
                "devices": [{
                    "identifier": "CORE-BOOTED",
                    "deviceProperties": {"name": "Booted iPhone", "bootState": "booted"},
                    "hardwareProperties": {
                        "reality": "physical",
                        "udid": "UDID-BOOTED"
                    },
                    "connectionProperties": {"pairingState": "paired"}
                }]
            }
        }"#;
        let devs = parse_devicectl_json(raw);
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].state, "connected");
    }

    #[test]
    fn parses_xcdevice_usb_physical_device_only() {
        let raw = r#"[
            {
                "simulator": true,
                "available": true,
                "platform": "com.apple.platform.iphonesimulator",
                "identifier": "SIMULATOR",
                "name": "iPhone Simulator"
            },
            {
                "simulator": false,
                "available": true,
                "platform": "com.apple.platform.iphoneos",
                "interface": "usb",
                "identifier": "UDID-USB",
                "name": "iPhone 16"
            },
            {
                "simulator": false,
                "available": true,
                "platform": "com.apple.platform.iphoneos",
                "interface": "network",
                "identifier": "UDID-WIFI",
                "name": "iPhone Wi-Fi"
            }
        ]"#;
        let devs = parse_xcdevice_json(raw);
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].raw_id, "UDID-USB");
        assert_eq!(devs[0].state, "connected");
    }

    #[test]
    fn parses_idevice_id_lines_and_rejects_diagnostics() {
        let ids = parse_idevice_id_output(
            "ABC-123\n\n  DEF_456  \nidevice_id: no device found\nUDID with spaces\n",
        );
        assert_eq!(ids, vec!["ABC-123", "DEF_456"]);
    }

    #[test]
    fn parses_go_ios_device_list_json() {
        let ids = parse_go_ios_list_output(
            r#"{"deviceList":["ABC-123", "ABC-123"], "ignored":"not-a-device"}"#,
        );
        assert_eq!(ids, vec!["ABC-123"]);

        let ids =
            parse_go_ios_list_output(r#"{"devices":[{"properties":{"serialNumber":"DEF_456"}}]}"#);
        assert_eq!(ids, vec!["DEF_456"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "需要 macOS 上已配对并通过 USB 连接的真实 iPhone"]
    fn live_devicectl_enumeration_uses_json_file() {
        let devices = list_ios_gs_devicectl().expect("devicectl 枚举失败");
        assert!(!devices.is_empty(), "devicectl 未返回已连接的实体 iPhone");
        assert!(devices
            .iter()
            .all(|device| device.kind == UsbDeviceKind::Iphone));
    }

    #[test]
    fn mask_id_keeps_tail() {
        assert_eq!(mask_id("0123456789ABCDEF"), "***CDEF");
        assert_eq!(mask_id("abc"), "****");
        assert_eq!(mask_id(""), "****");
    }
}
