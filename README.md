# 快投屏

跨宿主桌面应用（Windows 10 2004+ / macOS 13+）：把 **iPhone 与 Android** 的屏幕镜像到
各自独立的模拟器窗口，并通过相应控制链路向设备发送鼠标、键盘和文字输入。

- iPhone：AirPlay（[UxPlay](https://github.com/FDH2/UxPlay)，GPLv3）+ BLE HID 键鼠；
  可选使用 USB + 用户自备 WDA 的绝对坐标控制通道，避免 BLE 相对鼠标的累计误差。
- Android：[scrcpy](https://github.com/Genymobile/scrcpy)（Apache-2.0）：设备端
  `scrcpy-server` 编码，宿主 Rust 接收独立 H264/control 通道并把画面解码到 Tauri canvas。

产品与技术约束以 [快投屏 Spec v1.2](./doc/spec/PhoneBridge_Spec_v1.2.md) 为准。

## 技术栈

- 桌面壳：Tauri 2（Rust）；窗口形态：控制面板（主窗口）+ 模拟器窗口（每台已连接设备一个，可同时显示）
- 前端：React + TypeScript + Vite + Tailwind CSS v4
- Design system：shadcn/ui（`new-york`，Radix，源码位于 `src/components/ui/`）

## 目录结构

```text
src/
├── components/ui/         # shadcn/ui 原语（CLI 添加，见 components.json）
├── components/            # 产品组合组件（设备列表等）
├── features/session/      # 镜像画布、连接、控制栏、诊断、设置
├── lib/                   # Tauri invoke/events、契约类型、InputMapper 纯逻辑
├── styles/globals.css     # 主题 token（zinc + success/warning 语义变量）
src-tauri/
├── src/commands/          # 受控 Tauri commands（snake_case）
├── src/session/           # 状态机（state.rs）+ SessionManager + 事件出口
├── src/integrations/      # UxPlay/scrcpy 镜像、BLE/ADB 输入、剪贴板、诊断
├── capabilities/          # 最小权限声明
└── binaries/              # 审计过的 sidecar 构建物（缺失时返回可理解错误）
```

## 验证命令

```bash
pnpm install          # 安装前端依赖（registry 见 .npmrc）
pnpm lint             # ESLint（flat config）
pnpm test             # Vitest：坐标映射 / 键码归一 / 粘贴策略
pnpm build            # tsc + vite build
pnpm tauri:dev        # Tauri 开发启动（vite dev + 原生壳，双窗口可交互）
pnpm tauri:build      # Rust 编译 + 打包（当前宿主为目标平台）
pnpm tauri:build:dmg  # macOS DMG；App 与 DMG 均使用 ad-hoc（本地自签名）代码签名
pnpm tauri build --bundles app   # 跳过 DMG（无交互会话时更快）
```

macOS DMG 默认使用 `bundle.macOS.signingIdentity: "-"` 对 App 及其内置组件进行 ad-hoc 代码签名，
并由 `scripts/sign-macos-dmg.mjs` 对最终 DMG 文件本身签名；不需要提交 Apple Developer ID
证书或私钥。若构建机已有合法签名身份，可用 `APPLE_SIGNING_IDENTITY` 环境变量覆盖。
构建脚本使用无交互模式，ad-hoc 包首次打开仍可能需要用户在“隐私与安全性”中手动允许。

## GitHub 手动打包

`.github/workflows/package.yml` 只响应手动 `workflow_dispatch`，提交或 push 不会自动打包。先确保
当前分支已经提交并推送，然后运行：

```bash
gh auth login                 # 首次使用时执行
pnpm package:trigger          # 交互式输入版本号，例如 1.2.3
```

workflow 会分别生成 macOS Apple Silicon（M 芯片）、macOS Intel 和 Windows x64 安装包，随后创建或更新
对应的 `v<版本号>` GitHub Release。macOS DMG 使用 `signingIdentity: "-"` ad-hoc 自签名，不需要 Apple
Developer ID 证书；Windows 安装包当前不配置代码签名。

Windows job 会校验 `src-tauri/binaries/uxplay.exe` 是与 UxPlay 1.73.6 固定提交匹配、经过审计的 x64 构建物，
并使用 MSYS2 GStreamer runtime 随包分发。仓库没有该文件时，Windows job 会明确失败，不会把未审计的第三方
预编译文件打进公开 Release。

Rust 侧（在 `src-tauri/` 内）：

```bash
cargo check
cargo test            # 状态机 / HID 报告 / 文本编码 / ADB 参数 / 粘贴流程 / 诊断脱敏
```

本地开发（含热更新）：

```bash
pnpm tauri dev
```

## 已实现（第一版 + 多设备形态）

- **控制面板（主窗口，极简）**：像 Android Studio / Xcode 一样，USB 设备插上即自动识别
  并列出（iPhone: devicectl/system_profiler；Android: adb devices）；每台设备一个
  「投屏/取消投屏」按钮，可多台同时投屏；输入/粘贴只作用于获得焦点的投屏窗口；
  底部日志区用于排查。
- **模拟器窗口**：每台已连接设备一个独立窗口（`simulator-<sessionId>`），加载
  独立的 `simulator.html` 画布入口，只显示该设备画面并转发输入；多个设备窗口
  可同时显示，切换激活只切换输入目标，不会隐藏其它窗口。
- **多设备并发**：`SessionRegistry` + 每设备 `DeviceSession`；AirPlay 每实例独立端口、
  scrcpy 每设备独立实例；命令/事件携带 sessionId 路由。
- **USB 自动识别**：iPhone 使用 `xcrun devicectl`，在空结果/兼容版本下回退
  `xcdevice`、`system_profiler` 和可选的跨平台 `idevice_id`；Windows 不依赖
  `xcrun`/Xcode。Android 使用 `adb devices`，均为结构化枚举并脱敏展示。依赖缺失
  返回可理解错误，未完成信任的有线 iPhone 也会保留在列表中提示授权。
- **帧管线**：Android 使用 scrcpy-server 的原始 H264 video socket；macOS iOS 使用
  UxPlay headless 内部 GStreamer 直接解码为 RGBA，Windows iOS 使用 UxPlay RTP
  配合无界面 ffmpeg 解码。两者都推送到 Tauri 画布，不启动第三方原生窗口。
- **iOS 投屏前置动作**：面板中的「投屏」会启动快投屏的 AirPlay 接收器；随后
  需要在 iPhone「控制中心 → 屏幕镜像」中选择快投屏。iPhone 与电脑必须在同一
  局域网，AirPlay 负责画面；USB/WDA（若启用）只负责绝对坐标和文本控制，不承载
  屏幕视频。

- 会话状态机（Spec §5.5 全转移），Tauri 事件驱动前端。
- iPhone / Android 双链路适配器：
  - `MirrorAdapter`（UxPlay AirPlay headless + 平台解码管线，白名单参数、崩溃/元数据事件）；
  - iOS BLE：WinRT `GattServiceProvider` 骨架（Windows）+ macOS CoreBluetooth HOGP
    GATT 服务、报告特征和异步订阅状态回调；macOS 仍需真实 iPhone 配对验证；
  - Android：ADB 设备枚举、`wm size` 元数据、scrcpy-server 推送/启动、独立 H264 与 control 通道。
- **iOS USB/WDA 实验通道**：`IosUsbControl` 通过回环 HTTP/W3C Actions 发绝对坐标；
  `iproxy`、`idevice_id` 支持从应用资源目录加载，缺失时自动回退 BLE。Windows 构建
  不调用 macOS 专有的 `xcrun`、CoreBluetooth 或 Xcode。
- 画布坐标映射（黑边/旋转/缩放/clamp/相对拆分）、键盘归一、粘贴策略（≤32 KiB、字符反馈）；
  Android 使用画布内容区的绝对触摸坐标，iOS BLE 鼠标使用桌面指针实际 CSS 相对位移，
  避免把下采样视频像素重复放大；移动队列只保留最新绝对点/累加相对位移，并将大位移
  拆成完整 HID 报告。iOS 指针速度/加速度由系统控制，不能通过公开 HOGP 接口保证
  绝对“瞬移”到画布点；开启 USB/WDA 后改用 WDA 绝对坐标。
- 粘贴链路：iOS USB/WDA 模式使用 WDA 键盘动作，未启用/不可用时回退 BLE HID 键盘报告；
  Android 使用 scrcpy UTF-8 control message。
- 诊断：一键检查（宿主/网络/mDNS/蓝牙/依赖）与脱敏导出包；日志不记录输入内容。
- 设置：自动控制、粘贴快捷键、滚轮步长、粘贴上限、iOS BLE 速度倍率、实验性 USB/WDA
  绝对坐标通道；音频/录制开关预留（V1）。
- 单元测试：Rust 97 个通过、Vitest 57 个通过，覆盖状态机、多会话注册表、坐标映射、
  编码、ADB/设备枚举参数构造、WDA 坐标转换、输入队列、脱敏和帧管线（真实设备用例仍需
  在兼容性矩阵中单独记录）。

## 未实现 / 待真机验证（诚实边界）

- **iOS 真机帧验证**：macOS UxPlay 内部 GStreamer → RGBA canvas、Windows UxPlay RTP
  → ffmpeg → canvas 已接线，仍需在真实 iPhone、Bonjour/防火墙和 UxPlay 1.73+ 构建
  上完成端到端验证。
- **iOS HOGP 广播/配对**：WinRT 与 CoreBluetooth 实现需在 Windows/macOS + 真机验证。
- **iOS USB/WDA 绝对控制**：代码和跨平台 `iproxy`/`idevice_id` 资源入口已接入；
  仍需在 Windows/macOS 分别准备审计后的工具、Apple Mobile Device/usbmux 传输、
  用户信任并运行的 WDA，再完成 AT-026/AT-027 真机验证。
- **Android 连续多点输入**：scrcpy control 已支持单指触摸、滚轮、键码和 UTF-8 文本；
  多指手势仍待后续协议建模与真机验证。
- **双窗口形态**：控制面板 + 模拟器窗口代码已就绪；窗口布局与激活切换需在真实宿主
  环境验证（AT-017…AT-022）。
- **音频、录制、全屏视频渲染**：V1。
- 任何标记为「支持」的能力必须先在兼容性矩阵中填报真实设备组合（Spec §10.3）。

### macOS 防火墙：手机能看到「快投屏」但连接失败

macOS 防火墙（ALF）会对**未签名 / ad-hoc 签名**的 uxplay 静默丢弃入站连接（UxPlay
正常广播、监听，但手机点选后转圈几秒提示「无法连接」，接收器端收不到任何 TCP）。
排查与放行：

1. 临时验证：系统设置 → 网络 → 防火墙 → 关闭，再试镜像。能连上即确认是防火墙。
2. 放行（推荐）：`pnpm firewall:allow`（需要 sudo；**dev 每次重编译都会更换二进制
   签名，重编译后需重跑一次**），或在系统设置 → 网络 → 防火墙 → 防火墙选项 →
   添加应用中手动添加 `uxplay` 并设为「允许传入连接」。
3. 发布版 .app 内 UxPlay 位于 `Contents/Resources/binaries/uxplay-agent.app/Contents/MacOS/uxplay`；
   首次镜像时若系统弹窗询问是否允许传入连接，点「允许」。

> 实证：iPhone（AirPlay/960.13.1）在关闭防火墙后于 TCP 6000/6001 建立连接并跑通
> RAOP 握手与视频首帧推送；开启防火墙时同一手机的连接请求从未到达接收器。

### iOS 输入（BLE HID 与 USB/WDA）前提与局限

- 默认 iOS 键鼠输入走 BLE HID（HOGP）：宿主需具备蓝牙 LE Peripheral 能力。该通道
  是相对鼠标；Pointer Lock、有序移动队列和完整 HID 报告能显著降低误差，但由于
  iOS 系统指针加速度和公开 HOGP 接口限制，不能保证绝对坐标。
- 可选 USB/WDA 精确控制：在设置中开启后，应用通过同目录 `iproxy` 将 USB 上的 WDA
  `8100` 转至本机回环端口，WDA 接收绝对 W3C Actions；画面仍走 AirPlay。WDA/USB
  不可用时自动回退 BLE，日志记录原因，不能伪装成已启用精确通道。该方案不安装
  未知组件、不绕过签名/信任、不以越狱为前提。
- macOS：需开启蓝牙，并在 系统设置 → 隐私与安全性 → 蓝牙 中**允许本应用**；
  未授权时广播不会生效（App 会给出明确提示），iPhone 蓝牙列表中不会出现「快投屏」。
- Windows：无蓝牙模块的台式机请使用 **USB 蓝牙适配器**（BLE 4.0/5.0，Windows 10
  2004+ 原生 GattServiceProvider）；无蓝牙时仅支持镜像、不支持输入。
- 配对步骤：iPhone 设置 → 蓝牙 → 连接面板提示的本机名称（macOS 可能显示为
  `Mac mini`）；键盘配对后模拟器窗口聚焦即可输入。鼠标指针和点击还需在 iPhone
  设置 → 辅助功能 → 触控 → 辅助触控中开启，并在“设备 → 蓝牙设备”中选择该设备。

### 多窗口与 Dock

- 模拟器标题栏关闭由 Rust 原生 `CloseRequested` 事件统一执行 `stop_session`，
  清理完成后再销毁 WebView；即使 UxPlay 或 BLE 清理报错，窗口也不会卡在“关闭无响应”。
- macOS UxPlay 使用资源内的 `LSUIElement` 后台 agent，因此同一实例内只保留一个
  「快投屏」Dock 图标。若仍出现两个图标，请先退出旧版本/旧 dev 实例后重新启动，
  再按 AT-024 检查当前包。

## 外部依赖与许可证（发布前必须完成记录）

| 依赖 | 用途 | 许可证 | 状态 |
|---|---|---|---|
| UxPlay 1.73.6 | iPhone 镜像接收 | GPLv3 | 已随 `src-tauri/binaries/uxplay` 分发；记录见 `sidecars.json` |
| scrcpy / scrcpy-server | Android 设备端镜像与 control 协议 | Apache-2.0（含部分组件） | 同上；server 与宿主版本必须匹配 |
| ADB (Platform-Tools) | 设备发现与结构化命令 | Apache-2.0 | 同上 |
| ffmpeg-static b6.1.1 | 宿主 H264/RTP → RGBA 解码（无窗口） | GPL-3.0-or-later | macOS 按宿主架构固定 asset；SHA-256 见 `src-tauri/binaries/sidecars.json` |
| GStreamer 1.28.6 | UxPlay headless 视频管线 | LGPL-2.0/LGPL-2.1/MIT（按组件） | macOS/Windows 对应 runtime 随包（含 `videoconvertscale`）；文件 SHA-256 与声明见 `gstreamer/manifest.json`，运行时不依赖宿主安装 |
| `iproxy` / `idevice_id`（可选） | USB mux 隧道 / iPhone 识别 | GPL-2.0-or-later / LGPL-2.1-or-later | 仅在 `src-tauri/binaries/ios-usb/` 放入审计构建物后随资源分发；Windows 还需同目录 DLL 与 Apple Mobile Device/usbmux 传输 |
| windows-ble-hid | iOS HOGP 参考 | MIT（working spike） | 参考实现，不直接并入 |
| shadcn/ui 组件 | UI 原语 | 源码纳入 + Radix 清单 | `components.json` 记录 |

> 二进制策略见 `src-tauri/binaries/README.md`；缺失依赖时应用返回可理解的错误状态，
> 不会隐式成功。

## 真实设备验证记录（模板）

在兼容性矩阵中完成实测前，所有链接均视为「未支持」。请按以下格式记录：

| 日期 | 宿主 | 设备 | 版本 | 网络/蓝牙 | 结果 | AT 用例 |
|---|---|---|---|---|---|---|
| （示例） | Windows 11 23H2 x64 | iPhone 15 / iOS 17.5 | UxPlay 1.73 + GStreamer; 内置 Intel BT | 家庭 Wi-Fi | 镜像+控制待验证 | AT-001..AT-012 |

## 隐私

- 不读取/上传/落盘剪贴板内容或输入文本；日志只记录事件类型与错误码。
- 设备画面、音频、配对材料不离开本机/局域网；诊断包导出前显示包含清单。
- BLE 广播仅在用户点击「启用控制」后开启；失焦/断开/退出时释放全部按键与按钮。
