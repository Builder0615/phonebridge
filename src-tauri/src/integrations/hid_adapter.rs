//! HidAdapter：BLE HID 控制器抽象（Spec §5.3）。
//!
//! 前端只与 `IHidController` 交互；HOGP 实现细节被隔离在平台实现之后。
//! - Windows：优先 WinRT `GattServiceProvider`（M0/MVP 需真实设备验证），
//!   本版本实现 Peripheral 能力探测；广播流程已骨架化。
//! - 其它平台（开发环境）：返回 UnsupportedPlatform，保证可理解的错误状态。

use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::session::state::ControlState;

use super::hid_report::{KeyboardReport, MouseReport};
use super::AdapterError;

/// macOS CoreBluetooth 已订阅的输入报告位：用于诊断实际可用的 HID
/// 特征，而不是把任意 GATT 订阅误报成键鼠输入已就绪。
pub const INPUT_REPORT_MOUSE: u8 = 1 << 0;
pub const INPUT_REPORT_KEYBOARD: u8 = 1 << 1;
pub const INPUT_REPORT_BOOT_MOUSE: u8 = 1 << 2;
pub const INPUT_REPORT_BOOT_KEYBOARD: u8 = 1 << 3;

/// HID 控制状态（对应前端 HidStatus）。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HidStatus {
    /// `ble` / `usb_wda` / `scrcpy`; used by the canvas to select the
    /// corresponding coordinate and input semantics.
    pub control_transport: String,
    pub control: ControlState,
    /// 宿主蓝牙控制器（CBPeripheralManager/WinRT）是否已上电。
    pub powered_on: bool,
    /// HOGP GATT 服务已发布并且宿主已经收到广播成功回调。
    /// 仅 `powered_on` 为 true 不能代表 iPhone 已经能发现本设备。
    pub advertising: bool,
    pub paired: bool,
    pub connected: bool,
    pub subscribed: bool,
    /// 已订阅的输入报告位图：bit0=Report Mouse，bit1=Report Keyboard，
    /// bit2=Boot Mouse，bit3=Boot Keyboard。仅 macOS CoreBluetooth 提供；
    /// 其它平台为 0。用于区分“完成 BLE 连接”与“真正订阅键鼠输入”。
    pub input_report_mask: u8,
    /// macOS 在 iPhone 系统蓝牙列表中实际展示的本机/GAP 名称；它可能不
    /// 是应用的产品名（例如显示为“Mac mini”）。Windows 端为空时回退到
    /// 应用名。
    pub pairing_name: Option<String>,
    /// BLE 广播包中实际使用的短名称；用于诊断 UTF-8 名称长度和发现问题。
    pub advertised_name: Option<String>,
    pub device_name: Option<String>,
    pub connection_interval_ms: Option<u32>,
    pub assistive_touch_hint: bool,
    /// macOS CoreBluetooth 蓝牙授权码（0 未确定 / 1 受限制 / 2 已拒绝 /
    /// 3 始终允许）；其它平台为 None。用于识别「CoreBluetooth 回调了广播
    /// 成功但授权未确认，射频上实际没有发包」的假成功场景。
    pub authorization: Option<i32>,
    pub last_error: Option<String>,
}

/// CoreBluetooth 授权码 → 可读名称（仅在 macOS 上有意义；其它平台为 None）。
pub fn authorization_name(code: i32) -> &'static str {
    match code {
        0 => "未确定",
        1 => "受限制",
        2 => "已拒绝",
        3 => "始终允许",
        _ => "未知",
    }
}

/// BLE HOGP 链路状态回调。输入适配器在 GATT 中心订阅/取消订阅时通知会话层，
/// 这样“开始广播”不会被误认为“iPhone 已经能接收输入”。
pub type HidStatusCallback = Arc<dyn Fn(HidStatus) + Send + Sync>;

/// BLE HID 控制器接口（纯 Rust 结构，便于单元测试与 mock）。
pub trait IHidController: Send + Sync {
    /// 仅在用户明确点击「启用控制」后调用；停止后不可被新设备发现。
    fn start_advertising(&mut self) -> Result<(), AdapterError>;
    fn stop_advertising(&mut self) -> Result<(), AdapterError>;
    /// 异常/断开/退出时释放全部按键与鼠标按钮。
    fn release_all(&mut self);
    fn send_keyboard(&mut self, report: &KeyboardReport) -> Result<(), AdapterError>;
    fn send_mouse(&mut self, report: &MouseReport) -> Result<(), AdapterError>;
    fn status(&self) -> HidStatus;
    /// 更新未订阅/断开等状态（由会话层统一维护控制子状态）。
    fn set_control_state(&mut self, control: ControlState);
    fn set_device_info(
        &mut self,
        paired: bool,
        connected: bool,
        subscribed: bool,
        name: Option<String>,
    );
    /// 设置异步连接状态回调；Stub/尚未实现的平台保持默认空实现。
    fn set_status_callback(&mut self, _callback: Option<HidStatusCallback>) {}
}

/// 生产控制器入口：按平台选择实现。
pub fn create_hid_controller() -> Box<dyn IHidController> {
    #[cfg(target_os = "windows")]
    {
        Box::new(WinRtHidAdapter::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(crate::integrations::macos_hid::MacOsHidAdapter::new())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Box::new(StubHidAdapter::new())
    }
}

/// 平台能力探测：返回 (peripheral 是否支持, 适配器地址脱敏)。
pub fn probe_ble_capabilities() -> (bool, Option<String>, Option<String>) {
    #[cfg(target_os = "windows")]
    {
        let (supported, addr) = winrt_probe_peripheral();
        (supported, addr, None)
    }
    #[cfg(target_os = "macos")]
    {
        (
            true,
            None,
            Some("macOS 使用 CoreBluetooth CBPeripheralManager 提供 HOGP；iPhone 系统蓝牙列表可能显示本机/GAP 名称而不是应用名，首次使用需在对应条目上完成配对，真实设备兼容性仍需验证".into()),
        )
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        (
            false,
            None,
            Some("当前构建平台不是 Windows/macOS；BLE LE Peripheral 能力需在目标宿主上验证".into()),
        )
    }
}

// ---------------------------------------------------------------------------
// 开发平台 Stub
// ---------------------------------------------------------------------------

pub struct StubHidAdapter {
    status: Mutex<HidStatus>,
}

impl StubHidAdapter {
    pub fn new() -> Self {
        Self {
            status: Mutex::new(HidStatus {
                control_transport: "ble".into(),
                control: ControlState::Disabled,
                last_error: None,
                ..HidStatus::default()
            }),
        }
    }
}

impl IHidController for StubHidAdapter {
    fn start_advertising(&mut self) -> Result<(), AdapterError> {
        #[cfg(target_os = "macos")]
        let msg = "macOS 宿主的 HOGP（CoreBluetooth CBPeripheralManager）已骨架化，需真实 iPhone + macOS 13+ 验证"
            .to_string();
        #[cfg(not(target_os = "macos"))]
        let msg =
            "当前开发平台不支持 BLE LE Peripheral；请在 Windows 10 2004+ / macOS 13+ 目标上验证"
                .to_string();

        let mut s = self.status.lock().unwrap();
        s.last_error = Some(msg.clone());
        drop(s);
        Err(AdapterError::PendingRealDeviceValidation(msg))
    }

    fn stop_advertising(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }

    fn release_all(&mut self) {
        let mut s = self.status.lock().unwrap();
        s.connected = false;
        s.subscribed = false;
    }

    fn send_keyboard(&mut self, _report: &KeyboardReport) -> Result<(), AdapterError> {
        Err(AdapterError::ControlInactive(
            "开发构建未激活 BLE 控制".into(),
        ))
    }

    fn send_mouse(&mut self, _report: &MouseReport) -> Result<(), AdapterError> {
        Err(AdapterError::ControlInactive(
            "开发构建未激活 BLE 控制".into(),
        ))
    }

    fn status(&self) -> HidStatus {
        self.status.lock().unwrap().clone()
    }

    fn set_control_state(&mut self, control: ControlState) {
        self.status.lock().unwrap().control = control;
    }

    fn set_device_info(
        &mut self,
        paired: bool,
        connected: bool,
        subscribed: bool,
        name: Option<String>,
    ) {
        let mut s = self.status.lock().unwrap();
        s.paired = paired;
        s.connected = connected;
        s.subscribed = subscribed;
        s.device_name = name;
    }
}

// ---------------------------------------------------------------------------
// Windows WinRT 实现（HOGP/GattServiceProvider 流程骨架，需真实设备验证）
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub struct WinRtHidAdapter {
    status: Mutex<HidStatus>,
}

#[cfg(target_os = "windows")]
impl WinRtHidAdapter {
    pub fn new() -> Self {
        Self {
            status: Mutex::new(HidStatus {
                control_transport: "ble".into(),
                control: ControlState::Disabled,
                ..HidStatus::default()
            }),
        }
    }
}

/// WinRT BluetoothAdapter 能力探测：返回 (peripheral_supported, 地址脱敏)。
#[cfg(target_os = "windows")]
fn winrt_probe_peripheral() -> (bool, Option<String>) {
    use windows::Devices::Bluetooth::BluetoothAdapter;

    let op = match BluetoothAdapter::GetDefaultAsync() {
        Ok(op) => op,
        Err(e) => {
            log::error!("BluetoothAdapter::GetDefaultAsync failed: {e}");
            return (false, None);
        }
    };
    let adapter = match op.get() {
        Ok(a) => a,
        Err(e) => {
            log::error!("GetDefaultAdapter failed: {e}");
            return (false, None);
        }
    };
    let supported = adapter.IsPeripheralRoleSupported().unwrap_or(false);
    let addr_masked = adapter
        .BluetoothAddress()
        .ok()
        .map(|a| format!("{a:012X}"))
        .map(|s| {
            // 脱敏：仅保留前 6 位厂商段
            let mut chars: Vec<char> = s.chars().collect();
            for c in chars.iter_mut().skip(6) {
                *c = '*';
            }
            chars.into_iter().collect()
        });
    (supported, addr_masked)
}

#[cfg(target_os = "windows")]
impl IHidController for WinRtHidAdapter {
    fn start_advertising(&mut self) -> Result<(), AdapterError> {
        let (supported, addr) = winrt_probe_peripheral();
        if !supported {
            return Err(AdapterError::PeripheralUnsupported);
        }
        let mut s = self.status.lock().unwrap();
        s.control = ControlState::Broadcasting;
        s.connected = false;
        s.subscribed = false;
        s.device_name = Some(format!(
            "{} 输入 ({})",
            crate::APP_NAME,
            addr.as_deref().unwrap_or("00:xx:xx")
        ));
        // HOGP/GattServiceProvider 广播（0x1812 HID Service、
        // Report Map、Boot Keyboard/Mouse 特征）为 M0/MVP 设备验证项，
        // 当前版本如实返回未验证状态，避免把未经验证的广播当成功。
        Err(AdapterError::PendingRealDeviceValidation(
            "WinRT GattServiceProvider HOGP 广播已骨架化，需在 Windows + 支持 Peripheral 的适配器 + 真实 iPhone 上完成配对验证".into(),
        ))
    }

    fn stop_advertising(&mut self) -> Result<(), AdapterError> {
        self.release_all();
        self.status.lock().unwrap().control = ControlState::Disabled;
        Ok(())
    }

    fn release_all(&mut self) {
        // 真实实现中：向 iPhone 发送全按键释放报告（空键盘报告 + 空鼠标报告）。
        let _ = self.send_keyboard(&KeyboardReport::default());
        let _ = self.send_mouse(&MouseReport::default());
    }

    fn send_keyboard(&mut self, report: &KeyboardReport) -> Result<(), AdapterError> {
        let s = self.status.lock().unwrap();
        if s.control != ControlState::Connected {
            return Err(AdapterError::ControlInactive("BLE 控制未连接".into()));
        }
        // TODO(M0/MVP): 通过 GattServiceProvider Report 特征写入 keyboard report。
        Err(AdapterError::PendingRealDeviceValidation(
            "键盘报告写入需 GattServiceProvider Report 特征，随 HOGP 广播流程一并验证".into(),
        ))
    }

    fn send_mouse(&mut self, report: &MouseReport) -> Result<(), AdapterError> {
        let s = self.status.lock().unwrap();
        if s.control != ControlState::Connected {
            return Err(AdapterError::ControlInactive("BLE 控制未连接".into()));
        }
        // TODO(M0/MVP): 通过 Report 特征写入 mouse report。
        Err(AdapterError::PendingRealDeviceValidation(
            "鼠标报告写入需 GattServiceProvider Report 特征，随 HOGP 广播流程一并验证".into(),
        ))
    }

    fn status(&self) -> HidStatus {
        self.status.lock().unwrap().clone()
    }

    fn set_control_state(&mut self, control: ControlState) {
        self.status.lock().unwrap().control = control;
    }

    fn set_device_info(
        &mut self,
        paired: bool,
        connected: bool,
        subscribed: bool,
        name: Option<String>,
    ) {
        let mut s = self.status.lock().unwrap();
        s.paired = paired;
        s.connected = connected;
        s.subscribed = subscribed;
        s.device_name = name;
    }
}

/// 单元测试用 FakeHid：记录报告序列，便于验证发送顺序与 KeyDown/KeyUp 配对。
#[cfg(test)]
pub mod test_support {
    use super::*;

    pub struct FakeHid {
        pub keyboard_reports: Vec<KeyboardReport>,
        pub mouse_reports: Vec<MouseReport>,
        pub started: bool,
        pub fail_start: bool,
        status: Mutex<HidStatus>,
    }

    impl FakeHid {
        pub fn new() -> Self {
            Self {
                keyboard_reports: Vec::new(),
                mouse_reports: Vec::new(),
                started: false,
                fail_start: false,
                status: Mutex::new(HidStatus::default()),
            }
        }
    }

    impl IHidController for FakeHid {
        fn start_advertising(&mut self) -> Result<(), AdapterError> {
            if self.fail_start {
                return Err(AdapterError::PeripheralUnsupported);
            }
            self.started = true;
            self.status.lock().unwrap().control = ControlState::Broadcasting;
            Ok(())
        }

        fn stop_advertising(&mut self) -> Result<(), AdapterError> {
            self.started = false;
            self.status.lock().unwrap().control = ControlState::Disabled;
            Ok(())
        }

        fn release_all(&mut self) {
            self.keyboard_reports.push(KeyboardReport::default());
            self.mouse_reports.push(MouseReport::default());
        }

        fn send_keyboard(&mut self, report: &KeyboardReport) -> Result<(), AdapterError> {
            self.keyboard_reports.push(*report);
            Ok(())
        }

        fn send_mouse(&mut self, report: &MouseReport) -> Result<(), AdapterError> {
            self.mouse_reports.push(*report);
            Ok(())
        }

        fn status(&self) -> HidStatus {
            self.status.lock().unwrap().clone()
        }

        fn set_control_state(&mut self, control: ControlState) {
            self.status.lock().unwrap().control = control;
        }

        fn set_device_info(
            &mut self,
            paired: bool,
            connected: bool,
            subscribed: bool,
            name: Option<String>,
        ) {
            let mut s = self.status.lock().unwrap();
            s.paired = paired;
            s.connected = connected;
            s.subscribed = subscribed;
            s.device_name = name;
        }
    }
}
