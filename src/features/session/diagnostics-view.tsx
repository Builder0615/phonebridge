import { useCallback, useState } from "react"
import { AlertCircle, Download, Loader2 } from "lucide-react"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { ScrollArea } from "@/components/ui/scroll-area"
import { Skeleton } from "@/components/ui/skeleton"
import { exportDiagnostics, getDiagnostics } from "@/lib/api"
import { APP_NAME } from "@/lib/brand"
import type { DependencyState, DiagnosticsReport, ExportResult } from "@/lib/types"
import { cn } from "@/lib/utils"

function DepBadge({ dep }: { dep: DependencyState }) {
  const tone =
    dep.status === "ok"
      ? "success"
      : dep.status === "missing"
        ? "destructive"
        : "warning"
  return (
    <Badge variant={tone as "success" | "destructive" | "warning"}>{dep.status}</Badge>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{title}</CardTitle>
      </CardHeader>
      <CardContent className="text-xs text-muted-foreground">{children}</CardContent>
    </Card>
  )
}

export function DiagnosticsView() {
  const [report, setReport] = useState<DiagnosticsReport | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [exportResult, setExportResult] = useState<ExportResult | null>(null)
  const [exportOpen, setExportOpen] = useState(false)

  const run = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      setReport(await getDiagnostics())
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoading(false)
    }
  }, [])

  const confirmExport = useCallback(async () => {
    setExportOpen(false)
    setLoading(true)
    try {
      setExportResult(await exportDiagnostics())
    } finally {
      setLoading(false)
    }
  }, [])

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <Button onClick={() => void run()} disabled={loading}>
          {loading ? <Loader2 className="size-4 animate-spin" aria-hidden /> : <AlertCircle className="size-4" aria-hidden />}
          一键诊断
        </Button>
        <Button
          variant="outline"
          disabled={!report || loading}
          onClick={() => setExportOpen(true)}
        >
          <Download className="size-4" aria-hidden />
          导出诊断包
        </Button>
        {exportResult?.ok && exportResult.path && (
          <span className="truncate text-xs text-muted-foreground">
            已导出：{exportResult.path}
          </span>
        )}
      </div>

      {error && (
        <Alert variant="destructive">
          <AlertTitle>诊断失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}

      {loading && !report && (
        <Card>
          <CardContent className="space-y-2 pt-6">
            <Skeleton className="h-4 w-full" />
            <Skeleton className="h-4 w-full" />
            <Skeleton className="h-4 w-2/3" />
          </CardContent>
        </Card>
      )}

      {report && (
        <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
          <Section title="应用与系统">
            <p>{APP_NAME} {report.app.version}</p>
            <p>宿主：{report.os.family} · {report.os.version}</p>
            <p>构建号：{report.os.build ?? "未知"}</p>
            <p>
              Windows 门槛：{report.os.meetsMinimum ? "满足" : "未满足"}（≥ 19041，
              非 Windows 宿主不适用）
            </p>
            <p>Tauri {report.app.tauriVersion} · Rust {report.app.rustVersion}</p>
          </Section>

          <Section title="网络">
            <p>mDNS：<DepBadge dep={report.network.mdns} /> {report.network.mdns.detail}</p>
            <p>防火墙：{report.network.firewall.detail}</p>
            <ScrollArea className="h-40">
              <ul className="space-y-1">
                {report.network.interfaces.map((iface) => (
                  <li key={iface.name}>
                    {iface.name} · {iface.ipv4Masked ?? "无 IPv4"} · {iface.status}
                  </li>
                ))}
              </ul>
            </ScrollArea>
          </Section>

          <Section title="蓝牙（iPhone 控制）">
            <p>LE Peripheral 角色：{report.bluetooth.peripheralRole ? "支持" : "不支持/待验证"}</p>
            <p>适配器地址（脱敏）：{report.bluetooth.adapterAddressMasked ?? "未知"}</p>
            <p>{report.bluetooth.note}</p>
          </Section>

          <Section title="镜像依赖">
            <p>
              UxPlay：<DepBadge dep={report.mirror.uxplay} /> {report.mirror.uxplay.detail}
            </p>
            <p>
              GStreamer：<DepBadge dep={report.mirror.gstreamer} /> {report.mirror.gstreamer.detail}
            </p>
            <p>AirPlay 端口：{report.mirror.port}</p>
          </Section>

          <Section title="Android 链路（scrcpy-server）">
            <p>
              adb：<DepBadge dep={report.mirror.android.adb} /> · scrcpy-server：
              <DepBadge dep={report.mirror.android.scrcpyServer} /> · ffmpeg：
              <DepBadge dep={report.mirror.android.ffmpeg} />
            </p>
            <p>{report.mirror.android.detail}</p>
          </Section>

          <Section title="安全策略">
            <p>CSP：<code>{report.security.csp}</code></p>
            <p>剪贴板策略：{report.security.clipboardPolicy}</p>
            <ul className="mt-1 space-y-0.5">
              {report.security.capabilities.map((c) => (
                <li key={c}>{c}</li>
              ))}
            </ul>
          </Section>

          <Section title="最近错误（脱敏）">
            {report.recentErrors.length === 0 ? (
              <p>无</p>
            ) : (
              <ScrollArea className="h-40">
                <ul className="space-y-1">
                  {report.recentErrors.map((e, i) => (
                    <li key={`${e.at}-${i}`} className={cn(e.level === "error" && "text-destructive")}>
                      {e.at.slice(11, 19)} [{e.code}] {e.message}
                    </li>
                  ))}
                </ul>
              </ScrollArea>
            )}
          </Section>
        </div>
      )}

      <Dialog open={exportOpen} onOpenChange={setExportOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>确认导出诊断包</DialogTitle>
            <DialogDescription>
              诊断包只包含脱敏后的环境信息与错误摘要，不包含画面、音频、剪贴板文本、
              键盘原文或认证材料。
            </DialogDescription>
          </DialogHeader>
          {report && (
            <ul className="list-inside list-disc text-sm text-muted-foreground">
              {[
                "应用与系统版本",
                "网络接口（IP/MAC 脱敏）",
                "mDNS / 防火墙状态",
                "蓝牙 LE Peripheral 能力",
                "UxPlay / GStreamer 依赖",
                "scrcpy / ADB 依赖与设备（脱敏）",
                "最近错误（脱敏）",
              ].map((item) => (
                <li key={item}>{item}</li>
              ))}
            </ul>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setExportOpen(false)}>
              取消
            </Button>
            <Button onClick={() => void confirmExport()} disabled={loading}>
              确认导出
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {exportResult && !exportResult.ok && (
        <Alert variant="destructive">
          <AlertTitle>导出失败</AlertTitle>
          <AlertDescription>{exportResult.error}</AlertDescription>
        </Alert>
      )}
    </div>
  )
}
