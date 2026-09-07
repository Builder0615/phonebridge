//! ADB 工具：Android 设备枚举、屏幕尺寸查询与输入命令（scrcpy/ADB 链路）。
//!
//! 约束（Spec §5.1、AGENTS.md）：
//! - ADB 只做结构化调用（`adb devices`、`adb -s <serial> shell wm size`、
//!   `adb -s <serial> shell input …`），参数由本模块白名单生成，不拼接用户输入；
//! - 不读取设备文件、剪贴板或认证材料；
//! - 序列号只在 UI/诊断中脱敏展示（保留末 4 位）。

use std::path::{Path, PathBuf};
use std::process::Command;

use super::AdapterError;

// ---------------------------------------------------------------------------
// 工具定位：应用内置 sidecar 优先，再查环境变量与常见安装位置
// ---------------------------------------------------------------------------

/// 可执行文件名（按平台加 .exe）。
fn exe_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// 常见安装目录（按平台；不做存在性检查，由调用方逐个探测）：
/// - Android SDK：`$ANDROID_HOME/platform-tools`、`$ANDROID_SDK_ROOT/platform-tools`；
///   macOS `~/Library/Android/sdk/platform-tools`；Windows `%LOCALAPPDATA%/Android/Sdk/...`
/// - 通用工具目录：Homebrew、/usr/local、~/.local/bin 等。
fn common_search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    // Android SDK 环境变量
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(home) = std::env::var(var) {
            if !home.trim().is_empty() {
                dirs.push(PathBuf::from(home).join("platform-tools"));
            }
        }
    }

    // 用户目录
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        #[cfg(target_os = "macos")]
        dirs.push(home.join("Library/Android/sdk/platform-tools"));
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join("bin"));
    }

    // Windows SDK 位置（结合 LOCALAPPDATA / USERPROFILE）
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Android/Sdk/platform-tools"));
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            dirs.push(PathBuf::from(profile).join("AppData/Local/Android/Sdk/platform-tools"));
            dirs.push(PathBuf::from(profile).join("Android/Sdk/platform-tools"));
        }
        dirs.push(PathBuf::from("C:\\Android\\platform-tools"));
    }

    // 通用系统目录（GUI 启动的 Tauri 进程 PATH 常缺这些）
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/usr/bin"));

    dirs
}

/// PATH 环境变量遍历。
fn search_path(name: &str) -> Option<PathBuf> {
    let exe = exe_name(name);
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(&exe);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// 用 shell（登录 shell，读取用户 PATH 配置）兜底查询。
fn search_shell(name: &str) -> Option<PathBuf> {
    #[cfg(not(target_os = "windows"))]
    {
        // 固定命令，不拼接用户输入；GUI 进程里 shell 会加载用户配置拿到真实 PATH。
        let out = Command::new("sh")
            .args(["-lc", &format!("command -v {name}")])
            .output()
            .ok()?;
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
        None
    }
    #[cfg(target_os = "windows")]
    {
        let out = Command::new("where").arg(name).output().ok()?;
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()?
                .trim()
                .to_string();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
        None
    }
}

/// 统一工具定位。查找顺序（满足「优先用 app 打包的 sidecar」）：
/// 1. `resources/binaries/<name>[.exe]`（应用内置，发布时经 bundle.externalBin 打进包）
/// 2. `PHONEBRIDGE_<NAME>_PATH` 环境变量（文件或目录）
/// 3. Android SDK / Homebrew 等常见位置
/// 4. PATH 环境变量
/// 5. `command -v` / `where`（GUI 进程 PATH 不全时的兜底）
///
/// 失败时错误信息包含已探测的来源摘要，便于用户定位。
pub fn resolve_tool(resources: &Path, name: &str, env_var: &str) -> Result<PathBuf, String> {
    let exe = exe_name(name);
    let mut probed: Vec<String> = Vec::new();

    // 1. 应用内置 sidecar（最高优先）。候选布局（release 打包后）：
    //    - 包内约定目录：<resource_dir>/binaries/<name>
    //    - Windows/Linux externalBin：<resource_dir>/<name>
    //    - macOS externalBin：<resource_dir>/../MacOS/<name>
    // 开发模式（tauri dev）无打包产物 → 回退环境变量 / 系统工具，属预期。
    let candidates: Vec<PathBuf> = [
        Some(resources.join("binaries").join(&exe)),
        Some(resources.join(&exe)),
        Some(resources.join("../MacOS").join(&exe)),
    ]
    .into_iter()
    .flatten()
    .collect();
    for c in &candidates {
        probed.push(c.display().to_string());
        if c.exists() {
            return Ok(c.clone());
        }
    }

    // 2. 环境变量（文件或目录）
    if let Ok(p) = std::env::var(env_var) {
        let p = p.trim();
        if !p.is_empty() {
            let file = PathBuf::from(p);
            if file.is_file() && file.exists() {
                return Ok(file);
            }
            let in_dir = file.join(&exe);
            if in_dir.exists() {
                return Ok(in_dir);
            }
            probed.push(format!("{env_var}={p}（文件或内含 {exe} 的目录）"));
        }
    }

    // 3. 常见安装位置
    for dir in common_search_dirs() {
        let p = dir.join(&exe);
        probed.push(p.display().to_string());
        if p.exists() {
            return Ok(p);
        }
    }

    // 4. PATH
    if let Some(p) = search_path(name) {
        return Ok(p);
    }

    // 5. shell 兜底
    if let Some(p) = search_shell(name) {
        if p.exists() {
            return Ok(p);
        }
    }

    Err(format!(
        "未找到 {name}（已探测：binaries/、{env_var}、常见安装目录、PATH、command -v {}）。\
         请把审计过的构建物放入应用 binaries/（发布随包分发），或安装 Android SDK Platform-Tools 后设置 {env_var}",
        name
    ))
}

/// 兼容旧调用：仅资源目录 + 环境变量（现由 resolve_tool 统一覆盖，保留别名）。
pub fn resolve_tool_bundled(
    resources: &Path,
    name: &str,
    env_var: &str,
) -> Result<PathBuf, String> {
    resolve_tool(resources, name, env_var)
}

/// ADB 序列号脱敏：保留末 4 位。
pub fn mask_serial(serial: &str) -> String {
    let n = serial.len();
    if n <= 4 {
        return "****".to_string();
    }
    format!("{}****{}", &serial[..serial.len().min(2)], &serial[n - 4..])
}

/// `adb devices` 中一行设备记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbDevice {
    pub serial_masked: String,
    pub status: String, // "device" | "unauthorized" | "offline" | ...
}

/// 解析 `adb devices` 输出：原始 (序列号, 状态) 列表（内部使用，序列号不脱敏）。
pub fn parse_adb_devices_raw(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .skip(1)
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with("List of devices") {
                return None;
            }
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            let status = parts.next().unwrap_or("unknown").to_string();
            Some((serial, status))
        })
        .collect()
}

/// 解析 `adb devices` 输出（显示用途，序列号脱敏）。
pub fn parse_adb_devices(output: &str) -> Vec<AdbDevice> {
    parse_adb_devices_raw(output)
        .into_iter()
        .map(|(serial, status)| AdbDevice {
            serial_masked: mask_serial(&serial),
            status,
        })
        .collect()
}

/// 执行 `adb devices`（仅列表，不含任何设备内容）。
pub fn list_devices(adb: &Path) -> Result<Vec<AdbDevice>, AdapterError> {
    let out = Command::new(adb)
        .arg("devices")
        .output()
        .map_err(|e| AdapterError::Failed(format!("执行 adb devices 失败: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(parse_adb_devices(&text))
}

/// 返回第一台已授权设备的完整序列号（仅内部使用，不写入日志）。
pub fn first_authorized_serial(adb: &Path) -> Result<Option<String>, AdapterError> {
    let out = Command::new(adb)
        .arg("devices")
        .output()
        .map_err(|e| AdapterError::Failed(format!("执行 adb devices 失败: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(parse_adb_devices_raw(&text)
        .into_iter()
        .find(|(_, status)| status == "device")
        .map(|(serial, _)| serial))
}

/// 按前端使用的脱敏 session id 解析真实 serial（仅 Rust 内部使用）。
///
/// UI 不需要接触原始 serial；当末四位脱敏标识唯一时，仍可在多设备场景把
/// scrcpy 实例绑定到用户实际点击的那台设备。若发生尾部碰撞，明确报错而不
/// 静默把画面串到另一台设备。
pub fn serial_for_session_id(adb: &Path, session_id: &str) -> Result<Option<String>, AdapterError> {
    let out = Command::new(adb)
        .arg("devices")
        .output()
        .map_err(|e| AdapterError::Failed(format!("执行 adb devices 失败: {e}")))?;
    let requested = session_id
        .rsplit_once(':')
        .map(|(_, id)| id)
        .unwrap_or(session_id);
    let matches: Vec<String> = parse_adb_devices_raw(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .filter(|(_, status)| status == "device")
        .filter(|(serial, _)| {
            serial == requested || super::usb_devices::mask_id(serial) == requested
        })
        .map(|(serial, _)| serial)
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        _ => Err(AdapterError::Failed(
            "多个 Android 设备的脱敏标识相同，无法安全选择目标设备".into(),
        )),
    }
}

/// 解析 `adb shell wm size` 输出 → (宽, 高)。
pub fn parse_wm_size(output: &str) -> Option<(u32, u32)> {
    let body = output.lines().find(|l| l.contains("size"))?;
    let digits: Vec<u32> = body
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|s| s.parse::<u32>().ok())
        .collect();
    if digits.len() >= 2 && digits[0] >= 320 && digits[1] >= 320 {
        Some((digits[0], digits[1]))
    } else {
        None
    }
}

/// 查询设备屏幕尺寸（结构化命令，不读设备内容）。
pub fn query_wm_size(adb: &Path, serial: Option<&str>) -> Option<(u32, u32)> {
    let mut cmd = Command::new(adb);
    if let Some(s) = serial {
        cmd.arg("-s").arg(s);
    }
    cmd.arg("shell").arg("wm").arg("size");
    let out = cmd.output().ok()?;
    parse_wm_size(&String::from_utf8_lossy(&out.stdout))
}

// ---------------------------------------------------------------------------
// 输入命令（白名单参数构造）
// ---------------------------------------------------------------------------

/// 构造 `adb [-s serial] shell input <args>` 的参数列表（不经过 shell，无注入面）。
pub fn adb_input_args(serial: Option<&str>, input_args: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if let Some(s) = serial {
        args.push("-s".into());
        args.push(s.into());
    }
    args.push("shell".into());
    args.push("input".into());
    args.extend(input_args.iter().map(|a| a.to_string()));
    args
}

/// `input text` 转义：空格 → %s（ADB input text 约定），% → %s 占位保护。
/// 仅适用于 ASCII 子集；非 ASCII 由上层编码检查拦截。
pub fn escape_adb_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        match c {
            ' ' => out.push_str("%s"),
            '%' => out.push_str("%s"),
            other => out.push(other),
        }
    }
    out
}

/// DOM KeyboardEvent.code → Android KEYCODE（subset）。
pub fn android_keycode(code: &str) -> Option<&'static str> {
    if let Some(rest) = code.strip_prefix("Key") {
        if rest.len() == 1 {
            let c = rest.chars().next()?.to_ascii_uppercase();
            return Some(match c {
                'A' => "KEYCODE_A",
                'B' => "KEYCODE_B",
                'C' => "KEYCODE_C",
                'D' => "KEYCODE_D",
                'E' => "KEYCODE_E",
                'F' => "KEYCODE_F",
                'G' => "KEYCODE_G",
                'H' => "KEYCODE_H",
                'I' => "KEYCODE_I",
                'J' => "KEYCODE_J",
                'K' => "KEYCODE_K",
                'L' => "KEYCODE_L",
                'M' => "KEYCODE_M",
                'N' => "KEYCODE_N",
                'O' => "KEYCODE_O",
                'P' => "KEYCODE_P",
                'Q' => "KEYCODE_Q",
                'R' => "KEYCODE_R",
                'S' => "KEYCODE_S",
                'T' => "KEYCODE_T",
                'U' => "KEYCODE_U",
                'V' => "KEYCODE_V",
                'W' => "KEYCODE_W",
                'X' => "KEYCODE_X",
                'Y' => "KEYCODE_Y",
                'Z' => "KEYCODE_Z",
                _ => return None,
            });
        }
        return None;
    }
    if let Some(rest) = code.strip_prefix("Digit") {
        if rest.len() == 1 {
            return match rest {
                "0" => Some("KEYCODE_0"),
                "1" => Some("KEYCODE_1"),
                "2" => Some("KEYCODE_2"),
                "3" => Some("KEYCODE_3"),
                "4" => Some("KEYCODE_4"),
                "5" => Some("KEYCODE_5"),
                "6" => Some("KEYCODE_6"),
                "7" => Some("KEYCODE_7"),
                "8" => Some("KEYCODE_8"),
                "9" => Some("KEYCODE_9"),
                _ => None,
            };
        }
        return None;
    }
    let simple = match code {
        "Enter" => "KEYCODE_ENTER",
        "Backspace" => "KEYCODE_DEL",
        "Tab" => "KEYCODE_TAB",
        "Space" => "KEYCODE_SPACE",
        "Escape" => "KEYCODE_ESCAPE",
        "ArrowUp" => "KEYCODE_DPAD_UP",
        "ArrowDown" => "KEYCODE_DPAD_DOWN",
        "ArrowLeft" => "KEYCODE_DPAD_LEFT",
        "ArrowRight" => "KEYCODE_DPAD_RIGHT",
        "Home" => "KEYCODE_HOME",
        "End" => "KEYCODE_MOVE_END",
        "PageUp" => "KEYCODE_PAGE_UP",
        "PageDown" => "KEYCODE_PAGE_DOWN",
        _ => return None,
    };
    Some(simple)
}

/// DOM KeyboardEvent.code → Android numeric keycode（scrcpy control 协议使用数值）。
pub fn android_keycode_value(code: &str) -> Option<u32> {
    if let Some(rest) = code.strip_prefix("Key") {
        let c = rest.chars().next()?;
        if rest.len() == 1 && c.is_ascii_alphabetic() {
            return Some(29 + (c.to_ascii_uppercase() as u32 - 'A' as u32));
        }
        return None;
    }
    if let Some(rest) = code.strip_prefix("Digit") {
        if rest.len() == 1 {
            let digit = rest.chars().next()?.to_digit(10)?;
            return Some(if digit == 0 { 7 } else { 7 + digit });
        }
        return None;
    }
    Some(match code {
        "Enter" => 66,
        "Backspace" => 67,
        "Tab" => 61,
        "Space" => 62,
        "Escape" => 111,
        "ArrowUp" => 19,
        "ArrowDown" => 20,
        "ArrowLeft" => 21,
        "ArrowRight" => 22,
        "Home" => 3,
        "End" => 123,
        "PageUp" => 92,
        "PageDown" => 93,
        _ => return None,
    })
}

/// Android KeyEvent metastate 位（scrcpy control 协议沿用 Android 常量）。
pub fn android_metastate(ctrl: bool, shift: bool, alt: bool, meta: bool) -> u32 {
    u32::from(shift) | (u32::from(alt) << 1) | (u32::from(ctrl) << 12) | (u32::from(meta) << 16)
}

/// 兼容性的 Android 输入控制器（只用于慢速/诊断调用；连续输入由 scrcpy control
/// socket 承担）。
pub struct AdbInputController {
    adb: PathBuf,
    serial: Option<String>,
}

impl AdbInputController {
    pub fn new(adb: PathBuf, serial: Option<String>) -> Self {
        Self { adb, serial }
    }

    fn input(&self, args: &[&str]) -> Result<(), AdapterError> {
        let full = adb_input_args(self.serial.as_deref(), args);
        let out = Command::new(&self.adb)
            .args(&full)
            .output()
            .map_err(|e| AdapterError::Failed(format!("执行 adb input 失败: {e}")))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(AdapterError::Failed(format!(
                "adb input 返回 {}：{}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    /// 点击 / 按下-抬起触摸（x,y 为设备绝对坐标）。
    pub fn tap(&self, x: u32, y: u32) -> Result<(), AdapterError> {
        self.input(&["tap", &x.to_string(), &y.to_string()])
    }

    /// 持续按住按下（配合 move/up 完成拖拽）。
    pub fn touch_down(&self, x: u32, y: u32) -> Result<(), AdapterError> {
        self.input(&["motionevent", "DOWN", &x.to_string(), &y.to_string()])
    }

    pub fn touch_move(&self, x: u32, y: u32) -> Result<(), AdapterError> {
        self.input(&["motionevent", "MOVE", &x.to_string(), &y.to_string()])
    }

    pub fn touch_up(&self, x: u32, y: u32) -> Result<(), AdapterError> {
        self.input(&["motionevent", "UP", &x.to_string(), &y.to_string()])
    }

    pub fn key_event(&self, keycode: &str) -> Result<(), AdapterError> {
        self.input(&["keyevent", keycode])
    }

    /// 粘贴 ASCII 文本（非 ASCII 由上层编码检查拦截）。
    pub fn insert_text(&self, text: &str) -> Result<(), AdapterError> {
        let escaped = escape_adb_text(text);
        self.input(&["text", &escaped])
    }

    /// 检查设备是否已授权可用。
    pub fn check_ready(&self) -> Result<(), AdapterError> {
        let devices = list_devices(&self.adb)?;
        let mine = devices.iter().any(|d| d.status == "device");
        if mine {
            Ok(())
        } else {
            Err(AdapterError::Failed(
                "未找到已授权的 Android 设备；请开启 USB 调试并完成授权".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_adb_devices_output() {
        let out = "List of devices attached\nABC123XYZ\tdevice\nDEF456\tunauthorized\n\nGHI789\toffline\n";
        let devices = parse_adb_devices(out);
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].status, "device");
        assert!(devices[0].serial_masked.contains("****"));
        assert!(devices[0].serial_masked.ends_with("XYZ"));
        assert_eq!(devices[1].status, "unauthorized");
    }

    #[test]
    fn mask_serial_keeps_tail() {
        assert_eq!(mask_serial("0123456789ABCDEF"), "01****CDEF");
        assert_eq!(mask_serial("abc"), "****");
    }

    #[test]
    fn parses_wm_size() {
        assert_eq!(
            parse_wm_size("Physical size: 1080x2340"),
            Some((1080, 2340))
        );
        assert_eq!(parse_wm_size("Override size: 800x600"), Some((800, 600)));
        assert_eq!(parse_wm_size("unknown output"), None);
    }

    #[test]
    fn builds_whitelisted_input_args() {
        let args = adb_input_args(Some("SER"), &["tap", "100", "200"]);
        assert_eq!(
            args,
            vec!["-s", "SER", "shell", "input", "tap", "100", "200"]
        );
        let args = adb_input_args(None, &["keyevent", "KEYCODE_ENTER"]);
        assert_eq!(args, vec!["shell", "input", "keyevent", "KEYCODE_ENTER"]);
    }

    #[test]
    fn escapes_adb_text() {
        assert_eq!(escape_adb_text("hello world"), "hello%sworld");
        assert_eq!(escape_adb_text("a b"), "a%sb");
        assert_eq!(escape_adb_text("100%"), "100%s");
    }

    #[test]
    fn android_keycodes() {
        assert_eq!(android_keycode("KeyA"), Some("KEYCODE_A"));
        assert_eq!(android_keycode("Digit7"), Some("KEYCODE_7"));
        assert_eq!(android_keycode("Enter"), Some("KEYCODE_ENTER"));
        assert_eq!(android_keycode("Backspace"), Some("KEYCODE_DEL"));
        assert_eq!(android_keycode("ArrowUp"), Some("KEYCODE_DPAD_UP"));
        assert_eq!(android_keycode("F3"), None);
    }

    #[test]
    fn scrcpy_keycode_values_and_metastate() {
        assert_eq!(android_keycode_value("KeyA"), Some(29));
        assert_eq!(android_keycode_value("Digit7"), Some(14));
        assert_eq!(android_keycode_value("Enter"), Some(66));
        assert_eq!(android_keycode_value("F3"), None);
        assert_eq!(android_metastate(true, true, false, false), 0x1001);
    }
}

#[cfg(test)]
mod locator_tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn write_fake_exe(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, "#!/bin/sh\n").unwrap();
        #[cfg(not(target_os = "windows"))]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    #[test]
    fn bundled_binary_takes_precedence_over_env() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("pb-loc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp.join("binaries")).unwrap();
        let bundled = write_fake_exe(&tmp.join("binaries"), "adb");
        let other = write_fake_exe(&tmp, "adb-other");
        let _ = &other;
        // 即使 env 指向别处，应用内置 binaries/ 必须优先（打包优先原则）。
        std::env::set_var("PHONEBRIDGE_ADB_PATH", tmp.join("elsewhere"));
        let got = resolve_tool(&tmp, "adb", "PHONEBRIDGE_ADB_PATH").unwrap();
        assert_eq!(got, bundled);
        std::env::remove_var("PHONEBRIDGE_ADB_PATH");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn env_var_directory_is_resolved_with_exe() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("pb-loc-env-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let exe_path = write_fake_exe(&tmp, "adb");
        std::env::set_var("PHONEBRIDGE_ADB_PATH", &tmp);
        let got = resolve_tool(
            Path::new("/nonexistent-resources"),
            "adb",
            "PHONEBRIDGE_ADB_PATH",
        )
        .unwrap();
        assert_eq!(got, exe_path);
        std::env::remove_var("PHONEBRIDGE_ADB_PATH");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_tool_reports_probed_sources() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("pb-loc-miss-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        std::env::remove_var("PHONEBRIDGE_ADB_PATH");
        let err = resolve_tool(&tmp, "adb-quite-unique-xyz", "PHONEBRIDGE_ADB_PATH").unwrap_err();
        assert!(err.contains("未找到 adb-quite-unique-xyz"), "err: {err}");
        assert!(err.contains("已探测"), "err: {err}");
        let _ = fs::remove_dir_all(&tmp);
    }
}
