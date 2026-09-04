# AGENTS.md

## 项目定位

本仓库用于开发快投屏：跨宿主桌面应用（Windows 10/11、macOS 13+）。产品形态为
「控制面板（主窗口）+ 模拟器窗口」：控制面板集中完成所有操作，每台已连接的
iPhone/Android 各有一个独立模拟器窗口显示画面（同一时刻只显示当前激活台），
支持多设备同时连接（AirPlay 每实例独立端口、scrcpy 每设备独立实例），
输入/粘贴只转发到当前激活设备。

产品与技术约束以 [快投屏 Spec v1.2](./doc/spec/PhoneBridge_Spec_v1.2.md) 为准。
实现与 Spec 不一致时，先更新 Spec 中的范围、接口或验收条件，再修改代码。

## 工作范围

- 本仓库范围为（用户已明确要求）：iPhone 与 Android 分别投屏到 Windows 与 macOS；
  iOS 走 AirPlay + BLE HID，Android 走 scrcpy/ADB。
- 不把 Apple/Google Account、iOS 配套 App 或越狱作为 MVP 前提。
- `ioscpy` 的 USB 方案仅限研究参考，不得在文档或 UI 中表述为无越狱支持。
- 3D 手机外壳、录制增强、文件传输和双向系统剪贴板仍属于后续阶段，除非任务明确要求。

## 技术方向

- 桌面框架固定为 Tauri 2.x + Rust；前端固定为 React + TypeScript + Vite；
  宿主目标 Windows 10 2004（build 19041）+ 与 macOS 13+。
- Design system 固定为 shadcn/ui（需求中的“shadui”）+ Tailwind CSS；组件源码放在
  `src/components/ui/`，采用 `new-york`、Radix base、CSS variables 和语义 token。
- iPhone 镜像接收通过 Rust `MirrorAdapter` 隔离 UxPlay sidecar（Windows 用 GStreamer，
  macOS 用 VideoToolbox/OpenGL）；业务层不重写 AirPlay/FairPlay 协议，也不假设
  UxPlay 窗口可直接嵌入 React DOM。
- Android 镜像参考 scrcpy：ADB 只负责发现、推送/启动匹配版本的
  `scrcpy-server`、建立 reverse/forward 隧道；设备端 server 通过 MediaCodec +
  Surface 输出 H264，宿主在 `integrations/android_frame.rs` 通过独立 video socket
  解码为 RGBA 帧推送到画布，输入走独立 scrcpy control socket。不把
  `adb shell input` 或 CLI 文本输出当作连续输入/稳定业务接口。
- iOS BLE 控制通过 Rust `HidAdapter`/`IHidController` 隔离 HOGP 实现；优先
  Windows WinRT `GattServiceProvider` 与 macOS CoreBluetooth `CBPeripheralManager`，
  真机验证前不把广播流程标记为成功。
- Tauri commands/events/channels 是 React 与 Rust 的唯一业务边界；使用 capabilities
  和最小权限，不开放任意 shell、文件系统或本地网络访问。
- 坐标、按键、粘贴、状态机和主题 token 必须是可单元测试的模块，不能直接散落在
  React 组件事件处理器中。
- 依赖缺失、蓝牙不支持 LE Peripheral、mDNS 不可用、ADB/scrcpy 缺失、Tauri sidecar
  失败和镜像引擎崩溃都必须返回可理解的错误状态。

## 推荐目录

```text
src/
├── components/ui/       # shadcn/ui 源码组件，只从 CLI 添加或按规范扩展
├── components/          # 产品组合组件，不重复实现 UI 原语
├── features/session/    # 连接、镜像、控制和诊断页面
├── lib/                 # Tauri invoke/events、类型、cn() 和纯工具
└── styles/              # Tailwind 入口和 Design system tokens
src-tauri/
├── src/commands/        # 受控 Tauri commands
├── src/session/         # Rust 会话状态机和资源生命周期
├── src/integrations/    # UxPlay、HID、Clipboard、Diagnostics
├── capabilities/        # 最小权限声明
└── binaries/            # 仅存放经过审计和校验的 sidecar 构建物
```

## 外部依赖与许可证

- UxPlay 是 GPLv3；“独立进程”不自动消除 GPL 义务。新增、升级、打包或替换 UxPlay 前，记录版本、提交号、来源、SHA-256、许可证和分发结论。
- ADB（Android SDK Platform-Tools，Apache-2.0）与系统 ffmpeg（LGPL/GPL 组件）用于
  Android 链路；adb 随应用内置并记录版本/SHA-256，ffmpeg 运行期定位（缺失时给出可操作提示）。
- `windows-ble-hid` 为 MIT，但其上游自述状态是 working spike。使用其代码或行为时，先审查已知限制并通过真实 iPhone 验证。
- Tauri、Rust crates、React、Vite、Tailwind、shadcn/ui 生成组件和 Radix primitives 都必须进入依赖清单；不要把 shadcn/ui 当成无需归属的黑盒包。
- GStreamer、Bonjour 和其他运行时组件必须进入第三方声明和 SBOM；不要假设所有组件使用同一许可证。
- 不提交闭源安装包、未授权证书、私钥、真实设备 UDID、配对材料、剪贴板内容或屏幕录制。

## 输入与隐私安全

- BLE 广播只在用户明确点击“启用控制”后开启；停止、失焦、异常或断开时释放全部按键和鼠标按钮。
- 不默认注册全局键盘钩子；`Ctrl+V` 只在镜像画布获得焦点时拦截。
- 日志不能包含剪贴板文本、键盘原文、镜像帧、音频、认证材料或完整网络身份信息。
- WebView 只加载本地前端资源；保持明确 CSP，Tauri capabilities 只授予当前窗口需要的命令和 scope。
- 诊断包必须脱敏，并在导出前让用户确认包含的环境信息。
- 所有外部进程参数采用白名单和结构化参数传递；不把未经校验的用户输入拼进命令行。
- 不实现或测试绕过锁屏、Face ID、DRM、MDM 或其他系统安全边界的功能。

## 代码约定

- 公共接口使用清晰的领域命名：`MirrorAdapter`、`HidAdapter`、`InputMapper`、`ClipboardAdapter`、`SessionManager`。
- React 业务组件优先组合 shadcn/ui；已有原语存在时不要新增裸 `button`、`input`、`select` 或不可访问的点击 `div`。
- 基础视觉使用 `bg-background`、`bg-card`、`text-foreground`、`text-muted-foreground`、`border-border`、`ring-ring` 等主题 token；不在组件里散落任意 hex 值。
- 危险操作使用 `AlertDialog`，加载/错误/空状态必须有 `Skeleton`、`Alert` 或 `Empty` 等设计化状态。
- 异步操作必须可取消；网络、BLE、子进程和渲染资源必须有明确的 owner 和释放路径。
- 按键报告必须保证 KeyDown/KeyUp 配对；任何异常路径都调用 `ReleaseAllAsync`。
- 坐标映射必须考虑黑边、旋转、缩放、越界和相对 HID 报告上限。
- 不把版本号、端口、设备名、键盘布局等环境假设硬编码在 UI；使用配置对象并在诊断中显示实际值。
- 用户可见文案描述“AirPlay 镜像 + BLE 输入”，不要称为 Apple 官方 iPhone Mirroring 或真正的 iOS 系统剪贴板同步。

## 测试要求

- 每个新模块至少提供单元测试；优先覆盖状态机、坐标映射、HID 报告、文本编码、超时和清理逻辑。
- 任何涉及真实 iPhone、蓝牙适配器、mDNS、UxPlay 或防火墙的变更，都要补充集成测试记录和设备版本。
- 必须验证：首次配对、已配对重连、失焦、网络断开、蓝牙断开、子进程崩溃、应用退出、睡眠唤醒和重新连接。
- 验收用例以 Spec 的 `AT-*` 编号引用；未实测的 Windows/iOS/适配器组合不得标记为支持。
- 性能测试至少记录首帧时间、帧率、输入延迟、重连时间、CPU、内存和长时间会话稳定性。

## 变更流程

1. 先阅读 Spec 对应章节和当前目录的局部 `AGENTS.md`（若存在）。
2. 识别受影响的模块、接口、依赖许可证和验收用例。
3. 以最小变更实现，并为行为变化补测试或更新测试记录。
4. 若改变产品范围、外部依赖、隐私行为、许可证或兼容性，必须同步更新 Spec。
5. 提交前检查日志、诊断包、临时文件和安装脚本中没有敏感信息。

## 预期验证命令

仓库完成脚手架后，默认使用以下验证入口；若 package manager、脚本名或 Tauri 配置不同，应在 Spec 或 README 中记录实际命令。已有 lockfile 优先于下面的推荐命令：

```bash
pnpm install
pnpm lint
pnpm test
pnpm build
pnpm tauri build
```

首次初始化 Vite + shadcn/ui 时，使用非交互命令并保留 `components.json`；例如：

```bash
pnpm dlx shadcn@latest init -d --base radix
pnpm dlx shadcn@latest add button card badge alert dialog skeleton
```

若项目改用 npm，等价命令必须使用项目的 npm lockfile，不能在同一仓库混用两种包管理器。

涉及真实设备的集成验证不能用纯单元测试替代，必须在测试记录中写明 Windows 版本、iPhone 型号/iOS、网络环境、蓝牙适配器和 UxPlay/GStreamer 版本。
