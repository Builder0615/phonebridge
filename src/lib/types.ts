/**
 * 快投屏前后端契约类型（React ↔ Rust/Tauri 的唯一业务边界）。
 *
 * 本文件与 src-tauri 中的 serde 结构一一对应；字段名固定为 camelCase，
 * Rust 侧使用 #[serde(rename_all = "camelCase")]。修改任何字段时，
 * 必须同时更新 Rust 端定义并运行 cargo test / pnpm test 保持契约一致。
 */

// ---------------------------------------------------------------------------
// 会话状态（对应 Rust session::state）
// ---------------------------------------------------------------------------

export type DeviceKind = "iphone" | "android"

export type SessionStateName =
  | "idle"
  | "checking"
  | "ready"
  | "mirroring"
  | "mirroring_connected"
  | "control_pairing"
  | "control_ready"
  | "reconnecting"
  | "stopping"
  | "failed"

export type ControlState =
  | "disabled"
  | "broadcasting"
  | "waiting_pairing"
  | "connected"
  | "input_paused"

export type MirrorState =
  | "idle"
  | "starting"
  | "connecting"
  | "streaming"
  | "reconnecting"
  | "stopped"
  | "error"

export interface SessionError {
  code: string
  message: string
  recoverable: boolean
}

export interface SessionStateView {
  state: SessionStateName
  /** ISO-8601，状态进入时间 */
  since: string
  detail: string | null
  control: ControlState
  mirror: MirrorState
  lastError: SessionError | null
}

// ---------------------------------------------------------------------------
// 能力报告（check_capabilities）
// ---------------------------------------------------------------------------

export type DependencyStatus = "ok" | "missing" | "unsupported" | "unknown"

export interface DependencyState {
  status: DependencyStatus
  detail: string
}

export interface CapabilityReport {
  platform: string
  osVersion: string
  osBuild: number | null
  appVersion: string
  tauriVersion: string
  network: NetworkCapability
  mdns: DependencyState
  bluetooth: BluetoothCapability
  mirror: MirrorCapability
  android: AndroidCapability
  clipboard: boolean
  windowsTarget: WindowsTargetInfo | null
}

export interface NetworkCapability {
  available: boolean
  interfaceCount: number
  /** 脱敏后的主 IP（IPv4），如 192.168.*.* */
  primaryIpMasked: string | null
  error: string | null
}

export interface BluetoothCapability {
  supported: boolean
  peripheralRole: boolean
  adapterName: string | null
  /** 脱敏后的适配器地址 */
  adapterAddressMasked: string | null
  error: string | null
}

export interface MirrorCapability {
  status: DependencyStatus
  uxplayPresent: boolean
  uxplayVersion: string | null
  expectedPath: string
  detail: string
}

export interface WindowsTargetInfo {
  minBuild: number
  currentBuild: number
  meetsMinimum: boolean
}

export interface AndroidCapability {
  adbAvailable: boolean
  ffmpegAvailable: boolean
  scrcpyServerAvailable: boolean
  /** 已授权设备（脱敏序列号:状态） */
  authorizedDevices: string[]
  detail: string
}

// ---------------------------------------------------------------------------
// 镜像元数据（mirror://metadata、mirror://metrics）
// ---------------------------------------------------------------------------

export interface MirrorMetadata {
  width: number
  height: number
  rotation: 0 | 90 | 180 | 270
  codec: string | null
}

export interface MirrorMetrics {
  fps: number | null
  estimatedLatencyMs: number | null
  frameCount: number
  /** 首帧时间 ms；未产生首帧时为 null */
  firstFrameMs: number | null
}

// ---------------------------------------------------------------------------
// HID 控制（hid://status 与输入命令）
// ---------------------------------------------------------------------------

export interface HidStatus {
  /** iOS: ble / usb_wda；Android: scrcpy。 */
  controlTransport: "ble" | "usb_wda" | "scrcpy" | string
  control: ControlState
  /** 宿主蓝牙控制器是否已上电。 */
  poweredOn: boolean
  /** HOGP GATT 服务已发布且已收到广播成功回调。 */
  advertising: boolean
  paired: boolean
  connected: boolean
  subscribed: boolean
  /** 已订阅的 iOS 键鼠输入报告位图；其它平台为 0。 */
  inputReportMask: number
  /** iOS 系统蓝牙列表实际可能显示的宿主名称（macOS 例如 Mac mini）。 */
  pairingName: string | null
  /** BLE 广播包中实际使用的短名称；用于诊断发现问题。 */
  advertisedName: string | null
  deviceName: string | null
  connectionIntervalMs: number | null
  assistiveTouchHint: boolean
  /** macOS CoreBluetooth 蓝牙授权码：0 未确定 / 1 受限制 / 2 已拒绝 / 3 始终允许；其它平台 null */
  authorization: number | null
  lastError: string | null
}

export interface HidKeyStroke {
  /** DOM KeyboardEvent.code，如 "KeyA"、"Enter" */
  code: string
  /** DOM KeyboardEvent.key，如 "a"、"Enter" */
  key: string
  modifiers: {
    ctrl: boolean
    shift: boolean
    alt: boolean
    meta: boolean
  }
  repeat: boolean
}

export interface PointerMoveEvent {
  /** 相对位移（桌面 CSS 指针单位；iOS 由前端按 HID 上限拆分） */
  dx: number
  dy: number
  /** Android 绝对坐标（设备像素，可选） */
  absX?: number | null
  absY?: number | null
  /** 绝对坐标所属的解码帧尺寸（USB/WDA 模式用于独立于缩放的映射） */
  sourceWidth?: number | null
  sourceHeight?: number | null
}

export interface PointerButtonEvent {
  button: "left" | "middle" | "right"
  pressed: boolean
  /** Android 绝对坐标（设备像素，可选） */
  x?: number | null
  y?: number | null
  /** 绝对坐标所属的解码帧尺寸（USB/WDA 模式用于独立于缩放的映射） */
  sourceWidth?: number | null
  sourceHeight?: number | null
}

export interface WheelPayload {
  deltaY: number
}

// ---------------------------------------------------------------------------
// 粘贴（paste_plain_text）
// ---------------------------------------------------------------------------

export interface PasteResult {
  ok: boolean
  charCount: number
  byteCount: number
  truncated: boolean
  /** 无法通过当前 HID 键盘布局表达的字符位置（UTF-16 索引） */
  unencodablePositions: number[]
  error: PasteError | null
}

export interface PasteError {
  code: string
  message: string
}

// ---------------------------------------------------------------------------
// 诊断（get_diagnostics / export_diagnostics）
// ---------------------------------------------------------------------------

export interface DiagnosticsReport {
  generatedAt: string
  source: "live" | "cached"
  app: {
    version: string
    tauriVersion: string
    rustVersion: string
  }
  os: {
    family: string
    version: string
    build: number | null
    meetsMinimum: boolean
  }
  network: {
    interfaces: NetworkInterfaceInfo[]
    mdns: DependencyState
    firewall: FirewallCheck
  }
  bluetooth: BluetoothDiagnosticInfo
  mirror: MirrorDiagnosticInfo
  security: {
    csp: string
    capabilities: string[]
    clipboardPolicy: string
  }
  recentErrors: LogEntry[]
}

export interface NetworkInterfaceInfo {
  name: string
  ipv4Masked: string | null
  macMasked: string | null
  status: string
}

export interface FirewallCheck {
  checked: boolean
  allowed: boolean | null
  detail: string
}

export interface BluetoothDiagnosticInfo {
  peripheralRole: boolean
  adapterName: string | null
  adapterAddressMasked: string | null
  note: string
}

export interface MirrorDiagnosticInfo {
  uxplay: DependencyState
  gstreamer: DependencyState
  port: number
  lastExitCode: number | null
  android: AndroidDiagnosticInfo
}

export interface AndroidDiagnosticInfo {
  adb: DependencyState
  ffmpeg: DependencyState
  scrcpyServer: DependencyState
  /** 已授权设备（脱敏序列号:状态） */
  authorizedDevices: string[]
  detail: string
}

export interface LogEntry {
  at: string
  level: "info" | "warn" | "error"
  code: string
  message: string
}

export interface ExportResult {
  ok: boolean
  path: string | null
  items: string[]
  error: string | null
}

// ---------------------------------------------------------------------------
// 会话选项（命令参数）
// ---------------------------------------------------------------------------

export interface SessionPreferences {
  autoControl: boolean
  pasteShortcutEnabled: boolean
  scrollStep: number
  maxPasteBytes: number
  /** iOS BLE 相对鼠标的宿主侧速度倍率；Android 不使用。 */
  iosPointerScale: number
  /** 使用 USB + WDA 的 iOS 绝对坐标通道；不可用时不静默回退 BLE。 */
  iosUsbControlEnabled: boolean
}

export interface StartSessionOptions {
  /** "iphone" | "android" | null（保持当前选择） */
  device: DeviceKind | null
  prefs: SessionPreferences
}

export interface PasteOptions {
  prefs: Pick<SessionPreferences, "maxPasteBytes">
}
