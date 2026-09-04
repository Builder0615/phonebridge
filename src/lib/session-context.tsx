import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react"
import * as api from "./api"
import type { UsbDeviceView } from "./api"
import { DEFAULT_MAX_PASTE_BYTES } from "./paste-policy"
import type {
  CapabilityReport,
  HidStatus,
  LogEntry,
  MirrorMetadata,
  PasteResult,
  SessionPreferences,
  SessionStateView,
} from "./types"

export interface SessionDetail {
  state: SessionStateView | null
  hid: HidStatus | null
  mirror: MirrorMetadata | null
}

export interface SessionContextValue {
  capability: CapabilityReport | null
  capabilityLoading: boolean
  capabilityError: string | null
  /** 会话列表（面板展示） */
  sessions: api.SessionInfo[]
  /** 每会话的实时状态（state/hid/mirror） */
  details: Record<string, SessionDetail>
  activeId: string | null
  usbDevices: UsbDeviceView[]
  usbErrors: string[]
  errorFeed: LogEntry[]
  logs: LogEntry[]
  refreshLogs: () => Promise<void>
  prefs: SessionPreferences
  setPrefs: (patch: Partial<SessionPreferences>) => void
  refreshUsb: () => Promise<void>
  refreshCapability: () => Promise<void>
  refreshSessions: () => Promise<void>
  connect: (kind: "iphone" | "android", id: string) => Promise<void>
  disconnect: (id: string) => Promise<void>
  disconnectAll: () => Promise<void>
  activate: (id: string) => Promise<void>
  openSimulator: (id: string) => Promise<void>
  enableControl: (id: string) => Promise<void>
  stopControl: (id: string) => Promise<void>
  releaseAll: (id: string) => Promise<void>
  paste: (id: string) => Promise<PasteResult | null>
  startTestFrames: (id: string, enabled: boolean) => Promise<void>
}

const SessionContext = createContext<SessionContextValue | null>(null)

export function SessionProvider({ children }: { children: ReactNode }) {
  const [capability, setCapability] = useState<CapabilityReport | null>(null)
  const [capabilityLoading, setCapabilityLoading] = useState(true)
  const [capabilityError, setCapabilityError] = useState<string | null>(null)
  const [sessions, setSessions] = useState<api.SessionInfo[]>([])
  const [details, setDetails] = useState<Record<string, SessionDetail>>({})
  const [activeId, setActiveId] = useState<string | null>(null)
  const [errorFeed, setErrorFeed] = useState<LogEntry[]>([])
  const [usbDevices, setUsbDevices] = useState<UsbDeviceView[]>([])
  const [logs, setLogs] = useState<LogEntry[]>([])
  const [usbErrors, setUsbErrors] = useState<string[]>([])
  const usbRefreshSequence = useRef(0)
  const [prefs, setPrefsState] = useState<SessionPreferences>({
    autoControl: true,
    pasteShortcutEnabled: true,
    scrollStep: 80,
    maxPasteBytes: DEFAULT_MAX_PASTE_BYTES,
    iosPointerScale: 1,
    iosUsbControlEnabled: false,
  })
  const prefsRef = useRef(prefs)
  prefsRef.current = prefs

  const refreshCapability = useCallback(async () => {
    setCapabilityLoading(true)
    setCapabilityError(null)
    try {
      setCapability(await api.checkCapabilities())
    } catch (err) {
      setCapabilityError(err instanceof Error ? err.message : String(err))
    } finally {
      setCapabilityLoading(false)
    }
  }, [])

  const refreshUsb = useCallback(async () => {
    const sequence = ++usbRefreshSequence.current
    try {
      const report = await api.listUsbDevices()
      // xcdevice 是兼容兜底，在部分 Xcode 版本上可能比 devicectl 慢；
      // 丢弃较早请求的结果，避免旧的“空列表”覆盖刚刚完成的新枚举。
      if (sequence !== usbRefreshSequence.current) return
      setUsbDevices(report.devices)
      setUsbErrors(report.errorPerSource)
    } catch {
      if (sequence !== usbRefreshSequence.current) return
      setUsbErrors(["USB 设备枚举失败"])
    }
  }, [])

  const refreshLogs = useCallback(async () => {
    try {
      setLogs(await api.getAppLogs())
    } catch {
      // 后端不可用时保持现有日志
    }
  }, [])

  const refreshSessions = useCallback(async () => {
    try {
      const list = await api.listSessions()
      setSessions(list)
      setDetails((prev) => {
        const next = { ...prev }
        for (const s of list) {
          next[s.id] = {
            state: s.state,
            hid: s.hid,
            mirror: s.mirrorSize
              ? {
                  width: s.mirrorSize[0],
                  height: s.mirrorSize[1],
                  rotation: 0,
                  codec: null,
                }
              : prev[s.id]?.mirror ?? null,
          }
        }
        return next
      })
    } catch {
      // 面板可能尚未准备好；静默
    }
  }, [])

  useEffect(() => {
    void refreshCapability()
    void refreshUsb()
    void refreshSessions()
    void refreshLogs()
    const off: Array<() => void> = []
    let disposed = false
    const subscribe = (register: () => Promise<() => void>) => {
      void register()
        .then((unlisten) => {
          // React StrictMode 会在订阅 Promise 完成前执行一次清理；此时
          // 监听器已注册但还没进入 off 数组，必须在解析后立即注销。
          if (disposed) {
            unlisten()
          } else {
            off.push(unlisten)
          }
        })
        .catch(() => undefined)
    }
    subscribe(() => api.onSessionState((e) => {
        const id = e.sessionId
        // 控制面板直接读取 sessions；只更新 details 会让模拟器收到状态，
        // 但设备行继续显示旧的“等待手机连接”，直到下一次 3 秒轮询。
        // 状态事件本身就是后端的权威快照，两个投影必须一起更新。
        setSessions((prev) => prev.map((session) => session.id === id ? { ...session, state: e.payload } : session))
        setDetails((prev) => ({
          ...prev,
          [id]: { ...(prev[id] ?? { state: null, hid: null, mirror: null }), state: e.payload },
        }))
      }))
    subscribe(() => api.onHidStatus((e) => {
        const id = e.sessionId
        // HID 状态同样是设备行的权威快照；否则原生桥接已经报告了
        // CoreBluetooth 的真实原因，面板仍会保留旧的“等待手机连接”。
        setSessions((prev) => prev.map((session) => session.id === id ? { ...session, hid: e.payload } : session))
        setDetails((prev) => ({
          ...prev,
          [id]: { ...(prev[id] ?? { state: null, hid: null, mirror: null }), hid: e.payload },
        }))
      }))
    subscribe(() => api.onMirrorMetadata((e) => {
        const id = e.sessionId
        setDetails((prev) => ({
          ...prev,
          [id]: { ...(prev[id] ?? { state: null, hid: null, mirror: null }), mirror: e.payload },
        }))
      }))
    subscribe(() => api.onActiveChanged((id) => setActiveId(id)))
    subscribe(() => api.onDiagnosticError((e) => {
        setErrorFeed((prev) => [...prev, e.payload].slice(-20))
        setLogs((prev) => [...prev, e.payload].slice(-300))
      }))
    subscribe(() => api.onLogEntry((e) => setLogs((prev) => [...prev, e.payload].slice(-300))))
    const timer = window.setInterval(() => {
      void refreshSessions()
      void refreshUsb()
    }, 3000)
    return () => {
      disposed = true
      off.splice(0).forEach((f) => f())
      window.clearInterval(timer)
    }
  }, [refreshCapability, refreshUsb, refreshSessions, refreshLogs])

  const connect = useCallback(async (kind: "iphone" | "android", id: string) => {
    try {
      await api.startSession({ kind, id, prefs: prefsRef.current })
    } finally {
      // start_session 失败时 Rust 仍保留 Failed 会话用于诊断；无论成功与否
      // 都刷新列表，否则主窗口看不到失败原因，下一次点击还会重复撞 Busy。
      await refreshSessions()
    }
  }, [refreshSessions])

  const disconnect = useCallback(async (id: string) => {
    await api.stopSession(id)
    await refreshSessions()
  }, [refreshSessions])

  const disconnectAll = useCallback(async () => {
    await api.stopAll()
    await refreshSessions()
  }, [refreshSessions])

  const activate = useCallback(async (id: string) => {
    await api.activateDevice(id)
  }, [])

  const openSimulator = useCallback(async (id: string) => {
    await api.openSimulator(id)
  }, [])

  const enableControl = useCallback(async (id: string) => {
    await api.startControl(id, prefsRef.current)
  }, [])

  const stopControl = useCallback(async (id: string) => {
    await api.stopControl(id)
  }, [])

  const releaseAll = useCallback(async (id: string) => {
    await api.releaseAllInput(id)
  }, [])

  const paste = useCallback(async (id: string): Promise<PasteResult | null> => {
    try {
      return await api.pastePlainText(id, {
        prefs: { maxPasteBytes: prefsRef.current.maxPasteBytes },
      })
    } catch {
      return null
    }
  }, [])

  const startTestFrames = useCallback(async (id: string, enabled: boolean) => {
    await api.startTestFrames(id, enabled)
  }, [])

  const setPrefs = useCallback((patch: Partial<SessionPreferences>) => {
    setPrefsState((prev) => ({ ...prev, ...patch }))
  }, [])

  const value = useMemo<SessionContextValue>(
    () => ({
      capability,
      capabilityLoading,
      capabilityError,
      sessions,
      details,
      activeId,
      usbDevices,
      usbErrors,
      errorFeed,
      logs,
      refreshLogs,
      prefs,
      setPrefs,
      refreshUsb,
      refreshCapability,
      refreshSessions,
      connect,
      disconnect,
      disconnectAll,
      activate,
      openSimulator,
      enableControl,
      stopControl,
      releaseAll,
      paste,
      startTestFrames,
    }),
    [
      capability,
      capabilityLoading,
      capabilityError,
      sessions,
      details,
      activeId,
      usbDevices,
      usbErrors,
      errorFeed,
      logs,
      refreshLogs,
      prefs,
      setPrefs,
      refreshUsb,
      refreshCapability,
      refreshSessions,
      connect,
      disconnect,
      disconnectAll,
      activate,
      openSimulator,
      enableControl,
      stopControl,
      releaseAll,
      paste,
      startTestFrames,
    ],
  )

  return <SessionContext.Provider value={value}>{children}</SessionContext.Provider>
}

export function useSession(): SessionContextValue {
  const ctx = useContext(SessionContext)
  if (!ctx) throw new Error("useSession 必须在 <SessionProvider> 内使用")
  return ctx
}
