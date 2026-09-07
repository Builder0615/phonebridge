import { useState } from "react"
import { CheckCircle2, Loader2, RefreshCw } from "lucide-react"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Label } from "@/components/ui/label"
import { Separator } from "@/components/ui/separator"
import { Switch } from "@/components/ui/switch"
import { Input } from "@/components/ui/input"
import * as api from "@/lib/api"
import { useSession } from "@/lib/session-context"
import { DEFAULT_MAX_PASTE_BYTES } from "@/lib/paste-policy"

function wdaCheckVariant(status: string): "success" | "warning" | "destructive" | "outline" {
  if (status === "ok") return "success"
  if (status === "failed") return "destructive"
  if (status === "action_required" || status === "pending") return "warning"
  return "outline"
}

function Row({
  title,
  desc,
  children,
}: {
  title: string
  desc: string
  children: React.ReactNode
}) {
  return (
    <div className="flex items-center justify-between gap-4 py-2">
      <div className="min-w-0">
        <Label className="text-sm font-medium">{title}</Label>
        <p className="text-xs text-muted-foreground">{desc}</p>
      </div>
      {children}
    </div>
  )
}

export function SettingsView() {
  const {
    prefs,
    setPrefs,
    capability,
    iosControlCapability,
    iosControlCapabilityLoading,
    iosControlCapabilityError,
    refreshIosControlCapability,
    usbDevices,
  } = useSession()
  const [wdaSetup, setWdaSetup] = useState<api.IosWdaSetupReport | null>(null)
  const [wdaSetupLoading, setWdaSetupLoading] = useState(false)
  const [wdaSetupError, setWdaSetupError] = useState<string | null>(null)

  const selectedIphone = usbDevices.find((device) => device.kind === "iphone") ?? null

  const runWdaSetup = async () => {
    setWdaSetupLoading(true)
    setWdaSetupError(null)
    try {
      const report = await api.prepareIosWda(selectedIphone?.idMasked)
      setWdaSetup(report)
      if (report.ready) {
        // WDA is now the verified precision path. The active session is
        // restarted by the user so its input adapter is rebuilt.
        setPrefs({ iosUsbControlEnabled: true })
      }
      await refreshIosControlCapability()
    } catch (error) {
      setWdaSetupError(error instanceof Error ? error.message : String(error))
    } finally {
      setWdaSetupLoading(false)
    }
  }

  const iosControlStatus = iosControlCapabilityLoading
    ? "检查中…"
    : iosControlCapability?.wdaReachable
      ? "WDA 已可达"
      : iosControlCapability?.directInstallReady
        ? "内置 WDA 可直接安装"
        : iosControlCapability?.iproxyPresent
          ? "已找到 iproxy，等待设备/WDA"
          : iosControlCapability
            ? "缺少 iproxy"
            : "未检查"
  const iosControlStatusVariant = iosControlCapability?.wdaReachable
    ? "success"
    : iosControlCapability?.directInstallReady
      ? "success"
      : iosControlCapability?.iproxyPresent
        ? "warning"
        : "outline"

  return (
    <div className="max-w-xl space-y-1">
      <h2 className="text-base font-semibold">设置</h2>
      <p className="text-xs text-muted-foreground">
        所有开关仅影响本机会话行为，不修改系统设置；更改即时生效（音频与本地录制属于
        V1 范围，开关预留）。
      </p>
      <Separator className="my-3" />

      <Row
        title="自动启用控制"
        desc="镜像收到首帧后自动进入控制（BLE 广播 / ADB 通道）"
      >
        <Switch checked={prefs.autoControl} onCheckedChange={(v) => setPrefs({ autoControl: v })} />
      </Row>

      <Row
        title="画布 Ctrl+V 粘贴快捷键"
        desc="仅镜像画布获得焦点时拦截；关闭后可继续使用工具栏「粘贴」按钮"
      >
        <Switch
          checked={prefs.pasteShortcutEnabled}
          onCheckedChange={(v) => setPrefs({ pasteShortcutEnabled: v })}
        />
      </Row>

      <Row title="滚轮步长（像素）" desc="每步滚动的逻辑像素；换算为 HID 滚动值">
        <Input
          type="number"
          className="w-28"
          min={1}
          max={1000}
          value={prefs.scrollStep}
          onChange={(e) => {
            const v = Number(e.target.value)
            if (Number.isFinite(v) && v > 0) setPrefs({ scrollStep: Math.round(v) })
          }}
          aria-label="滚轮步长"
        />
      </Row>

      <Row
        title="iOS 鼠标速度倍率"
        desc="仅影响 iOS BLE 相对鼠标；BLE 不能把手机光标定位到画布绝对点，精准点击请启用 USB/WDA；建议先将 iPhone 跟踪速度设为 6，再微调此值；按 Esc 释放鼠标捕获后可拖动窗口"
      >
        <Input
          type="number"
          className="w-28"
          min={0.25}
          max={4}
          step={0.05}
          value={prefs.iosPointerScale}
          onChange={(e) => {
            const v = Number(e.target.value)
            if (Number.isFinite(v) && v >= 0.25 && v <= 4) {
              setPrefs({ iosPointerScale: Math.round(v * 100) / 100 })
            }
          }}
          aria-label="iOS 鼠标速度倍率"
        />
      </Row>

      <Row
        title="iOS USB/WDA 精确控制（实验）"
        desc="发布版内置已签名 WDA，应用负责安装、启动和校验后，通过 USB 发送绝对坐标和 Unicode 文本；未就绪时直接提示，不会偷偷回退 BLE"
      >
        <Switch
          checked={prefs.iosUsbControlEnabled}
          disabled={wdaSetupLoading}
          onCheckedChange={(v) => {
            if (v) {
              void runWdaSetup()
            } else {
              setPrefs({ iosUsbControlEnabled: false })
            }
          }}
        />
      </Row>

      <Alert className="mt-2">
        <AlertTitle>中英文混合粘贴与 USB/WDA 精确控制</AlertTitle>
        <AlertDescription>
          <p>
            BLE HID 键盘只能发送有限的键位，中文、Emoji 等字符不能可靠表达。中英文混合粘贴必须让当前 iOS 控制通道显示为
            <code className="mx-1 rounded bg-muted px-1">usb_wda</code>；文本会通过 WDA 的
            <code className="mx-1 rounded bg-muted px-1">/wda/keys</code> 发送到手机当前获得焦点的可编辑控件。
          </p>
          <ol className="mt-2 list-decimal space-y-1 pl-4">
            <li>
              发布版会自动检查内置签名 WDA IPA、安装器、go-ios、USB 设备和 Bundle ID；点击“一键准备 WDA”后，应用直接安装并启动 WDA，不要求用户手动安装。
            </li>
            <li>
              首次运行仍需要按 Apple 的系统提示确认开发者模式、信任此电脑或开发者签名；这些安全确认不能被桌面应用静默绕过，但应用会把卡住的具体步骤显示在下面。开发版没有内置 IPA 时，macOS 才会显示 Xcode 备用流程。
            </li>
            <li>准备成功后，应用会自动打开 USB/WDA 开关；如果当前已经在投屏，请先取消投屏再重新投屏，让会话切换到绝对坐标通道。</li>
          </ol>
          <div className="mt-3 flex flex-wrap items-center gap-2">
            <Badge variant={iosControlStatusVariant as "success" | "warning" | "outline"}>
              USB/WDA：{iosControlStatus}
            </Badge>
            <Button
              size="sm"
              variant="outline"
              disabled={iosControlCapabilityLoading}
              onClick={() => void refreshIosControlCapability()}
            >
              <RefreshCw className="size-3.5" aria-hidden />
              检查
            </Button>
            <Button size="sm" disabled={wdaSetupLoading} onClick={() => void runWdaSetup()}>
              {wdaSetupLoading ? <Loader2 className="size-3.5 animate-spin" aria-hidden /> : <CheckCircle2 className="size-3.5" aria-hidden />}
              {wdaSetupLoading ? "正在准备…" : "一键准备 WDA"}
            </Button>
          </div>
          {iosControlCapabilityError && (
            <p className="mt-2 text-xs text-destructive">检查失败：{iosControlCapabilityError}</p>
          )}
          {iosControlCapability && (
            <p className="mt-2 text-xs">{iosControlCapability.detail}</p>
          )}
          <p className="mt-2 text-xs text-muted-foreground">
            当前目标：{selectedIphone ? `${selectedIphone.name} · ${selectedIphone.idMasked}` : "未发现 USB iPhone，连接后再点击准备"}
          </p>
          {wdaSetupError && <p className="mt-2 text-xs text-destructive">WDA 流程失败：{wdaSetupError}</p>}
          {wdaSetup && (
            <div className="mt-3 space-y-1.5 rounded-md border border-border/70 bg-muted/20 p-2">
              <p className="text-xs font-medium">{wdaSetup.detail}</p>
              {wdaSetup.checks.map((item) => (
                <div key={item.key} className="flex items-start gap-2 text-xs">
                  <Badge variant={wdaCheckVariant(item.status)}>
                    {item.status === "ok" ? "通过" : item.status === "failed" ? "失败" : "待处理"}
                  </Badge>
                  <span className="min-w-0"><span className="font-medium">{item.title}：</span>{item.detail}</span>
                </div>
              ))}
            </div>
          )}
        </AlertDescription>
      </Alert>

      <Alert className="mt-2">
        <AlertTitle>iOS 蓝牙配对失败时</AlertTitle>
        <AlertDescription>
          macOS 版本不会在 iPhone HID 握手期间动态修改 GATT 表；若 iPhone 仍显示“配对不成功”，
          请按下面步骤让应用完整重建广播并清理旧绑定：
          <ol className="mt-2 list-decimal space-y-1 pl-4">
            <li>点击“取消投屏”，再完全退出并重新打开快投屏。</li>
            <li>
              在 iPhone 设置 → 蓝牙中，打开对应的 <code className="rounded bg-muted px-1">Mac</code> 或
              <code className="mx-1 rounded bg-muted px-1">Mac mini</code> 详情，选择“忽略此设备”。
            </li>
            <li>重新开始投屏，并在蓝牙列表中选择刚出现的本机条目；日志出现“输入已就绪”才表示配对完成。</li>
          </ol>
          <p className="mt-2 text-xs text-muted-foreground">
            iOS 不提供公开接口让桌面应用自动删除系统蓝牙绑定，因此这一步需要在手机上确认一次。
          </p>
        </AlertDescription>
      </Alert>

      <Row
        title="粘贴最大字节数"
        desc={`超过 ${DEFAULT_MAX_PASTE_BYTES} 字节（默认 32 KiB）时拒绝并提示，不静默截断`}
      >
        <Input
          type="number"
          className="w-28"
          min={1024}
          max={1024 * 1024}
          value={prefs.maxPasteBytes}
          onChange={(e) => {
            const v = Number(e.target.value)
            if (Number.isFinite(v) && v >= 1024) setPrefs({ maxPasteBytes: Math.round(v) })
          }}
          aria-label="粘贴最大字节数"
        />
      </Row>

      <Separator className="my-3" />

      <Row title="音频输出（V1）" desc="iPhone 镜像音频输出到默认设备 + 静音开关">
        <Switch disabled checked={false} onCheckedChange={() => undefined} />
      </Row>

      <Row title="本地录制（V1）" desc="录制当前镜像画面到 MP4（录制前显示路径与隐私提醒）">
        <Switch disabled checked={false} onCheckedChange={() => undefined} />
      </Row>

      <Separator className="my-3" />
      <p className="text-xs text-muted-foreground">
        当前宿主：{capability?.platform ?? "未知"} · {capability?.osVersion ?? ""}. 所有输入和
        粘贴内容不会被写入日志或上传；设备画面仅在同一局域网/ADB 通道内传输。
      </p>
    </div>
  )
}
