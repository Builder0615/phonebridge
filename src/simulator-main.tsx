import { useEffect, useState } from "react"
import React from "react"
import ReactDOM from "react-dom/client"
import { getCurrentWindow } from "@tauri-apps/api/window"
import * as api from "@/lib/api"
import { TooltipProvider } from "@/components/ui/tooltip"
import { SessionProvider } from "@/lib/session-context"
import {
  SimulatorWindow,
  extractSessionIdFromLabel,
  extractSessionIdFromSearch,
} from "@/features/simulator/simulator-window"
import "@/styles/globals.css"

/**
 * 模拟器使用独立 HTML 入口，入口本身不引入 ControlPanel。
 * URL/label 是同步提示，Rust 命令是最后兜底；解析失败时也只显示纯黑画布。
 */
export function SimulatorEntry() {
  const windowLabel = getCurrentWindow().label
  const routeHint =
    extractSessionIdFromSearch(window.location.search) ?? extractSessionIdFromLabel(windowLabel)
  const [sessionId, setSessionId] = useState<string | null | undefined>(routeHint ?? undefined)

  useEffect(() => {
    if (routeHint) return

    let alive = true
    void api
      .currentSimulatorSession()
      .then((id) => {
        if (alive) setSessionId(id)
      })
      .catch(() => {
        if (alive) setSessionId(null)
      })

    return () => {
      alive = false
    }
  }, [routeHint])

  if (!sessionId) return <div className="h-screen w-screen bg-black" />

  return (
    <SessionProvider>
      <TooltipProvider delayDuration={200}>
        <SimulatorWindow sessionId={sessionId} />
      </TooltipProvider>
    </SessionProvider>
  )
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <SimulatorEntry />
  </React.StrictMode>,
)
