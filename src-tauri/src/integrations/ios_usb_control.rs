//! iOS USB/WDA precision-control adapter.
//!
//! The normal iOS path remains AirPlay + BLE HID.  This module is an optional
//! precision path: a signed WebDriverAgent (WDA) is reached through a loopback
//! tunnel or the in-process registration owned by the bundled `go-ios` runner,
//! and receives absolute W3C input actions.  The WDA installer never creates or
//! re-signs the IPA; Apple provisioning/trust remains an explicit device check.
//!
//! Only the Rust standard library and serde_json are used here.  In particular,
//! there is no macOS-only framework or compile-time dependency in this module.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use super::process::hidden_command;
use super::AdapterError;

const DEFAULT_WDA_PORT: u16 = 8100;
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const TUNNEL_WAIT_TIMEOUT: Duration = Duration::from_secs(8);
const WDA_TEXT_BATCH_CHARS: usize = 128;
const WDA_TEXT_FREQUENCY: u32 = 600;
const POINTER_SOURCE_ID: &str = "phonebridge-touch";
const KEY_SOURCE_ID: &str = "phonebridge-keyboard";
const WHEEL_SOURCE_ID: &str = "phonebridge-wheel";

static WDA_ENDPOINTS: OnceLock<Mutex<HashMap<String, u16>>> = OnceLock::new();

fn wda_endpoints() -> &'static Mutex<HashMap<String, u16>> {
    WDA_ENDPOINTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a host port owned by the cross-platform WDA runner.  The mapping
/// stays inside the Rust process; the raw UDID is never returned to the UI.
pub(crate) fn register_wda_endpoint(raw_udid: &str, host_port: u16) {
    wda_endpoints()
        .lock()
        .unwrap()
        .insert(raw_udid.to_string(), host_port);
}

pub(crate) fn unregister_wda_endpoint(raw_udid: &str) {
    wda_endpoints().lock().unwrap().remove(raw_udid);
}

fn registered_wda_endpoint(raw_udid: &str) -> Option<LoopbackEndpoint> {
    wda_endpoints()
        .lock()
        .unwrap()
        .get(raw_udid)
        .copied()
        .map(|port| LoopbackEndpoint {
            host: "127.0.0.1".into(),
            port,
        })
}

/// A logical screen size used by the absolute-coordinate conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenSize {
    pub width: u32,
    pub height: u32,
}

/// Host-side readiness snapshot for the optional USB/WDA input path.  A WDA
/// session is device-specific, so without an active session this reports
/// whether the tunnel tool is available and whether an explicitly configured
/// loopback WDA endpoint answers `/status`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosControlCapability {
    pub usb_wda_enabled: bool,
    pub wda_reachable: bool,
    pub iproxy_present: bool,
    pub wda_artifact_present: bool,
    pub direct_install_ready: bool,
    pub detail: String,
}

/// Map a point from the decoded mirror's coordinate space into WDA's logical
/// viewport.  The end points map exactly (`0 -> 0`, `width-1 -> target-1`),
/// which avoids the one-pixel drift caused by repeatedly rounding a scale.
pub fn map_absolute_point(
    source: ScreenSize,
    target: ScreenSize,
    x: u32,
    y: u32,
) -> Option<(u32, u32)> {
    if source.width == 0 || source.height == 0 || target.width == 0 || target.height == 0 {
        return None;
    }

    let source_x = x.min(source.width.saturating_sub(1));
    let source_y = y.min(source.height.saturating_sub(1));
    let target_x = if source.width == 1 || target.width == 1 {
        0
    } else {
        ((source_x as f64 * (target.width - 1) as f64) / (source.width - 1) as f64).round() as u32
    };
    let target_y = if source.height == 1 || target.height == 1 {
        0
    } else {
        ((source_y as f64 * (target.height - 1) as f64) / (source.height - 1) as f64).round() as u32
    };
    Some((target_x, target_y))
}

#[derive(Debug, Clone)]
struct LoopbackEndpoint {
    host: String,
    port: u16,
}

impl LoopbackEndpoint {
    fn address(&self) -> String {
        if self.host == "::1" {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Parse only local, clear-text WDA URLs.  WDA is intentionally reachable
/// through loopback only; accepting an arbitrary URL would turn input commands
/// into a network request primitive.
fn parse_loopback_url(raw: &str) -> Result<LoopbackEndpoint, AdapterError> {
    let rest = raw
        .strip_prefix("http://")
        .ok_or_else(|| AdapterError::IosUsbUnavailable("WDA 地址必须是 http:// 回环地址".into()))?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || authority.contains('?') {
        return Err(AdapterError::IosUsbUnavailable(
            "WDA 地址格式无效，只允许 127.0.0.1/localhost/[::1]".into(),
        ));
    }

    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let end = bracketed
            .find(']')
            .ok_or_else(|| AdapterError::IosUsbUnavailable("WDA IPv6 回环地址格式无效".into()))?;
        let host = &bracketed[..end];
        let port = bracketed[end + 1..]
            .strip_prefix(':')
            .unwrap_or("8100")
            .parse::<u16>()
            .map_err(|_| AdapterError::IosUsbUnavailable("WDA 端口无效".into()))?;
        (host.to_string(), port)
    } else {
        let (host, port) = authority.rsplit_once(':').unwrap_or((authority, "8100"));
        let port = port
            .parse::<u16>()
            .map_err(|_| AdapterError::IosUsbUnavailable("WDA 端口无效".into()))?;
        (host.to_string(), port)
    };

    if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1") {
        return Err(AdapterError::IosUsbUnavailable(
            "WDA 只允许通过本机回环地址访问".into(),
        ));
    }
    if port == 0 {
        return Err(AdapterError::IosUsbUnavailable("WDA 端口不能为 0".into()));
    }
    Ok(LoopbackEndpoint { host, port })
}

fn open_loopback(endpoint: &LoopbackEndpoint) -> Result<TcpStream, AdapterError> {
    let mut addresses = endpoint
        .address()
        .to_socket_addrs()
        .map_err(|e| AdapterError::IosUsbUnavailable(format!("连接 WDA 回环端口失败：{e}")))?;
    let address = addresses
        .next()
        .ok_or_else(|| AdapterError::IosUsbUnavailable("WDA 回环地址没有可连接的端口".into()))?;
    let stream = TcpStream::connect_timeout(&address, HTTP_TIMEOUT)
        .map_err(|e| AdapterError::IosUsbUnavailable(format!("连接 WDA 回环端口失败：{e}")))?;
    stream
        .set_read_timeout(Some(HTTP_TIMEOUT))
        .map_err(|e| AdapterError::IosUsbUnavailable(format!("设置 WDA 读取超时失败：{e}")))?;
    stream
        .set_write_timeout(Some(HTTP_TIMEOUT))
        .map_err(|e| AdapterError::IosUsbUnavailable(format!("设置 WDA 写入超时失败：{e}")))?;
    Ok(stream)
}

/// Minimal HTTP/1.1 client for WDA's loopback JSON API.
///
/// Keeping one connection open is important for pointer move latency.  The
/// client accepts Content-Length (WDA's normal response), chunked responses,
/// and close-delimited responses for compatibility with older WDA builds.
struct WdaClient {
    endpoint: LoopbackEndpoint,
    stream: TcpStream,
    read_buffer: Vec<u8>,
    session_id: String,
}

fn stop_tunnel(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Owns an iproxy child while WDA setup is in progress. `Child` does not kill
/// the process when it is dropped, so every failed setup path must explicitly
/// terminate the tunnel instead of leaving a listener behind.
struct TunnelGuard(Option<Child>);

impl Drop for TunnelGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            stop_tunnel(child);
        }
    }
}

impl WdaClient {
    fn open(endpoint: LoopbackEndpoint) -> Result<Self, AdapterError> {
        Ok(Self {
            stream: open_loopback(&endpoint)?,
            endpoint,
            read_buffer: Vec::new(),
            session_id: String::new(),
        })
    }

    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, AdapterError> {
        let payload = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|e| AdapterError::IosUsbUnavailable(format!("编码 WDA 请求失败：{e}")))?;
        let payload_len = payload.as_ref().map(|bytes| bytes.len()).unwrap_or(0);
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\nAccept: application/json\r\n",
            self.endpoint.address()
        );
        if payload.is_some() {
            request.push_str("Content-Type: application/json; charset=utf-8\r\n");
        }
        request.push_str(&format!("Content-Length: {payload_len}\r\n\r\n"));
        self.stream
            .write_all(request.as_bytes())
            .and_then(|_| {
                if let Some(payload) = payload.as_ref() {
                    self.stream.write_all(payload)?;
                }
                self.stream.flush()
            })
            .map_err(|e| AdapterError::IosUsbUnavailable(format!("发送 WDA 请求失败：{e}")))?;

        let (status, bytes) = self.read_response()?;
        if !(200..300).contains(&status) {
            let detail = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| {
                    v.get("value")
                        .and_then(|value| value.get("message"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Err(AdapterError::IosUsbUnavailable(format!(
                "WDA 请求失败：{method} {path}（{detail}）"
            )));
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes)
            .map_err(|e| AdapterError::IosUsbUnavailable(format!("解析 WDA 响应失败：{e}")))
    }

    fn read_response(&mut self) -> Result<(u16, Vec<u8>), AdapterError> {
        let header_end = loop {
            if let Some(index) = find_bytes(&self.read_buffer, b"\r\n\r\n") {
                break index;
            }
            self.read_more()?;
        };
        let header_bytes = self.read_buffer[..header_end].to_vec();
        let header_text = String::from_utf8_lossy(&header_bytes);
        let mut lines = header_text.split("\r\n");
        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or_else(|| AdapterError::IosUsbUnavailable("WDA 返回了无效 HTTP 状态".into()))?;
        let mut content_length = None;
        let mut chunked = false;
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse::<usize>().ok();
                } else if name.eq_ignore_ascii_case("transfer-encoding")
                    && value.to_ascii_lowercase().contains("chunked")
                {
                    chunked = true;
                }
            }
        }
        let body_start = header_end + 4;
        let body = if chunked {
            self.read_chunked_body(body_start)?
        } else if let Some(length) = content_length {
            self.read_fixed_body(body_start, length)?
        } else {
            // WDA normally sends Content-Length.  A close-delimited response
            // cannot be reused, so consume it and reconnect before the next
            // command rather than returning a partial JSON body.
            self.read_to_close(body_start)?
        };
        Ok((status, body))
    }

    fn read_fixed_body(
        &mut self,
        body_start: usize,
        length: usize,
    ) -> Result<Vec<u8>, AdapterError> {
        while self.read_buffer.len() < body_start + length {
            self.read_more()?;
        }
        let body = self.read_buffer[body_start..body_start + length].to_vec();
        self.read_buffer.drain(..body_start + length);
        Ok(body)
    }

    fn read_chunked_body(&mut self, mut cursor: usize) -> Result<Vec<u8>, AdapterError> {
        let mut body = Vec::new();
        loop {
            let line_end = loop {
                if let Some(index) = find_bytes_from(&self.read_buffer, b"\r\n", cursor) {
                    break index;
                }
                self.read_more()?;
            };
            let size_text = String::from_utf8_lossy(&self.read_buffer[cursor..line_end]);
            let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("0").trim(), 16)
                .map_err(|_| AdapterError::IosUsbUnavailable("WDA 分块响应长度无效".into()))?;
            cursor = line_end + 2;
            if size == 0 {
                while self.read_buffer.len() < cursor + 2 {
                    self.read_more()?;
                }
                self.read_buffer.drain(..cursor + 2);
                return Ok(body);
            }
            while self.read_buffer.len() < cursor + size + 2 {
                self.read_more()?;
            }
            body.extend_from_slice(&self.read_buffer[cursor..cursor + size]);
            cursor += size + 2;
        }
    }

    fn read_to_close(&mut self, body_start: usize) -> Result<Vec<u8>, AdapterError> {
        let mut bytes = self.read_buffer[body_start..].to_vec();
        self.read_buffer.clear();
        self.stream
            .read_to_end(&mut bytes)
            .map_err(|e| AdapterError::IosUsbUnavailable(format!("读取 WDA 响应失败：{e}")))?;
        // The next request will fail on this stream.  Reopen lazily; session
        // creation and actions are still safe because WDA keeps the session.
        self.stream = open_loopback(&self.endpoint)?;
        Ok(bytes)
    }

    fn read_more(&mut self) -> Result<(), AdapterError> {
        let mut chunk = [0u8; 4096];
        let count = self
            .stream
            .read(&mut chunk)
            .map_err(|e| AdapterError::IosUsbUnavailable(format!("读取 WDA 响应失败：{e}")))?;
        if count == 0 {
            return Err(AdapterError::IosUsbUnavailable(
                "WDA 关闭了回环连接，镜像控制已停止".into(),
            ));
        }
        self.read_buffer.extend_from_slice(&chunk[..count]);
        Ok(())
    }

    fn status(&mut self) -> Result<(), AdapterError> {
        let response = self.request("GET", "/status", None)?;
        if response.get("value").is_some() || response.get("status").is_some() {
            Ok(())
        } else {
            Err(AdapterError::IosUsbUnavailable(
                "WDA 返回缺少 status/value 字段".into(),
            ))
        }
    }

    fn create_session(&mut self, udid: Option<&str>) -> Result<(), AdapterError> {
        let mut always_match = json!({
            "platformName": "iOS",
            "appium:automationName": "XCUITest",
            "appium:noReset": true,
            "appium:newCommandTimeout": 3600,
        });
        if let Some(udid) = udid {
            always_match["appium:udid"] = Value::String(udid.to_string());
        }
        let body = json!({
            "capabilities": { "alwaysMatch": always_match },
            "desiredCapabilities": { "platformName": "iOS" },
        });
        let response = self.request("POST", "/session", Some(&body))?;
        let value = response.get("value").unwrap_or(&response);
        let session_id = value
            .get("sessionId")
            .and_then(Value::as_str)
            .or_else(|| response.get("sessionId").and_then(Value::as_str))
            .filter(|id| !id.is_empty())
            .ok_or_else(|| AdapterError::IosUsbUnavailable("WDA 未返回 sessionId".into()))?;
        if !session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(AdapterError::IosUsbUnavailable(
                "WDA sessionId 含有不安全字符".into(),
            ));
        }
        self.session_id = session_id.to_string();
        Ok(())
    }

    fn session_path(&self, suffix: &str) -> Result<String, AdapterError> {
        if self.session_id.is_empty() {
            return Err(AdapterError::IosUsbUnavailable("WDA 会话尚未建立".into()));
        }
        Ok(format!("/session/{}{suffix}", self.session_id))
    }

    fn window_size(&mut self) -> Result<ScreenSize, AdapterError> {
        let path = self.session_path("/window/size")?;
        let response = self.request("GET", &path, None)?;
        let value = response.get("value").unwrap_or(&response);
        let width = value
            .get("width")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| AdapterError::IosUsbUnavailable("WDA 未返回屏幕宽度".into()))?;
        let height = value
            .get("height")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| AdapterError::IosUsbUnavailable("WDA 未返回屏幕高度".into()))?;
        Ok(ScreenSize { width, height })
    }

    fn actions(&mut self, actions: Value) -> Result<(), AdapterError> {
        let path = self.session_path("/actions")?;
        let body = json!({ "actions": actions });
        self.request("POST", &path, Some(&body)).map(|_| ())
    }

    fn release_actions(&mut self) -> Result<(), AdapterError> {
        let path = self.session_path("/actions")?;
        self.request("DELETE", &path, None).map(|_| ())
    }

    /// WDA's dedicated keyboard route is important for paste.  Sending one
    /// W3C key action per Unicode scalar looks tempting, but WDA treats that
    /// route as physical key events and iOS may drop non-ASCII characters.
    /// `/wda/keys` delegates to XCTest's text-input path and accepts a UTF-8
    /// string, including mixed Chinese/Latin text, when an editable target is
    /// focused on the phone.
    fn keys(&mut self, text: &str) -> Result<(), AdapterError> {
        let body = wda_keys_payload(text);
        let path = self.session_path("/wda/keys")?;
        self.request("POST", &path, Some(&body)).map(|_| ())
    }

    fn delete_session(&mut self) -> Result<(), AdapterError> {
        if self.session_id.is_empty() {
            return Ok(());
        }
        let path = self.session_path("")?;
        let result = self.request("DELETE", &path, None).map(|_| ());
        self.session_id.clear();
        result
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    find_bytes_from(haystack, needle, 0)
}

fn find_bytes_from(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    if needle.is_empty() || start > haystack.len() || needle.len() > haystack.len() {
        return None;
    }
    haystack[start..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}

fn executable_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn resolve_runtime_tool(resources: &Path, name: &str, env_name: &str) -> Option<PathBuf> {
    let executable = executable_name(name);
    let mut candidates = vec![
        resources.join("binaries").join(&executable),
        resources.join("binaries/ios-usb").join(&executable),
        resources.join(&executable),
    ];
    if let Ok(configured) = std::env::var(env_name) {
        let configured = PathBuf::from(configured);
        candidates.push(if configured.is_dir() {
            configured.join(&executable)
        } else {
            configured
        });
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(&executable)));
    }
    candidates.into_iter().find(|path| path.is_file())
}

fn wda_keys_payload(text: &str) -> Value {
    json!({
        // WDA's /wda/keys handler joins the value array before calling
        // FBTypeText.  Keep the whole chunk as one item so Unicode grapheme
        // sequences and mixed scripts are not split into separate requests.
        "value": [text],
        // The WDA default is intentionally conservative (60 chars/minute),
        // which makes a normal clipboard paste appear hung.  The bounded
        // request size above still gives WDA a chance to process the field.
        "frequency": WDA_TEXT_FREQUENCY,
    })
}

pub(crate) fn resolve_iproxy(resources: &Path) -> Option<PathBuf> {
    let executable = executable_name("iproxy");
    let mut candidates = vec![
        resources.join("binaries").join(&executable),
        resources.join("binaries").join("ios-usb").join(&executable),
        resources.join(&executable),
        resources.join("../MacOS").join(&executable),
    ];
    if let Ok(configured) = std::env::var("PHONEBRIDGE_IPROXY_PATH") {
        let configured = PathBuf::from(configured);
        candidates.push(if configured.is_dir() {
            configured.join(&executable)
        } else {
            configured
        });
    }
    #[cfg(target_os = "macos")]
    {
        candidates.extend([
            PathBuf::from("/opt/homebrew/bin").join(&executable),
            PathBuf::from("/usr/local/bin").join(&executable),
        ]);
    }
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

/// Inspect the optional precision path without creating a WDA session or
/// touching a physical device. This is safe to call from a diagnostics panel.
pub fn inspect_capability(resources: &Path) -> IosControlCapability {
    let iproxy_present = resolve_iproxy(resources).is_some();
    let mut wda_artifact_candidates = vec![
        resources.join("binaries/ios-usb/WebDriverAgentRunner.ipa"),
        resources.join("binaries/ios-wda/WebDriverAgentRunner.ipa"),
    ];
    if let Ok(path) = std::env::var("PHONEBRIDGE_WDA_IPA_PATH") {
        wda_artifact_candidates.push(PathBuf::from(path));
    }
    let wda_artifact_present = wda_artifact_candidates.iter().any(|path| path.is_file());
    let direct_install_ready = wda_artifact_present
        && resolve_runtime_tool(
            resources,
            "ideviceinstaller",
            "PHONEBRIDGE_IDEVICEINSTALLER_PATH",
        )
        .is_some()
        && resolve_runtime_tool(resources, "ios", "PHONEBRIDGE_GO_IOS_PATH").is_some();
    let configured_url = std::env::var("PHONEBRIDGE_IOS_WDA_URL").ok();
    let (wda_reachable, detail) = match configured_url {
        Some(url) => match parse_loopback_url(&url) {
            Ok(endpoint) => match WdaClient::open(endpoint) {
                Ok(mut client) => match client.status() {
                    Ok(()) => (true, "已连接到配置的本机 WDA 回环端点".into()),
                    Err(error) => (false, format!("配置的 WDA 回环端点不可用：{error}")),
                },
                Err(error) => (false, format!("无法连接配置的 WDA 回环端点：{error}")),
            },
            Err(error) => (false, error.to_string()),
        },
        None if direct_install_ready => (
            false,
            "已找到签名 WDA IPA、安装器和 go-ios；点击“一键准备 WDA”即可由应用安装并启动".into(),
        ),
        None if wda_artifact_present => (
            false,
            "已找到签名 WDA IPA，但缺少安装器或 go-ios；请检查发布包的 ios-usb 资源".into(),
        ),
        None if iproxy_present => (
            false,
            "已找到 iproxy；选择具体 iPhone 后将通过 USB 隧道检测 WDA".into(),
        ),
        None => (
            false,
            "未找到 iproxy；请将审计后的跨平台工具放入应用 binaries/ios-usb/，或设置 PHONEBRIDGE_IPROXY_PATH".into(),
        ),
    };
    IosControlCapability {
        usb_wda_enabled: iproxy_present || wda_reachable || direct_install_ready,
        wda_reachable,
        iproxy_present,
        wda_artifact_present,
        direct_install_ready,
        detail,
    }
}

fn valid_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn raw_udid_for_session(resources: &Path, session_id: &str) -> Result<String, AdapterError> {
    let token = session_id.strip_prefix("iphone:").unwrap_or(session_id);
    if let Ok(configured) = std::env::var("PHONEBRIDGE_IOS_UDID") {
        if valid_device_id(&configured) {
            return Ok(configured);
        }
        return Err(AdapterError::IosUsbUnavailable(
            "PHONEBRIDGE_IOS_UDID 格式无效".into(),
        ));
    }
    if valid_device_id(token) {
        return Ok(token.to_string());
    }
    super::usb_devices::resolve_ios_raw_id(resources, token)
}

pub(crate) fn allocate_local_port() -> Result<u16, AdapterError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| AdapterError::IosUsbUnavailable(format!("分配 iOS WDA 本地端口失败：{e}")))?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|e| AdapterError::IosUsbUnavailable(format!("读取 iOS WDA 本地端口失败：{e}")))
}

/// Probe an already-running WDA without creating a WebDriver session.  The
/// setup wizard uses this to distinguish “WDA is installed and serving” from
/// “the phone is merely visible over USB”.  The temporary tunnel is always
/// killed on return.
pub(crate) fn probe_wda(resources: &Path, raw_udid: &str) -> Result<(), AdapterError> {
    let configured_url = std::env::var("PHONEBRIDGE_IOS_WDA_URL").ok();
    let (endpoint, tunnel) = if let Some(url) = configured_url {
        (parse_loopback_url(&url)?, None)
    } else if let Some(endpoint) = registered_wda_endpoint(raw_udid) {
        (endpoint, None)
    } else {
        if !valid_device_id(raw_udid) {
            return Err(AdapterError::IosUsbUnavailable(
                "iPhone UDID 格式无效，无法建立 WDA 隧道".into(),
            ));
        }
        let iproxy = resolve_iproxy(resources).ok_or_else(|| {
            AdapterError::IosUsbUnavailable("未找到 iproxy，无法检测 iPhone 上的 WDA".into())
        })?;
        let local_port = allocate_local_port()?;
        let forward_spec = iproxy_forward_spec(local_port, DEFAULT_WDA_PORT);
        let child = hidden_command(iproxy)
            .args(["-u", raw_udid, &forward_spec])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| AdapterError::IosUsbUnavailable(format!("启动 iproxy 失败：{e}")))?;
        (
            LoopbackEndpoint {
                host: "127.0.0.1".into(),
                port: local_port,
            },
            Some(child),
        )
    };
    let mut tunnel = TunnelGuard(tunnel);
    let deadline = Instant::now() + TUNNEL_WAIT_TIMEOUT;
    loop {
        if let Some(child) = tunnel.0.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                return Err(AdapterError::IosUsbUnavailable(
                    "iproxy 已退出，iPhone 上的 WDA 没有响应".into(),
                ));
            }
        }
        match WdaClient::open(endpoint.clone()) {
            Ok(mut client) => match client.status() {
                Ok(()) => return Ok(()),
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                }
                Err(error) => return Err(error),
            },
            Err(error) if Instant::now() < deadline => {
                let _ = error;
            }
            Err(error) => return Err(error),
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

/// Probe a host port explicitly while a cross-platform WDA runner owns the
/// forwarding process.
pub(crate) fn probe_wda_local(host_port: u16) -> Result<(), AdapterError> {
    let endpoint = LoopbackEndpoint {
        host: "127.0.0.1".into(),
        port: host_port,
    };
    let deadline = Instant::now() + TUNNEL_WAIT_TIMEOUT;
    loop {
        match WdaClient::open(endpoint.clone()) {
            Ok(mut client) => match client.status() {
                Ok(()) => return Ok(()),
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                }
                Err(error) => return Err(error),
            },
            Err(error) if Instant::now() < deadline => {
                let _ = error;
            }
            Err(error) => return Err(error),
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

fn iproxy_forward_spec(local_port: u16, device_port: u16) -> String {
    format!("{local_port}:{device_port}")
}

/// iOS absolute-coordinate control over an already-running WDA.
pub struct IosUsbControl {
    client: WdaClient,
    tunnel: Option<Child>,
    target_size: ScreenSize,
    pointer_active: bool,
    owns_session: bool,
}

impl IosUsbControl {
    /// Connect to `PHONEBRIDGE_IOS_WDA_URL` when configured, otherwise create a
    /// loopback `iproxy` tunnel to the physical iPhone's WDA port 8100.
    pub fn connect(
        resources: &Path,
        session_id: &str,
        source_size: Option<ScreenSize>,
    ) -> Result<Self, AdapterError> {
        let configured_url = std::env::var("PHONEBRIDGE_IOS_WDA_URL").ok();
        let (endpoint, tunnel, udid) = if let Some(url) = configured_url {
            (parse_loopback_url(&url)?, None, None)
        } else {
            let udid = raw_udid_for_session(resources, session_id)?;
            if let Some(endpoint) = registered_wda_endpoint(&udid) {
                (endpoint, None, Some(udid))
            } else {
                let iproxy = resolve_iproxy(resources).ok_or_else(|| {
                    AdapterError::IosUsbUnavailable(
                        "未找到 iproxy；请把经过审计的跨平台 iproxy 放入应用 binaries/，或设置 PHONEBRIDGE_IPROXY_PATH".into(),
                    )
                })?;
                let local_port = allocate_local_port()?;
                let forward_spec = iproxy_forward_spec(local_port, DEFAULT_WDA_PORT);
                let child = hidden_command(iproxy)
                    .args(["-u", &udid, &forward_spec])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| {
                        AdapterError::IosUsbUnavailable(format!("启动 iproxy 失败：{e}"))
                    })?;
                (
                    LoopbackEndpoint {
                        host: "127.0.0.1".into(),
                        port: local_port,
                    },
                    Some(child),
                    Some(udid),
                )
            }
        };
        let mut tunnel = TunnelGuard(tunnel);

        let deadline = Instant::now() + TUNNEL_WAIT_TIMEOUT;
        let mut client = loop {
            if let Some(child) = tunnel.0.as_mut() {
                if let Ok(Some(_)) = child.try_wait() {
                    return Err(AdapterError::IosUsbUnavailable(
                        "iproxy 已退出，未能连接到 iPhone 上的 WDA".into(),
                    ));
                }
            }
            match WdaClient::open(endpoint.clone()) {
                Ok(mut candidate) => match candidate.status() {
                    Ok(()) => break candidate,
                    Err(_) if Instant::now() < deadline => {}
                    Err(error) => return Err(error),
                },
                Err(error) if Instant::now() < deadline => {
                    let _ = error;
                }
                Err(error) => return Err(error),
            }
            std::thread::sleep(Duration::from_millis(120));
        };

        // An optional pre-created session is useful for WDA variants that do
        // not accept the Appium capability envelope.  It is still restricted
        // to the user-supplied loopback endpoint.
        let owns_session = if let Ok(existing) = std::env::var("PHONEBRIDGE_IOS_WDA_SESSION") {
            if !existing.is_empty()
                && existing
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            {
                client.session_id = existing;
                false
            } else {
                return Err(AdapterError::IosUsbUnavailable(
                    "PHONEBRIDGE_IOS_WDA_SESSION 格式无效".into(),
                ));
            }
        } else {
            client.create_session(udid.as_deref())?;
            true
        };

        let target_size = match client.window_size() {
            Ok(size) => size,
            Err(error) => match source_size {
                Some(size) => size,
                None => {
                    if owns_session {
                        let _ = client.delete_session();
                    }
                    return Err(error);
                }
            },
        };
        Ok(Self {
            client,
            tunnel: tunnel.0.take(),
            target_size,
            pointer_active: false,
            owns_session,
        })
    }

    pub fn target_size(&self) -> ScreenSize {
        self.target_size
    }

    pub fn pointer_move(&mut self, source: ScreenSize, x: u32, y: u32) -> Result<(), AdapterError> {
        let (x, y) = map_absolute_point(source, self.target_size, x, y)
            .ok_or_else(|| AdapterError::IosUsbUnavailable("iOS WDA 坐标尺寸无效".into()))?;
        if !self.pointer_active {
            return Ok(());
        }
        self.client.actions(json!([{
            "type": "pointer",
            "id": POINTER_SOURCE_ID,
            "parameters": { "pointerType": "touch" },
            "actions": [{
                "type": "pointerMove",
                "duration": 0,
                "origin": "viewport",
                "x": x,
                "y": y
            }]
        }]))
    }

    pub fn pointer_button(
        &mut self,
        source: ScreenSize,
        x: u32,
        y: u32,
        pressed: bool,
    ) -> Result<(), AdapterError> {
        let (x, y) = map_absolute_point(source, self.target_size, x, y)
            .ok_or_else(|| AdapterError::IosUsbUnavailable("iOS WDA 坐标尺寸无效".into()))?;
        let button_action = if pressed { "pointerDown" } else { "pointerUp" };
        self.client.actions(json!([{
            "type": "pointer",
            "id": POINTER_SOURCE_ID,
            "parameters": { "pointerType": "touch" },
            "actions": [
                {
                    "type": "pointerMove",
                    "duration": 0,
                    "origin": "viewport",
                    "x": x,
                    "y": y
                },
                { "type": button_action, "button": 0 }
            ]
        }]))?;
        self.pointer_active = pressed;
        Ok(())
    }

    pub fn wheel(&mut self, delta_y: i32) -> Result<(), AdapterError> {
        self.client.actions(json!([{
            "type": "wheel",
            "id": WHEEL_SOURCE_ID,
            "actions": [{
                "type": "scroll",
                "x": self.target_size.width / 2,
                "y": self.target_size.height / 2,
                "deltaX": 0,
                "deltaY": delta_y,
                "duration": 0,
                "origin": "viewport"
            }]
        }]))
    }

    pub fn key_stroke(
        &mut self,
        code: &str,
        key: &str,
        ctrl: bool,
        shift: bool,
        alt: bool,
        meta: bool,
    ) -> Result<(), AdapterError> {
        let mut actions = Vec::new();
        let modifiers = [
            (ctrl, "\u{E009}"),
            (shift, "\u{E008}"),
            (alt, "\u{E00A}"),
            (meta, "\u{E03D}"),
        ];
        for (enabled, value) in modifiers {
            if enabled {
                actions.push(json!({ "type": "keyDown", "value": value }));
            }
        }
        let value = webdriver_key_value(code, key)
            .ok_or_else(|| AdapterError::Failed(format!("未映射的 iOS WDA 按键：{code}")))?;
        actions.push(json!({ "type": "keyDown", "value": value }));
        actions.push(json!({ "type": "keyUp", "value": value }));
        for (enabled, value) in modifiers.into_iter().rev() {
            if enabled {
                actions.push(json!({ "type": "keyUp", "value": value }));
            }
        }
        self.client.actions(json!([{
            "type": "key",
            "id": KEY_SOURCE_ID,
            "actions": actions
        }]))
    }

    /// Type text through WDA's UTF-8 `/wda/keys` route. Text is intentionally
    /// sent in bounded batches so a large clipboard value cannot create an
    /// unbounded single HTTP request. Unlike BLE HID, this path can carry
    /// mixed Chinese/Latin text (and other Unicode accepted by XCTest).
    pub fn text(&mut self, text: &str) -> Result<(), AdapterError> {
        for chunk in text
            .chars()
            .collect::<Vec<_>>()
            .chunks(WDA_TEXT_BATCH_CHARS)
        {
            let chunk: String = chunk.iter().collect();
            self.client.keys(&chunk)?;
        }
        Ok(())
    }

    pub fn release_all(&mut self) {
        if self.pointer_active {
            let _ = self.client.actions(json!([{
                "type": "pointer",
                "id": POINTER_SOURCE_ID,
                "parameters": { "pointerType": "touch" },
                "actions": [{ "type": "pointerUp", "button": 0 }]
            }]));
            self.pointer_active = false;
        }
        let _ = self.client.release_actions();
    }
}

impl Drop for IosUsbControl {
    fn drop(&mut self) {
        self.release_all();
        if self.owns_session {
            let _ = self.client.delete_session();
        }
        if let Some(mut tunnel) = self.tunnel.take() {
            stop_tunnel(&mut tunnel);
        }
    }
}

fn webdriver_key_value(code: &str, key: &str) -> Option<String> {
    let special = match code {
        "Backspace" => "\u{E003}",
        "Tab" => "\u{E004}",
        "Enter" | "NumpadEnter" => "\u{E007}",
        "ShiftLeft" | "ShiftRight" => "\u{E008}",
        "ControlLeft" | "ControlRight" => "\u{E009}",
        "AltLeft" | "AltRight" => "\u{E00A}",
        "Escape" => "\u{E00C}",
        "PageUp" => "\u{E00E}",
        "PageDown" => "\u{E00F}",
        "End" => "\u{E010}",
        "Home" => "\u{E011}",
        "ArrowLeft" => "\u{E012}",
        "ArrowUp" => "\u{E013}",
        "ArrowRight" => "\u{E014}",
        "ArrowDown" => "\u{E015}",
        "Insert" => "\u{E016}",
        "Delete" => "\u{E017}",
        "MetaLeft" | "MetaRight" => "\u{E03D}",
        "F1" => "\u{E031}",
        "F2" => "\u{E032}",
        "F3" => "\u{E033}",
        "F4" => "\u{E034}",
        "F5" => "\u{E035}",
        "F6" => "\u{E036}",
        "F7" => "\u{E037}",
        "F8" => "\u{E038}",
        "F9" => "\u{E039}",
        "F10" => "\u{E03A}",
        "F11" => "\u{E03B}",
        "F12" => "\u{E03C}",
        _ => {
            return if key.chars().count() == 1 {
                Some(key.to_string())
            } else {
                None
            }
        }
    };
    Some(special.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        iproxy_forward_spec, map_absolute_point, parse_loopback_url, wda_keys_payload,
        webdriver_key_value, ScreenSize, WDA_TEXT_FREQUENCY,
    };

    #[test]
    fn absolute_mapping_preserves_endpoints() {
        let source = ScreenSize {
            width: 480,
            height: 1040,
        };
        let target = ScreenSize {
            width: 393,
            height: 852,
        };
        assert_eq!(map_absolute_point(source, target, 0, 0), Some((0, 0)));
        assert_eq!(
            map_absolute_point(source, target, 479, 1039),
            Some((392, 851))
        );
    }

    #[test]
    fn absolute_mapping_clamps_out_of_range_points() {
        let size = ScreenSize {
            width: 100,
            height: 200,
        };
        assert_eq!(map_absolute_point(size, size, 1000, 1000), Some((99, 199)));
        assert_eq!(
            map_absolute_point(
                size,
                ScreenSize {
                    width: 0,
                    height: 1
                },
                0,
                0
            ),
            None
        );
    }

    #[test]
    fn only_loopback_wda_urls_are_accepted() {
        assert!(parse_loopback_url("http://127.0.0.1:8100").is_ok());
        assert!(parse_loopback_url("http://localhost:8100/").is_ok());
        assert!(parse_loopback_url("http://[::1]:8100").is_ok());
        assert!(parse_loopback_url("https://127.0.0.1:8100").is_err());
        assert!(parse_loopback_url("http://192.168.1.8:8100").is_err());
    }

    #[test]
    fn webdriver_special_and_printable_keys_are_mapped() {
        assert_eq!(
            webdriver_key_value("Enter", "Enter"),
            Some("\u{E007}".into())
        );
        assert_eq!(
            webdriver_key_value("ArrowLeft", "ArrowLeft"),
            Some("\u{E012}".into())
        );
        assert_eq!(webdriver_key_value("KeyA", "a"), Some("a".into()));
        assert_eq!(webdriver_key_value("Unknown", "中文"), None);
    }

    #[test]
    fn iproxy_uses_current_single_forward_spec() {
        assert_eq!(iproxy_forward_spec(49152, 8100), "49152:8100");
    }

    #[test]
    fn wda_keys_payload_preserves_mixed_unicode_text() {
        let payload = wda_keys_payload("Hello，世界 123🙂");
        assert_eq!(payload["value"][0], "Hello，世界 123🙂");
        assert_eq!(payload["frequency"], WDA_TEXT_FREQUENCY);
    }
}
