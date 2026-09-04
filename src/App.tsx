import { Component, type ErrorInfo, type ReactNode } from "react"
import * as api from "@/lib/api"
import { APP_NAME } from "@/lib/brand"
import { TooltipProvider } from "@/components/ui/tooltip"
import { SessionProvider } from "@/lib/session-context"
import { ControlPanel } from "@/features/panel/control-panel"

interface AppErrorBoundaryState {
  hasError: boolean
}

class AppErrorBoundary extends Component<{ children: ReactNode }, AppErrorBoundaryState> {
  state: AppErrorBoundaryState = { hasError: false }

  static getDerivedStateFromError(): AppErrorBoundaryState {
    return { hasError: true }
  }

  componentDidCatch(error: Error, _info: ErrorInfo) {
    // 不把堆栈或页面内容写入日志，只记录错误类型；黑屏时至少给用户可见反馈。
    void api.reportFrontendError(`render_error:${error.name}`).catch(() => undefined)
  }

  render() {
    if (this.state.hasError) {
      return (
        <div className="flex h-screen items-center justify-center bg-background px-6 text-center text-sm text-foreground">
          {APP_NAME} 页面加载失败，请重新打开应用。
        </div>
      )
    }
    return this.props.children
  }
}

export default function App() {
  return (
    <AppErrorBoundary>
      <SessionProvider>
        <TooltipProvider delayDuration={200}>
          <ControlPanel />
        </TooltipProvider>
      </SessionProvider>
    </AppErrorBoundary>
  )
}
