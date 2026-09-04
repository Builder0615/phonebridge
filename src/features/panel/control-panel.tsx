import { useCallback, useMemo, useState } from "react"
import { ClipboardPaste, Copy, Plug, RefreshCw, Smartphone, X } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { ScrollArea } from "@/components/ui/scroll-area"
import { cn } from "@/lib/utils"
import { APP_NAME } from "@/lib/brand"
import { useSession } from "@/lib/session-context"
import type { UsbDeviceView } from "@/lib/api"

const MOUSE_INPUT_REPORT_MASK = 0x01 | 0x04
const KEYBOARD_INPUT_REPORT_MASK = 0x02 | 0x08

async function copyText(text: string) {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text)
      return
    }
  } catch {
    // Tauri WebView 的剪贴板权限可能因宿主版本不同而不可用，继续使用兼容路径。
  }

  const textarea = document.createElement("textarea")
  textarea.value = text
  textarea.setAttribute("readonly", "true")
  textarea.style.position = "fixed"
  textarea.style.opacity = "0"
  document.body.appendChild(textarea)
  textarea.select()
  const copied = document.execCommand("copy")
  textarea.remove()
  if (!copied) throw new Error("clipboard copy failed")
}

/**
 * 极简控制面板（Spec v1.2 · 用户要求）：
 * - 像 Android Studio / Xcode 一样：USB 插上即自动识别并列出；
 * - 每台设备一个「投屏/取消投屏」按钮，可多台同时投屏；
 * - 输入/粘贴只作用于获得焦点的投屏窗口；
 * - 底部日志区用于排查问题；无其它多余元素。
 */
export function ControlPanel() {
  const {
    usbDevices,
    usbErrors,
    refreshUsb,
    sessions,
    connect,
    disconnect,
    activate,
    openSimulator,
    paste,
    logs,
    refreshLogs,
  } =
    useSession()
  const [busy, setBusy] = useState<string | null>(null)
  const [filter, setFilter] = useState<"all" | "iphone" | "android">("all")
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle")
  const [pasteState, setPasteState] = useState<Record<string, string>>({})

  const logText = useMemo(
    () => logs.map((l) => `${l.at} ${l.level} [${l.code}] ${l.message}`).join("\n"),
    [logs],
  )

  const copyLogs = useCallback(async () => {
    if (!logText) return

    try {
      await copyText(logText)
      setCopyState("copied")
    } catch {
      setCopyState("failed")
    }

    window.setTimeout(() => setCopyState("idle"), 1800)
  }, [logText])

  const streamingIds = useMemo(
    () =>
      new Set(
        sessions
          .filter((s) => !["failed", "idle", "stopping"].includes(s.state.state))
          .map((s) => s.id),
      ),
    [sessions],
  )

  const run = useCallback(async (key: string, fn: () => Promise<unknown>) => {
    setBusy(key)
    try {
      await fn()
      await refreshUsb()
    } catch {
      // 后端已通过 diagnostic://error 记录具体原因；刷新历史日志，避免
      // 点击投屏失败变成未处理 Promise，进而影响 WebView 渲染。
      await refreshLogs()
    } finally {
      setBusy(null)
    }
  }, [refreshLogs, refreshUsb])

  const pasteToDevice = useCallback(async (id: string) => {
    setBusy(`paste-${id}`)
    setPasteState((prev) => ({ ...prev, [id]: "正在读取电脑剪贴板…" }))
    try {
      // The panel action explicitly selects the target simulator first, then
      // invokes the same one-shot clipboard command used by Cmd/Ctrl+V.
      await activate(id)
      const result = await paste(id)
      const message = result?.ok
        ? `已发送 ${result.charCount} 个字符`
        : result?.error?.message ?? "粘贴失败，请查看日志"
      setPasteState((prev) => ({ ...prev, [id]: message }))
    } catch {
      setPasteState((prev) => ({ ...prev, [id]: "粘贴失败，请查看日志" }))
    } finally {
      setBusy(null)
      window.setTimeout(() => {
        setPasteState((prev) => {
          const next = { ...prev }
          delete next[id]
          return next
        })
      }, 3500)
    }
  }, [activate, paste])

  const toggleProjection = useCallback(
    (d: UsbDeviceView) => {
      const id = `${d.kind}:${d.idMasked}`
      const existing = sessions.find((s) => s.id === id)
      if (existing?.state.state === "failed") {
        // 失败会话由后端保留用于诊断；重试前先清理它，避免下一次启动被
        // “设备已在会话中”拦截。
        void run(`connect-${id}`, async () => {
          await disconnect(id)
          await connect(d.kind, id)
          await openSimulator(id)
        })
      } else if (streamingIds.has(id)) {
        void run(`disconnect-${id}`, () => disconnect(id))
      } else {
        void run(`connect-${id}`, async () => {
          await connect(d.kind, id)
          await openSimulator(id)
        })
      }
    },
    [sessions, streamingIds, connect, disconnect, openSimulator, run],
  )

  const devices = usbDevices.filter((d) => (filter === "all" ? true : d.kind === filter))
  const visible = devices.length > 0

  return (
    <div className="flex h-screen min-h-0 flex-col overflow-hidden bg-background text-foreground">
      <header className="flex h-12 shrink-0 items-center gap-2 border-b border-border px-3">
        <h1 className="text-sm font-semibold tracking-tight">{APP_NAME}</h1>
        <div className="ml-auto flex items-center gap-1">
          {(["all", "iphone", "android"] as const).map((k) => (
            <Button
              key={k}
              size="sm"
              variant={filter === k ? "secondary" : "ghost"}
              className="h-6 text-xs"
              onClick={() => setFilter(k)}
            >
              {k === "all" ? "全部" : k === "iphone" ? "iPhone" : "Android"}
            </Button>
          ))}
          <Button size="sm" variant="ghost" className="h-6" onClick={() => void run("refresh", refreshUsb)}>
            <RefreshCw className="size-3.5" aria-hidden />
          </Button>
        </div>
      </header>

      <div className="flex min-h-0 flex-1 flex-col">
        {/* 已连接设备列表：与日志各占标题栏以下的 50% */}
        <main className="min-h-0 flex-1 basis-1/2 overflow-y-auto p-3">
          {!visible ? (
            <p className="py-8 text-center text-sm text-muted-foreground">
              {usbErrors.length > 0 ? (
                <>
                  尚未检测到设备。
                  <span className="mt-2 block text-xs text-muted-foreground/80">{usbErrors.join("；")}</span>
                </>
              ) : (
                "连接设备后自动显示（USB）。"
              )}
            </p>
          ) : (
            <ul className="mx-auto max-w-2xl space-y-2">
              {devices.map((d) => {
                const id = `${d.kind}:${d.idMasked}`
                const isStreaming = streamingIds.has(id)
                const session = sessions.find((s) => s.id === id)
                const isFailed = session?.state.state === "failed"
                const waitingForAirPlay = d.kind === "iphone" && session?.state.state === "mirroring"
                const reconnecting = session?.state.state === "reconnecting"
                const isBusy = busy === `connect-${id}` || busy === `disconnect-${id}`
                const control = session?.state.control
                const waitingForPairing = control === "broadcasting" || control === "waiting_pairing"
                const controlReady = control === "connected"
                const controlTransport = session?.hid.controlTransport ?? "ble"
                const usbWdaReady = d.kind === "iphone" && controlReady && controlTransport === "usb_wda"
                const hidError = session?.hid.lastError
                const hidNotAdvertising = d.kind === "iphone" && controlTransport !== "usb_wda" && isStreaming && waitingForPairing &&
                  session?.hid.advertising !== true
                const pairingName = session?.hid.pairingName ?? APP_NAME
                const advertisedName = session?.hid.advertisedName ?? APP_NAME
                const pairingUsesHostName = d.kind === "iphone" &&
                  session?.hid.pairingName != null && session.hid.pairingName !== APP_NAME
                const inputReportMask = session?.hid.inputReportMask ?? 0
                const mouseInputReady = (inputReportMask & MOUSE_INPUT_REPORT_MASK) !== 0
                const keyboardInputReady = (inputReportMask & KEYBOARD_INPUT_REPORT_MASK) !== 0
                // iPhone 的画面链路走 AirPlay，USB 只负责识别；即使手机尚未
                // 信任此 Mac，也可以先启动接收器并在控制中心完成镜像连接。
                const canStart = d.authorized || d.kind === "iphone"
                return (
                  <li key={id} className="flex flex-wrap items-center gap-3 rounded-lg border border-border bg-card p-2.5">
                    {d.kind === "iphone" ? (
                      <Smartphone className="size-5 shrink-0 text-muted-foreground" aria-hidden />
                    ) : (
                      <Plug className="size-5 shrink-0 text-muted-foreground" aria-hidden />
                    )}
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-sm font-medium">{d.name}</p>
                      <p className="truncate text-xs text-muted-foreground">
                        {d.kind === "iphone" ? "iPhone · " : "Android · "}
                        {d.idMasked} · {d.state}
                      </p>
                    </div>
                    {isFailed ? (
                      <Badge variant="destructive" className="shrink-0">
                        投屏失败
                      </Badge>
                    ) : reconnecting ? (
                      <Badge variant="warning" className="shrink-0">
                        重连中…
                      </Badge>
                    ) : waitingForAirPlay ? (
                      <Badge variant="warning" className="shrink-0">
                        等待手机连接
                      </Badge>
                    ) : isStreaming ? (
                      <Badge variant="success" className="shrink-0">
                        投屏中
                      </Badge>
                    ) : (
                      !d.authorized && (
                        <Badge variant="warning" className="shrink-0">
                          待授权
                        </Badge>
                      )
                    )}
                    <Button
                      size="sm"
                      className="shrink-0"
                      variant={isStreaming ? "outline" : isFailed ? "destructive" : "default"}
                      disabled={isBusy || (!canStart && !isStreaming)}
                      onClick={() => toggleProjection(d)}
                    >
                      {isFailed ? (
                        "重试投屏"
                      ) : isStreaming ? (
                        <>
                          <X className="size-3.5" aria-hidden />
                          取消投屏
                        </>
                      ) : (
                        "投屏"
                      )}
                    </Button>
                    {controlReady && (
                      <Button
                        size="sm"
                        variant="outline"
                        className="shrink-0"
                        disabled={busy !== null}
                        title="将电脑剪贴板纯文本发送到此设备"
                        aria-label={`粘贴到 ${d.name}`}
                        onClick={() => void pasteToDevice(id)}
                      >
                        <ClipboardPaste className="size-3.5" aria-hidden />
                        粘贴
                      </Button>
                    )}
                    {waitingForAirPlay && (
                      <p className="basis-full pl-8 text-xs text-warning">
                        请在 iPhone 控制中心 → 屏幕镜像中选择 {APP_NAME}；手机和电脑需在同一局域网，且 macOS 防火墙需允许本应用/UxPlay 接受传入连接
                      </p>
                    )}
                    {pasteState[id] && (
                      <p className="basis-full pl-8 text-xs text-muted-foreground">
                        {pasteState[id]}
                      </p>
                    )}
                    {d.kind === "iphone" && (usbWdaReady ? (
                      <p className="basis-full pl-8 text-xs text-success">
                        iOS USB/WDA 绝对坐标输入已连接；画面仍走 AirPlay，模拟器窗口内可直接点击和拖动
                      </p>
                    ) : isStreaming && hidError && !controlReady ? (
                      <p className="basis-full pl-8 text-xs text-warning">
                        iOS 键鼠输入未就绪：{hidError}
                      </p>
                    ) : hidNotAdvertising ? (
                      <p className="basis-full pl-8 text-xs text-warning">
                        iOS 键鼠广播尚未就绪，请等待 CoreBluetooth 完成服务发布；若持续没有出现对应的 HID 条目，请查看日志中的 BLE 错误
                      </p>
                    ) : waitingForPairing ? (
                      <p className="basis-full pl-8 text-xs text-warning">
                        iOS 键鼠输入：请先在 iPhone 设置 → 辅助功能 → 触控 → 辅助触控中开启，再进入“设备 → 蓝牙设备”选择「{pairingName}」
                        {pairingUsesHostName && `（macOS 系统列表使用本机名称；BLE 广播短名为「${advertisedName}」）`}
                      </p>
                    ) : controlReady && isStreaming ? (
                      <p className={cn("basis-full pl-8 text-xs", mouseInputReady && session?.hid.assistiveTouchHint ? "text-warning" : "text-success")}>
                        iOS {keyboardInputReady ? "键盘" : "键盘报告未订阅"}{keyboardInputReady && mouseInputReady ? "、" : ""}{mouseInputReady ? "鼠标 BLE 报告" : "鼠标报告未订阅"}已连接；
                        {mouseInputReady && session?.hid.assistiveTouchHint
                          ? "请在 iPhone 设置 → 辅助功能 → 触控 → 辅助触控中开启，并在辅助触控 → 设备 → 蓝牙设备中选择该 Mac"
                          : "模拟器窗口聚焦后即可操作"}
                      </p>
                    ) : control === "disabled" && isStreaming ? (
                      session?.hid.poweredOn && session.hid.advertising ? (
                        <p className="basis-full pl-8 text-xs text-warning">
                          iOS 键鼠输入已断开；请在 iPhone 辅助触控 → 设备 → 蓝牙设备中重新连接「{pairingName}」后再操作模拟器
                        </p>
                      ) : (
                        <p className="basis-full pl-8 text-xs text-warning">
                          iOS 输入未就绪：macOS 请到 系统设置→隐私与安全性→蓝牙 允许本应用后重启投屏；Windows 无蓝牙模块的电脑请使用 USB 蓝牙适配器（输入只能走蓝牙，USB 仅供识别）
                        </p>
                      )
                    ) : null)}
                  </li>
                )
              })}
            </ul>
          )}
        </main>

        {/* 日志区：固定在下半区，只有日志内容滚动 */}
        <footer className="flex min-h-0 flex-1 basis-1/2 flex-col border-t border-border">
          <div className="flex shrink-0 items-center justify-between px-3 py-1.5">
            <h2 className="text-xs font-medium uppercase tracking-wide text-muted-foreground">日志</h2>
            <div className="flex items-center gap-1">
              <Button
                size="sm"
                variant="ghost"
                className="h-6 gap-1 text-xs"
                disabled={!logText}
                title="复制日志"
                aria-label="复制日志"
                onClick={() => void copyLogs()}
              >
                <Copy className="size-3" aria-hidden />
                {copyState === "copied" ? "已复制" : copyState === "failed" ? "复制失败" : "复制"}
              </Button>
              <Button size="sm" variant="ghost" className="h-6 text-xs" title="刷新日志" aria-label="刷新日志" onClick={() => void run("logs", refreshLogs)}>
                <RefreshCw className="size-3" aria-hidden />
              </Button>
            </div>
          </div>
          <ScrollArea className="min-h-0 flex-1 overflow-hidden px-3 pb-2 font-mono text-[11px] leading-relaxed">
            {logs.length === 0 ? (
              <p className="text-muted-foreground">暂无日志。</p>
            ) : (
              logs.map((l, i) => (
                <p key={`${l.at}-${i}`} className={cn("whitespace-pre-wrap", l.level === "error" && "text-destructive")}>
                  <span className="text-muted-foreground">{l.at.slice(11, 23)}</span>{" "}
                  <span className={cn(l.level === "info" ? "text-primary" : l.level === "warn" ? "text-warning" : "")}>
                    {l.level}
                  </span>{" "}
                  [{l.code}] {l.message}
                </p>
              ))
            )}
          </ScrollArea>
        </footer>
      </div>
    </div>
  )
}
