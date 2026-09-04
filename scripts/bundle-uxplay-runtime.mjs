/* global console */
// bundle-uxplay-runtime.mjs —— 为 macOS/Windows 的 UxPlay sidecar 收集
// 应用内 GStreamer 运行时。
//
// UxPlay 是 GPLv3 独立进程；这里不下载或修改其源码，只把已经审计的 UxPlay
// 可执行文件与运行它所需的 GStreamer 动态库、插件和 plugin-scanner 放进
// src-tauri/binaries/。macOS 的 Homebrew 绝对依赖会被改写为 @rpath；Windows
// 则把 DLL 与插件放进独立目录并由 Rust 启动器设置 PATH。发布后的应用不依赖
// 用户机器上的 Homebrew、MSYS2 或系统 GStreamer。
//
// 默认只保留快投屏的 headless 管线所需插件：macOS 内部 RGBA 输出使用
// appsrc、queue/fakesink、h264parse、decodebin、videoconvert/videoscale；
// Windows 的 RTP 输出还需要 rtph264pay 和 udpsink。传入 --refresh 可在升级
// GStreamer 后重建。

import { execFileSync, spawnSync } from "node:child_process"
import { createHash } from "node:crypto"
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs"
import { dirname, join, basename } from "node:path"
import { fileURLToPath } from "node:url"
import process from "node:process"

const here = dirname(fileURLToPath(import.meta.url))
const root = join(here, "..")
const bins = join(root, "src-tauri", "binaries")
const uxplay = join(bins, process.platform === "win32" ? "uxplay.exe" : "uxplay")
const uxplayAgent = join(bins, "uxplay-agent.app")
const uxplayAgentExecutable = join(uxplayAgent, "Contents", "MacOS", "uxplay")
const runtime = join(bins, "gstreamer")
const runtimeLib = join(runtime, "lib")
const runtimePlugins = join(runtime, "plugins")
const runtimeScanner = join(runtime, "libexec", "gstreamer-1.0", "gst-plugin-scanner")
const runtimeWindowsBin = join(runtime, "bin")
const runtimeWindowsScanner = join(runtime, "libexec", "gstreamer-1.0", "gst-plugin-scanner.exe")
const forceRefresh = process.argv.includes("--refresh")

const REQUIRED_PLUGINS = [
  "libgstapp.dylib",
  "libgstcoreelements.dylib",
  // UxPlay 1.73.6 validates these base plugins during gst_init(), even when
  // 快投屏 disables audio with -as 0. Keep the upstream validation intact.
  "libgstlibav.dylib",
  "libgstplayback.dylib",
  "libgstautodetect.dylib",
  // GStreamer 1.28 将 videoconvert/videoscale 放在这个合并插件中；UxPlay
  // 的无界面视频管线仍需要它完成 H264 解码后的颜色转换与尺寸处理。
  "libgstvideoconvertscale.dylib",
  "libgstvideoparsersbad.dylib",
  "libgstrtp.dylib",
  "libgstudp.dylib",
]

function commandOutput(command, args) {
  try {
    return execFileSync(command, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim()
  } catch {
    return null
  }
}

function run(command, args) {
  const result = spawnSync(command, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] })
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} 失败：${result.stderr?.trim() || "unknown error"}`)
  }
}

function sha256File(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex")
}

function otoolDependencies(path) {
  const output = commandOutput("otool", ["-L", path])
  if (!output) return []
  return output
    .split("\n")
    .slice(1)
    .map((line) => line.trim().replace(/ \(compatibility version.*$/, ""))
    .filter(Boolean)
}

function hasRpath(path, value) {
  const output = commandOutput("otool", ["-l", path]) || ""
  return output.includes(`path ${value} `)
}

function addRpath(path, value) {
  if (!hasRpath(path, value)) run("install_name_tool", ["-add_rpath", value, path])
}

function removeRpath(path, value) {
  if (hasRpath(path, value)) run("install_name_tool", ["-delete_rpath", value, path])
}

function isHomebrewPath(path) {
  return path.startsWith("/opt/homebrew/") || path.startsWith("/usr/local/Cellar/") || path.startsWith("/usr/local/opt/")
}

function resolveSourcePath(from, dependency, prefix, knownSources) {
  const candidates = []
  if (dependency.startsWith("/")) {
    candidates.push(dependency)
  } else if (dependency.startsWith("@loader_path/")) {
    candidates.push(join(dirname(from), dependency.slice("@loader_path/".length)))
  } else if (dependency.startsWith("@rpath/")) {
    const name = dependency.slice("@rpath/".length)
    candidates.push(join(dirname(from), name), join(prefix, "lib", name), join(prefix, "lib", "gstreamer-1.0", name))
  }
  for (const candidate of candidates) {
    if (!existsSync(candidate)) continue
    const resolved = realpathSync(candidate)
    if (isHomebrewPath(resolved) || knownSources.has(resolved)) return resolved
  }
  return null
}

function runtimeIsComplete() {
  if (!existsSync(runtimeScanner)) return false
  return REQUIRED_PLUGINS.every((name) => existsSync(join(runtimePlugins, name))) && existsSync(join(runtimeLib, "libgstreamer-1.0.0.dylib"))
}

function windowsRuntimeScanner() {
  const candidates = [runtimeWindowsScanner, join(runtime, "libexec", "gstreamer-1.0", "gst-plugin-scanner")]
  return candidates.find((path) => existsSync(path)) || null
}

function windowsRuntimeIsComplete() {
  if (!existsSync(runtimeWindowsBin) || !existsSync(runtimePlugins)) return false
  if (!windowsRuntimeScanner()) return false
  const hasDll = readdirSync(runtimeWindowsBin).some((entry) => entry.toLowerCase().endsWith(".dll"))
  const hasPlugin = readdirSync(runtimePlugins).some((entry) => entry.toLowerCase().endsWith(".dll"))
  return hasDll && hasPlugin
}

function bundleMacUxplayAgent() {
  if (process.platform !== "darwin") return
  if (!existsSync(uxplay)) return
  const infoPlist = join(uxplayAgent, "Contents", "Info.plist")
  if (!existsSync(infoPlist)) {
    throw new Error(`缺少 UxPlay macOS 后台 agent 的 Info.plist：${infoPlist}`)
  }
  mkdirSync(dirname(uxplayAgentExecutable), { recursive: true })
  copyFileSync(uxplay, uxplayAgentExecutable)
  chmodSync(uxplayAgentExecutable, 0o755)
  console.log(`[ok] UxPlay 已封装为 macOS LSUIElement 后台 agent：${uxplayAgent}`)
}

function gstreamerVersion() {
  const raw = commandOutput("brew", ["info", "--json=v2", "--formula", "gstreamer"])
  if (!raw) return "unknown"
  try {
    const formula = JSON.parse(raw).formulae?.[0]
    return formula?.versions?.stable || formula?.versioned_formulae?.[0] || "unknown"
  } catch {
    return "unknown"
  }
}

function listRuntimeFiles() {
  const files = []
  const walk = (directory, relativePrefix) => {
    const entries = readdirSync(directory)
    for (const entry of entries) {
      const source = join(directory, entry)
      const relative = join(relativePrefix, entry)
      if (statSync(source).isDirectory()) walk(source, relative)
      else files.push({ path: relative, sha256: sha256File(source) })
    }
  }
  walk(runtime, "")
  return files.sort((a, b) => a.path.localeCompare(b.path))
}

function windowsGstreamerPrefix() {
  const programFiles = process.env.ProgramFiles || "C:\\Program Files"
  const candidates = [
    process.env.PHONEBRIDGE_GSTREAMER_PREFIX,
    process.env.GSTREAMER_ROOT_DIR,
    join(programFiles, "gstreamer", "1.0", "msvc_x86_64"),
    join(programFiles, "gstreamer", "1.0", "mingw_x86_64"),
    "C:\\gstreamer\\1.0\\msvc_x86_64",
    "C:\\msys64\\ucrt64",
    "C:\\msys64\\mingw64",
  ].filter(Boolean)
  return candidates.find((prefix) => existsSync(join(prefix, "bin")) && existsSync(join(prefix, "lib", "gstreamer-1.0"))) || null
}

function windowsScannerSource(prefix) {
  const candidates = [
    join(prefix, "libexec", "gstreamer-1.0", "gst-plugin-scanner.exe"),
    join(prefix, "libexec", "gstreamer-1.0", "gst-plugin-scanner"),
    join(prefix, "bin", "gst-plugin-scanner.exe"),
    join(prefix, "bin", "gst-plugin-scanner"),
  ]
  return candidates.find((path) => existsSync(path)) || null
}

function copyMatchingFiles(sourceDir, destinationDir, predicate) {
  if (!existsSync(sourceDir)) return []
  mkdirSync(destinationDir, { recursive: true })
  const copied = []
  for (const entry of readdirSync(sourceDir)) {
    const source = join(sourceDir, entry)
    if (!statSync(source).isFile() || !predicate(entry, source)) continue
    const destination = join(destinationDir, entry)
    copyFileSync(source, destination)
    copied.push(destination)
  }
  return copied
}

function windowsGstreamerVersion(prefix) {
  const executable = [
    join(prefix, "bin", "gst-launch-1.0.exe"),
    join(prefix, "bin", "gst-launch-1.0"),
  ].find((path) => existsSync(path))
  return executable ? commandOutput(executable, ["--version"]) || "unknown" : "unknown"
}

function bundleWindowsRuntime() {
  if (!existsSync(uxplay)) {
    throw new Error(`未找到已审计的 UxPlay 构建物：${uxplay}。请先放入 UxPlay，再运行此脚本。`)
  }
  if (!forceRefresh && windowsRuntimeIsComplete() && existsSync(join(runtime, "manifest.json"))) {
    console.log(`[ok] Windows UxPlay GStreamer runtime 已存在（${runtime}）；需要升级时使用 --refresh`)
    return
  }

  const prefix = windowsGstreamerPrefix()
  const scannerSource = prefix && windowsScannerSource(prefix)
  if (!prefix || !scannerSource) {
    throw new Error(
      "未找到 Windows GStreamer runtime。请安装与 UxPlay 构建匹配的 Windows GStreamer，或设置 PHONEBRIDGE_GSTREAMER_PREFIX；运行时会随应用打包，不依赖用户安装。",
    )
  }

  const pluginSourceDir = join(prefix, "lib", "gstreamer-1.0")
  const dlls = readdirSync(join(prefix, "bin"))
    .filter((entry) => entry.toLowerCase().endsWith(".dll"))
  const plugins = readdirSync(pluginSourceDir)
    .filter((entry) => entry.toLowerCase().endsWith(".dll"))
  if (!dlls.length || !plugins.length) {
    throw new Error(`Windows GStreamer 安装不完整：${prefix}`)
  }

  rmSync(runtime, { recursive: true, force: true })
  mkdirSync(runtimeWindowsBin, { recursive: true })
  mkdirSync(runtimePlugins, { recursive: true })
  mkdirSync(dirname(runtimeWindowsScanner), { recursive: true })
  copyMatchingFiles(join(prefix, "bin"), runtimeWindowsBin, (entry) => entry.toLowerCase().endsWith(".dll"))
  copyMatchingFiles(pluginSourceDir, runtimePlugins, (entry) => entry.toLowerCase().endsWith(".dll"))
  copyFileSync(scannerSource, runtimeWindowsScanner)

  const manifest = {
    runtime: "gstreamer",
    platform: "windows",
    source: "GStreamer Windows runtime",
    sourceUrl: "https://gstreamer.freedesktop.org/download/",
    version: windowsGstreamerVersion(prefix),
    license: "LGPL-2.0-or-later; LGPL-2.1-or-later; MIT (GStreamer and selected plugins)",
    plugins,
    files: listRuntimeFiles(),
    distributionConclusion:
      "GStreamer is bundled as a separate runtime for the GPLv3 UxPlay process. Preserve the corresponding notices and source information in release materials.",
  }
  writeFileSync(join(runtime, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n")
  console.log(`[ok] Windows UxPlay GStreamer runtime 已内置：${manifest.files.length} 个文件，${manifest.version}`)
}

function main() {
  if (process.platform === "win32") {
    bundleWindowsRuntime()
    return
  }
  if (process.platform !== "darwin") {
    console.log("[skip] 当前平台不是 macOS/Windows，UxPlay GStreamer runtime 由对应平台构建流程提供")
    return
  }
  if (!existsSync(uxplay)) {
    throw new Error(`未找到已审计的 UxPlay 构建物：${uxplay}。请先放入 UxPlay，再运行此脚本。`)
  }
  if (!forceRefresh && runtimeIsComplete() && existsSync(join(runtime, "manifest.json"))) {
    console.log(`[ok] UxPlay GStreamer runtime 已存在（${runtime}）；需要升级时使用 --refresh`)
    bundleMacUxplayAgent()
    return
  }

  const prefix = process.env.PHONEBRIDGE_GSTREAMER_PREFIX || commandOutput("brew", ["--prefix", "gstreamer"])
  if (!prefix || !existsSync(prefix)) {
    throw new Error("未找到 GStreamer。请安装 GStreamer runtime，或设置 PHONEBRIDGE_GSTREAMER_PREFIX")
  }
  const pluginSourceDir = join(prefix, "lib", "gstreamer-1.0")
  const scannerSource = join(prefix, "libexec", "gstreamer-1.0", "gst-plugin-scanner")
  if (!existsSync(pluginSourceDir) || !existsSync(scannerSource)) {
    throw new Error(`GStreamer 安装不完整：${prefix}`)
  }

  const sourceByName = new Map()
  const roleBySource = new Map()
  const queue = []
  const register = (path, role = "library") => {
    const resolved = realpathSync(path)
    const name = basename(resolved)
    const previous = sourceByName.get(name)
    if (previous && previous !== resolved) {
      throw new Error(`GStreamer runtime 中存在同名但来源不同的库：${name}`)
    }
    sourceByName.set(name, resolved)
    const previousRole = roleBySource.get(resolved)
    // otool -L always reports a Mach-O dylib's own install name as its first
    // dependency. Keep the explicit plugin/scanner role when that self-entry
    // is encountered during recursive dependency collection.
    if (!previousRole || (previousRole === "library" && role !== "library")) {
      roleBySource.set(resolved, role)
    }
    if (!queue.includes(resolved)) queue.push(resolved)
  }

  register(uxplay, "executable")
  for (const plugin of REQUIRED_PLUGINS) {
    const path = join(pluginSourceDir, plugin)
    if (!existsSync(path)) throw new Error(`GStreamer 缺少快投屏所需插件：${plugin}`)
    register(path, "plugin")
  }
  register(scannerSource, "scanner")

  for (let index = 0; index < queue.length; index += 1) {
    const current = queue[index]
    for (const dependency of otoolDependencies(current)) {
      if (!isHomebrewPath(dependency) && !dependency.startsWith("@")) continue
      const source = resolveSourcePath(current, dependency, prefix, new Set(sourceByName.values()))
      if (source) register(source)
    }
  }

  rmSync(runtime, { recursive: true, force: true })
  mkdirSync(runtimeLib, { recursive: true })
  mkdirSync(runtimePlugins, { recursive: true })
  mkdirSync(dirname(runtimeScanner), { recursive: true })

  for (const [source, role] of roleBySource) {
    if (role === "library") {
      const destination = join(runtimeLib, basename(source))
      copyFileSync(source, destination)
      // Homebrew marks formula files read-only. Tauri's resource staging and
      // watcher need a regular readable file in the generated bundle.
      chmodSync(destination, 0o644)
    } else if (role === "plugin") {
      const destination = join(runtimePlugins, basename(source))
      copyFileSync(source, destination)
      chmodSync(destination, 0o644)
    } else if (role === "scanner") {
      copyFileSync(source, runtimeScanner)
      chmodSync(runtimeScanner, 0o755)
    }
  }

  const sourceName = (dependency) => {
    if (dependency.startsWith("/")) {
      try {
        return sourceByName.get(basename(realpathSync(dependency))) ? basename(realpathSync(dependency)) : null
      } catch {
        return null
      }
    }
    if (dependency.startsWith("@rpath/") || dependency.startsWith("@loader_path/")) {
      const name = basename(dependency)
      return sourceByName.has(name) ? name : null
    }
    return null
  }

  const patch = (path, role) => {
    for (const dependency of otoolDependencies(path)) {
      const name = sourceName(dependency)
      if (name) run("install_name_tool", ["-change", dependency, `@rpath/${name}`, path])
    }
    if (role === "library") {
      run("install_name_tool", ["-id", `@rpath/${basename(path)}`, path])
      addRpath(path, "@loader_path")
    } else if (role === "plugin") {
      // The first line of otool -L for a plugin is its own install id, not a
      // normal dependency; rewrite it explicitly so the bundle contains no
      // Homebrew path even for that self-entry.
      run("install_name_tool", ["-id", `@rpath/${basename(path)}`, path])
      addRpath(path, "@loader_path/../lib")
    } else if (role === "scanner") {
      addRpath(path, "@loader_path/../../lib")
    } else if (role === "executable") {
      // Tauri 保留 resources 的 binaries/ 前缀：
      // 发布位置 Contents/MacOS/uxplay -> Contents/Resources/binaries/gstreamer。
      removeRpath(path, "@loader_path/../Resources/gstreamer/lib")
      addRpath(path, "@loader_path/../Resources/binaries/gstreamer/lib")
      addRpath(path, "@loader_path/gstreamer/lib")
    }
  }

  for (const [source, role] of roleBySource) {
    if (role === "library") patch(join(runtimeLib, basename(source)), role)
    if (role === "plugin") patch(join(runtimePlugins, basename(source)), role)
    if (role === "scanner") patch(runtimeScanner, role)
  }
  patch(uxplay, "executable")

  const patchedFiles = [uxplay, ...listRuntimeFiles().map(({ path }) => join(runtime, path))]
  for (const path of patchedFiles) {
    const unresolvedHomebrew = otoolDependencies(path).filter((dependency) => isHomebrewPath(dependency))
    if (unresolvedHomebrew.length) {
      throw new Error(`未能移除 ${path} 的 Homebrew 动态依赖：${unresolvedHomebrew.join(", ")}`)
    }
  }

  const manifest = {
    runtime: "gstreamer",
    source: "Homebrew formula gstreamer",
    sourceUrl: "https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/g/gstreamer.rb",
    formulaVersion: gstreamerVersion(),
    license: "LGPL-2.0-or-later; LGPL-2.1-or-later; MIT (GStreamer and selected plugins)",
    plugins: REQUIRED_PLUGINS,
    files: listRuntimeFiles(),
    distributionConclusion:
      "GStreamer is bundled as a separate runtime for the GPLv3 UxPlay process. Preserve the corresponding notices and source information in release materials.",
  }
  writeFileSync(join(runtime, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n")
  bundleMacUxplayAgent()
  console.log(`[ok] UxPlay GStreamer runtime 已内置：${manifest.files.length} 个文件，GStreamer ${manifest.formulaVersion}`)
}

try {
  main()
} catch (error) {
  console.error(`[error] ${error.message}`)
  process.exitCode = 1
}
