# 快投屏 Spec v1.2（多设备 · 控制面板 + 模拟器窗口）

> 状态：Draft
> 版本：1.2（代替 v1.1：新增多设备并发、控制面板主窗口、独立模拟器窗口形态）
> 日期：2026-09-02
> 宿主平台：Windows 10 2004（build 19041）+ / Windows 11、macOS 13+
> 设备平台：iPhone（AirPlay + BLE HID；可选 USB + WDA 绝对坐标控制）、Android（scrcpy/ADB）
> 桌面技术栈：Tauri 2 + Rust + React + TypeScript + Vite
> Design system：shadcn/ui + Tailwind CSS
> 语言：中文

## 1. 产品定义

快投屏是跨宿主桌面应用：把 iPhone 与 Android 的屏幕镜像到独立的
「模拟器窗口」，并在控制面板中集中完成所有操作（连接、状态、诊断、设置、
当前设备切换）。

### 1.1 窗口形态与多设备（v1.2 核心）

```text
┌──────────────────────────────┐     ┌──────────────────────┐
│  控制面板（主窗口）            │     │ 模拟器窗口（设备 A）    │
│  ─────────────────────────── │     │ ┌──────────────────┐ │
│  设备列表  ● iPhone A(激活)    │     │ │  iPhone 画面/输入   │ │
│            ○ iPhone B         │     │ │   (仅当前激活可输入) │ │
│            ○ Android C        │ ──▶ │ └──────────────────┘ │
│  连接/断开/激活/诊断/设置       │     └──────────────────────┘
│  （所有操作集中于此）            │        （每台已连接设备各有一个，
└──────────────────────────────┘          多台可同时显示）
```

- **控制面板 = 主窗口**：所有操作（连接/断开、激活切换、诊断、设置、粘贴等）
  集中在一个面板上完成。
- **模拟器窗口 = 独立窗口**：每台**已连接**的设备各有一个模拟器窗口，
  像模拟器一样显示该设备的手机页面；多个设备的模拟器窗口可以同时显示，
  窗口之间互不隐藏，保留各自会话与画面。
- **多设备同时连接**：iPhone 与 Android 可并发接入各自链路
  （UxPlay/AirPlay 每实例独立端口；scrcpy-server 每设备独立实例）。
- **输入只跟随激活设备**：键盘/鼠标/滚轮/粘贴仅转发到当前激活设备；切换激活
  即切换输入目标，但不会隐藏其它模拟器窗口。BLE 广播受硬件限制始终保持单活动控制。

### 1.2 会话模型

- 每个**连接中的设备**是一个独立 `DeviceSession`：拥有自己的状态机、
  镜像适配器、输入控制器、帧通道、元数据与错误日志。
- `SessionRegistry`（注册表）管理多会话：连接/断开、激活切换、
  按 sessionId 路由命令与事件。
- 所有 Tauri 命令与事件带 `sessionId`（设备标识），前端按窗口归属订阅。

### 1.3 体验承诺与明确限制

| 项目 | 承诺 | 限制或前置条件 |
|---|---|---|
| 多设备并发 | iPhone/Android 可同时保持镜像会话 | 每设备独立的 UxPlay（独立端口）/scrcpy-server 实例 |
| 模拟器窗口 | 已连接设备各有一个独立窗口，可同时显示 | Tauri 多窗口；打开/激活某台时显示并聚焦目标，不隐藏其它窗口 |
| 输入焦点 | 键盘/鼠标只转发到激活设备 | BLE 单活动控制；焦点窗口语义 |
| 控制面板 | 所有操作集中在一个主窗口 | 设备列表、状态、诊断、设置、粘贴 |
| USB 自动识别 | 自动列出已连接 iPhone/Android | iPhone: xcrun devicectl/xcdevice/system_profiler；Android: adb；缺依赖返回可理解错误 |
| iPhone 镜像启动 | 面板启动 AirPlay 接收器并提示连接步骤 | iPhone 必须在控制中心“屏幕镜像”中选择快投屏；USB 仅用于识别，画面不走 USB |
| 文字粘贴 | 显式粘贴只发往当前激活设备 | iOS USB/WDA 模式走 WDA 文本输入，未启用精确模式时回退 BLE HID；Android 使用 scrcpy control text message |
| iOS 输入通道 | 默认经 BLE HID（HOGP）转发；可选 USB + WDA 绝对坐标 | BLE 兼容版不要求 WDA，仍要求宿主具备 LE Peripheral；WDA 精准版内置发布方签名的 `WebDriverAgentRunner.ipa`、`ideviceinstaller` 与 `go-ios`，用户点击准备后由应用自动安装、启动、校验 WDA，并通过回环端口发送绝对输入。显式开启 USB/WDA 精确模式但 WDA 不可用时必须报错并保持未连接，不能静默回退 BLE（避免用户误把相对鼠标当成绝对坐标）；不绕过 Apple 签名、信任、开发者模式或设备授权，不以越狱或私有框架为前提 |

其余（链路、安全、依赖、性能、测试要求）继承 v1.1 §2–§12。

## 2. 用户流程（v1.2 增量）

1. 启动后主窗口（控制面板）自动枚举 USB 设备（AirPlay 发现仍可用）。
2. 选择设备 → 「连接」：为该设备创建 `DeviceSession` 并打开对应模拟器窗口。
3. 面板显示每台设备的状态；点击设备行/「激活」聚焦目标窗口并切换输入目标。
4. 断开某台设备：结束该会话、关闭其模拟器窗口，面板状态更新。
5. 「全部断开」：逐一按 §6.3 顺序清理所有会话与窗口。

## 3. 功能需求增量（多设备 / 窗口）

| ID | 优先级 | 需求 | 验收标准 |
|---|---|---:|---|---|
| FR-MUL-001 | P0 | 多设备会话注册表 | 同时保持 ≥2 台设备会话，互不串扰；每会话独立状态机与链路 |
| FR-MUL-002 | P0 | 激活切换 | 面板切换激活设备后：目标模拟器窗口显示并聚焦、输入/粘贴转发目标改变，其它窗口保持显示 |
| FR-MUL-003 | P0 | 模拟器窗口生命周期 | 连接时创建/显示窗口；断开或退出时关闭窗口与回收资源；不残留孤儿窗口 |
| FR-MUL-004 | P0 | 命令/事件按 sessionId 路由 | 所有会话类命令与 `session://state` 等事件带 sessionId；前端按窗口归属消费 |
| FR-MUL-005 | P0 | 控制面板集中操作 | 连接/断开、激活、诊断、设置、粘贴、全部断开均可在主窗口完成 |
| FR-MUL-006 | P0 | USB 自动识别 | 面板自动/手动刷新已连接 iPhone/Android（脱敏展示，缺依赖清晰报错） |
| FR-MUL-007 | P1 | 并发上限保护 | 超过资源上限（如端口段/窗口数上限）时明确提示，不静默降级 |
| FR-MUL-008 | P1 | 崩溃隔离 | 单设备 UxPlay/scrcpy 崩溃只影响该设备会话，不影响其它会话与面板 |

## 4. 用户界面（v1.2 更新）

### 4.1 控制面板（主窗口）

```text
┌──────────────────────────────────────────────┐
│ 快投屏                         macOS ⚙ ◎     │
├──────────────────────────────┬───────────────┤
│ 设备（已连接/可连接）          │  面板操作       │
│ ● iPhone A  [激活] [断开]     │  全部断开       │
│ ● Android C [激活] [断开]     │  刷新 USB      │
│ ○ iPhone B（未连接）          │  粘贴→激活设备  │
│                              │  诊断/设置 Tabs │
├──────────────────────────────┴───────────────┤
│ 激活：iPhone A · 30 FPS · 控制已连接         │
└──────────────────────────────────────────────┘
```

### 4.2 模拟器窗口（每台已连接设备一个）

```text
┌────────────────────┐
│                    │
│     手机画面        │
│   （可输入）         │
│                    │
└────────────────────┘
```

- 模拟器窗口使用系统原生标题栏以便直接拖动窗口；标题栏不增加快投屏自定义
  按钮，内容区只显示设备画面。标题栏支持关闭但禁用最大化，关闭时复用主窗口的会话
  清理流程。
- 画布：Android 将画布内容区坐标映射为 scrcpy 绝对触摸坐标；iOS HOGP 是相对鼠标，
  按桌面指针的实际 CSS 位移发送 HID 报告，不把缩放后的视频像素当作 HID 单位；支持
  Pointer Lock 时优先使用 `movementX/movementY`，并应用可调的 iOS 鼠标速度倍率，
  失焦或按 Escape 时解除捕获。iOS 系统的指针速度/加速度仍由手机控制，公开 HOGP
  鼠标接口不能保证把鼠标“瞬移”到画布中的绝对点；`Ctrl+V` 或系统粘贴事件均粘贴到
  当前激活设备。BLE 移动事件必须使用单一有序队列：相对移动累加、绝对移动只保留
  最新点，最多一个移动请求在途，避免视频/蓝牙短暂卡顿后继续发送过期坐标。启用
  USB/WDA 精确模式时不使用 Pointer Lock，画布点按/拖动映射为 WDA 逻辑屏幕的
  绝对坐标；画面仍由 AirPlay 提供，WDA 只负责输入。
- 主窗口的「取消投屏」和模拟器标题栏关闭都会停止会话、回收镜像/输入资源并关闭窗口。

### 4.3 快捷键

| 快捷键 | 行为 | 范围 |
|---|---|---|
| `Ctrl+V` | 粘贴到当前激活设备 | 模拟器窗口有焦点时 |
| `Ctrl+Alt+Q` | 释放全部按键/鼠标按钮 | 应用内（面板与模拟器窗口均可） |
| `Ctrl+Alt+Pause` | 暂停/恢复输入转发 | 应用内，可禁用 |

## 5. 技术架构（v1.2 增量）

### 5.1 双窗口

- 主窗口 `main`：控制面板；始终可见。
  默认尺寸为 `520×560`，最小宽度为 `440`；标题栏以下的设备列表与日志区各占 50%。
  日志内容在日志区内部滚动，不能带动整个主窗口滚动。
- 模拟器窗口 `simulator-<hex(sessionId)>`：每台已连接设备一个；
  `WebviewWindowBuilder` 创建，加载独立的 `simulator.html` 入口，只渲染设备画布，
  不加载控制面板、设备列表或日志；会话 id 由 URL/窗口 label 传入并由 Rust 兜底确认。
- 模拟器窗口使用系统原生装饰以支持拖动和关闭，禁用最大化；关闭请求复用
  `stop_session(sessionId)`，由统一清理路径回收资源后销毁窗口。
- macOS 的 UxPlay 进程封装在应用资源内的 `uxplay-agent.app` 中，并声明
  `LSUIElement` 后台代理属性；它可以使用 GStreamer 的 macOS application wrapper，
  但不得在程序坞注册第二个前台应用图标。模拟器窗口的 `CloseRequested` 由 Rust
  原生窗口事件处理，清理失败也不得让关闭按钮永久失效。
- 激活切换：`activate_device(session_id)` → 确保目标窗口 `show()` 并聚焦，
  不隐藏其它模拟器窗口；输入/粘贴目标仍按当前激活会话路由。

### 5.2 会话注册表

```text
SessionRegistry
├── DeviceSession (iPhone A)    状态机＋镜像(UxPlay:6000)＋BLE＋帧
├── DeviceSession (Android C)   状态机＋镜像(scrcpy/serial)＋ADB＋帧
└── active: sessionId           激活会话（输入/粘贴/聚焦）
```

- 事件：`session://state`、`mirror://metadata`、`hid://status`、
  `diagnostic://error` 均携带 `sessionId`；帧通道 per-session
  （`attach_frame_channel(sessionId, channel)`）。
- 命令：`start_session(device, sessionId?)`、`stop_session(sessionId)`、
  `activate_device(sessionId)`、`attach_frame_channel(sessionId, channel)`、
  `list_sessions`、`list_usb_devices`、`open_simulator(sessionId)` 等。

### 5.3 并发链路

- AirPlay：每台 iPhone 一个 UxPlay 接收器实例；`-p n` 会占用 TCP `n/n+1` 与
  UDP `n/n+1/n+2`，因此实例从 6000 开始按 3 递增分配不重叠端口段；业务层不实现
  AirPlay/FairPlay 协议。
- iOS 视频参考 UxPlay 的 headless 输出模式：UxPlay 负责 AirPlay 会话、解密和
  H264/H265 接收，但不得创建用户可见窗口。macOS 使用
  `-avdec -vc ... -vs "fdsink fd=3" -nc no`
  让 UxPlay 内部 GStreamer 管线直接完成 RTP 解包、H264/H265 解码和 RGBA 转换，
  再通过额外文件描述符交给 Rust；`-vc` 的宽高 caps 必须放在 `videoscale`
  之后（`videoconvert` 不能缩放，把尺寸 caps 放它前面会导致首帧 caps 协商
  失败、镜像刚连接就断开）；Windows 使用 `-vs fakesink -vrtp ...` 输出，
  由随包的无界面 ffmpeg/SDP 解码分支生成 RGBA。macOS 与 Windows 发布包均内置
  对应平台的 UxPlay 1.73.6 runtime（官方提交
  `21eef8df25d91e12635c36d8176ad192725baca2`）和 GStreamer runtime，版本、来源、
  SHA-256 与 GPL/LGPL/MIT 组件记录见 `src-tauri/binaries/sidecars.json`。
  快投屏使用 `-h265 -s 1920x1080@30 -fps 30`，UxPlay 不得直接创建用户可见的镜像窗口；
  若当前构建不具备该输出能力，会话必须返回“待验证/依赖缺失”，不能以黑色占位窗口伪装成功。
- Android 视频参考 scrcpy：ADB 只负责发现、推送/启动服务端和建立隧道（优先
  reverse，失败时回退 forward）；`scrcpy-server` 在设备端通过 `MediaCodec + Surface`
  编码，宿主通过独立视频 TCP 通道接收原始 H264 并解码成 RGBA 帧；控制使用独立的
  scrcpy 二进制控制通道，不把 `adb shell input` 作为连续输入主链路，也不启动
  scrcpy 原生窗口。
- 宿主 ffmpeg 仅作为无窗口解码器随包分发；Windows iOS RTP 与 Android H264 均使用
  匹配平台的内置 ffmpeg，macOS Android H264 使用 macOS 架构匹配的内置 ffmpeg。
  macOS sidecar 必须通过 `lipo` 校验并匹配当前宿主架构。Apple Silicon 固定使用
  `ffmpeg-static` `b6.1.1` 的 `arm64` 单文件，不得把 Intel-only 构建物复制后伪装成
  `aarch64-apple-darwin`。
- Android 每设备一个 `scrcpy-server` 实例（serial + 独立 `scid` 区分）。
- BLE：全局单活动控制（硬件限制）；仅激活设备启用广播。
- iOS USB/WDA 精确控制为可选 P1 通道：WDA 精准版发行包将签名的 WDA IPA、`ideviceinstaller`
  与 `go-ios` 放入 `ios-usb/` 资源目录。用户点击准备后，应用通过 USB 安装 IPA，使用
  `go-ios ui run wda` 启动 XCTest runner 并将设备 8100 转发至本机回环端口，Rust
  通过 W3C Actions 发送绝对点按、拖动、滚轮和按键事件；粘贴使用 WDA 的 `/wda/keys`
  UTF-8 文本接口，以支持中英文混合文本。画面仍走 AirPlay，不把 WDA 当作视频通道。
  显式开启精确模式后，WDA/安装器/启动器/Apple USB 驱动或信任状态任一不满足时，记录
  `ios_usb_required` 并拒绝启动控制，不把控制状态伪装成 USB 已连接，也不静默使用 BLE。
  Windows 运行时不能依赖 macOS 的 xcrun、CoreBluetooth 或 Xcode，但必须具备 Apple
  Mobile Device/usbmux 传输层和随包 DLL。
- WDA IPA 的“自签名”仅表示发布方使用合法 Apple 开发/企业身份完成签名；开发签名的
  provisioning profile 仍限制可安装设备。仓库不提交私钥/p12/profile，应用不在运行时
  重签名，也不绕过设备信任、开发者模式或系统安全提示。没有签名 IPA 的 BLE 兼容版
  仍可使用 BLE；只有 macOS 开发构建保留 Xcode 备用流程，不能宣称 Windows 可自动安装 WDA。
- iOS 鼠标指针和点击依赖系统的 AssistiveTouch：配对后必须由用户在 iPhone
  设置 → 辅助功能 → 触控 → 辅助触控中开启，并在“设备 → 蓝牙设备”中选择该设备；
  键盘输入不依赖 AssistiveTouch。应用只能提示该设置，不能通过公开 API 自动开启。
- iOS BLE 粘贴复用 HID 键盘报告，仅能发送当前键盘布局可表达的字符；不可表达的
  中文、Emoji 等文本必须返回明确错误，不能静默丢失。启用 USB/WDA 后，粘贴切换到
  WDA `/wda/keys` 文本接口发送混合 Unicode；手机必须有当前获得焦点的可编辑控件，
  且 WDA 会话必须保持可用。
- macOS CoreBluetooth 不得手动添加系统维护的 GAP/GATT 服务（如 `0x1800`）；
  广播前按 Battery Information → Device Information → HID 服务顺序发布应用服务。
  GATT 数据库中的 SIG 标准服务和特征必须使用完整的 Bluetooth Base UUID
  （例如 HID 服务为 `00001812-0000-1000-8000-00805F9B34FB`）；macOS 外设角色对
  本项目的服务表使用紧凑 UUID 会返回 `CBErrorUUIDNotAllowed`。只有广播包中的 HID
  服务标识使用等价的紧凑 `0x1812`，以节省主广播空间。前台主广播只发送 HID 服务和
  不超过 8 个 UTF-8 字节的短 ASCII 名称（当前兼容短名为 `KTP`），避免服务 UUID 被
  CoreBluetooth 放入 iOS 系统设置不会主动扫描的 overflow 区域。macOS 的公开
  CoreBluetooth 外设接口可能让 iOS 系统蓝牙列表显示宿主电脑的 GAP 名称，面板必须
  展示该实际配对名称，不能要求用户只搜索产品名。服务发布或广播失败必须进入可诊断
  的错误状态。
- GATT 服务发布完成并开始广播后，不得在 iPhone 的 HID 发现/加密握手期间动态添加或
  删除服务；旧缓存恢复只能通过完整停止广播、移除并重新发布固定 GATT 表，再重新开始
  广播。应用不能通过公开 API 删除 iPhone 系统蓝牙绑定；仍失败时，界面必须引导用户
  在 iPhone 设置中忽略对应的 Mac 条目后重新投屏。

### 5.4 接口草案（增量）

```typescript
export type SessionId = string // "iphone:<udid>" | "android:<serial>"

interface BackendApi {
  listSessions(): Promise<SessionInfo[]>
  startSession(id: SessionId, kind: DeviceKind): Promise<void>
  stopSession(id: SessionId): Promise<void>
  activateDevice(id: SessionId): Promise<void>
  openSimulator(id: SessionId): Promise<void>
  attachFrameChannel(id: SessionId, channel: Channel): Promise<void>
  // 内部帧背压确认；只推进最新帧，不进入用户可见操作面板
  acknowledgeFrame(id: SessionId, sequence: number): Promise<void>
  listUsbDevices(): Promise<UsbDevicesReport>
  getIosControlCapability(): Promise<IosControlCapability> // 宿主侧只读探测，不创建 WDA 会话
  refreshDiagnostics(): Promise<DiagnosticsReport>
  // 原单设备命令带上 sessionId
}

interface IosControlCapability {
  usbWdaEnabled: boolean
  wdaReachable: boolean
  iproxyPresent: boolean
  detail: string
}
```

## 6. 关键实现规则（继承 v1.1，增量）

- 停止某设备顺序：暂停输入 → 释放按键/按钮 → 关闭广播/输入通道 → 停止镜像
  → 关闭其模拟器窗口 → 清理临时文件（v1.1 §6.3）。
- 切换激活时：先释放旧设备的按键/按钮（Release All），再切换输入目标。
- 每设备错误日志独立；诊断页聚合全部会话（脱敏）。
- 链路断开（UxPlay/scrcpy 崩溃、网络断链）进入 `Reconnecting` 后自动重启镜像
  接收器：退避 1s→16s 指数增长、最多 5 次，期间状态机保持 `Reconnecting`；
  重新收到连接/首帧事件即转回 `MirroringConnected`，次数耗尽转 `Failed`
  并提示手动重试；用户停止会话可随时取消自动重连。
- 应用启动时清理上一次异常退出（SIGKILL/崩溃/`tauri dev` 重启）遗留的
  UxPlay 孤儿实例（按内置可执行文件路径 + 「快投屏」服务名匹配，macOS）：
  孤儿会继续占用 AirPlay 端口段并广播同名服务，导致新会话接收器无法绑定端口、
  手机连上被孤儿接收器接走即断（表现为“一直连接不上”）。
- macOS 防火墙（ALF）会静默丢弃**未签名 / ad-hoc 签名** uxplay 的入站连接：
  接收器正常广播与监听（手机屏幕镜像列表能看到服务），但手机点选后转圈几秒
  提示“无法连接”，接收器端收不到任何 TCP。属于连接前提：dev 构建需在防火墙
  中放行 uxplay（`pnpm firewall:allow`，每次重编译后需重跑），发布构建需在
  首次镜像时允许系统弹窗的入站许可（详见 README「macOS 防火墙」章节）。
- 主窗口 `stop_session(sessionId)` 与模拟器窗口销毁、子进程回收绑定；模拟器标题栏关闭也调用
  同一命令，避免出现窗口已关闭但会话仍存活的孤儿状态。

## 10. 测试与验收（增量）

| ID | 步骤摘要 | 通过条件 |
|---|---|---|
| AT-017 | 同时连接 iPhone + Android | 两个会话并存，面板显示各自状态，互不串扰 |
| AT-018 | 切换激活设备 | 目标模拟器窗口显示并聚焦，其它窗口保持显示；输入/粘贴目标随之改变，旧设备 Release All |
| AT-019 | 在主窗口取消某设备投屏 | 该会话被断开并回收，其模拟器窗口关闭，其它会话不受影响 |
| AT-020 | 面板一键全部断开 | 所有会话、窗口、子进程、广播被清理 |
| AT-021 | USB 自动识别 | 面板自动列出已连接 iPhone/Android（脱敏）；拔插刷新正确 |
| AT-022 | 单设备崩溃 | 仅该会话显示失败并可重连；面板与其它会话正常 |
| AT-023 | 移动/关闭模拟器窗口 | 拖动系统原生标题栏可移动；最大化入口不可用；点击关闭会停止会话并回收资源；普通画面拖动仍转发到设备 |
| AT-024 | macOS iOS 投屏时查看程序坞并关闭模拟器 | 同一应用实例只有一个快投屏图标；点击模拟器关闭按钮后会话、UxPlay 与窗口均被回收 |
| AT-025 | macOS 授权蓝牙后启用 iOS 控制 | CoreBluetooth 服务发布和广播成功；iPhone 设置→蓝牙可发现并连接面板提示的实际条目（macOS 可能显示宿主电脑 GAP 名称，原始 BLE 广播短名为 `KTP`），连接后键盘报告可用；开启 AssistiveTouch 后鼠标指针和点击可用 |
| AT-026 | iOS USB/WDA 绝对坐标（实验） | 发布包内含与目标设备匹配的签名 IPA、`ideviceinstaller`、`go-ios` 和 Windows 所需 DLL；用户点击准备后应用完成安装、启动和 `/status` 校验，USB/WDA session 建立，画布四角与中心点击落在对应系统坐标，拖动不因视频掉帧错位；状态明确显示 `usb_wda` |
| AT-027 | USB/WDA 精确模式依赖缺失 | 开启 USB/WDA 后签名 IPA、安装器、启动器、USB 信任或设备服务任一缺失时，控制启动失败并显示可操作原因；日志记录 `ios_usb_required`，状态不伪造 `usb_wda`，用户关闭该开关后才可明确选择 BLE |
| AT-028 | iOS USB/WDA 混合文本粘贴 | WDA 控制已显示 `usb_wda`，手机上的可编辑控件已获得焦点；工具栏粘贴与画布 Ctrl/Cmd+V 均能完整发送中英文、标点和 Emoji，不产生 `unencodable_chars`；WDA 不可用时明确提示切换 USB/WDA |
| AT-029 | macOS iOS BLE 旧 GATT 缓存恢复 | 停止并重新发布固定 GATT 表、重新开始广播后，iPhone 能重新发现并进入 `control_connected`；仍失败时 UI 给出“忽略对应 Mac 配对记录后重试”的明确引导 |

## 11. 交付阶段（v1.2 更新）

- 构建分为两个明确变体：BLE 兼容版使用 `pnpm tauri:build` 或
  `pnpm tauri:build:ble`，不要求 WDA 资源，iOS 输入走 BLE；WDA 精准版使用
  `pnpm tauri:build:wda`（`pnpm tauri:build:release` 为兼容别名），强制检查签名
  IPA、`ideviceinstaller`、`go-ios` 和对应宿主 DLL。macOS DMG 分别使用
  `pnpm tauri:build:dmg` 与 `pnpm tauri:build:dmg:wda`。两种版本都必须明确显示当前
  实际输入通道，不能把 BLE 状态伪装成 `usb_wda`。
- macOS DMG 默认使用 Tauri `bundle.macOS.signingIdentity: "-"` 对 App 及其内置组件进行 ad-hoc
  （本地自签名）代码签名，并由 `scripts/sign-macos-dmg.mjs` 对最终 DMG 文件本身签名；不在仓库
  提交证书或私钥。若发布机配置了合法签名身份，可由 `APPLE_SIGNING_IDENTITY` 覆盖；未公证的
  ad-hoc 包首次打开仍可能需要用户在系统“隐私与安全性”中手动允许。
- GitHub Actions 打包 workflow 只允许通过 `workflow_dispatch` 触发，版本号由触发脚本交互式输入；
  每次打包同时产出 macOS Apple Silicon（arm64/M 芯片）与 Intel（x86_64）DMG，以及 Windows x64
  NSIS/MSI 安装包，不因普通提交或 push 自动打包。macOS 两种架构均使用目标架构匹配的 sidecar；
  Windows 仅接受经过审计的 UxPlay 1.73.6 x64 构建物。
- M0：同 v1.1，另加「双窗口生命周期与 activate 切换」验证。
- MVP：控制面板 + 单设备模拟器窗口 + 自动识别 + 帧管线；
  多设备并发（≥2）为 P1 优先完成。
- V1：多设备并发、音频/录制、已配对管理、更新器。
- P1：USB + WDA iOS 绝对坐标实验通道（需分别在 Windows/macOS、实际 Apple USB
  驱动、安装器、go-ios、签名 IPA 与真机组合完成 AT-026/027 后再标记支持）。

> 历史：v1.0（Windows→iPhone 单链）、v1.1（iOS/Android→Windows/macOS 双宿主）
> 归档于 `doc/spec/archive/`。
