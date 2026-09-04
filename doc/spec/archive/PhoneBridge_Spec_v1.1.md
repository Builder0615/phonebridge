# PhoneBridge Spec v1.1（多设备 · 多宿主）

> 状态：Draft
> 版本：1.1（代替 v1.0：范围扩展至 Android 设备与 macOS 宿主）
> 日期：2026-09-02
> 宿主平台：Windows 10 2004（build 19041）+ / Windows 11、macOS 13+
> 设备平台：iPhone（AirPlay + BLE HID）、Android（scrcpy/ADB）
> 桌面技术栈：Tauri 2 + Rust + React + TypeScript + Vite
> Design system：shadcn/ui + Tailwind CSS
> 语言：中文

## 1. 产品定义

PhoneBridge 是跨宿主（Windows/macOS）的桌面应用：把 iPhone 与 Android 的屏幕镜像到
统一画布，并让电脑向设备发送鼠标、键盘与文字输入。不同设备走不同链路：

```text
                    iPhone ──────────────────┐
              ┌──────┴──────┐                │
         AirPlay 镜像     BLE HID            │
              │             │                │
              ↓             ↑                ↓
        UxPlay 接收器   Windows/macOS     Android 设备
        (GStreamer /    BLE Peripheral   (scrcpy/ADB)
         VideoToolbox)   HOGP             ──┬──────┘
              │             │               │
              └──────┬──────┼───────────────┤
                     ↓      ↓               ↓
              PhoneBridge UI（Windows 10/11 · macOS 13+）
```

四条产品链路：

| 宿主 | 设备 | 镜像 | 控制 |
|---|---|---|---|
| Windows | iPhone | AirPlay → UxPlay（GStreamer） | HOGP BLE HID（WinRT `GattServiceProvider`） |
| macOS | iPhone | AirPlay → UxPlay（VideoToolbox/OpenGL） | HOGP BLE HID（CoreBluetooth `CBPeripheralManager`） |
| Windows | Android | ADB + scrcpy server 视频流 | scrcpy 输入通道 / ADB input |
| macOS | Android | ADB + scrcpy server 视频流 | scrcpy 输入通道 / ADB input |

- 粘贴：iOS 走 BLE HID 键盘事件；Android 走 ADB `input text`（ASCII 子集）或
  scrcpy 剪贴板通道（需验证）；均非系统级剪贴板双向同步。
- Android 链路以 [Genymobile/scrcpy](https://github.com/Genymobile/scrcpy)
  （Apache-2.0，Mozilla Public License 2.0 核心）为参考实现；
  ADB 仅用于结构化命令，不把命令行文本输出当作稳定业务接口。

### 1.1 MVP 结论

- 本版（v1.1）把「Android 镜像 + 控制」与「macOS 宿主支持」纳入产品范围，
  但真实设备链路仍受 M0 验证门约束（见 §11）：未实测的组合不标记为“支持”。
- 帧桥接决策（UxPlay/GStreamer 与 scrcpy 视频流 → Tauri 画布）是 M0/M1 的关键
  技术门；本版先交付适配器骨架、状态机、输入映射、诊断与 UI，真实帧桥接按验证
  结果落地（native overlay 或边解码通道），不在前端伪造帧。

### 1.2 体验承诺与明确限制

| 项目 | 承诺 | 限制或前置条件 |
|---|---|---|
| iPhone → Windows/macOS 投屏 | 同一局域网原生屏幕镜像 | 受 iOS 版本、网络、UxPlay 兼容性与宿主视频栈影响 |
| Windows/macOS 控制 iPhone | 移动、单击、拖拽、滚轮、常用键 | iPhone 需启用 AssistiveTouch（指针）；BLE 需宿主支持 LE Peripheral |
| Android → Windows/macOS 投屏 | 通过 ADB + scrcpy 镜像屏幕 | 需 USB 调试（或同一 Wi-Fi + `adb pair`）；scrcpy 版本固定 |
| Windows/macOS 控制 Android | 触摸/点击/拖拽/滚轮/按键 | Android 无需辅助功能；输入走 scrcpy/ADB 通道 |
| 文字粘贴 | 画布内 `Ctrl+V` 与按钮 | iOS=模拟键盘；Android=ADB text；不承诺读/写系统剪贴板 |
| Apple Account / Google Account | 均不要求 | 不使用私有认证链路 |
| USB 越狱方案（ioscpy） | 不纳入 | 仅研究参考，不表述为无越狱支持 |

### 1.3 非目标（v1.1 保持）

- 不逆向或复制商业 Wormhole 的专有实现、品牌或界面资源。
- 不绕过 iOS/Android 锁屏、Biometric、DRM、企业管理策略或用户授权。
- 不实现真实的双向系统剪贴板同步（MVP 只做显式文字粘贴）。
- 不承诺完整多点触控、系统级专用命令、部分受保护内容渲染。
- 不提交未经许可的第三方二进制、证书或密钥。

## 2. 用户流程

### 2.1 首次连接（iPhone）

1. 启动 PhoneBridge，运行本地诊断（宿主版本、网络、mDNS、BLE Peripheral、依赖）。
2. iPhone 打开「控制中心 → 屏幕镜像」，选择 PhoneBridge 接收器。
3. 点击「启用控制」，宿主短时开启 BLE HID 广播；在 iPhone 蓝牙中选择并配对。
4. 按引导开启 AssistiveTouch；应用发送测试移动/单击事件。

### 2.2 首次连接（Android）

1. 启动 PhoneBridge，运行本地诊断（ADB、scrcpy、设备授权）。
2. 手机开启「开发者选项 → USB 调试」并连接（USB 或 `adb pair` Wi-Fi）。
3. 应用执行 `adb devices` 确认设备已授权（不读取任何设备内容）。
4. 选择设备后启动会话；scrcpy 服务端在设备侧运行并提供视频流与输入通道。

### 2.3 日常使用与异常恢复

- 鼠标在画布内移动 → 坐标映射 → iOS 相对 HID 报告 / Android 绝对触摸坐标。
- 键盘仅在画布有焦点时转发；`Ctrl+V`/工具栏粘贴触发文字发送。
- 异常恢复表沿用 v1.0 语义并补充：ADB 断开（显示「设备已断开」，释放全部输入）、
  scrcpy 崩溃（显示「文件传输/镜像服务已停止」，一键重启）、BLE 断开（画面仍在）。
- 「停止控制」先释放全部按键/按钮再关闭广播；「断开」按固定顺序回收子进程。

## 3. 功能需求

优先级：P0 为 MVP 必须，P1 为首个稳定版本，P2 为后续扩展。

### 3.1 连接与镜像

| ID | 优先级 | 需求 | 验收标准 |
|---|---|---:|---|---|
| FR-CON-001 | P0 | 发现/选择 iPhone（AirPlay） | iPhone 屏幕镜像列表见到 PhoneBridge；失败时诊断指出 mDNS/防火墙/网络原因 |
| FR-CON-002 | P0 | 建立 iPhone 镜像会话 | 握手后显示第一帧；不要求同一 Apple Account |
| FR-CON-003 | P0 | 会话状态 | UI 显示发现/连接中/已连接/重连中/失败/已停止六种状态（含控制子状态） |
| FR-CON-004 | P0 | 安全接入 | AirPlay PIN/密码或等效确认；默认不接受任意局域网客户端长期连接 |
| FR-CON-005 | P0 | 断开清理 | 停止或退出后子进程、端口、渲染管线、临时文件被回收 |
| FR-CON-006 | P0 | Android 设备发现 | `adb devices` 列出已授权设备，未授权的设备给出授权引导（不读取设备内容） |
| FR-CON-007 | P0 | Android 镜像会话 | scrcpy server 在设备侧启动并回传视频流；宿主无对应解码/桥接时返回明确错误 |
| FR-CON-008 | P0 | 宿主可移植性 | 同一代码库在 Windows 与 macOS 构建；宿主相关能力（BLE、防火墙）分别诊断 |
| FR-MIR-001 | P0 | 实时画面 | 画面进入统一渲染区域；参考配置 720p 稳定 30 FPS（实测后填报） |
| FR-MIR-002 | P0 | 方向与比例 | 竖/横屏、旋转、缩放；不拉伸，等比缩放 + 黑边 |
| FR-MIR-003 | P0 | 画面暂停/恢复 | 最小化或切后台不崩溃；恢复后继续渲染或明确提示会话失效 |
| FR-MIR-004 | P1 | 音频 | iPhone 音频到默认输出设备 + 静音开关；Android 音频随后续版本 |
| FR-MIR-005 | P1 | 录制 | 录制当前画面到 MP4；录制前显示路径与隐私提醒 |

### 3.2 控制（iOS：BLE HID；Android：scrcpy/ADB）

| ID | 优先级 | 需求 | 验收标准 |
|---|---|---:|---|---|
| FR-HID-001 | P0 | 仅按用户操作广播 BLE | 只有点击「启用控制」后才广播；停止后不可被新设备发现 |
| FR-HID-002 | P0 | iPhone 配对 | iPhone 识别为输入设备并完成配对；显示已配对/已连接/未订阅 |
| FR-HID-003 | P0 | 鼠标移动 | 相对 HID 报告（iOS）/绝对坐标（Android）；不越界、无缩放跳变 |
| FR-HID-004 | P0 | 单击与拖拽 | 按下/释放顺序正确；失焦、断开、退出强制释放 |
| FR-HID-005 | P0 | 键盘输入 | 字母、数字、常用标点、退格、回车、Tab、Shift/Ctrl/Alt/Command 与方向键 |
| FR-HID-006 | P0 | 滚轮 | 垂直滚动；步长可配置；断线重连后不积压旧事件 |
| FR-HID-007 | P0 | Android 触摸 | 单击/长按/拖拽转换为 scrcpy 或 `adb shell input` 触摸事件 |
| FR-HID-008 | P1 | 单活动会话 | 只允许一个活动控制会话；切换前释放旧会话全部按键/按钮 |
| FR-HID-009 | P1 | 诊断 | 展示外设能力、适配器名、连接间隔、订阅状态与最近错误（两种链路） |

### 3.3 文字粘贴

| ID | 优先级 | 需求 | 验收标准 |
|---|---|---:|---|---|
| FR-CLP-001 | P0 | 显式粘贴 | 工具栏「粘贴」把宿主纯文本剪贴板发送到设备当前输入框 |
| FR-CLP-002 | P0 | 画布快捷键 | 画布有焦点时 `Ctrl+V` 触发同一流程；其它应用不受影响 |
| FR-CLP-003 | P0 | 内容保护 | 默认不写日志、不上传、不落盘；可关闭自动读取 |
| FR-CLP-004 | P0 | 大小限制 | MVP 处理 ≤ 32 KiB 纯文本；超出提示并提供手动分段策略 |
| FR-CLP-005 | P0 | 字符反馈 | 无法表达的字符显示数量与位置（iOS 编码子集 / Android ASCII 子集） |
| FR-CLP-006 | P1 | Unicode 优化 | 验证设备键盘布局后扩展范围；Emoji/复杂输入法单独验收 |

### 3.4 诊断与设置

| ID | 优先级 | 需求 | 验收标准 |
|---|---|---:|---|---|
| FR-DIAG-001 | P0 | 一键诊断 | 宿主版本、网络、mDNS、防火墙、BLE Peripheral、ADB/scrcpy、依赖版本、最近错误 |
| FR-DIAG-002 | P0 | 可导出诊断包 | 只含脱敏日志与环境信息；不含画面、音频、剪贴板内容或认证材料 |
| FR-DIAG-003 | P0 | 用户控制权限 | 可分别关闭自动控制、快捷键粘贴、音频与本地录制 |
| FR-DIAG-004 | P1 | 多设备列表 | 保存用户明确配对过的设备名与非敏感标识；不保存未经确认的设备列表 |

## 4. 用户界面

### 4.1 主窗口

```text
┌──────────────────────────────────────────────┐
│ PhoneBridge                       macOS ⚙ ●   │
├────────────┬─────────────────────────────────┤
│ Devices    │                                 │
│            │       设备镜像画布（统一）        │
│ ● iPhone   │                                 │
│   Connected│                                 │
│ ○ Android  │                                 │
│   （未连接） │                                 │
│ + Add      │                                 │
├────────────┴─────────────────────────────────┤
│ iPhone · Connected · 30 FPS · AirPlay · BLE   │
│  Mouse  Keyboard  Paste  Fullscreen           │
└──────────────────────────────────────────────┘
```

### 4.2 必须可见的状态

- 画面连接状态与最近错误（含链路：AirPlay / scrcpy）。
- 控制状态：未启用、广播中、等待配对、已连接、输入可用、输入已暂停。
- FPS/分辨率/估算延迟；不可用时显示“未知”，不伪造数值。
- AssistiveTouch 提示：仅 iOS 控制会话首次配对成功或检测到无指针时显示。
- Android USB 调试授权状态与设备序列号（脱敏尾部）。
- 当前应用是否拥有画布焦点。

### 4.3 快捷键

| 快捷键 | 行为 | 范围 |
|---|---|---|
| `Ctrl+V` | 读取宿主纯文本剪贴板并发送 | 仅画布有焦点时 |
| `Esc` | 退出全屏或停止当前拖拽 | 应用内 |
| `Ctrl+Alt+Pause` | 暂停/恢复输入转发 | 应用内，可禁用 |
| `Ctrl+Alt+Q` | 释放全部按键/鼠标按钮 | 应用内紧急操作 |

### 4.4 Design system

同 v1.0 §4.4：shadcn/ui（`new-york`、Radix、CSS variables、zinc 基色 + 单一品牌色），
默认深色保留浅色；交互控件复用 `src/components/ui/`；危险操作使用 AlertDialog；
加载/错误/空状态使用 Skeleton/Alert/Empty；不散落任意 hex；键盘可达、焦点可见、
屏幕阅读器标签、高对比度/缩放支持；颜色不是唯一状态表达。

## 5. 技术架构

### 5.1 推荐实现

- 桌面壳：Tauri 2.x；宿主目标 Windows 10 2004+ / macOS 13+（x64/ARM64）。
- 前端：React + TypeScript + Vite；页面与交互编排在 `src/`。
- Design system：shadcn/ui + Tailwind CSS，组件源码在 `src/components/ui/`。
- 原生后端：Rust/Tauri commands 负责 `SessionManager`、子进程、BLE、ADB/scrcpy、
  剪贴板、诊断与资源生命周期。
- iOS 镜像：UxPlay 独立适配器（Windows 用 GStreamer 渲染，macOS 用 VideoToolbox/
  OpenGL）；业务层不重写 AirPlay/FairPlay。
- Android 镜像：以 scrcpy（Apache-2.0）为参考；镜像/输入适配器放在
  `MirrorAdapter`/`InputController` 之后，ADB 只做结构化调用。
- 控制输入：
  - iOS：Windows 上 WinRT `GattServiceProvider`，macOS 上 CoreBluetooth
    `CBPeripheralManager`；两者都是 LE Peripheral HOGP，M0 需真机验证。
  - Android：scrcpy 输入通道或 `adb shell input`（固定参数，白名单）。
- 帧桥接：UxPlay/GStreamer 与 scrcpy 视频流到统一画布的低延迟路径是 M0 决策门；
  优先受控 native frame bridge 或 Tauri channel，不假设第三方窗口可嵌入 React DOM。
- 日志：结构化本地日志；默认不记录剪贴板、屏幕内容、键盘原文与配对材料。

### 5.2 数据流

```mermaid
flowchart LR
    iPhone -->|AirPlay/mDNS| UxPlay[UxPlay Adapter]
    Android -->|ADB + scrcpy server| Scrcpy[Scrcpy Adapter]
    UxPlay -->|视频/音频| Decode[宿主解码]
    Scrcpy -->|视频流| Decode
    Decode --> Canvas[统一画布]
    Canvas --> App[SessionManager]
    Mouse[宿主鼠标/键盘] --> Mapper[坐标与按键映射]
    Mapper -->|iOS| Ble[BleHid Controller]
    Mapper -->|Android| AdbIn[AdbInput Controller]
    Ble --> iPhone
    AdbIn --> Android
    Clip[宿主剪贴板] --> App --> Ble / AdbIn
    App --> UxPlay / Scrcpy
```

### 5.3 模块边界

| 模块 | 实现位置 | 职责 | 不负责的内容 |
|---|---|---|---|
| `AppShell` | React + shadcn/ui | 窗口、导航、设备选择、状态、权限提示、设置 | 协议细节 |
| `SessionManager` | Rust/Tauri | 会话状态机、启停、重连、资源回收 | 解析视频包 |
| `MirrorAdapter` | Rust + UxPlay/scrcpy | 按设备类型启动镜像、接收元数据/事件 | 键鼠映射 |
| `RenderSurface` | React canvas/native bridge | 画面显示、比例、旋转、帧率统计 | 连接设备或配对 |
| `HidAdapter` | Rust/WinRT 或 CoreBluetooth | iOS BLE 广播、配对、键鼠报告 | 画布坐标计算 |
| `InputController` | Rust + ADB/scrcpy | Android 触摸/按键/文字、状态 | BLE 连接管理 |
| `InputMapper` | TypeScript/Rust 纯模块 | 画布坐标 → 设备坐标、按钮/滚轮/按键转换 | 连接管理 |
| `ClipboardAdapter` | Rust/Tauri | 读纯文本、限长、编码能力检测、发送队列 | 读设备剪贴板 |
| `Diagnostics` | Rust + React view | 环境检查、脱敏日志、导出报告 | 悄悄改系统设置 |
| `SecurityPolicy` | Rust/Tauri capabilities | PIN、允许设备、广播窗口、隐私开关 | 绕过系统权限 |

### 5.4 接口草案

```typescript
export type DeviceKind = "iphone" | "android"

export interface BackendApi {
  checkCapabilities(): Promise<CapabilityReport>
  startSession(options: { device: DeviceKind; prefs: SessionPreferences }): Promise<void>
  stopSession(): Promise<void>
  startControl(prefs: SessionPreferences): Promise<void>
  stopControl(): Promise<void>
  releaseAllInput(): Promise<void>
  pastePlainText(prefs: Pick<SessionPreferences, "maxPasteBytes">): Promise<PasteResult>
}
```

事件：`session://state`、`mirror://metadata`、`hid://status`、`diagnostic://error`；
视频帧不得默认走高频 JSON/base64 事件，帧桥接方式由 M0 验证决定。

### 5.5 会话状态机

与 v1.0 §5.5 一致（Idle ⇄ Checking ⇄ Ready ⇄ Mirroring ⇄ MirroringConnected ⇄
ControlPairing ⇄ ControlReady ⇄ Reconnecting ⇄ Stopping ⇄ Failed），
同一状态机服务两种设备链路；`Mirroring*` 细分的镜像子状态标记链路来源（AirPlay/scrcpy）。

## 6. 关键实现规则

### 6.1 画面与坐标映射（两链路通用）

1. 渲染区域记录内容矩形 `contentRect`，黑边排除在可点击区外。
2. `scale = min(contentW/deviceW, contentH/deviceH)`。
3. 屏幕点 → 减原点 → 除缩放 → clamp 到设备分辨率；旋转/换源清空 hover/drag。
4. iOS 鼠标为相对 HID 报告（≤ int8，拆分）；Android 为绝对坐标触摸（scrcpy 输入）。
5. 移动事件有界队列可合并；按下/释放不可丢弃或重排。
6. Android 设备分辨率来自 `adb shell wm size`（结构化查询），不假设硬编码。

### 6.2 键盘与粘贴

- 显式物理键 → HID usage 表（iOS）；Android 按键走 `keyevent` 映射表。
- KeyDown/KeyUp 必须成对；失焦/暂停/异常/断开执行 `ReleaseAll`。
- 粘贴只读纯文本；`Ctrl+V` 仅画布有焦点时拦截。
- 文本编码器先处理安全子集，再处理可验证字符；不可表达必须返回失败位置。
- 粘贴队列可取消；停止控制后不续发旧文本。

### 6.3 子进程与资源回收

- UxPlay / scrcpy / adb 以受控子进程启动；启动参数由应用白名单生成，不拼接用户输入。
- 停止顺序：暂停输入 → 释放按键/按钮 → 关闭广播/输入通道 → 停止镜像 → 关闭渲染器
  → 删除临时文件。
- 子进程异常退出不自动重启、不继续发送输入。
- 启动前检查端口（UxPlay 6000 段）与依赖（adb/scrcpy 二进制），避免重复占用。

## 7. 平台、依赖与许可证

### 7.1 基线依赖

| 依赖 | 用途 | 基线 | 许可证/状态 | 集成策略 |
|---|---|---|---|---|
| Tauri 2 | 桌面壳、commands、权限、sidecar | 2.x；版本入 Cargo.lock/发布清单 | Rust 桌面框架 | capabilities + CSP 最小权限 |
| React/Vite/TS | 前端 | Node LTS；版本入 lock | 前端运行时/构建 | UI 只调 Tauri API |
| shadcn/ui | Design system | new-york、Radix、Tailwind | 源码纳入；primitives 清单化 | CLI 添加至 `src/components/ui/` |
| UxPlay | iPhone 镜像接收 | Windows/macOS 构建；版本随发布固定 | GPLv3 | `MirrorAdapter` 隔离；发布前完成许可审查 |
| GStreamer | UxPlay 解码渲染（Windows） | 与 UxPlay 构建匹配 | 多组件许可证 | SBOM + 第三方声明 |
| VideoToolbox/OpenGL | UxPlay 渲染（macOS） | 系统框架 | 系统 API | 记录版本；不自行分发 |
| scrcpy | Android 镜像 + 输入参考 | 由发布清单固定版本/提交 | Apache-2.0（含部分 MPL-2.0/BSD 组件） | 独立适配器；发布前记录版本、SHA-256 与许可证清单 |
| ADB（Android SDK Platform-Tools） | 设备发现与结构化命令 | Google 发布版 | Apache-2.0 | 仅结构化调用；不依赖 CLI 文本作为稳定接口 |
| windows-ble-hid | iOS HOGP 参考 | Win10 2004+；需 LE Peripheral | MIT（working spike） | 参考 WinRT 行为，真机验证后落地 |
| CoreBluetooth | iOS HOGP（macOS） | macOS 13+ | 系统框架 | 真机验证后落地 |
| Bonjour/mDNS | AirPlay 发现 | 依 UxPlay 构建 | 分发条款单独审查 | 先走已验证路径 |
| AirPlayPC | 诊断参考 | 参考安装/防火墙/AssistiveTouch | MIT | 只吸收诊断思路 |
| ioscpy | USB 越狱研究 | 仅越狱 iPhone | MIT | 不进 MVP |

### 7.2 许可证门槛

1. UxPlay（GPLv3）、scrcpy（Apache-2.0 为主）、GStreamer、Bonjour 的分发义务
   分别记录：版本、提交号、下载地址、SHA-256、许可证、已知兼容性与分发结论。
2. 不复制第三方代码/图标/脚本/测试数据，除非许可证、版权与目标发行模式已登记。
3. 每次升级外部依赖按上表记录；二进制（uxplay/scrcpy/adb）仅放入
   `src-tauri/binaries/` 且完成审计与校验，缺失时应用返回可理解错误而非隐式成功。

## 8. 安全与隐私

### 8.1 威胁模型

| 资产 | 威胁 | 控制措施 |
|---|---|---|
| iPhone 画面/音频 | 局域网窥视 | 同一广播域、PIN/允许列表、明确开始/停止 |
| Android 画面 | USB/Wi-Fi 通道被窥视 | 仅用户选择设备后启动 scrcpy；不读取设备文件 |
| BLE 控制权 | 配对窗口抢占 | 仅按操作开启广播、缩短可配对窗口、配对后关闭可发现性 |
| ADB 授权 | 未授权设备注入 | `adb devices` 仅列出已授权；引导提示不代替授权 |
| 宿主剪贴板 | 敏感文字外泄 | 仅用户触发读取；不落盘/上传/写日志；可关闭快捷键 |
| 键盘事件 | 粘滞或错发设备 | 单一活动会话、焦点门控、断开 Release All、紧急释放 |
| 第三方二进制 | 被替换 | 固定版本 + SHA-256、签名、启动前校验 |
| 诊断包 | 标识信息泄露 | 脱敏 IP/MAC/序列号尾部；导出前清单确认 |

### 8.2 隐私原则

- 无云端账户/远程服务器；不上传帧、音频、键盘原文、剪贴板文本或配对数据。
- 日志记录事件类型与错误码；临时文件入应用专属目录，退出后删除。
- Android 只进行结构化 ADB 调用（设备列表、wm size、input），不申请文件读写权限。
- Tauri capabilities 最小 scope；不开放任意 shell/filesystem；CSP 明确。

## 9. 性能与质量目标

| 指标 | 目标 | 测量方式 |
|---|---|---:|---|
| 首帧时间 | P95 ≤ 5 秒（实测后填报） | 确认镜像到第一帧 |
| 镜像帧率 | 参考配置 720p ≥ 30 FPS | 5 分钟稳定会话统计 |
| 输入延迟 | P95 ≤ 150 ms | 屏幕测试目标 + 时间戳 |
| 重连时间 | P95 ≤ 15 秒 | 注入短断统计恢复 |
| 资源占用 | 应用自身 ≤ 300 MB；CPU ≤ 30% | 5 分钟窗口平均 |
| 稳定性 | 8 小时会话无崩溃、无持续内存增长 | 参考组合 |
| 键鼠安全 | 100% 用例无 sticky | 断开/失焦/异常/停止/重连全路径 |

## 10. 测试与验收

### 10.1 测试层级

| 层级 | 内容 | 必须覆盖 |
|---|---|---|
| 单元测试 | 坐标映射、旋转、HID 报告、ADB 参数构造、键盘映射、文本限长、状态机 | 边界值、空输入、超长、负坐标、断链 |
| 组件测试 | MirrorAdapter（UxPlay/scrcpy）、HidAdapter、InputController、ClipboardAdapter 契约 | 缺依赖、子进程退出、订阅变化、超时 |
| 集成测试 | 真实 UxPlay、BLE 适配器、ADB/scrcpy、真实设备 | 发现、镜像、配对、点击、键盘、粘贴、停止 |
| 回归测试 | Windows 10/11、macOS 13+、x64/ARM64、不同蓝牙芯片 | 版本与驱动变化 |
| 安全测试 | 防火墙、配对窗口、日志与诊断包 | 无明文剪贴板/键盘/认证材料泄露 |

### 10.2 MVP 验收用例

| ID | 步骤摘要 | 通过条件 |
|---|---|---|
| AT-001 | Windows + iPhone 同一 Wi-Fi，打开屏幕镜像 | PhoneBridge 出现在列表并显示第一帧 |
| AT-002 | 阻断 mDNS 后运行诊断 | 诊断准确指出服务发现失败 |
| AT-003 | 启用控制（Peripheral 适配器） | iPhone 配对 HID，应用显示控制可用 |
| AT-004 | 未开启 AssistiveTouch 时移动鼠标 | 显示引导；开启后指针跟随 |
| AT-005 | 点击、拖拽、滚轮、快速移动 | 事件顺序正确，无跳点/stuck |
| AT-006 | iPhone 文本框输入英文/数字/标点 | 字符一致，KeyDown/KeyUp 成对 |
| AT-007 | 复制 1 KiB 纯文本按 `Ctrl+V` | iPhone 输入框出现相同文本 |
| AT-008 | 粘贴超 32 KiB 或含不可编码字符 | 明确提示，不静默截断 |
| AT-009 | 失焦/断网/拔适配器 | 立即暂停并释放全部键鼠状态 |
| AT-010 | 停止会话后重连 | 旧进程与广播已清理 |
| AT-011 | 导出诊断包检查 | 不含画面/音频/剪贴板/键盘/认证材料 |
| AT-012 | 受保护视频内容 | 稳定且显示范围限制，不绕过 DRM |
| AT-013 | macOS + iPhone 同网镜像并控制 | macOS 宿主完成镜像与 BLE 控制（CoreBluetooth 验证） |
| AT-014 | Windows/macOS + Android（USB 调试） | scrcpy 会话建立、画面与触摸可用（链路一） |
| AT-015 | Android 断开/重连 | 输入释放、诊断提示、重连成功 |
| AT-016 | 跨宿主导出诊断包 | 两份报告平台字段正确且脱敏 |

### 10.3 兼容性矩阵（首版至少记录）

| 维度 | 最低覆盖 |
|---|---|
| 宿主 | Windows 10 2004/22H2、Windows 11、macOS 13/14/15；x64，ARM64 增强项 |
| 设备 | iPhone 至少两代、Android 至少三个厂商/版本 |
| 链路 | iOS：AirPlay+BLE；Android：scrcpy 视频 + ADB 输入 |
| 蓝牙 | 内置 Intel、USB 适配器、不支持 Peripheral 的适配器 |
| 网络 | 家庭路由、企业隔离 Wi-Fi、无 mDNS、防火墙开启 |
| 状态 | 首次配对、已配对重连、应用重启、睡眠唤醒、设备锁屏/解锁 |

## 11. 交付阶段

### 11.1 M0：可行性 Spike（范围扩展后）

- 目标蓝牙适配器/主机报告 `Peripheral=True`（Windows 与 macOS 各一次）。
- iPhone 配对 BLE HID 有效；AssistiveTouch 后指针有效。
- UxPlay 在 Windows 与 macOS 分别连续镜像 10 分钟。
- scrcpy 在 Windows 与 macOS 分别建立视频流与输入通道（记录版本与设备）。
- Tauri + React 最小窗口；验证 UxPlay/GStreamer/scrcpy 帧低延迟进入 React 渲染面；
  不能则形成 native overlay/frame bridge 决策。
- shadcn/ui 五个基础组件与主题 token 验证；画布坐标离线测试。
- 记录 UxPlay、GStreamer、scrcpy、ADB、Windows、macOS、iOS、Android、蓝牙版本。

### 11.2 MVP：单设备可用（v1.1 范围）

- 主窗口、设备列表（iPhone/Android）、状态、诊断页；shadcn/ui 主题。
- iPhone 镜像 + BLE 键鼠 + 粘贴（Windows/macOS 宿主）。
- Android 镜像会话 + 触摸/按键/文字（Windows/macOS 宿主）。
- 启停、错误恢复、脱敏日志、依赖清单；完成 AT-001 至 AT-016 并填报兼容性矩阵。

### 11.3 V1：稳定性与内容能力

- 音频、录制、全屏、更多键盘布局；已配对设备管理；自动重连。
- Android 输入通道全面验证；scrcpy 版本升级流程。
- 更新器、代码签名、SBOM、崩溃报告本地脱敏导出。

### 11.4 V2：统一移动设备工作台

- 3D 手机外壳、多屏、录制模板；文件传输与真正双向剪贴板需独立协议与隐私评审。
- iOS USB 仅在合法无越狱可分发的实现被验证后重新评估。

## 12. 方案依据与决策路径

| Evidence | Finding | Path |
|---|---|---|
| [UxPlay](https://github.com/FDH2/UxPlay)（GPLv3，Win/macOS 支持，GStreamer/VideoToolbox） | 承担 iPhone 镜像接收；版本/发现/许可义务固定 | `MirrorAdapter` 隔离；发布前许可证审查 |
| [scrcpy](https://github.com/Genymobile/scrcpy)（Apache-2.0，官方支持 Win/macOS 客户端、ADB 输入通道） | Android 镜像与输入的成熟参考，跨宿主可用 | `MirrorAdapter`/`InputController` 隔离；版本与 SHA-256 记录 |
| [windows-ble-hid](https://github.com/abhishek-raj/windows-ble-hid)（MIT，LE Peripheral + iPhone 验证） | Windows BLE HID 取决于 Peripheral 角色 | M0 能力探测；控制链路独立于 AirPlay |
| CoreBluetooth `CBPeripheralManager`（系统框架） | macOS 可做 LE Peripheral；需真机验证 | macOS HID 适配器单独验证并记录 |
| [AirPlayPC](https://github.com/gbulog/pcairplay)（MIT） | 投屏+HID≠官方镜像；手机需解锁 | 文案/验收避免过度承诺 |
| [Tauri](https://v2.tauri.app/start/) | 多桌面壳 + Rust 后端 + invoke/capabilities | commands/events 边界；最小权限 |
| [shadcn/ui](https://ui.shadcn.com/docs/installation/vite) | Vite + Tailwind + 源码组件 | `src/components/ui/` + token + CLI 流程 |

## 13. 开放决策门

1. 固定 UxPlay 版本、Windows/macOS 构建来源、GStreamer 打包与 mDNS 路径。
2. PhoneBridge 整体许可证与 UxPlay GPLv3、scrcpy Apache-2.0 的分发边界。
3. 是否分发 Bonjour；无 Bonjour 网络是否启用内部 mDNS。
4. iOS HOGP 是改造 MIT 项目、独立重写还是外部进程集成（Windows/macOS 各一）。
5. Android 帧桥接方案：native overlay / scrcpy 视频通道解码 / 其他（M0 决策）。
6. 固定最小 iOS/Android 版本、首批蓝牙适配器与宿主版本，实测写入兼容矩阵。

## 14. 参考资料

- [Genymobile/scrcpy](https://github.com/Genymobile/scrcpy)：Android 镜像/输入参考实现（Apache-2.0）。
- [FDH2/UxPlay](https://github.com/FDH2/UxPlay)：AirPlay 镜像接收器（GPLv3）。
- [abhishek-raj/windows-ble-hid](https://github.com/abhishek-raj/windows-ble-hid)：Windows HOGP 参考（MIT）。
- [gbulog/pcairplay](https://github.com/gbulog/pcairplay)：Windows/Arch 集成与诊断参考（MIT）。
- [lautarovculic/ioscpy](https://github.com/lautarovculic/ioscpy)：越狱 iPhone USB 研究（MIT，不进入 MVP）。
- [Tauri](https://v2.tauri.app/start/)：桌面壳与 Rust/前端 IPC 文档。
- [shadcn/ui Vite](https://ui.shadcn.com/docs/installation/vite)：Vite 安装与组件体系。

> 历史：v1.0（Windows → iPhone 单链）归档为
> `doc/spec/archive/PhoneBridge_Windows_iPhone_Spec_v1.0.md`。