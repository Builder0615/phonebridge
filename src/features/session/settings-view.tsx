import { Label } from "@/components/ui/label"
import { Separator } from "@/components/ui/separator"
import { Switch } from "@/components/ui/switch"
import { Input } from "@/components/ui/input"
import { useSession } from "@/lib/session-context"
import { DEFAULT_MAX_PASTE_BYTES } from "@/lib/paste-policy"

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
  const { prefs, setPrefs, capability } = useSession()

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
        desc="仅影响 iOS BLE 相对鼠标；建议先将 iPhone 跟踪速度设为 6，再微调此值；按 Esc 释放鼠标捕获后可拖动窗口"
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
        desc="下次启用控制时使用已安装并运行的 WDA 通过 USB 发送绝对坐标；未就绪时自动回退 BLE，不改变 AirPlay 画面链路"
      >
        <Switch
          checked={prefs.iosUsbControlEnabled}
          onCheckedChange={(v) => setPrefs({ iosUsbControlEnabled: v })}
        />
      </Row>

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
