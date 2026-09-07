# binaries/ — 内置 sidecar（随 app 分发）

运行时依赖作为 sidecar 随应用内置：

- `adb[.exe]` / `adb-<target-triple>`（Android 设备枚举、推送与 reverse/forward，Apache-2.0）
- `scrcpy-server`（Android 设备端 H264/控制服务，必须与宿主协议版本匹配，Apache-2.0）
- `scrcpy[.exe]` / `scrcpy-<target-triple>`（仅用于收集匹配 server 与诊断，快投屏不启动其原生窗口，Apache-2.0）
- `uxplay[.exe]` / `uxplay-<target-triple>`（iPhone 镜像，UxPlay 1.73.6，GPLv3，已记录源码提交与 SHA-256）
- `gstreamer/`（随目标平台分发的 UxPlay headless runtime：macOS 为 dylib、
  插件和 `gst-plugin-scanner`，由 UxPlay 内部管线输出 RGBA；Windows 为 DLL、
  插件和 scanner，供 UxPlay RTP 输出使用；均由应用资源目录加载，不依赖
  用户安装 Homebrew、MSYS2 或系统 GStreamer）
- `licenses/UXPLAY-LICENSE.txt` 与 `licenses/GSTREAMER-LICENSE.txt`（随应用资源分发）
- `ffmpeg[.exe]` 必须是与目标平台/架构匹配的静态单文件；macOS Apple Silicon 使用
  固定的 `eugeneware/ffmpeg-static` `b6.1.1` `ffmpeg-darwin-arm64`
  （GPL-3.0-or-later），Windows 使用固定来源的 Windows 静态构建；不使用浮动下载
  地址，也不把 Intel-only 文件误命名成 arm64。
- `sidecars.json`：来源 / 版本 / SHA-256 记录（诊断页与审计用）
- `ios-usb/`：WDA 精准版发布包应包含发布方签名的 `WebDriverAgentRunner.ipa`、
  `ideviceinstaller[.exe]`、`ios[.exe]`（go-ios）及其 Windows DLL；应用会在用户点击
  后自动安装、启动和校验 WDA。`iproxy[.exe]`、`idevice_id[.exe]` 仍用于 macOS
  开发回退和设备诊断。WDA 的签名包不从公开源下载，私钥/p12/profile 不进仓库。
  `iproxy` 为 GPL-2.0-or-later，`idevice_id` 为 LGPL-2.1-or-later，`ideviceinstaller`
  为 GPL-2.0-or-later，`go-ios` 为 MIT；发布时必须分别提供许可证、源码获取信息和
  SHA-256 记录。

## 收集机制

```bash
pnpm sidecars          # 从系统定位并复制 adb / scrcpy / scrcpy-server，并校验 UxPlay
pnpm sidecars:download # 系统缺失时从固定来源下载（固定版本 + SHA-256 记录）
pnpm sidecars:ble      # BLE 兼容版：下载并收集基础 sidecar，不要求 WDA
pnpm sidecars:release  # WDA 精准版：同上，并强制要求签名 WDA IPA 已注入
pnpm sidecars:uxplay   # 使用当前构建平台的 GStreamer 重建/校验 UxPlay runtime
pnpm tauri:build       # BLE 兼容版：sidecars 收集 → 前端构建 → tauri build
pnpm tauri:build:wda   # WDA 精准版：额外强制检查 WDA IPA 和宿主工具
```

- 查找顺序（Rust 运行时）：应用内置（resource_dir/binaries、resource_dir、
  macOS 的 resource_dir/../MacOS）→ 环境变量 → Android SDK/Homebrew 常见位置
  → PATH → `command -v`/`where` 兜底（GUI 进程 PATH 不含 shell 配置时仍可定位）。
- `bundle.externalBin` 已声明 `binaries/adb`、`binaries/ffmpeg`、`binaries/uxplay`；
  `scrcpy-server`、UxPlay 的 `VERSION`、目标平台 GStreamer runtime 和许可证通过
  `bundle.resources` 随应用分发。Windows 构建必须在打包前准备 `uxplay.exe` 和
  匹配的 Windows GStreamer runtime，运行时不要求用户另装 GStreamer。可选
  `ios-usb/` 也随资源目录进入应用；只有 WDA 精准版发布需把签名 WDA IPA、
  `ideviceinstaller.exe`、`ios.exe`、`iproxy.exe`、`idevice_id.exe` 及依赖 DLL 放入该目录。
- UxPlay 记录：官方仓库 `FDH2/UxPlay`、release `v1.73.6`、commit
  `21eef8df25d91e12635c36d8176ad192725baca2`、源码归档 SHA-256
  `3a1a754bc7ed4b0f72b6237aa4d769238b9c20a71b651bc3fe9ac679e2a67f18`；
  构建物与 GStreamer 文件 SHA-256 见 `sidecars.json` 和 `gstreamer/manifest.json`。
- macOS 使用 `-h265 -s 1920x1080@30 -fps 30 -avdec -vc ... -vs "fdsink fd=3" -as 0 -nc no -nohold`：
  不创建第三方窗口，通过额外文件描述符读取 UxPlay 内部 GStreamer 输出的 RGBA 帧；
  `-avdec` 固定使用随包的 libav，避免 macOS `decodebin` 选择不可用的硬件解码器，
  `-nc no` 让无界面 renderer 在连接结束或失败时走正常清理路径。
  Windows 使用 `-h265 -s 1920x1080@30 -fps 30 -vs fakesink -as 0 -vsync no -nohold -vrtp ...`：将当前
  协商出的 RTP 复制到 H.264/H.265 两个本机回环 ffmpeg 分支，由首个有效分支
  输出到 RGBA 画布。多实例端口从 6000 开始按 3 递增，避免 UxPlay 的 TCP/UDP
  端口段互相覆盖；`-nh` 保证手机端显示的服务名与面板提示一致。

## 原则

- 依赖缺失 = 未就绪（可操作引导），不是错误：面板以中性提示展示，并提供
  `pnpm sidecars` / `sidecars:download` 指引。
- 不静默下载：`--download` 才访问网络；所有产物记录来源与校验和。
