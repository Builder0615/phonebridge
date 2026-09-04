/* global console */
// allow-uxplay-firewall.mjs —— macOS 防火墙放行本应用的 UxPlay 入站连接。
//
// 背景：macOS 防火墙（ALF）对「未签名 / ad-hoc 签名」的二进制默认拦截
// 入站连接（静默丢弃 SYN）。表现为：iPhone 屏幕镜像列表能看到「快投屏」，
// 但点选后转圈几秒提示「无法连接」，接收器端（UxPlay 日志/连接观察）却收不到
// 任何 TCP 连接。这与视频管线无关（管线已由 mirror_adapter 的 -vc 修复）。
//
// 用法：
//   pnpm firewall:allow        # 为当前 dev 构建放行（每个 dev 构建签名会变，
//                              # 重编译后需重跑一次）
//   sudo 密码在终端提示中输入；发布版 App 首次镜像时系统会弹窗，
//   点「允许」即可，无需本脚本。
//
// 也会列出需要放行的路径，便于在「系统设置 → 网络 → 防火墙 → 防火墙选项 →
// 添加应用」中手动添加（不依赖 sudo）。

import { spawnSync } from "node:child_process"
import { existsSync } from "node:fs"
import { join, dirname } from "node:path"
import { fileURLToPath } from "node:url"
import process from "node:process"

const here = dirname(fileURLToPath(import.meta.url))
const root = join(here, "..")

// 可能存在的 uxplay 路径：dev（target/debug）与打包资源（binaries）。
const candidates = [
  join(root, "src-tauri", "target", "debug", "uxplay"),
  join(root, "src-tauri", "target", "debug", "binaries", "uxplay"),
  join(root, "src-tauri", "binaries", "uxplay"),
]

const existing = candidates.filter((p) => existsSync(p))
if (existing.length === 0) {
  console.error(
    "[error] 未找到 uxplay 二进制。先运行 pnpm sidecars 或 pnpm tauri:dev。",
  )
  process.exit(1)
}

console.log("将放行以下 uxplay 路径：")
for (const p of existing) console.log("  -", p)

const fw = "/usr/libexec/ApplicationFirewall/socketfilterfw"
if (!existsSync(fw)) {
  console.error(`[error] 未找到 ${fw}（仅支持 macOS）`)
  process.exit(1)
}

let ok = true
for (const p of existing) {
  for (const args of [
    ["--add", p],
    ["--unblockapp", p],
  ]) {
    const r = spawnSync("sudo", [fw, ...args], { stdio: "inherit" })
    if (r.status !== 0) {
      console.error(`[warn] \`sudo ${fw} ${args.join(" ")}\` 未成功（status ${r.status}）`)
      ok = false
    }
  }
}

if (ok) {
  console.log(
    "\n[ok] 已放行。如仍连不上：系统设置 → 网络 → 防火墙 → 防火墙选项，" +
      "把上面的 uxplay 添加为「允许传入连接」。\n" +
      "注意：dev 每次重编译都会改变二进制签名，重编译后请重跑 pnpm firewall:allow。",
  )
} else {
  console.log(
    "\n[info] 部分命令未执行成功。请手动在系统设置 → 网络 → 防火墙中添加放行，",
    "或直接关闭防火墙（仅测试期建议）。",
  )
}