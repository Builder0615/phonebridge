# iOS USB/WDA runtime

这个目录是 iOS USB/WDA 资源目录。BLE 兼容版不需要签名 WDA 和安装器，但 Windows
要自动列出 USB iPhone 时仍应至少包含经过审计的 `ios.exe`（go-ios）；WDA 精准版
应另外包含同一版本、同一签名策略下的以下文件：

- `WebDriverAgentRunner.ipa`：已经由发布方签名的 WDA runner；
- `WebDriverAgentRunner.json`：由收集脚本生成，记录 WDA 的 Bundle ID；
- `ideviceinstaller` / `ideviceinstaller.exe`：向 USB iPhone 安装 IPA；
- `ios` / `ios.exe`：`go-ios`，列出 USB 设备，并在 WDA 版中启动 XCTest/WDA、把设备
  8100 转发到本机回环端口；
- `iproxy` / `iproxy.exe`、`idevice_id` / `idevice_id.exe`：macOS 开发回退路径和
  设备诊断使用；Windows 还要把所需 DLL（包括 go-ios 构建要求的 `wintun.dll`）放在
  对应可执行文件旁边。

WDA 精准版会通过 Tauri resources 把整个目录带入 macOS 和 Windows 安装包。用户点击
“一键准备 WDA”后，应用会自动完成：检查 USB 信任 → 安装内置 IPA → 启动 WDA →
轮询 `/status` → 注册本机回环端口；用户不需要手动安装或启动 WDA。Rust 输入层随后
通过 W3C Actions 发送绝对坐标，通过 WDA `/wda/keys` 发送混合 Unicode 文本。

## 发布构建

签名 IPA 不能从公开源下载，也不能把 Apple 私钥、p12 或 provisioning profile 提交到
仓库。发布机准备好 IPA 后，使用明确的输入路径收集资源：

macOS / CI shell：

```bash
PHONEBRIDGE_WDA_IPA_PATH=/secure/release/WebDriverAgentRunner.ipa \
PHONEBRIDGE_WDA_BUNDLE_ID=com.facebook.WebDriverAgentRunner.xctrunner \
PHONEBRIDGE_IDEVICEINSTALLER_PATH=/secure/bin/ideviceinstaller \
PHONEBRIDGE_GO_IOS_PATH=/secure/bin/ios \
pnpm sidecars:release
```

Windows PowerShell：

```powershell
$env:PHONEBRIDGE_WDA_IPA_PATH = 'C:\release\WebDriverAgentRunner.ipa'
$env:PHONEBRIDGE_WDA_BUNDLE_ID = 'com.facebook.WebDriverAgentRunner.xctrunner'
$env:PHONEBRIDGE_IDEVICEINSTALLER_PATH = 'C:\release\ideviceinstaller.exe'
$env:PHONEBRIDGE_GO_IOS_PATH = 'C:\release\ios.exe'
pnpm sidecars:release
```

`sidecars:release` / `tauri:build:wda` 会拒绝没有 IPA、`ideviceinstaller` 或 `go-ios` 的构建，并把 IPA 的
SHA-256、Bundle ID、来源和许可证记录到 `src-tauri/binaries/sidecars.json`。如果只做本地开发，可以使用
`pnpm sidecars` 或 `pnpm tauri:build:ble`；没有 IPA 时仍可使用 BLE，WDA 设置页会显示
资源未就绪。macOS 开发构建仍保留 Xcode 备用流程。

## Apple 授权边界

“自签名”在这里指使用合法 Apple 开发/企业分发身份签出的 WDA IPA，不是 macOS 的
ad-hoc 应用签名。开发签名 IPA 只能安装到 provisioning profile 授权的设备；首次使用
仍可能需要解锁 iPhone、信任电脑、开启开发者模式并确认开发者信任。应用不会绕过这些
系统安全检查，也不会在运行时生成或重新签名 IPA。

推荐来源：

- <https://github.com/appium/WebDriverAgent>（WDA 上游，BSD-3-Clause）；
- <https://github.com/danielpaulus/go-ios>（跨平台安装/启动和 RemoteXPC，MIT）；
- <https://github.com/libimobiledevice/ideviceinstaller>（IPA 安装工具）；
- <https://github.com/libimobiledevice/libusbmuxd>（`iproxy`，GPL-2.0-or-later）；
- <https://github.com/libimobiledevice/libimobiledevice>（`idevice_id`，LGPL-2.1-or-later）。
