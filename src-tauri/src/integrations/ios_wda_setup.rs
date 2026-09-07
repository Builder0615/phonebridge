//! In-app preparation flow for the optional iOS WDA precision channel.
//!
//! WDA is an XCTest runner, so the release pipeline must provide an IPA signed
//! for the target devices.  This module owns the repeatable runtime flow:
//! discover the phone, install the bundled signed IPA, launch the runner with
//! the cross-platform `go-ios` helper, forward a loopback port, and report the
//! exact step that needs attention.  macOS keeps a source/Xcode fallback for
//! development builds that do not contain the release artifact.  It never
//! accepts an arbitrary shell command from the UI.

use std::path::Path;

use serde::Serialize;

use super::AdapterError;

pub const WDA_SOURCE_VERSION: &str = "16.12.3";
pub const WDA_SOURCE_URL: &str =
    "https://registry.npmjs.org/appium-webdriveragent/-/appium-webdriveragent-16.12.3.tgz";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosWdaCheck {
    pub key: String,
    pub title: String,
    /// `ok` | `pending` | `action_required` | `failed` | `unsupported`.
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosWdaSetupReport {
    /// `ready` | `needs_action` | `preparing` | `failed` | `unsupported`.
    pub stage: String,
    pub ready: bool,
    pub device_id_masked: Option<String>,
    pub checks: Vec<IosWdaCheck>,
    pub detail: String,
}

fn check(key: &str, title: &str, status: &str, detail: impl Into<String>) -> IosWdaCheck {
    IosWdaCheck {
        key: key.into(),
        title: title.into(),
        status: status.into(),
        detail: detail.into(),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn unsupported_report() -> IosWdaSetupReport {
    IosWdaSetupReport {
        stage: "unsupported".into(),
        ready: false,
        device_id_masked: None,
        checks: vec![check(
            "platform",
            "WDA 自动安装环境",
            "unsupported",
            "当前平台不支持 iOS WDA 的 USB 安装与 XCTest 启动",
        )],
        detail: "当前平台不能执行 iOS WDA 的签名安装流程".into(),
    }
}

/// Read-only setup inspection.  The macOS implementation may briefly probe a
/// loopback WDA endpoint, but it does not create a WebDriver session or send
/// input to the phone.
pub fn inspect(
    resources: &Path,
    app_data: &Path,
    device_id_masked: Option<&str>,
) -> IosWdaSetupReport {
    #[cfg(target_os = "macos")]
    {
        if let Some(report) = prebuilt::inspect(resources, app_data, device_id_masked) {
            return report;
        }
        return macos::inspect(resources, app_data, device_id_masked);
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(report) = prebuilt::inspect(resources, app_data, device_id_masked) {
            return report;
        }
        return windows::inspect(resources, app_data, device_id_masked);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (resources, app_data, device_id_masked);
        unsupported_report()
    }
}

/// Explicit user-triggered preparation. All potentially external mutations
/// (installing the bundled WDA IPA, launching the bundled runner, or using the
/// macOS development fallback) happen only through this operation.
pub fn prepare(
    resources: &Path,
    app_data: &Path,
    device_id_masked: Option<&str>,
) -> IosWdaSetupReport {
    #[cfg(target_os = "macos")]
    {
        return macos::prepare(resources, app_data, device_id_masked);
    }
    #[cfg(target_os = "windows")]
    {
        return windows::prepare(resources, app_data, device_id_masked);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (resources, app_data, device_id_masked);
        unsupported_report()
    }
}

/// Stop any WDA `xcodebuild test` children owned by the app.
pub fn stop_all() {
    #[cfg(target_os = "macos")]
    macos::stop_all();
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    prebuilt::stop_all();
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::collections::HashMap;
    use std::fs::{self, OpenOptions};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ExitStatus, Output, Stdio};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use super::super::ios_usb_control::{probe_wda, resolve_iproxy};
    use super::super::process::hidden_command;
    use super::super::usb_devices::{list_usb_devices, UsbDevice, UsbDeviceKind};

    const WDA_WAIT_TIMEOUT: Duration = Duration::from_secs(90);
    const WDA_LOG_TAIL_BYTES: usize = 8 * 1024;

    struct WdaProcess {
        child: Child,
        log_path: PathBuf,
    }

    static WDA_PROCESSES: OnceLock<Mutex<HashMap<String, WdaProcess>>> = OnceLock::new();

    fn processes() -> &'static Mutex<HashMap<String, WdaProcess>> {
        WDA_PROCESSES.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn valid_project(path: &Path) -> bool {
        path.is_dir()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".xcodeproj"))
    }

    fn project_candidates(resources: &Path, app_data: &Path) -> Vec<PathBuf> {
        let mut candidates = Vec::new();
        if let Ok(path) = std::env::var("PHONEBRIDGE_WDA_PROJECT_PATH") {
            candidates.push(PathBuf::from(path));
        }
        candidates.extend([
            resources.join("ios-wda/WebDriverAgent.xcodeproj"),
            resources.join("binaries/ios-usb/WebDriverAgent.xcodeproj"),
            app_data
                .join("wda-source")
                .join(format!("appium-webdriveragent-{WDA_SOURCE_VERSION}"))
                .join("WebDriverAgent.xcodeproj"),
            std::env::current_dir()
                .unwrap_or_default()
                .join("node_modules/appium-webdriveragent/WebDriverAgent.xcodeproj"),
        ]);
        candidates
    }

    fn find_wda_project(resources: &Path, app_data: &Path) -> Option<PathBuf> {
        project_candidates(resources, app_data)
            .into_iter()
            .find(|path| valid_project(path))
    }

    fn xcode_version() -> Result<String, String> {
        let xcodebuild = Path::new("/usr/bin/xcodebuild");
        if !xcodebuild.is_file() {
            return Err(
                "未找到 /usr/bin/xcodebuild，请安装 Xcode 并在首次启动时完成组件安装".into(),
            );
        }
        let output = hidden_command(xcodebuild)
            .arg("-version")
            .output()
            .map_err(|error| format!("执行 xcodebuild -version 失败：{error}"))?;
        if !output.status.success() {
            return Err(format_output_error("xcodebuild -version", &output));
        }
        let version = String::from_utf8_lossy(&output.stdout)
            .lines()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");
        if version.is_empty() {
            Err("Xcode 已找到，但没有返回版本信息".into())
        } else {
            Ok(version)
        }
    }

    fn choose_device(
        resources: &Path,
        requested_id: Option<&str>,
    ) -> (Option<UsbDevice>, Option<String>) {
        let (devices, notes) = list_usb_devices(resources);
        let mut iphones: Vec<UsbDevice> = devices
            .into_iter()
            .filter(|device| device.kind == UsbDeviceKind::Iphone)
            .collect();
        if let Some(requested_id) = requested_id {
            if let Some(index) = iphones
                .iter()
                .position(|device| device.id_masked == requested_id)
            {
                return (Some(iphones.swap_remove(index)), None);
            }
            return (
                None,
                Some(format!(
                    "没有找到标识为 {requested_id} 的 USB iPhone；请保持 USB 连接并解锁设备"
                )),
            );
        }
        if let Some(device) = iphones.into_iter().next() {
            return (Some(device), None);
        }
        let detail = if notes.is_empty() {
            "没有发现通过 USB 连接的 iPhone；请连接、解锁并信任此 Mac".into()
        } else {
            format!("没有发现通过 USB 连接的 iPhone；{}", notes.join("；"))
        };
        (None, Some(detail))
    }

    fn tool_detail(path: Option<&Path>, name: &str) -> (String, String) {
        match path {
            Some(_) => ("ok".into(), format!("已找到 {name}")),
            None => ("action_required".into(), format!("未找到 {name}")),
        }
    }

    fn probe_configured_or_device(
        resources: &Path,
        device: Option<&UsbDevice>,
    ) -> Result<(), AdapterError> {
        if std::env::var("PHONEBRIDGE_IOS_WDA_URL").is_ok() {
            return probe_wda(resources, "");
        }
        let device = device.ok_or_else(|| {
            AdapterError::IosUsbUnavailable("没有可用于 WDA 检查的 USB iPhone".into())
        })?;
        if device.state != "connected" {
            return Err(AdapterError::IosUsbUnavailable(
                "iPhone 尚未完成 USB 配对/信任".into(),
            ));
        }
        probe_wda(resources, &device.raw_id)
    }

    fn inspect_inner(
        resources: &Path,
        app_data: &Path,
        requested_id: Option<&str>,
    ) -> (IosWdaSetupReport, Option<UsbDevice>, Option<PathBuf>) {
        let (device, device_error) = choose_device(resources, requested_id);
        let xcode = xcode_version();
        let iproxy = resolve_iproxy(resources);
        let project = find_wda_project(resources, app_data);
        let wda_probe = if xcode.is_ok() && iproxy.is_some() {
            probe_configured_or_device(resources, device.as_ref())
        } else {
            Err(AdapterError::IosUsbUnavailable(
                "WDA 宿主依赖尚未准备好".into(),
            ))
        };
        let wda_ready = wda_probe.is_ok();

        let mut checks = Vec::new();
        checks.push(check(
            "platform",
            "macOS + Xcode",
            "ok",
            "当前平台支持在应用内准备 WDA",
        ));
        checks.push(match xcode.as_ref() {
            Ok(version) => check("xcode", "Xcode / xcodebuild", "ok", version),
            Err(detail) => check("xcode", "Xcode / xcodebuild", "action_required", detail),
        });
        checks.push(match device.as_ref() {
            Some(device) if device.state == "connected" => check(
                "device",
                "USB iPhone",
                "ok",
                format!(
                    "{}（{}）已连接并可用于签名启动",
                    device.name, device.id_masked
                ),
            ),
            Some(device) => check(
                "device",
                "USB iPhone",
                "action_required",
                format!(
                    "{}（{}）状态为 {}；请解锁 iPhone、信任此 Mac，并开启开发者模式",
                    device.name, device.id_masked, device.state
                ),
            ),
            None => check(
                "device",
                "USB iPhone",
                "action_required",
                device_error
                    .clone()
                    .unwrap_or_else(|| "未发现 USB iPhone".into()),
            ),
        });
        let (iproxy_status, iproxy_detail) = tool_detail(iproxy.as_deref(), "iproxy");
        checks.push(check(
            "iproxy",
            "USB 隧道 iproxy",
            &iproxy_status,
            iproxy_detail,
        ));
        checks.push(match project.as_ref() {
            Some(_) => check(
                "wda_source",
                "WDA 工程",
                "ok",
                format!("已准备 Appium WebDriverAgent {WDA_SOURCE_VERSION} 源码"),
            ),
            None => check(
                "wda_source",
                "WDA 工程",
                "pending",
                format!("点击“一键准备 WDA”后将从官方 npm 源自动获取版本 {WDA_SOURCE_VERSION}"),
            ),
        });
        checks.push(if wda_ready {
            check(
                "wda_service",
                "WDA 服务",
                "ok",
                "WDA 已在 iPhone 上运行，USB 回环状态检查通过",
            )
        } else {
            check(
                "wda_service",
                "WDA 服务",
                "pending",
                wda_probe
                    .as_ref()
                    .err()
                    .map(|error| format!("尚未响应：{error}"))
                    .unwrap_or_else(|| "尚未响应".into()),
            )
        });

        let detail = if wda_ready {
            "WDA 已可用；开启 iOS USB/WDA 精确控制后重新投屏即可使用绝对坐标".into()
        } else if let Some(error) = device_error {
            error
        } else if xcode.is_err() {
            "请先安装并启动 Xcode，完成 iOS 组件安装".into()
        } else {
            "点击“一键准备 WDA”，应用会继续下载源码、启动签名流程并逐项检查结果".into()
        };
        (
            IosWdaSetupReport {
                stage: if wda_ready { "ready" } else { "needs_action" }.into(),
                ready: wda_ready,
                device_id_masked: device.as_ref().map(|device| device.id_masked.clone()),
                checks,
                detail,
            },
            device,
            project,
        )
    }

    pub(super) fn inspect(
        resources: &Path,
        app_data: &Path,
        requested_id: Option<&str>,
    ) -> IosWdaSetupReport {
        inspect_inner(resources, app_data, requested_id).0
    }

    fn set_check(
        report: &mut IosWdaSetupReport,
        key: &str,
        status: &str,
        detail: impl Into<String>,
    ) {
        if let Some(item) = report.checks.iter_mut().find(|item| item.key == key) {
            item.status = status.into();
            item.detail = detail.into();
        } else {
            report.checks.push(check(key, key, status, detail));
        }
    }

    fn summarize_output(output: &Output) -> String {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let text = if !stderr.trim().is_empty() {
            stderr
        } else {
            stdout
        };
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        let start = lines.len().saturating_sub(4);
        let detail = lines[start..].join("；");
        if detail.is_empty() {
            "没有返回可读错误".into()
        } else {
            detail.chars().take(1200).collect()
        }
    }

    fn format_output_error(command: &str, output: &Output) -> String {
        format!(
            "{command} 失败（{}）：{}",
            output.status,
            summarize_output(output)
        )
    }

    fn brew_path() -> Option<PathBuf> {
        ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
    }

    fn ensure_iproxy(resources: &Path) -> Result<PathBuf, String> {
        if let Some(path) = resolve_iproxy(resources) {
            return Ok(path);
        }
        let Some(brew) = brew_path() else {
            return Err(
                "系统没有找到 iproxy，也没有检测到 Homebrew；应用不能在没有用户确认的情况下替系统安装 Homebrew。请安装 Homebrew 后再次点击“一键准备 WDA”".into(),
            );
        };
        let output = hidden_command(&brew)
            .args(["install", "libusbmuxd"])
            .output()
            .map_err(|error| format!("调用 Homebrew 安装 iproxy 失败：{error}"))?;
        if !output.status.success() {
            return Err(format_output_error("brew install libusbmuxd", &output));
        }
        resolve_iproxy(resources)
            .ok_or_else(|| "Homebrew 安装完成，但应用仍未找到 iproxy；请重启快投屏后重试".into())
    }

    fn download_wda_source(app_data: &Path) -> Result<PathBuf, String> {
        let source_root = app_data.join("wda-source");
        let target = source_root.join(format!("appium-webdriveragent-{WDA_SOURCE_VERSION}"));
        let project = target.join("WebDriverAgent.xcodeproj");
        if valid_project(&project) {
            return Ok(project);
        }
        fs::create_dir_all(&source_root)
            .map_err(|error| format!("创建 WDA 源码目录失败：{error}"))?;
        let staging = source_root.join(format!(".staging-{}", std::process::id()));
        if staging.exists() {
            fs::remove_dir_all(&staging)
                .map_err(|error| format!("清理上次 WDA 下载临时目录失败：{error}"))?;
        }
        fs::create_dir_all(&staging)
            .map_err(|error| format!("创建 WDA 下载临时目录失败：{error}"))?;
        let archive = staging.join("wda.tgz");
        let curl = Path::new("/usr/bin/curl");
        if !curl.is_file() {
            return Err("未找到系统 curl，无法自动获取 WDA 源码".into());
        }
        let download = hidden_command(curl)
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--retry",
                "2",
                "--connect-timeout",
                "10",
                "--max-time",
                "180",
                "--output",
            ])
            .arg(&archive)
            .arg(WDA_SOURCE_URL)
            .output()
            .map_err(|error| format!("下载 WDA 源码失败：{error}"))?;
        if !download.status.success() {
            return Err(format_output_error("下载 WDA 源码", &download));
        }
        let tar = Path::new("/usr/bin/tar");
        if !tar.is_file() {
            return Err("未找到系统 tar，无法解压 WDA 源码".into());
        }
        let extract = hidden_command(tar)
            .args(["-xzf"])
            .arg(&archive)
            .args(["-C"])
            .arg(&staging)
            .output()
            .map_err(|error| format!("解压 WDA 源码失败：{error}"))?;
        if !extract.status.success() {
            return Err(format_output_error("解压 WDA 源码", &extract));
        }
        let package = staging.join("package");
        if !valid_project(&package.join("WebDriverAgent.xcodeproj")) {
            return Err("下载包中没有找到 WebDriverAgent.xcodeproj，已停止安装".into());
        }
        if target.exists() {
            fs::remove_dir_all(&target).map_err(|error| format!("替换旧 WDA 源码失败：{error}"))?;
        }
        fs::rename(&package, &target).map_err(|error| format!("保存 WDA 源码失败：{error}"))?;

        let checksum = hidden_command("/usr/bin/shasum")
            .args(["-a", "256"])
            .arg(&archive)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .unwrap_or_else(|| "unavailable".into());
        let manifest = serde_json::json!({
            "source": WDA_SOURCE_URL,
            "version": WDA_SOURCE_VERSION,
            "sha256": checksum,
            "note": "下载包内 LICENSE 文件随源码保留；仅在用户点击 WDA 准备后获取",
        });
        let _ = fs::write(
            target.join("phonebridge-source-manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
        );
        let _ = fs::remove_dir_all(&staging);
        Ok(target.join("WebDriverAgent.xcodeproj"))
    }

    fn redacted_tail(path: &Path, raw_udid: &str) -> String {
        let Ok(bytes) = fs::read(path) else {
            return "没有读取到 xcodebuild 日志；请打开 Xcode 查看签名错误".into();
        };
        let start = bytes.len().saturating_sub(WDA_LOG_TAIL_BYTES);
        let mut text = String::from_utf8_lossy(&bytes[start..]).to_string();
        if !raw_udid.is_empty() {
            text = text.replace(raw_udid, "<已隐藏设备标识>");
        }
        text.trim().chars().take(2400).collect()
    }

    fn launch_xcodebuild(
        project: &Path,
        app_data: &Path,
        raw_udid: &str,
    ) -> Result<PathBuf, String> {
        let mut table = processes().lock().unwrap();
        if let Some(existing) = table.get_mut(raw_udid) {
            if existing
                .child
                .try_wait()
                .map_err(|error| format!("读取 WDA 启动进程状态失败：{error}"))?
                .is_none()
            {
                return Ok(existing.log_path.clone());
            }
        }
        table.remove(raw_udid);

        let log_dir = app_data.join("wda-logs");
        fs::create_dir_all(&log_dir).map_err(|error| format!("创建 WDA 日志目录失败：{error}"))?;
        let log_path = log_dir.join(format!("wda-{}.log", std::process::id()));
        let log = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)
            .map_err(|error| format!("创建 WDA 启动日志失败：{error}"))?;
        let stdout = log
            .try_clone()
            .map_err(|error| format!("准备 WDA 标准输出失败：{error}"))?;
        let project_name = project
            .file_name()
            .ok_or_else(|| "WDA 工程路径无效".to_string())?;
        let destination = format!("id={raw_udid}");
        let derived_data = app_data.join("wda-derived-data");
        fs::create_dir_all(&derived_data)
            .map_err(|error| format!("创建 WDA 构建目录失败：{error}"))?;
        let mut command = hidden_command("/usr/bin/xcodebuild");
        command
            .current_dir(project.parent().unwrap_or_else(|| Path::new("/")))
            .args(["-project"])
            .arg(project_name)
            .args(["-scheme", "WebDriverAgentRunner", "-destination"])
            .arg(destination)
            .args(["-derivedDataPath"])
            .arg(&derived_data)
            .args([
                "-allowProvisioningUpdates",
                "-allowProvisioningDeviceRegistration",
                "CODE_SIGN_STYLE=Automatic",
                "COMPILER_INDEX_STORE_ENABLE=NO",
                "test",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(log));
        if let Ok(team) = std::env::var("PHONEBRIDGE_IOS_DEVELOPMENT_TEAM") {
            if !team.is_empty()
                && team.len() <= 32
                && team
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
            {
                command.env("DEVELOPMENT_TEAM", team);
            }
        }
        let child = command
            .spawn()
            .map_err(|error| format!("启动 xcodebuild 失败：{error}"))?;
        table.insert(
            raw_udid.to_string(),
            WdaProcess {
                child,
                log_path: log_path.clone(),
            },
        );
        Ok(log_path)
    }

    fn process_exit(raw_udid: &str) -> Option<ExitStatus> {
        let mut table = processes().lock().unwrap();
        let process = table.get_mut(raw_udid)?;
        match process.child.try_wait() {
            Ok(Some(status)) => {
                table.remove(raw_udid);
                Some(status)
            }
            Ok(None) | Err(_) => None,
        }
    }

    fn success_report(
        mut report: IosWdaSetupReport,
        detail: impl Into<String>,
    ) -> IosWdaSetupReport {
        report.stage = "ready".into();
        report.ready = true;
        report.detail = detail.into();
        set_check(
            &mut report,
            "wda_service",
            "ok",
            "WDA 已在 iPhone 上运行，USB 回环状态检查通过",
        );
        report
    }

    pub(super) fn prepare(
        resources: &Path,
        app_data: &Path,
        requested_id: Option<&str>,
    ) -> IosWdaSetupReport {
        if let Some(report) = super::prebuilt::prepare(resources, app_data, requested_id) {
            return report;
        }
        let (mut report, mut device, mut project) =
            inspect_inner(resources, app_data, requested_id);
        if report.ready {
            return report;
        }

        let Some(selected) = device.take() else {
            report.stage = "needs_action".into();
            return report;
        };
        report.device_id_masked = Some(selected.id_masked.clone());
        if selected.state != "connected" {
            report.detail = format!(
                "iPhone 当前状态为 {}；请在手机上解锁、信任此 Mac，并开启开发者模式后重试",
                selected.state
            );
            let detail = report.detail.clone();
            set_check(&mut report, "device", "action_required", detail);
            return report;
        }

        if let Err(error) = xcode_version() {
            report.detail = error.clone();
            set_check(&mut report, "xcode", "action_required", error);
            return report;
        }

        if let Err(error) = ensure_iproxy(resources) {
            report.stage = "failed".into();
            report.detail = error.clone();
            set_check(&mut report, "iproxy", "failed", error);
            return report;
        }
        set_check(
            &mut report,
            "iproxy",
            "ok",
            "已找到或已由 Homebrew 准备 iproxy",
        );

        if project.is_none() {
            match download_wda_source(app_data) {
                Ok(path) => {
                    project = Some(path);
                    set_check(
                        &mut report,
                        "wda_source",
                        "ok",
                        format!("已自动获取并准备 WDA {WDA_SOURCE_VERSION} 源码"),
                    );
                }
                Err(error) => {
                    report.stage = "failed".into();
                    report.detail = error.clone();
                    set_check(&mut report, "wda_source", "failed", error);
                    return report;
                }
            }
        }
        let Some(project) = project else {
            report.stage = "failed".into();
            report.detail = "WDA 工程准备失败".into();
            return report;
        };

        if probe_wda(resources, &selected.raw_id).is_ok() {
            return success_report(
                report,
                "WDA 已经在 iPhone 上运行；开启 iOS USB/WDA 精确控制后重新投屏即可",
            );
        }

        let log_path = match launch_xcodebuild(&project, app_data, &selected.raw_id) {
            Ok(path) => path,
            Err(error) => {
                report.stage = "failed".into();
                report.detail = error.clone();
                set_check(&mut report, "wda_service", "failed", error);
                return report;
            }
        };
        report.stage = "preparing".into();
        report.detail =
            "正在通过 Xcode 签名、安装并启动 WDA；首次运行可能需要在 iPhone 上确认开发者模式/信任"
                .into();
        let detail = report.detail.clone();
        set_check(&mut report, "wda_service", "pending", detail);

        let deadline = Instant::now() + WDA_WAIT_TIMEOUT;
        loop {
            if probe_wda(resources, &selected.raw_id).is_ok() {
                return success_report(
                    report,
                    "WDA 已由应用完成启动；现在可以开启 iOS USB/WDA 精确控制并重新投屏",
                );
            }
            if let Some(status) = process_exit(&selected.raw_id) {
                let log_detail = redacted_tail(&log_path, &selected.raw_id);
                report.stage = "failed".into();
                report.detail = if status.success() {
                    "xcodebuild 已结束，但 WDA 没有响应；请确认 iPhone 处于解锁状态并重试".into()
                } else {
                    format!("WDA 启动失败：{log_detail}")
                };
                let detail = report.detail.clone();
                set_check(&mut report, "wda_service", "failed", detail);
                return report;
            }
            if Instant::now() >= deadline {
                report.stage = "failed".into();
                report.detail = format!(
                    "等待 WDA 超时：{}；请查看 Xcode 签名/开发者模式提示后重试",
                    redacted_tail(&log_path, &selected.raw_id)
                );
                let detail = report.detail.clone();
                set_check(&mut report, "wda_service", "failed", detail);
                return report;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    pub(super) fn stop_all() {
        let mut table = processes().lock().unwrap();
        for (_, process) in table.iter_mut() {
            let _ = process.child.kill();
            let _ = process.child.wait();
        }
        table.clear();
    }
}

/// Shared direct-install path for a signed WDA artifact.  It is intentionally
/// separate from the macOS source/build fallback: the exact same signed IPA
/// and runner works on macOS and Windows, while `go-ios` supplies the
/// cross-platform XCTest/RemoteXPC launch and host-port forwarding.
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod prebuilt {
    use super::*;
    use std::collections::HashMap;
    use std::fs::{self, OpenOptions};
    use std::io::{BufRead, BufReader, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Output, Stdio};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use super::super::ios_usb_control::{
        allocate_local_port, probe_wda, probe_wda_local, register_wda_endpoint,
        unregister_wda_endpoint,
    };
    use super::super::process::hidden_command;
    use super::super::usb_devices::{list_usb_devices, UsbDevice, UsbDeviceKind};

    const WDA_RUNNER_TIMEOUT: Duration = Duration::from_secs(120);

    struct RunnerProcess {
        child: Child,
        host_port: u16,
        log_path: PathBuf,
    }

    static RUNNERS: OnceLock<Mutex<HashMap<String, RunnerProcess>>> = OnceLock::new();

    fn runners() -> &'static Mutex<HashMap<String, RunnerProcess>> {
        RUNNERS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn exe(name: &str) -> String {
        if cfg!(target_os = "windows") {
            format!("{name}.exe")
        } else {
            name.into()
        }
    }

    fn resolve_tool(resources: &Path, name: &str, env_name: &str) -> Option<PathBuf> {
        let executable = exe(name);
        let mut candidates = vec![
            resources.join("binaries").join(&executable),
            resources.join("binaries/ios-usb").join(&executable),
            resources.join(&executable),
        ];
        if let Ok(path) = std::env::var(env_name) {
            let path = PathBuf::from(path);
            candidates.push(if path.is_dir() {
                path.join(&executable)
            } else {
                path
            });
        }
        if let Some(path) = std::env::var_os("PATH") {
            candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(&executable)));
        }
        candidates.into_iter().find(|path| path.is_file())
    }

    fn signed_ipa(resources: &Path, app_data: &Path) -> Option<PathBuf> {
        let mut candidates = Vec::new();
        if let Ok(path) = std::env::var("PHONEBRIDGE_WDA_IPA_PATH") {
            candidates.push(PathBuf::from(path));
        }
        candidates.extend([
            resources.join("binaries/ios-usb/WebDriverAgentRunner.ipa"),
            resources.join("binaries/ios-wda/WebDriverAgentRunner.ipa"),
            app_data.join("wda/WebDriverAgentRunner.ipa"),
        ]);
        candidates.into_iter().find(|path| path.is_file())
    }

    fn valid_bundle_id(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 255
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
            })
    }

    fn bundle_id_from_manifest(path: &Path) -> Option<String> {
        let text = fs::read_to_string(path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        let bundle_id = value.get("bundleId")?.as_str()?.trim();
        valid_bundle_id(bundle_id).then(|| bundle_id.to_string())
    }

    fn wda_bundle_id(resources: &Path, app_data: &Path) -> String {
        if let Ok(value) = std::env::var("PHONEBRIDGE_WDA_BUNDLE_ID") {
            let value = value.trim();
            if valid_bundle_id(value) {
                return value.to_string();
            }
        }
        [
            resources.join("binaries/ios-usb/WebDriverAgentRunner.json"),
            resources.join("binaries/ios-wda/WebDriverAgentRunner.json"),
            app_data.join("wda/WebDriverAgentRunner.json"),
        ]
        .iter()
        .find_map(|path| bundle_id_from_manifest(path))
        .unwrap_or_else(|| "com.facebook.WebDriverAgentRunner.xctrunner".into())
    }

    fn choose_device(
        resources: &Path,
        requested_id: Option<&str>,
    ) -> (Option<UsbDevice>, Option<String>) {
        let (devices, notes) = list_usb_devices(resources);
        let mut iphones: Vec<UsbDevice> = devices
            .into_iter()
            .filter(|device| device.kind == UsbDeviceKind::Iphone)
            .collect();
        if let Some(requested_id) = requested_id {
            if let Some(index) = iphones
                .iter()
                .position(|device| device.id_masked == requested_id)
            {
                return (Some(iphones.swap_remove(index)), None);
            }
            return (
                None,
                Some(format!("没有找到标识为 {requested_id} 的 USB iPhone")),
            );
        }
        if let Some(device) = iphones.into_iter().next() {
            return (Some(device), None);
        }
        (
            None,
            Some(if notes.is_empty() {
                "没有发现 USB iPhone；请连接、解锁并信任此电脑".into()
            } else {
                format!("没有发现 USB iPhone；{}", notes.join("；"))
            }),
        )
    }

    fn set_check(
        report: &mut IosWdaSetupReport,
        key: &str,
        title: &str,
        status: &str,
        detail: impl Into<String>,
    ) {
        report.checks.push(check(key, title, status, detail));
    }

    fn base_report(
        ipa: &Path,
        installer: Option<&Path>,
        runner: Option<&Path>,
        device: Option<&UsbDevice>,
        device_error: Option<String>,
        bundle_id: &str,
    ) -> IosWdaSetupReport {
        let mut report = IosWdaSetupReport {
            stage: "needs_action".into(),
            ready: false,
            device_id_masked: device.map(|device| device.id_masked.clone()),
            checks: Vec::new(),
            detail: String::new(),
        };
        set_check(
            &mut report,
            "platform",
            "WDA 运行方式",
            "ok",
            "使用内置签名 IPA + go-ios；macOS 和 Windows 走同一套安装/启动流程",
        );
        set_check(
            &mut report,
            "wda_artifact",
            "签名 WDA 包",
            "ok",
            format!(
                "已找到 {}（构建时会记录 SHA-256）",
                ipa.file_name().unwrap_or_default().to_string_lossy()
            ),
        );
        set_check(
            &mut report,
            "wda_bundle_id",
            "WDA Bundle ID",
            "ok",
            bundle_id,
        );
        set_check(
            &mut report,
            "ideviceinstaller",
            "iOS 安装器",
            if installer.is_some() {
                "ok"
            } else {
                "action_required"
            },
            if installer.is_some() {
                "已找到 ideviceinstaller，应用可直接安装 WDA"
            } else {
                "缺少 ideviceinstaller；发布包需要内置对应平台的审计构建物和 DLL"
            },
        );
        set_check(
            &mut report,
            "go_ios",
            "WDA 启动器 / USB 隧道",
            if runner.is_some() {
                "ok"
            } else {
                "action_required"
            },
            if runner.is_some() {
                "已找到 go-ios，应用可启动 WDA 并建立本机回环端口"
            } else {
                "缺少 go-ios；Windows 不能用 xcodebuild，macOS 也无法完成跨平台直接启动"
            },
        );
        set_check(
            &mut report,
            "device",
            "USB iPhone",
            match device {
                Some(device) if device.state == "connected" => "ok",
                Some(_) => "action_required",
                None => "action_required",
            },
            match device {
                Some(device) if device.state == "connected" => {
                    format!("{}（{}）已连接", device.name, device.id_masked)
                }
                Some(device) => format!("设备状态为 {}；请解锁并信任此电脑", device.state),
                None => device_error.unwrap_or_else(|| "未发现 USB iPhone".into()),
            },
        );
        set_check(
            &mut report,
            "wda_service",
            "WDA 服务",
            "pending",
            "尚未验证；点击准备后应用会安装、启动并轮询 /status",
        );
        report.detail = "应用会安装内置签名 WDA，并用 go-ios 启动 XCTest runner；首次运行仍可能需要 iPhone 的开发者模式/信任确认".into();
        report
    }

    fn output_detail(output: &Output, raw_udid: &str) -> String {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut detail = if !stderr.trim().is_empty() {
            stderr.to_string()
        } else {
            stdout.to_string()
        };
        if !raw_udid.is_empty() {
            detail = detail.replace(raw_udid, "<已隐藏设备标识>");
        }
        let lines: Vec<&str> = detail
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        let start = lines.len().saturating_sub(5);
        let text = lines[start..].join("；");
        if text.is_empty() {
            "没有返回可读错误".into()
        } else {
            text.chars().take(1800).collect()
        }
    }

    fn log_tail(path: &Path, raw_udid: &str) -> String {
        let Ok(bytes) = fs::read(path) else {
            return "没有读取到 go-ios 日志".into();
        };
        let start = bytes.len().saturating_sub(8 * 1024);
        let mut text = String::from_utf8_lossy(&bytes[start..]).to_string();
        if !raw_udid.is_empty() {
            text = text.replace(raw_udid, "<已隐藏设备标识>");
        }
        text.trim().chars().take(1800).collect()
    }

    fn forward_redacted_output<R>(reader: R, log_path: PathBuf, raw_udid: String)
    where
        R: std::io::Read + Send + 'static,
    {
        std::thread::spawn(move || {
            let Ok(mut log) = OpenOptions::new().create(true).append(true).open(log_path) else {
                return;
            };
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let line = if raw_udid.is_empty() {
                    line
                } else {
                    line.replace(&raw_udid, "<已隐藏设备标识>")
                };
                let _ = writeln!(log, "{line}");
            }
        });
    }

    fn install(ideviceinstaller: &Path, ipa: &Path, raw_udid: &str) -> Result<(), String> {
        let output = hidden_command(ideviceinstaller)
            .args(["-u", raw_udid, "install"])
            .arg(ipa)
            .output()
            .map_err(|error| format!("启动 ideviceinstaller 失败：{error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "WDA IPA 安装失败：{}",
                output_detail(&output, raw_udid)
            ))
        }
    }

    fn start_runner(
        runner: &Path,
        app_data: &Path,
        raw_udid: &str,
        bundle_id: &str,
    ) -> Result<(u16, PathBuf), String> {
        let mut table = runners().lock().unwrap();
        if let Some(existing) = table.get_mut(raw_udid) {
            if existing
                .child
                .try_wait()
                .map_err(|error| format!("读取 go-ios 状态失败：{error}"))?
                .is_none()
            {
                register_wda_endpoint(raw_udid, existing.host_port);
                return Ok((existing.host_port, existing.log_path.clone()));
            }
        }
        table.remove(raw_udid);
        let host_port = allocate_local_port().map_err(|error| error.to_string())?;
        let log_dir = app_data.join("wda-logs");
        fs::create_dir_all(&log_dir).map_err(|error| format!("创建 WDA 日志目录失败：{error}"))?;
        let log_path = log_dir.join(format!("go-ios-wda-{}.log", std::process::id()));
        let _log = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)
            .map_err(|error| format!("创建 go-ios 日志失败：{error}"))?;
        let mut command = hidden_command(runner);
        command
            .args(["ui", "run", "wda"])
            .arg(format!("--udid={raw_udid}"))
            .arg(format!("--bundleid={bundle_id}"))
            .arg(format!("--test-runner-bundleid={bundle_id}"))
            .arg("--xctest-config=WebDriverAgentRunner.xctest")
            .arg(format!("--host-port={host_port}"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("启动 go-ios WDA runner 失败：{error}"))?;
        if let Some(stdout) = child.stdout.take() {
            forward_redacted_output(stdout, log_path.clone(), raw_udid.to_string());
        }
        if let Some(stderr) = child.stderr.take() {
            forward_redacted_output(stderr, log_path.clone(), raw_udid.to_string());
        }
        register_wda_endpoint(raw_udid, host_port);
        table.insert(
            raw_udid.to_string(),
            RunnerProcess {
                child,
                host_port,
                log_path: log_path.clone(),
            },
        );
        Ok((host_port, log_path))
    }

    fn process_exited(raw_udid: &str) -> Option<bool> {
        let mut table = runners().lock().unwrap();
        let process = table.get_mut(raw_udid)?;
        match process.child.try_wait() {
            Ok(Some(status)) => {
                table.remove(raw_udid);
                unregister_wda_endpoint(raw_udid);
                Some(status.success())
            }
            Ok(None) | Err(_) => None,
        }
    }

    pub(super) fn inspect(
        resources: &Path,
        app_data: &Path,
        requested_id: Option<&str>,
    ) -> Option<IosWdaSetupReport> {
        let ipa = signed_ipa(resources, app_data)?;
        let installer = resolve_tool(
            resources,
            "ideviceinstaller",
            "PHONEBRIDGE_IDEVICEINSTALLER_PATH",
        );
        let runner = resolve_tool(resources, "ios", "PHONEBRIDGE_GO_IOS_PATH");
        let (device, device_error) = choose_device(resources, requested_id);
        let bundle_id = wda_bundle_id(resources, app_data);
        let mut report = base_report(
            &ipa,
            installer.as_deref(),
            runner.as_deref(),
            device.as_ref(),
            device_error,
            &bundle_id,
        );
        if let Some(device) = device.as_ref().filter(|device| device.state == "connected") {
            if probe_wda(resources, &device.raw_id).is_ok() {
                report.stage = "ready".into();
                report.ready = true;
                report.detail = "内置 WDA 已在设备上运行；应用已验证 USB 控制服务".into();
                if let Some(check) = report
                    .checks
                    .iter_mut()
                    .find(|check| check.key == "wda_service")
                {
                    check.status = "ok".into();
                    check.detail = "已通过 USB 回环连接到 WDA".into();
                }
            }
        }
        Some(report)
    }

    pub(super) fn prepare(
        resources: &Path,
        app_data: &Path,
        requested_id: Option<&str>,
    ) -> Option<IosWdaSetupReport> {
        let ipa = signed_ipa(resources, app_data)?;
        let installer = resolve_tool(
            resources,
            "ideviceinstaller",
            "PHONEBRIDGE_IDEVICEINSTALLER_PATH",
        );
        let runner = resolve_tool(resources, "ios", "PHONEBRIDGE_GO_IOS_PATH");
        let (device, device_error) = choose_device(resources, requested_id);
        let bundle_id = wda_bundle_id(resources, app_data);
        let mut report = base_report(
            &ipa,
            installer.as_deref(),
            runner.as_deref(),
            device.as_ref(),
            device_error,
            &bundle_id,
        );
        if let (Some(device), Some(installer), Some(runner)) =
            (device.as_ref(), installer.as_ref(), runner.as_ref())
        {
            if device.state != "connected" {
                report.detail = "请先解锁 iPhone、信任此电脑并开启开发者模式".into();
                return Some(report);
            }
            if probe_wda(resources, &device.raw_id).is_ok() {
                report.stage = "ready".into();
                report.ready = true;
                report.detail = "WDA 已经在设备上运行；应用已验证 USB 控制服务".into();
                if let Some(check) = report
                    .checks
                    .iter_mut()
                    .find(|check| check.key == "wda_service")
                {
                    check.status = "ok".into();
                    check.detail = "已通过 USB 隧道连接到 WDA".into();
                }
                return Some(report);
            }
            if let Err(error) = install(installer, &ipa, &device.raw_id) {
                report.stage = "failed".into();
                report.detail = error.clone();
                if let Some(check) = report
                    .checks
                    .iter_mut()
                    .find(|check| check.key == "wda_service")
                {
                    check.status = "failed".into();
                    check.detail = error;
                }
                return Some(report);
            }
            let (host_port, log_path) =
                match start_runner(runner, app_data, &device.raw_id, &bundle_id) {
                    Ok(value) => value,
                    Err(error) => {
                        report.stage = "failed".into();
                        report.detail = error.clone();
                        if let Some(check) = report
                            .checks
                            .iter_mut()
                            .find(|check| check.key == "wda_service")
                        {
                            check.status = "failed".into();
                            check.detail = error;
                        }
                        return Some(report);
                    }
                };
            report.stage = "preparing".into();
            report.detail =
                "正在安装并启动内置 WDA；首次运行可能需要在 iPhone 上确认开发者模式/信任".into();
            if let Some(check) = report
                .checks
                .iter_mut()
                .find(|check| check.key == "wda_service")
            {
                check.detail = report.detail.clone();
            }
            let deadline = Instant::now() + WDA_RUNNER_TIMEOUT;
            loop {
                if probe_wda_local(host_port).is_ok() {
                    report.stage = "ready".into();
                    report.ready = true;
                    report.detail =
                        "内置签名 WDA 已安装并启动；现在可以重新投屏使用 USB/WDA 精确控制".into();
                    if let Some(check) = report
                        .checks
                        .iter_mut()
                        .find(|check| check.key == "wda_service")
                    {
                        check.status = "ok".into();
                        check.detail = "WDA /status 已响应".into();
                    }
                    return Some(report);
                }
                if let Some(success) = process_exited(&device.raw_id) {
                    report.stage = "failed".into();
                    report.detail = if success {
                        "go-ios 已结束，但 WDA 没有响应；请检查开发者模式和签名 profile".into()
                    } else {
                        format!("WDA 启动失败：{}", log_tail(&log_path, &device.raw_id))
                    };
                    if let Some(check) = report
                        .checks
                        .iter_mut()
                        .find(|check| check.key == "wda_service")
                    {
                        check.status = "failed".into();
                        check.detail = report.detail.clone();
                    }
                    return Some(report);
                }
                if Instant::now() >= deadline {
                    report.stage = "failed".into();
                    report.detail = format!(
                        "等待 WDA 超时：{}；请检查开发者模式、设备信任和签名 profile",
                        log_tail(&log_path, &device.raw_id)
                    );
                    if let Some(check) = report
                        .checks
                        .iter_mut()
                        .find(|check| check.key == "wda_service")
                    {
                        check.status = "failed".into();
                        check.detail = report.detail.clone();
                    }
                    return Some(report);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        Some(report)
    }

    pub(super) fn stop_all() {
        let mut table = runners().lock().unwrap();
        for (raw_udid, process) in table.iter_mut() {
            let _ = process.child.kill();
            let _ = process.child.wait();
            unregister_wda_endpoint(raw_udid);
        }
        table.clear();
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::*;
    use std::path::Path;

    use super::super::ios_usb_control::{probe_wda, resolve_iproxy};
    use super::super::usb_devices::{list_usb_devices, UsbDevice, UsbDeviceKind};

    fn choose_device(
        resources: &Path,
        requested_id: Option<&str>,
    ) -> (Option<UsbDevice>, Option<String>) {
        let (devices, notes) = list_usb_devices(resources);
        let mut iphones: Vec<UsbDevice> = devices
            .into_iter()
            .filter(|device| device.kind == UsbDeviceKind::Iphone)
            .collect();
        if let Some(requested_id) = requested_id {
            if let Some(index) = iphones
                .iter()
                .position(|device| device.id_masked == requested_id)
            {
                return (Some(iphones.swap_remove(index)), None);
            }
            return (
                None,
                Some(format!(
                    "没有找到标识为 {requested_id} 的 USB iPhone；请保持 USB 连接并解锁设备"
                )),
            );
        }
        if let Some(device) = iphones.into_iter().next() {
            return (Some(device), None);
        }
        let detail = if notes.is_empty() {
            "没有发现 USB iPhone；请连接、解锁并信任此 Windows 电脑".into()
        } else {
            format!("没有发现 USB iPhone；{}", notes.join("；"))
        };
        (None, Some(detail))
    }

    fn inspect_inner(
        resources: &Path,
        app_data: &Path,
        requested_id: Option<&str>,
    ) -> (IosWdaSetupReport, Option<UsbDevice>) {
        let _ = app_data;
        let (device, device_error) = choose_device(resources, requested_id);
        let iproxy = resolve_iproxy(resources);
        let wda_probe = if iproxy.is_some() {
            if let Some(device) = device.as_ref().filter(|device| device.state == "connected") {
                probe_wda(resources, &device.raw_id)
            } else {
                Err(AdapterError::IosUsbUnavailable(
                    "iPhone 尚未完成 USB 配对/信任".into(),
                ))
            }
        } else {
            Err(AdapterError::IosUsbUnavailable("未找到 iproxy.exe".into()))
        };
        let wda_ready = wda_probe.is_ok();
        let mut checks = vec![check(
            "platform",
            "Windows WDA 运行模式",
            "ok",
            "Windows 不需要 Xcode；发布包由应用安装签名 WDA，并负责启动、USB 转发、状态检查和绝对输入",
        )];
        checks.push(match device.as_ref() {
            Some(device) if device.state == "connected" => check(
                "device",
                "USB iPhone",
                "ok",
                format!("{}（{}）已连接", device.name, device.id_masked),
            ),
            Some(device) => check(
                "device",
                "USB iPhone",
                "action_required",
                format!(
                    "{}（{}）状态为 {}；请解锁、信任此电脑，并开启开发者模式",
                    device.name, device.id_masked, device.state
                ),
            ),
            None => check(
                "device",
                "USB iPhone",
                "action_required",
                device_error
                    .clone()
                    .unwrap_or_else(|| "未发现 USB iPhone".into()),
            ),
        });
        checks.push(match iproxy.as_ref() {
            Some(_) => check(
                "iproxy",
                "USB 隧道 iproxy.exe",
                "ok",
                "已找到 iproxy.exe；Windows 运行期不调用 Xcode/xcrun",
            ),
            None => check(
                "iproxy",
                "USB 隧道 iproxy.exe",
                "action_required",
                "发布包需要内置经过审计的 iproxy.exe 及其 DLL，或通过 PHONEBRIDGE_IPROXY_PATH 指定",
            ),
        });
        checks.push(check(
            "wda_source",
            "签名 WDA 包",
            "action_required",
            "当前构建未内置 WebDriverAgentRunner.ipa；发布包应通过 PHONEBRIDGE_WDA_IPA_PATH 纳入签名包",
        ));
        checks.push(if wda_ready {
            check(
                "wda_service",
                "WDA 服务",
                "ok",
                "已通过 USB 隧道连接到 iPhone 上运行的 WDA",
            )
        } else {
            check(
                "wda_service",
                "WDA 服务",
                "action_required",
                "未连接到 WDA；点击准备会安装内置签名包并由 go-ios 启动，当前开发构建也可使用已运行的外部 WDA",
            )
        });
        let detail = if wda_ready {
            "WDA 已可用；开启 iOS USB/WDA 精确控制后重新投屏即可".into()
        } else if device.is_none() {
            device_error.unwrap_or_else(|| "请连接 USB iPhone".into())
        } else {
            "当前 Windows 开发构建没有内置签名 WDA；发布构建会由应用自动安装并启动，开发调试可暂时使用已签名运行的 WDA".into()
        };
        (
            IosWdaSetupReport {
                stage: if wda_ready { "ready" } else { "needs_action" }.into(),
                ready: wda_ready,
                device_id_masked: device.as_ref().map(|device| device.id_masked.clone()),
                checks,
                detail,
            },
            device,
        )
    }

    pub(super) fn inspect(
        resources: &Path,
        app_data: &Path,
        requested_id: Option<&str>,
    ) -> IosWdaSetupReport {
        inspect_inner(resources, app_data, requested_id).0
    }

    pub(super) fn prepare(
        resources: &Path,
        app_data: &Path,
        requested_id: Option<&str>,
    ) -> IosWdaSetupReport {
        if let Some(report) = super::prebuilt::prepare(resources, app_data, requested_id) {
            return report;
        }
        let (mut report, device) = inspect_inner(resources, app_data, requested_id);
        if report.ready {
            return report;
        }
        let Some(device) = device else {
            return report;
        };
        if device.state != "connected" {
            report.detail =
                "请先解锁 iPhone、信任此 Windows 电脑并开启开发者模式，再点击准备".into();
            return report;
        }
        if resolve_iproxy(resources).is_none() {
            report.stage = "failed".into();
            report.detail =
                "Windows 端缺少 iproxy.exe；发布包必须内置经过审计的 iproxy.exe 及其 DLL".into();
            return report;
        }
        // Do not pretend an unsigned WDA source tree can be installed on
        // Windows.  The app can still complete the useful part of the flow:
        // re-check the signed runner after the user/IT provisioning step.
        report.stage = "needs_action".into();
        report.detail = "Windows 已具备 USB 通道，但 WDA 尚未运行。请用已签名的 WDA 运行器启动一次；启动后再次点击此按钮，应用会自动验证并切换到精确控制".into();
        let _ = app_data;
        report
    }
}

#[cfg(test)]
mod tests {
    use super::{IosWdaCheck, IosWdaSetupReport};

    #[test]
    fn report_serializes_stable_camel_case_fields() {
        let report = IosWdaSetupReport {
            stage: "ready".into(),
            ready: true,
            device_id_masked: Some("***0123".into()),
            checks: vec![IosWdaCheck {
                key: "device".into(),
                title: "USB iPhone".into(),
                status: "ok".into(),
                detail: "ok".into(),
            }],
            detail: "ok".into(),
        };
        let value = serde_json::to_value(report).expect("report should serialize");
        assert_eq!(value["deviceIdMasked"], "***0123");
        assert_eq!(
            value["checks"][0]["deviceIdMasked"],
            serde_json::Value::Null
        );
    }
}
