/* global console */
// set-release-version.mjs —— 仅供 CI 打包时把输入版本写入构建工作区。
// 该脚本不会创建提交；GitHub Actions 的 runner 在任务结束后会被销毁。

import { readFileSync, writeFileSync } from "node:fs"
import { resolve } from "node:path"
import process from "node:process"

const root = resolve(import.meta.dirname, "..")
const version = process.argv[2]?.trim()

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/

if (!version || !SEMVER.test(version)) {
  console.error("用法：node scripts/set-release-version.mjs <x.y.z>\n版本号必须符合 SemVer，例如 1.2.3 或 1.2.3-rc.1。")
  process.exit(1)
}

function updateJson(relativePath) {
  const path = resolve(root, relativePath)
  const document = JSON.parse(readFileSync(path, "utf8"))
  document.version = version
  writeFileSync(path, `${JSON.stringify(document, null, 2)}\n`)
}

function updateText(relativePath, pattern, label) {
  const path = resolve(root, relativePath)
  const source = readFileSync(path, "utf8")
  if (!pattern.test(source)) {
    throw new Error(`未能在 ${relativePath} 中定位 ${label} 版本字段`)
  }
  const updated = source.replace(pattern, (_match, prefix, suffix) => `${prefix}${version}${suffix}`)
  if (updated !== source) writeFileSync(path, updated)
}

updateJson("package.json")
updateJson("src-tauri/tauri.conf.json")
updateText(
  "src-tauri/Cargo.toml",
  /(\[package\][\s\S]*?\r?\nversion = ")[^"]+("\r?\n)/,
  "Cargo.toml package",
)
updateText(
  "src-tauri/Cargo.lock",
  /(\[\[package\]\]\r?\nname = "phonebridge"\r?\nversion = ")[^"]+("\r?\n)/,
  "Cargo.lock phonebridge package",
)

console.log(`[ok] CI 构建版本已设置为 ${version}（仅当前 runner 工作区）`)
