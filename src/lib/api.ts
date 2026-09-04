/**
 * React ↔ Rust 的唯一业务边界：受控 Tauri commands 与事件订阅。
 *
 * 命令名与事件名使用 snake_case（Spec v1.2 §5.4）；会话命令均携带 sessionId。
 * 所有错误统一转为可读 CommandError，不把原生错误字符串直接暴露给 UI。
 */

import { invoke, Channel } from "@tauri-apps/api/core"
import { listen, type UnlistenFn } from "@tauri-apps/api/event"
import type {
  CapabilityReport,
  DiagnosticsReport,
  ExportResult,
  HidStatus,
  LogEntry,
  MirrorMetadata,
  PasteOptions,
  PasteResult,
  PointerButtonEvent,
  PointerMoveEvent,
  SessionPreferences,
  SessionStateView,
  WheelPayload,
} from "./types"
import { framePacketFromChannelMessage } from "./frame-channel"

export const EVENTS = {
  sessionState: "session://state",
  activeChanged: "session://active",
  mirrorMetadata: "mirror://metadata",
  hidStatus: "hid://status",
  diagnosticError: "diagnostic://error",
  logEntry: "log://entry",
} as const

export class CommandError extends Error {
  constructor(
    public readonly code: string,
    message: string,
  ) {
    super(message)
    this.name = "CommandError"
  }
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args)
  } catch (err) {
    if (
      typeof err === "object" &&
      err !== null &&
      "code" in err &&
      "message" in err &&
      typeof (err as { code: unknown }).code === "string"
    ) {
      const e = err as { code: string; message: string }
      throw new CommandError(e.code, e.message)
    }
    throw new CommandError("unknown_error", String(err))
  }
}

// ---------------------------------------------------------------------------
// 能力
// ---------------------------------------------------------------------------

export const checkCapabilities = (): Promise<CapabilityReport> =>
  call<CapabilityReport>("check_capabilities")

export interface IosControlCapability {
  usbWdaEnabled: boolean
  wdaReachable: boolean
  iproxyPresent: boolean
  detail: string
}

export const getIosControlCapability = (): Promise<IosControlCapability> =>
  call<IosControlCapability>("get_ios_control_capability")

// ---------------------------------------------------------------------------
// 多设备会话
// ---------------------------------------------------------------------------

export const listSessions = (): Promise<SessionInfo[]> => call<SessionInfo[]>("list_sessions")

export interface SessionInfo {
  id: string
  kind: "iphone" | "android"
  active: boolean
  state: SessionStateView
  hid: HidStatus
  mirrorSize: [number, number] | null
}

export interface StartSessionRequest {
  kind: "iphone" | "android"
  id: string
  prefs?: SessionPreferences
}

export const startSession = (request: StartSessionRequest): Promise<void> =>
  call<void>("start_session", { request })

export const stopSession = (id: string): Promise<void> => call<void>("stop_session", { id })

export const stopAll = (): Promise<void> => call<void>("stop_all")

export const activateDevice = (id: string): Promise<void> => call<void>("activate_device", { id })

export const openSimulator = (id: string): Promise<void> => call<void>("open_simulator", { id })

/** 从 Rust 当前窗口身份确认模拟器路由，避免副窗口误渲染主面板。 */
export const currentSimulatorSession = (): Promise<string | null> =>
  call<string | null>("current_simulator_session")

export const startControl = (id: string, prefs: SessionPreferences): Promise<void> =>
  call<void>("start_control", { id, prefs })

export const stopControl = (id: string): Promise<void> => call<void>("stop_control", { id })

export const releaseAllInput = (id: string): Promise<void> =>
  call<void>("release_all_input", { id })

export const acknowledgeFrame = (id: string, sequence: number): Promise<void> =>
  call<void>("acknowledge_frame", { id, sequence })

// ---------------------------------------------------------------------------
// 输入（均携带 sessionId）
// ---------------------------------------------------------------------------

export const hidPointerMove = (id: string, event: PointerMoveEvent): Promise<void> =>
  call<void>("hid_pointer_move", { id, event })

export const hidPointerButton = (id: string, event: PointerButtonEvent): Promise<void> =>
  call<void>("hid_pointer_button", { id, event })

export const hidWheel = (id: string, event: WheelPayload): Promise<void> =>
  call<void>("hid_wheel", { id, event })

export const hidKeyStroke = (
  id: string,
  stroke: {
    code: string
    key: string
    modifiers: { ctrl: boolean; shift: boolean; alt: boolean; meta: boolean }
    repeat: boolean
  },
): Promise<void> => call<void>("hid_key_stroke", { id, stroke })

// ---------------------------------------------------------------------------
// 粘贴
// ---------------------------------------------------------------------------

export const pastePlainText = (id: string, options: PasteOptions): Promise<PasteResult> =>
  call<PasteResult>("paste_plain_text", { id, options })

// ---------------------------------------------------------------------------
// 帧桥接（Tauri Channel 二进制帧）
// ---------------------------------------------------------------------------

export async function attachFrameChannel(
  id: string,
  handlers: {
    onFrame: (bytes: Uint8Array) => void
    onSize: (width: number, height: number) => void
  },
): Promise<() => void> {
  // Tauri 2 的 Rust `InvokeResponseBody::Raw` 会在 JS 回调中以 ArrayBuffer
  // 交付（小消息直接执行，大消息经 fetch 通道），不是稳定的 Uint8Array。
  // 两种形态都要接收，否则后端即使持续解码出 RGBA 帧，画布也会一直黑屏。
  const channel = new Channel<unknown>()
  channel.onmessage = (msg) => {
    const packet = framePacketFromChannelMessage(msg)
    if (packet) {
      try {
        handlers.onFrame(packet.bytes)
      } finally {
        if (packet.sequence !== null) {
          void acknowledgeFrame(id, packet.sequence).catch(() => undefined)
        }
      }
      return
    }
    if (msg && typeof msg === "object" && "width" in msg && "height" in msg) {
      handlers.onSize(Number(msg.width), Number(msg.height))
    }
  }
  await invoke<void>("attach_frame_channel", { id, channel })
  return () => undefined
}

export const startTestFrames = (id: string, enabled: boolean): Promise<void> =>
  call<void>("frame_test_mode", { id, enabled })

export interface FrameStatus {
  hasSink: boolean
  testMode: boolean
  width: number
  height: number
}

export const getFrameStatus = (id: string): Promise<FrameStatus> =>
  call<FrameStatus>("frame_status", { id })

// ---------------------------------------------------------------------------
// USB 设备自动识别
// ---------------------------------------------------------------------------

export interface UsbDeviceView {
  kind: "iphone" | "android"
  name: string
  idMasked: string
  state: string
  authorized: boolean
}

export interface UsbDevicesReport {
  devices: UsbDeviceView[]
  errorPerSource: string[]
}

export const listUsbDevices = (): Promise<UsbDevicesReport> =>
  call<UsbDevicesReport>("list_usb_devices_cmd")

// ---------------------------------------------------------------------------
// 诊断
// ---------------------------------------------------------------------------

export const getDiagnostics = (): Promise<DiagnosticsReport> => call<DiagnosticsReport>("get_diagnostics")

export const exportDiagnostics = (): Promise<ExportResult> => call<ExportResult>("export_diagnostics")

// ---------------------------------------------------------------------------
// 事件订阅（payload 均携带 sessionId）
// ---------------------------------------------------------------------------

export interface SessionEvent<T> {
  sessionId: string
  payload: T
}

/**
 * Rust 事件使用语义字段包裹 payload（state/status/metadata/entry），而不是
 * 直接叫 payload。统一在 API 边界归一化，避免错误日志变成 undefined 后把
 * ControlPanel 的渲染链打崩成黑屏。
 */
function normalizeSessionEvent<T>(value: unknown, field: string): SessionEvent<T> | null {
  if (!value || typeof value !== "object") return null
  const record = value as Record<string, unknown>
  if (typeof record.sessionId !== "string") return null
  const payload = record.payload ?? record[field]
  if (payload === undefined) return null
  return { sessionId: record.sessionId, payload: payload as T }
}

function listenSessionEvent<T>(
  eventName: string,
  field: string,
  handler: (event: SessionEvent<T>) => void,
): Promise<UnlistenFn> {
  return listen<unknown>(eventName, (event) => {
    const normalized = normalizeSessionEvent<T>(event.payload, field)
    if (normalized) handler(normalized)
  })
}

export function onSessionState(handler: (e: SessionEvent<SessionStateView>) => void): Promise<UnlistenFn> {
  return listenSessionEvent(EVENTS.sessionState, "state", handler)
}

export function onActiveChanged(handler: (sessionId: string | null) => void): Promise<UnlistenFn> {
  return listen<{ sessionId: string | null }>(EVENTS.activeChanged, (ev) => handler(ev.payload.sessionId))
}

export function onMirrorMetadata(handler: (e: SessionEvent<MirrorMetadata>) => void): Promise<UnlistenFn> {
  return listenSessionEvent(EVENTS.mirrorMetadata, "metadata", handler)
}

export function onHidStatus(handler: (e: SessionEvent<HidStatus>) => void): Promise<UnlistenFn> {
  return listenSessionEvent(EVENTS.hidStatus, "status", handler)
}

export function onDiagnosticError(handler: (e: SessionEvent<LogEntry>) => void): Promise<UnlistenFn> {
  return listenSessionEvent(EVENTS.diagnosticError, "entry", handler)
}

/** 运行时日志（log://entry，含 info/warn/error）。 */
export function onLogEntry(handler: (e: SessionEvent<LogEntry>) => void): Promise<UnlistenFn> {
  return listenSessionEvent(EVENTS.logEntry, "entry", handler)
}

/** 拉取历史日志（应用启动后填充日志面板）。 */
export const getAppLogs = (): Promise<LogEntry[]> => call<LogEntry[]>("get_app_logs")

/** 上报前端页面错误（写入日志面板，帮助排查黑屏/空白）。 */
export const reportFrontendError = (message: string): Promise<void> =>
  call<void>("report_frontend_error", { message })
