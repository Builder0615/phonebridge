/* global console */
// prepare-sidecars.mjs —— 把运行时依赖（adb / scrcpy / UxPlay / iOS WDA）收集进 src-tauri/binaries/
// 作为 app 内置 sidecar（Spec v1.2、AGENTS.md「binaries/ 仅存放经校验的构建物」）。
//
// 策略（显式命令，不静默下载）：
//   1. 系统已有（command -v / where / 常见位置）→ 复制进 binaries/ 并记录来源与版本；
//   2. 没有 → 从固定来源下载（Android Platform-Tools、scrcpy 官方发布、
//      macOS 分架构的 ffmpeg-static release），
//      计算 SHA-256 写入 binaries/sidecars.json（记录版本/来源/校验和，供审计与诊断）。
//   3. UxPlay 只接受已审计的官方源码构建物，不从 PATH 静默复制；macOS/Windows
//      的 GStreamer runtime 由 bundle-uxplay-runtime.mjs 收集进 app resources。
//
// 产物：
//   src-tauri/binaries/adb[.exe]      （Android）
//   src-tauri/binaries/scrcpy[.exe]   （Android）
//   src-tauri/binaries/sidecars.json  （来源/版本/SHA-256/许可证记录）
//   src-tauri/binaries/README.md      （策略说明）
//
// 用法：node scripts/prepare-sidecars.mjs   （可加 --download 允许从官方源下载；
//        发布构建加 --require-wda，强制要求签名 WDA IPA）

import { execFileSync, spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import {
  existsSync,
  mkdirSync,
  writeFileSync,
  chmodSync,
  copyFileSync,
  readFileSync,
  readdirSync,
  statSync,
  unlinkSync,
} from "node:fs"
import { tmpdir } from "node:os"
import { join, dirname, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import process from "node:process"
const { platform } = process

const here = dirname(fileURLToPath(import.meta.url))
const root = join(here, "..")
const bins = join(root, "src-tauri", "binaries")
const iosUsbBins = join(bins, "ios-usb")
const allowDownload = process.argv.includes("--download")
const requireWda = process.argv.includes("--require-wda") || process.env.PHONEBRIDGE_REQUIRE_WDA === "1" || process.env.PHONEBRIDGE_REQUIRE_WDA === "true"

const UXPLAY_VERSION = "1.74"
// go-ios is used for Windows USB discovery even in the BLE-compatible build.
// Pin the official release archive so a release never picks up an arbitrary
// executable from PATH.
const GO_IOS_VERSION = "v1.3.2"
const GO_IOS_WINDOWS_URL = `https://github.com/danielpaulus/go-ios/releases/download/${GO_IOS_VERSION}/go-ios-win.zip`
const GO_IOS_WINDOWS_SHA256 = "939c6bcaafed183a92afb9f79cc11b1f935fa6389bfc94d3902e3f52c4dff3fe"
const UXPLAY_SOURCE_URL = "https://github.com/FDH2/UxPlay"
const UXPLAY_RELEASE = "master@d19d22adcf1314124ecf4c27cbc5cf0ae7d05f83"
const UXPLAY_COMMIT = "d19d22adcf1314124ecf4c27cbc5cf0ae7d05f83"
const UXPLAY_ARCHIVE_SHA256 = "e20f8752a9415d3e81af55c55797e2b1d3e4f5bfe846332822bb3dcbc96451f1"

mkdirSync(bins, { recursive: true })
mkdirSync(iosUsbBins, { recursive: true })

const exe = (n) => (platform === "win32" ? `${n}.exe` : n)

// host target triple（externalBin 命名约定：name-<target>）
function hostTriple() {
  if (platform === "darwin") return process.arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin"
  if (platform === "win32") return process.arch === "arm64" ? "aarch64-pc-windows-msvc" : "x86_64-pc-windows-msvc"
  if (platform === "linux") return process.arch === "arm64" ? "aarch64-unknown-linux-gnu" : "x86_64-unknown-linux-gnu"
  return "unknown"
}
const TRIPLE = hostTriple()

function hostArchitecture() {
  if (platform === "darwin") return process.arch === "arm64" ? "arm64" : "x86_64"
  return process.arch
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------
function sh(cmd, args) {
  try {
    return execFileSync(cmd, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim()
  } catch {
    return null
  }
}
function locateSystem(name) {
  const n = exe(name)
  // PATH / command -v（GUI 场景下 shell 会加载用户配置）
  const which =
    platform === "win32" ? sh("where", [name]) : sh("sh", ["-lc", `command -v ${name}`])
  if (which && existsSync(which.trim())) return which.trim()
  // 常见位置
  const candidates = [
    process.env.ANDROID_HOME ? join(process.env.ANDROID_HOME, "platform-tools", n) : null,
    process.env.ANDROID_SDK_ROOT ? join(process.env.ANDROID_SDK_ROOT, "platform-tools", n) : null,
    join(process.env.HOME || "", "Library/Android/sdk/platform-tools", n),
    "/opt/homebrew/bin/" + n,
    "/usr/local/bin/" + n,
  ].filter(Boolean)
  for (const c of candidates) if (existsSync(c)) return c
  return null
}
function sha256File(p) {
  return createHash("sha256").update(readFileSync(p)).digest("hex")
}
function executableArchitectures(bin) {
  if (platform !== "darwin") return null
  const out = sh("lipo", ["-archs", bin])
  return out ? out.split(/\s+/).filter(Boolean) : null
}
function isHostCompatibleExecutable(bin) {
  if (!existsSync(bin)) return false
  if (platform !== "darwin") return true
  return executableArchitectures(bin)?.includes(hostArchitecture()) ?? false
}
function thinToHostArchitecture(bin) {
  if (platform !== "darwin") return
  const archs = executableArchitectures(bin)
  if (!archs || archs.length <= 1) return

  const tmp = join(tmpdir(), `phonebridge-${process.pid}-${Date.now()}-${archs.join("-")}`)
  try {
    const r = spawnSync("lipo", ["-thin", hostArchitecture(), bin, "-output", tmp], {
      stdio: ["ignore", "pipe", "pipe"],
    })
    if (r.status !== 0 || !existsSync(tmp)) {
      throw new Error(`无法将 ${bin} 裁剪为 ${hostArchitecture()}`)
    }
    copyFileSync(tmp, bin)
    chmodSync(bin, 0o755)
    console.log(`[info] ${bin} 已裁剪为 ${hostArchitecture()}，不携带未使用的 Intel slice`)
  } finally {
    if (existsSync(tmp)) unlinkSync(tmp)
  }
}
function download(url, dest) {
  // 使用系统 curl 下载（跨平台可用；Windows 10+ 自带 curl）
  const r = spawnSync("curl", ["-fSL", "--retry", "2", "-o", dest, url], { stdio: ["ignore", "pipe", "pipe"] })
  if (r.status !== 0) throw new Error(`下载失败 ${url}: ${r.stderr?.toString().slice(0, 300)}`)
}
function unzip(zipPath, outDir) {
  if (platform === "win32") {
    const r = spawnSync("tar", ["-xf", zipPath, "-C", outDir], { stdio: "ignore" })
    if (r.status !== 0) throw new Error("解压失败（tar）")
  } else {
    const r = spawnSync("unzip", ["-oq", zipPath, "-d", outDir], { stdio: "ignore" })
    if (r.status !== 0) throw new Error("解压失败（unzip）")
  }
}

function prepareWindowsGoIosDiscovery() {
  if (platform !== "win32" || !allowDownload) return
  const dst = join(iosUsbBins, "ios.exe")
  if (existsSync(dst)) return

  const archive = join(tmpdir(), `phonebridge-go-ios-${GO_IOS_VERSION}.zip`)
  const extractDir = join(tmpdir(), `phonebridge-go-ios-${GO_IOS_VERSION}`)
  download(GO_IOS_WINDOWS_URL, archive)
  const digest = sha256File(archive)
  if (digest !== GO_IOS_WINDOWS_SHA256) {
    throw new Error(
      `go-ios Windows 构建 SHA-256 不匹配：${digest}；预期 ${GO_IOS_WINDOWS_SHA256}`,
    )
  }
  mkdirSync(extractDir, { recursive: true })
  unzip(archive, extractDir)
  const extracted = join(extractDir, "ios.exe")
  if (!existsSync(extracted)) {
    throw new Error(`go-ios Windows 压缩包中没有 ios.exe：${GO_IOS_WINDOWS_URL}`)
  }
  copyFileSync(extracted, dst)
  chmodSync(dst, 0o755)
  console.log(`[ok] Windows go-ios ${GO_IOS_VERSION} 已内置（SHA-256 ${sha256File(dst).slice(0, 16)}…）`)
}

// ---------------------------------------------------------------------------
// 各工具收集
// ---------------------------------------------------------------------------
const record = {}

function tripleFilename(name) {
  // Tauri expects the target triple before the Windows executable suffix:
  // `adb-x86_64-pc-windows-msvc.exe`, not `adb.exe-x86_64-pc-windows-msvc`.
  return platform === "win32" ? `${name}-${TRIPLE}.exe` : `${name}-${TRIPLE}`
}

function writeTripleCopy(name) {
  // externalBin 打包源：name-<target triple>（tauri 复制进包时去掉后缀）
  const dst = join(bins, tripleFilename(name))
  const src = join(bins, exe(name))
  if (existsSync(src)) {
    if (!isHostCompatibleExecutable(src)) {
      throw new Error(`${src} 不包含当前宿主架构 ${hostArchitecture()}`)
    }
    thinToHostArchitecture(src)
    // 不能只在目标不存在时复制：旧的错误架构文件会因此一直残留。
    copyFileSync(src, dst)
    chmodSync(dst, 0o755)
  }
  return dst
}
function collectCopy(name, displayName) {
  const src = locateSystem(name)
  if (src && !isHostCompatibleExecutable(src)) {
    console.warn(`[warn] ${displayName}（${src}）不包含当前宿主架构 ${hostArchitecture()}，跳过内置`)
    return false
  }
  const dst = join(bins, exe(name))
  if (src && src !== dst) {
    copyFileSync(src, dst)
    chmodSync(dst, 0o755)
    writeTripleCopy(name)
    record[displayName] = { source: "system", path: src, version: toolVersion(name, displayName) }
    console.log(`[ok] ${displayName} 已内置（来自 ${src}，打包名 ${tripleFilename(name)}）`)
    return true
  } else if (src === dst) {
    writeTripleCopy(name)
    record[displayName] = { source: "bundled", path: dst, version: toolVersion(name, displayName) }
    console.log(`[ok] ${displayName} 已是内置版本`)
    return true
  }
  return false
}
function toolVersion(name, displayName) {
  if (displayName === "adb") return sh(exe("adb"), ["version"])?.split("\n")[0] ?? "unknown"
  if (displayName === "scrcpy") return sh(exe("scrcpy"), ["--version"])?.split("\n")[0] ?? "unknown"
  return "unknown"
}

function copyWindowsSiblingDlls(src) {
  if (platform !== "win32") return []
  const sourceDir = dirname(src)
  const copied = []
  for (const entry of readdirSync(sourceDir)) {
    if (!entry.toLowerCase().endsWith(".dll")) continue
    const from = join(sourceDir, entry)
    const to = join(iosUsbBins, entry)
    if (!statSync(from).isFile()) continue
    if (resolve(from) !== resolve(to)) copyFileSync(from, to)
    copied.push(`src-tauri/binaries/ios-usb/${entry}`)
  }
  return copied
}

// UxPlay 是 GPLv3 独立进程：只接受已经放入 binaries/ 的审计构建物，
// 不从用户 PATH 复制一个无法追溯来源的版本。macOS/Windows 下同时校验
// UxPlay 与它旁边的 GStreamer runtime 是否可用于当前架构。
function collectUxplay() {
  const dst = join(bins, exe("uxplay"))
  if (!existsSync(dst)) {
    console.warn(
      `[warn] UxPlay 未内置：请将审计过的 UxPlay ${UXPLAY_VERSION} 构建物放入 ${dst}，` +
        "并运行 pnpm sidecars:uxplay 生成/校验 GStreamer runtime。",
    )
    return false
  }
  if (!isHostCompatibleExecutable(dst)) {
    throw new Error(`内置 UxPlay（${dst}）不包含当前宿主架构 ${hostArchitecture()}`)
  }
  thinToHostArchitecture(dst)
  writeTripleCopy("uxplay")

  const reportedOutput = sh(dst, ["-v"])
  const reported = reportedOutput?.split("\n").find((line) => line.includes("UxPlay version")) || null
  if (reported && !reported.includes(UXPLAY_VERSION)) {
    console.warn(`[warn] UxPlay 实际版本为 ${reported}，sidecar 审计记录预期为 ${UXPLAY_VERSION}`)
  }
  record.uxplay = {
    source: "github:FDH2/UxPlay",
    sourceUrl: UXPLAY_SOURCE_URL,
    release: UXPLAY_RELEASE,
    commit: UXPLAY_COMMIT,
    archiveSha256: UXPLAY_ARCHIVE_SHA256,
    version: reported || `UxPlay version ${UXPLAY_VERSION}`,
    path: "src-tauri/binaries/uxplay",
    sha256: sha256File(dst),
    license: "GPLv3",
    packaging: "独立进程；macOS 动态库改写为 @rpath，Windows DLL/插件随包分发，不修改 UxPlay 源码",
    distributionConclusion: "随应用分发时必须同时提供 GPLv3 许可证、对应源代码获取信息，并在发布前完成 copyleft 义务审查。",
  }

  if (platform !== "darwin" && platform !== "win32") return true

  const runtimeManifestPath = join(bins, "gstreamer", "manifest.json")
  if (!existsSync(runtimeManifestPath)) {
    console.warn(
      `[warn] UxPlay 已内置，但 ${platform === "win32" ? "Windows" : "macOS"} GStreamer runtime 缺失；` +
        "请准备对应平台的 GStreamer runtime 后运行 pnpm sidecars:uxplay，发布包不能依赖宿主机安装。",
    )
  } else {
    try {
      const runtimeManifest = JSON.parse(readFileSync(runtimeManifestPath, "utf8"))
      record.gstreamer = {
        source: runtimeManifest.source,
        sourceUrl: runtimeManifest.sourceUrl,
        version: runtimeManifest.formulaVersion || runtimeManifest.version || "unknown",
        path: "src-tauri/binaries/gstreamer",
        license: runtimeManifest.license,
        plugins: runtimeManifest.plugins,
        files: runtimeManifest.files,
        distributionConclusion: runtimeManifest.distributionConclusion,
      }
      console.log(
        `[ok] UxPlay 已内置（${record.uxplay.version}，SHA-256 ${record.uxplay.sha256.slice(0, 16)}…，` +
          `GStreamer ${runtimeManifest.formulaVersion || runtimeManifest.version || "unknown"}）`,
      )
    } catch (error) {
      throw new Error(`GStreamer runtime manifest 无法读取：${error.message}`)
    }
  }
  return true
}

function collectAndroidAdb() {
  if (collectCopy("adb", "adb")) return
  if (!allowDownload) {
    console.warn("[warn] adb 未找到；用 --download 自动从官方 Platform-Tools 获取（固定版本 + SHA-256 记录）")
    return
  }
  // 官方 platform-tools 下载（固定版本）
  const ver = "36.0.0"
  const dlMap = {
    darwin: `https://dl.google.com/android/repository/platform-tools_r${ver}-darwin.zip`,
    win32: `https://dl.google.com/android/repository/platform-tools_r${ver}-windows.zip`,
    linux: `https://dl.google.com/android/repository/platform-tools_r${ver}-linux.zip`,
  }
  const url = dlMap[platform]
  if (!url) {
    console.warn(`[warn] 当前平台 ${platform} 暂不支持自动获取 adb`)
    return
  }
  try {
    const tmp = join(tmpdir(), `pb-adb-${Date.now()}`)
    const zip = join(tmp, "pt.zip")
    mkdirSync(tmp, { recursive: true })
    console.log(`[info] 下载 Platform-Tools ${ver} …`)
    download(url, zip)
    unzip(zip, tmp)
    const src = join(tmp, "platform-tools", exe("adb"))
    if (!existsSync(src)) throw new Error("压缩包内未找到 adb")
    const dst = join(bins, exe("adb"))
    copyFileSync(src, dst)
    chmodSync(dst, 0o755)
    writeTripleCopy("adb")
    record.adb = { source: "official", version: `platform-tools ${ver}`, sha256: sha256File(dst) }
    console.log(`[ok] adb 已内置（官方 Platform-Tools ${ver}，SHA-256 ${record.adb.sha256.slice(0, 16)}…）`)
  } catch (e) {
    console.warn(`[warn] adb 获取失败：${e.message}`)
  }
}

// 投屏解码走 ffmpeg；ffmpeg 也随应用内置（静态构建单文件，见 collectFfmpeg）。
function isStaticExecutable(bin) {
  if (platform === "darwin") {
    // otool：出现 libavcodec 依赖 = 动态（brew 版），不能单文件随包分发
    const out = sh("otool", ["-L", bin])
    return !(out && out.includes("libavcodec"))
  }
  if (platform === "linux") {
    const out = sh("ldd", [bin]) || ""
    return !out.includes("libavcodec")
  }
  // Windows：官方 static/essentials 构建通常自包含；默认视为可用
  return true
}

function collectFfmpeg() {
  const bundled = join(bins, exe("ffmpeg"))
  if (existsSync(bundled)) {
    if (!isHostCompatibleExecutable(bundled)) {
      console.warn(`[warn] 已有内置 ffmpeg（${bundled}）不包含当前宿主架构 ${hostArchitecture()}，需要替换。`)
    } else if (isStaticExecutable(bundled)) {
      writeTripleCopy("ffmpeg")
      console.log(`[ok] ffmpeg 已使用内置静态版本（${bundled}）`)
      return
    } else {
      console.warn(`[warn] 已有内置 ffmpeg（${bundled}）不是静态单文件，需要替换。`)
    }
  }
  const sys = locateSystem("ffmpeg")
  if (sys) {
    if (!isHostCompatibleExecutable(sys)) {
      console.warn(`[warn] 系统 ffmpeg（${sys}）不包含当前宿主架构 ${hostArchitecture()}，跳过内置。`)
    } else if (isStaticExecutable(sys)) {
      if (collectCopy("ffmpeg", "ffmpeg")) return
    } else {
      console.warn(
        `[warn] 系统 ffmpeg（${sys}）是动态链接版本（依赖 libav*），不能单文件随包分发。` +
          "请用 --download 获取固定来源的静态构建。",
      )
    }
  }
  if (!allowDownload) {
    console.warn("[warn] ffmpeg 未内置；用 --download 获取匹配架构的静态构建（含 SHA-256 记录）")
    return
  }
  // macOS 使用固定版本、分架构的单文件构建。evermeet 的浮动下载地址曾返回
  // x86_64 文件，即使文件名被复制成 aarch64-apple-darwin 也会在 Apple Silicon
  // 应用中触发 macOS 的 Intel 兼容性提示。
  const ffmpegStaticRelease = "b6.1.1"
  const dl = {
    darwin:
      `https://github.com/eugeneware/ffmpeg-static/releases/download/${ffmpegStaticRelease}/ffmpeg-darwin-${
        process.arch === "arm64" ? "arm64" : "x64"
      }`,
    win32: "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip",
    linux: "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz",
  }[platform]
  if (!dl) return
  try {
    const tmp = join(tmpdir(), `pb-ffmpeg-${Date.now()}`)
    mkdirSync(tmp, { recursive: true })
    console.log(`[info] 下载 ffmpeg 静态构建 …`)
    let src = null
    if (platform === "darwin") {
      // 该 release asset 本身就是已按架构构建的 Mach-O 单文件，不是 zip。
      src = join(tmp, "ffmpeg")
      download(dl, src)
      chmodSync(src, 0o755)
    } else {
      const arc = join(tmp, "f." + (dl.endsWith(".zip") ? "zip" : "tar.xz"))
      download(dl, arc)
      if (arc.endsWith(".zip")) unzip(arc, tmp)
      else spawnSync("tar", ["-xJf", arc, "-C", tmp], { stdio: "ignore" })
      // 找 bin/ffmpeg(.exe)
      const stack = [tmp]
      while (stack.length && !src) {
        const d = stack.pop()
        for (const e of readdirSync(d)) {
          const p = join(d, e)
          if (/ffmpeg(\.exe)?$/.test(e) && statSync(p).isFile()) { src = p; break }
          if (statSync(p).isDirectory()) stack.push(p)
        }
      }
    }
    if (!src) throw new Error("静态构建内未找到 ffmpeg")
    if (!isHostCompatibleExecutable(src)) {
      throw new Error(`下载的 ffmpeg 不包含当前宿主架构 ${hostArchitecture()}`)
    }
    if (!isStaticExecutable(src)) throw new Error("下载的构建不是静态单文件")
    const dst = join(bins, exe("ffmpeg"))
    copyFileSync(src, dst)
    chmodSync(dst, 0o755)
    writeTripleCopy("ffmpeg")
    const ver = sh(dst, ["-version"])?.split("\n")[0] ?? "unknown"
    const sourceMetadata =
      platform === "darwin"
        ? {
            source: "github:eugeneware/ffmpeg-static",
            sourceUrl: "https://github.com/eugeneware/ffmpeg-static",
            release: ffmpegStaticRelease,
            asset: `ffmpeg-darwin-${process.arch === "arm64" ? "arm64" : "x64"}`,
            license: "GPL-3.0-or-later",
          }
        : { source: dl }
    record.ffmpeg = {
      ...sourceMetadata,
      version: ver,
      sha256: sha256File(dst),
    }
    console.log(`[ok] ffmpeg 已内置（${ver.slice(0, 60)}，SHA-256 ${record.ffmpeg.sha256.slice(0, 16)}…）`)
  } catch (e) {
    console.warn(`[warn] ffmpeg 获取失败：${e.message}`)
  }
}

function validatePackagedArchitectures() {
  if (platform !== "darwin") return
  // The host scrcpy executable is diagnostic-only and is not listed in any
  // Tauri externalBin/resources entry.  Only validate executables that are
  // actually packaged; otherwise a stale architecture-specific scrcpy file
  // left in the workspace can incorrectly block an Intel/ARM release.
  for (const name of ["adb", "ffmpeg", "uxplay"]) {
    for (const p of [join(bins, exe(name)), join(bins, `${exe(name)}-${TRIPLE}`)]) {
      if (existsSync(p) && !isHostCompatibleExecutable(p)) {
        throw new Error(`内置 ${p} 不包含当前宿主架构 ${hostArchitecture()}，请运行 pnpm sidecars:download`)
      }
    }
  }
  for (const name of ["iproxy", "idevice_id"]) {
    const p = join(iosUsbBins, exe(name))
    if (existsSync(p) && !isHostCompatibleExecutable(p)) {
      throw new Error(`内置可选 iOS USB 工具 ${p} 不包含当前宿主架构 ${hostArchitecture()}`)
    }
  }
  const runtime = join(bins, "gstreamer")
  if (existsSync(runtime)) {
    const stack = [runtime]
    while (stack.length) {
      const directory = stack.pop()
      for (const entry of readdirSync(directory)) {
        const path = join(directory, entry)
        if (statSync(path).isDirectory()) stack.push(path)
        else if ((path.endsWith(".dylib") || entry === "gst-plugin-scanner") && !isHostCompatibleExecutable(path)) {
          throw new Error(`内置 GStreamer runtime ${path} 不包含当前宿主架构 ${hostArchitecture()}`)
        }
      }
    }
  }
}

function locateScrcpyServer(src) {
  const configured = process.env.PHONEBRIDGE_SCRCPY_SERVER_PATH
  const candidates = [
    configured,
    src ? join(dirname(src), "scrcpy-server") : null,
    src ? join(dirname(src), "../share/scrcpy/scrcpy-server") : null,
    src ? join(dirname(src), "../../share/scrcpy/scrcpy-server") : null,
    "/opt/homebrew/opt/scrcpy/share/scrcpy/scrcpy-server",
    "/opt/homebrew/share/scrcpy/scrcpy-server",
    "/usr/local/opt/scrcpy/share/scrcpy/scrcpy-server",
    "/usr/share/scrcpy/scrcpy-server",
  ].filter(Boolean)
  return candidates.find((p) => existsSync(p)) || null
}

function collectScrcpy() {
  const host = locateSystem("scrcpy")
  if (host) {
    collectCopy("scrcpy", "scrcpy")
  } else {
    console.warn("[warn] scrcpy 客户端未找到；快投屏仍需要匹配版本的 scrcpy-server")
  }

  const server = locateScrcpyServer(host)
  if (!server) {
    console.warn(
      "[warn] scrcpy-server 未找到；请安装 scrcpy 4.0，或设置 PHONEBRIDGE_SCRCPY_SERVER_PATH，" +
        "再运行 pnpm sidecars",
    )
    return
  }
  const dst = join(bins, "scrcpy-server")
  if (server !== dst) copyFileSync(server, dst)
  chmodSync(dst, 0o644)
  record.scrcpyServer = {
    source: server === dst ? "bundled" : "system",
    path: server,
    version: "scrcpy-server 4.0 (must match host protocol)",
    sha256: sha256File(dst),
  }
  console.log(`[ok] scrcpy-server 已内置（${server}，SHA-256 ${record.scrcpyServer.sha256.slice(0, 16)}…）`)
}

// iOS USB/WDA is deliberately opt-in. Do not copy an arbitrary PATH binary
// into a release: a release build must receive an explicitly audited tool via
// an environment variable or a file already checked into binaries/ios-usb.
function collectOptionalIosUsbTool(name, recordName, envVar, license, sourceUrl) {
  const dst = join(iosUsbBins, exe(name))
  const configured = process.env[envVar]
  let src = configured
    ? (existsSync(configured) && statSync(configured).isDirectory()
        ? join(configured, exe(name))
        : configured)
    : (existsSync(dst) ? dst : null)

  if (!src || !existsSync(src)) {
    if (requireWda && (name === "ideviceinstaller" || name === "ios")) {
      throw new Error(`发布构建要求内置 ${name}${platform === "win32" ? ".exe" : ""}；请设置 ${envVar}`)
    }
    console.log(`[info] 可选 iOS USB/WDA 工具 ${name} 未收集；需要时设置 ${envVar} 或放入 ${dst}`)
    return
  }
  if (!statSync(src).isFile()) {
    if (requireWda && (name === "ideviceinstaller" || name === "ios")) {
      throw new Error(`${envVar} 不是有效文件：${src}`)
    }
    console.warn(`[warn] ${envVar} 不是文件，跳过 ${name}`)
    return
  }
  if (!isHostCompatibleExecutable(src)) {
    if (requireWda && (name === "ideviceinstaller" || name === "ios")) {
      throw new Error(`${name}（${src}）不包含当前宿主架构 ${hostArchitecture()}`)
    }
    console.warn(`[warn] ${name}（${src}）不包含当前宿主架构 ${hostArchitecture()}，跳过内置`)
    return
  }
  const samePath = resolve(src) === resolve(dst)
  if (!samePath) copyFileSync(src, dst)
  const windowsDependencies = copyWindowsSiblingDlls(src)
  chmodSync(dst, 0o755)
  thinToHostArchitecture(dst)
  record[recordName] = {
    source: samePath ? "bundled" : "explicit-path",
    sourceUrl,
    path: `src-tauri/binaries/ios-usb/${exe(name)}`,
    version: sh(dst, ["--version"]) || "unknown",
    sha256: sha256File(dst),
    ...(windowsDependencies.length > 0 ? { windowsDependencies } : {}),
    license,
    packaging: "可选资源；Windows 依赖同目录 DLL 与 Apple Mobile Device/usbmux 传输服务",
    distributionConclusion: "发布前须随包提供对应许可证、来源、源码获取信息和完整依赖清单。",
  }
  console.log(`[ok] 可选 ${name} 已内置（${dst}，SHA-256 ${record[recordName].sha256.slice(0, 16)}…）`)
}

function collectIosUsbTools() {
  collectOptionalIosUsbTool(
    "iproxy",
    "iproxy",
    "PHONEBRIDGE_IPROXY_PATH",
    "GPL-2.0-or-later",
    "https://github.com/libimobiledevice/libusbmuxd",
  )
  collectOptionalIosUsbTool(
    "idevice_id",
    "ideviceId",
    "PHONEBRIDGE_IDEVICE_ID_PATH",
    "LGPL-2.1-or-later",
    "https://github.com/libimobiledevice/libimobiledevice",
  )
  collectOptionalIosUsbTool(
    "ideviceinstaller",
    "ideviceInstaller",
    "PHONEBRIDGE_IDEVICEINSTALLER_PATH",
    "GPL-2.0-or-later",
    "https://github.com/libimobiledevice/ideviceinstaller",
  )
  collectOptionalIosUsbTool(
    "ios",
    "goIos",
    "PHONEBRIDGE_GO_IOS_PATH",
    "MIT",
    `https://github.com/danielpaulus/go-ios/releases/tag/${GO_IOS_VERSION}`,
  )
  collectSignedWdaArtifact()
}

// A signed WDA runner is device/provisioning-profile material, not something
// the build can safely download from a public source.  Release CI must receive
// it explicitly and records its digest so the desktop app can install exactly
// the audited artifact on both macOS and Windows.
function collectSignedWdaArtifact() {
  const dst = join(iosUsbBins, "WebDriverAgentRunner.ipa")
  const manifestPath = join(iosUsbBins, "WebDriverAgentRunner.json")
  const configured = process.env.PHONEBRIDGE_WDA_IPA_PATH
  const src = configured || (existsSync(dst) ? dst : null)
  if (!src || !existsSync(src)) {
    if (requireWda) {
      throw new Error("发布构建要求内置签名 WDA IPA；请设置 PHONEBRIDGE_WDA_IPA_PATH")
    }
    console.log(`[info] 未收集签名 WDA IPA；发布构建请设置 PHONEBRIDGE_WDA_IPA_PATH`)
    return
  }
  if (!statSync(src).isFile() || !src.toLowerCase().endsWith(".ipa")) {
    if (requireWda) {
      throw new Error(`PHONEBRIDGE_WDA_IPA_PATH 不是有效的 .ipa 文件：${src}`)
    }
    console.warn(`[warn] PHONEBRIDGE_WDA_IPA_PATH 不是 .ipa 文件，跳过：${src}`)
    return
  }
  const samePath = resolve(src) === resolve(dst)
  if (!samePath) copyFileSync(src, dst)
  const bundleId = (process.env.PHONEBRIDGE_WDA_BUNDLE_ID || "com.facebook.WebDriverAgentRunner.xctrunner").trim()
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,254}$/.test(bundleId)) {
    throw new Error(`PHONEBRIDGE_WDA_BUNDLE_ID 格式无效：${bundleId}`)
  }
  writeFileSync(
    manifestPath,
    JSON.stringify(
      {
        artifact: "WebDriverAgentRunner.ipa",
        bundleId,
        source: "appium/WebDriverAgent",
        note: "由发布方提供的已签名 WDA runner；运行时由应用安装并通过 go-ios 启动。",
      },
      null,
      2,
    ),
  )
  record.wda = {
    source: samePath ? "bundled" : "explicit-path",
    sourceUrl: "https://github.com/appium/WebDriverAgent",
    artifact: "WebDriverAgentRunner.ipa",
    path: "src-tauri/binaries/ios-usb/WebDriverAgentRunner.ipa",
    manifest: "src-tauri/binaries/ios-usb/WebDriverAgentRunner.json",
    bundleId,
    sha256: sha256File(dst),
    license: "BSD-3-Clause (upstream WDA; verify the signed artifact provenance)",
    packaging: "签名/Provisioning Profile 由发布方提供；应用运行时只安装和启动，不生成或绕过 Apple 签名",
    distributionConclusion: "仅向已授权设备分发；不要把开发者私钥、p12 或 mobileprovision 私密材料放入仓库。",
  }
  console.log(`[ok] 签名 WDA IPA 已内置（${dst}，SHA-256 ${record.wda.sha256.slice(0, 16)}…）`)
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------
collectAndroidAdb()
collectFfmpeg()
collectScrcpy()
collectUxplay()
prepareWindowsGoIosDiscovery()
collectIosUsbTools()
validatePackagedArchitectures()

// 写记录
// 保留旧记录中本轮未收集的工具（避免覆盖已有内置记录）
const oldPath = join(bins, "sidecars.json")
if (existsSync(oldPath)) {
  try {
    const prev = JSON.parse(readFileSync(oldPath, "utf8"))
    for (const [k, v] of Object.entries(prev)) {
      if (!(k in record)) record[k] = v
    }
  } catch { /* 旧文件损坏则忽略 */ }
}
// Do not preserve a stale WDA manifest/digest after the IPA was removed from
// the current build input.  A sidecars record must never claim that an absent
// signed artifact is bundled.
const bundledWda = join(iosUsbBins, "WebDriverAgentRunner.ipa")
if (!existsSync(bundledWda)) {
  delete record.wda
  const staleWdaManifest = join(iosUsbBins, "WebDriverAgentRunner.json")
  if (existsSync(staleWdaManifest)) unlinkSync(staleWdaManifest)
}
writeFileSync(join(bins, "sidecars.json"), JSON.stringify(record, null, 2))
console.log(`\nsidecars.json 已更新：${Object.keys(record).join(", ") || "（空）"}`)
