/* global console */
import { existsSync, readdirSync } from "node:fs"
import { join, resolve } from "node:path"
import { spawnSync } from "node:child_process"
import process from "node:process"

if (process.platform !== "darwin") {
  throw new Error("macOS DMG 签名只能在 macOS 上执行")
}

const repoRoot = resolve(import.meta.dirname, "..")
const target = process.env.TAURI_TARGET?.trim()
const dmgDirectories = [
  target ? join(repoRoot, "src-tauri", "target", target, "release", "bundle", "dmg") : null,
  join(repoRoot, "src-tauri", "target", "release", "bundle", "dmg"),
].filter(Boolean)
const dmgDir = dmgDirectories.find((directory) => existsSync(directory))

if (!dmgDir) {
  throw new Error(`未找到待签名的 DMG 目录：${dmgDirectories.join("、")}`)
}

const dmgFiles = readdirSync(dmgDir).filter((file) => file.endsWith(".dmg"))

if (dmgFiles.length === 0) {
  throw new Error(`未找到待签名的 DMG：${dmgDir}`)
}

const dmgPath = join(dmgDir, dmgFiles.sort().at(-1))
const identity = process.env.APPLE_SIGNING_IDENTITY?.trim() || "-"

function run(command, args) {
  const result = spawnSync(command, args, { stdio: "inherit" })
  if (result.error) {
    throw result.error
  }
  if (result.status !== 0) {
    throw new Error(`${command} 执行失败，退出码：${result.status ?? "unknown"}`)
  }
}

console.log(`[info] 使用 identity "${identity}" 签名 DMG：${dmgPath}`)
run("codesign", ["--force", "--sign", identity, dmgPath])
run("codesign", ["--verify", "--verbose=2", dmgPath])
console.log(`[ok] DMG 签名验证通过：${dmgPath}`)
