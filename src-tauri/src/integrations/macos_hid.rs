//! macOS CoreBluetooth HOGP 输入适配器。
//!
//! macOS 的 `CBPeripheralManager` 没有 Rust 官方绑定；`macos_hid.m` 提供一个
//! 很小的 C ABI 桥接层，Rust 仍然拥有 `IHidController`、状态和生命周期，
//! Objective-C 侧只负责 CoreBluetooth 的 GATT 数据库与通知发送。
//!
//! GATT 布局参考 Bluetooth HID over GATT Profile 以及开源的
//! `darwin-bt-remote` LowEnergy 实现，但没有把其代码或资源引入本项目。

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::{Arc, Mutex};

use super::hid_adapter::{authorization_name, HidStatus, HidStatusCallback, IHidController};
use crate::integrations::hid_report::{KeyboardReport, MouseReport};
use crate::integrations::AdapterError;
use crate::session::state::ControlState;

type RawStatusCallback = unsafe extern "C" fn(
    context: *mut c_void,
    powered_on: i32,
    advertising: i32,
    subscribed: i32,
    connected: i32,
    input_report_mask: i32,
    error_code: i32,
    manager_state: i32,
    authorization: i32,
    native_error_code: i32,
);

extern "C" {
    fn phonebridge_hid_get_host_name(buffer: *mut c_char, length: usize);
    fn phonebridge_hid_create(
        callback: Option<RawStatusCallback>,
        context: *mut c_void,
        local_name: *const c_char,
    ) -> *mut c_void;
    fn phonebridge_hid_start(handle: *mut c_void) -> i32;
    fn phonebridge_hid_stop(handle: *mut c_void);
    fn phonebridge_hid_destroy(handle: *mut c_void);
    fn phonebridge_hid_send_keyboard(handle: *mut c_void, bytes: *const u8, length: usize) -> i32;
    fn phonebridge_hid_send_mouse(handle: *mut c_void, bytes: *const u8, length: usize) -> i32;
}

struct MacOsHidShared {
    status: Mutex<HidStatus>,
    callback: Mutex<Option<HidStatusCallback>>,
}

impl MacOsHidShared {
    fn new(pairing_name: Option<String>, advertised_name: String) -> Self {
        Self {
            status: Mutex::new(HidStatus {
                control_transport: "ble".into(),
                control: ControlState::Disabled,
                pairing_name,
                advertised_name: Some(advertised_name),
                device_name: Some(format!("{} BLE HID", crate::APP_NAME)),
                // iOS only exposes a Bluetooth mouse pointer after the user
                // enables AssistiveTouch; the app cannot toggle that system
                // setting through a public API.
                assistive_touch_hint: true,
                ..HidStatus::default()
            }),
            callback: Mutex::new(None),
        }
    }
}

/// CoreBluetooth HOGP 控制器。CoreBluetooth 对象本身在 Objective-C 主队列中
/// 访问，因此这个 Rust 句柄可以由 Tauri 命令线程安全地调用。
pub struct MacOsHidAdapter {
    handle: *mut c_void,
    callback_context: *const MacOsHidShared,
    shared: Arc<MacOsHidShared>,
}

// Objective-C 对象内部统一在主队列访问；Rust 侧只通过 C ABI 访问句柄。
unsafe impl Send for MacOsHidAdapter {}
unsafe impl Sync for MacOsHidAdapter {}

impl MacOsHidAdapter {
    pub fn new() -> Self {
        let shared = Arc::new(MacOsHidShared::new(
            host_name(),
            compact_advertised_name(crate::APP_NAME),
        ));
        // C ABI 对象在销毁前持有这一个 Arc strong reference；回调每次临时增加
        // 引用，避免异步 CoreBluetooth 回调读取已经释放的 Rust 状态。
        let callback_context = Arc::into_raw(shared.clone());
        let local_name = CString::new(crate::APP_NAME).expect("APP_NAME 不含 NUL");
        let handle = unsafe {
            phonebridge_hid_create(
                Some(status_callback),
                callback_context as *mut c_void,
                local_name.as_ptr(),
            )
        };
        Self {
            handle,
            callback_context,
            shared,
        }
    }

    fn set_local_status(&self, control: ControlState, last_error: Option<String>) {
        let (snapshot, callback) = {
            let mut status = self.shared.status.lock().unwrap();
            status.control = control;
            status.last_error = last_error;
            (status.clone(), self.shared.callback.lock().unwrap().clone())
        };
        if let Some(callback) = callback {
            callback(snapshot);
        }
    }

    fn send_result(&self, result: i32, operation: &str) -> Result<(), AdapterError> {
        if result == 0 {
            Ok(())
        } else {
            let pairing_name = self
                .shared
                .status
                .lock()
                .unwrap()
                .pairing_name
                .clone()
                .unwrap_or_else(|| crate::APP_NAME.to_string());
            let detail = if result == -2 {
                format!("iOS 尚未订阅{operation}对应的 HID 输入特征，当前连接可能仍在完成 HID 握手")
            } else {
                format!("iOS BLE HID 尚未订阅，无法发送{operation}")
            };
            Err(AdapterError::ControlInactive(format!(
                "{detail}；请在 iPhone 设置→蓝牙中重新连接「{pairing_name}」",
            )))
        }
    }
}

/// Apple 的前台 BLE 广播在保留完整 128-bit HID 服务 UUID 后只剩 8 个
/// UTF-8 字节给本地名称。按字符边界截断，避免 CoreBluetooth 把 HID UUID
/// 放入 overflow 广播区；iOS 系统设置不会主动扫描该区域。
fn compact_advertised_name(name: &str) -> String {
    const MAX_UTF8_BYTES: usize = 8;
    let mut end = 0;
    for character in name.chars() {
        let next = end + character.len_utf8();
        if next > MAX_UTF8_BYTES {
            break;
        }
        end = next;
    }
    if end == 0 {
        "KTP".into()
    } else {
        name[..end].to_owned()
    }
}

fn host_name() -> Option<String> {
    let mut buffer = [0 as c_char; 512];
    unsafe {
        phonebridge_hid_get_host_name(buffer.as_mut_ptr(), buffer.len());
        CStr::from_ptr(buffer.as_ptr())
            .to_str()
            .ok()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
    }
}

fn manager_state_name(code: i32) -> &'static str {
    match code {
        0 => "Unknown",
        1 => "Resetting",
        2 => "Unsupported",
        3 => "Unauthorized",
        4 => "PoweredOff",
        5 => "PoweredOn",
        _ => "未知",
    }
}

/// 将 CoreBluetooth 的状态和 ABI 错误码转换成不会误导用户的诊断信息。
/// `CBPeripheralManager.state` 与授权状态分开传递，避免把“蓝牙权限已允许”
/// 错误地等同于“外设角色已经可用”。
fn native_status_error(
    error_code: i32,
    manager_state: i32,
    authorization: i32,
    native_error_code: i32,
) -> Option<String> {
    let auth = authorization_name(authorization);
    let state = manager_state_name(manager_state);
    let state_error = match manager_state {
        0 => Some(format!(
            "macOS CoreBluetooth 状态仍为 Unknown，授权状态为“{auth}”，正在等待蓝牙控制器回调"
        )),
        1 => Some("macOS 蓝牙控制器正在重置（Resetting），正在等待恢复".into()),
        2 => Some("此 Mac 不支持 BLE 外设角色（CoreBluetooth Unsupported）".into()),
        3 => Some(format!(
            "macOS 未授权“快投屏”使用 BLE 外设角色（授权状态：{auth}）"
        )),
        4 => Some("macOS 蓝牙当前已关闭（CoreBluetooth PoweredOff）".into()),
        5 => None,
        _ => Some(format!(
            "macOS CoreBluetooth 返回未知状态 {manager_state}（授权状态：{auth}）"
        )),
    };

    if let Some(message) = state_error {
        return Some(message);
    }
    if error_code == 0 {
        return None;
    }
    let operation = match error_code {
        -20 => "发布 HOGP GATT 服务失败，请查看 CoreBluetooth 服务错误",
        -21 => "启动 BLE 广播失败，请查看 CoreBluetooth 广播错误",
        -30 => return Some(format!(
            "macOS 蓝牙权限尚未确认（授权状态：{auth}）：CoreBluetooth 已回调广播成功，但射频不会真正发包，iPhone 无法发现「{}」；请在 系统设置→隐私与安全性→蓝牙 允许本应用，然后完全退出并重启快投屏再启用控制",
            crate::APP_NAME
        )),
        _ => "macOS CoreBluetooth HOGP 操作失败",
    };
    if native_error_code != 0 {
        Some(format!(
            "{operation}（错误码 {error_code}，CoreBluetooth 错误码 {native_error_code}，状态 {state}）"
        ))
    } else {
        Some(format!("{operation}（错误码 {error_code}，状态 {state}）"))
    }
}

impl IHidController for MacOsHidAdapter {
    fn start_advertising(&mut self) -> Result<(), AdapterError> {
        if self.handle.is_null() {
            return Err(AdapterError::UnsupportedPlatform(
                "macOS CoreBluetooth HOGP 初始化失败".into(),
            ));
        }
        let result = unsafe { phonebridge_hid_start(self.handle) };
        if result != 0 {
            let detail = self
                .status()
                .last_error
                .unwrap_or_else(|| "macOS CoreBluetooth HOGP 启动失败".into());
            return Err(AdapterError::Failed(detail));
        }
        // start() 可能已经收到 Unknown/Resetting 的异步状态回调；保留该
        // 诊断，不能再用本地“Broadcasting”状态把它清掉。
        let pending_error = self.status().last_error;
        self.set_local_status(ControlState::Broadcasting, pending_error);
        Ok(())
    }

    fn stop_advertising(&mut self) -> Result<(), AdapterError> {
        if !self.handle.is_null() {
            unsafe { phonebridge_hid_stop(self.handle) };
        }
        self.set_local_status(ControlState::Disabled, None);
        Ok(())
    }

    fn release_all(&mut self) {
        let _ = self.send_keyboard(&KeyboardReport::default());
        let _ = self.send_mouse(&MouseReport::default());
    }

    fn send_keyboard(&mut self, report: &KeyboardReport) -> Result<(), AdapterError> {
        if self.handle.is_null() {
            return Err(AdapterError::ControlInactive(
                "macOS BLE HID 控制器不存在".into(),
            ));
        }
        let result = unsafe {
            phonebridge_hid_send_keyboard(
                self.handle,
                report as *const KeyboardReport as *const u8,
                std::mem::size_of::<KeyboardReport>(),
            )
        };
        self.send_result(result, "键盘报告")
    }

    fn send_mouse(&mut self, report: &MouseReport) -> Result<(), AdapterError> {
        if self.handle.is_null() {
            return Err(AdapterError::ControlInactive(
                "macOS BLE HID 控制器不存在".into(),
            ));
        }
        let result = unsafe {
            phonebridge_hid_send_mouse(
                self.handle,
                report as *const MouseReport as *const u8,
                std::mem::size_of::<MouseReport>(),
            )
        };
        self.send_result(result, "鼠标报告")
    }

    fn status(&self) -> HidStatus {
        self.shared.status.lock().unwrap().clone()
    }

    fn set_control_state(&mut self, control: ControlState) {
        self.shared.status.lock().unwrap().control = control;
    }

    fn set_device_info(
        &mut self,
        paired: bool,
        connected: bool,
        subscribed: bool,
        name: Option<String>,
    ) {
        let mut status = self.shared.status.lock().unwrap();
        status.paired = paired;
        status.connected = connected;
        status.subscribed = subscribed;
        if name.is_some() {
            status.device_name = name;
        }
    }

    fn set_status_callback(&mut self, callback: Option<HidStatusCallback>) {
        *self.shared.callback.lock().unwrap() = callback;
    }
}

impl Drop for MacOsHidAdapter {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                phonebridge_hid_stop(self.handle);
                phonebridge_hid_destroy(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
        // 与 create 时的 Arc::into_raw 配对；shared 字段本身仍由 Rust 正常释放。
        unsafe { drop(Arc::from_raw(self.callback_context)) };
    }
}

unsafe extern "C" fn status_callback(
    context: *mut c_void,
    powered_on: i32,
    advertising: i32,
    subscribed: i32,
    connected: i32,
    input_report_mask: i32,
    error_code: i32,
    manager_state: i32,
    authorization: i32,
    native_error_code: i32,
) {
    if context.is_null() {
        return;
    }
    let pointer = context as *const MacOsHidShared;
    // 回调执行期间保留一份 strong reference；destroy 只会释放 C ABI 持有的那一份。
    Arc::increment_strong_count(pointer);
    let shared = Arc::from_raw(pointer);
    let (snapshot, callback) = {
        let mut status = shared.status.lock().unwrap();
        status.powered_on = powered_on != 0;
        status.advertising = advertising != 0;
        status.connected = connected != 0;
        status.subscribed = subscribed != 0;
        status.input_report_mask = input_report_mask.clamp(0, u8::MAX as i32) as u8;
        status.paired = status.connected;
        status.authorization = Some(authorization);
        status.control = if status.connected {
            ControlState::Connected
        } else if advertising != 0 {
            ControlState::Broadcasting
        } else {
            ControlState::Disabled
        };
        status.last_error =
            native_status_error(error_code, manager_state, authorization, native_error_code);
        (status.clone(), shared.callback.lock().unwrap().clone())
    };
    if let Some(callback) = callback {
        callback(snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::{compact_advertised_name, native_status_error};

    #[test]
    fn compacts_chinese_name_without_splitting_utf8() {
        assert_eq!(compact_advertised_name("快投屏"), "快投");
    }

    #[test]
    fn preserves_short_ascii_and_utf8_names_and_falls_back_for_empty_name() {
        assert_eq!(compact_advertised_name("KTP"), "KTP");
        assert_eq!(compact_advertised_name("😀😀"), "😀😀");
        assert_eq!(compact_advertised_name(""), "KTP");
    }

    #[test]
    fn reports_authorization_separately_from_manager_state() {
        let message = native_status_error(0, 0, 3, 0).expect("Unknown state should be diagnosed");
        assert!(message.contains("Unknown"));
        assert!(message.contains("始终允许"));
    }

    #[test]
    fn reports_service_failure_after_power_on() {
        let message =
            native_status_error(-20, 5, 3, 14).expect("service failure should be diagnosed");
        assert!(message.contains("发布 HOGP GATT 服务失败"));
        assert!(message.contains("CoreBluetooth 错误码 14"));
        assert!(message.contains("PoweredOn"));
    }

    #[test]
    fn reports_pending_permission_even_when_advertising_reported_success() {
        // macOS 上授权「未确定」时 CoreBluetooth 仍会回调广播成功（PoweredOn，
        // error_code=0 → 但桥接层专门用 -30 上报假成功）。
        let message =
            native_status_error(-30, 5, 0, 0).expect("permission pending must be diagnosed");
        assert!(message.contains("权限尚未确认"));
        assert!(message.contains("未确定"));
        assert!(message.contains("射频不会真正发包"));
    }
}
