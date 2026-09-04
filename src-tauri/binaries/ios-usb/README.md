# iOS USB/WDA runtime（可选）

把经过审计、与目标平台和架构匹配的以下运行时放在本目录，发布包会通过
`tauri.conf.json` 的 resources 一起带上：

- `iproxy` / `iproxy.exe`：通过 usbmux 将 iPhone 的 WDA `8100` 转到本机回环端口；
- `idevice_id` / `idevice_id.exe`：USB 设备枚举和会话 UDID 解析；
- Windows 所需的同目录 DLL，以及对应的许可证/来源/校验记录。

应用不会自动下载、安装或替换这些组件。开发时也可以分别使用
`PHONEBRIDGE_IPROXY_PATH`、`PHONEBRIDGE_IDEVICE_ID_PATH` 指定路径；发布构建应将
审计过的文件直接放入本目录，避免用户额外配置 PATH。

推荐来源：

- <https://github.com/libimobiledevice/libusbmuxd>（`iproxy` 工具，GPL-2.0-or-later）；
- <https://github.com/libimobiledevice/libimobiledevice>（`idevice_id`，LGPL-2.1-or-later）。

Windows 仍需要系统已安装并运行 Apple Mobile Device / usbmux 传输组件；本目录只
承载应用自身可分发的跨平台工具，不把 macOS 的 `xcrun`、CoreBluetooth 或 Xcode
作为 Windows 依赖。使用前 iPhone 还必须解锁、信任此电脑，并由用户自行安装、签名、
信任和启动 WDA；应用不绕过 iOS 授权或安装流程。
