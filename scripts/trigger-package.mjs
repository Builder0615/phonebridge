/* global console */
// trigger-package.mjs —— 交互式触发 GitHub Actions 手动打包。

import { execFileSync, spawnSync } from "node:child_process"
import { existsSync } from "node:fs"
import { dirname, join } from "node:path"
import process from "node:process"
import readline from "node:readline/promises"
import { fileURLToPath } from "node:url"

const workflow = "package.yml"
const here = dirname(fileURLToPath(import.meta.url))
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/

function git(args, optional = false) {
  try {
    return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim()
  } catch (error) {
    if (optional) return ""
    throw new Error(`git ${args.join(" ")} 执行失败：${error.stderr?.toString().trim() || error.message}`)
  }
}

function normalizeVersion(input) {
  const version = input.trim().replace(/^v/i, "")
  if (!SEMVER.test(version)) {
    throw new Error("版本号必须符合 SemVer，例如 1.2.3 或 1.2.3-rc.1")
  }
  return version
}

function repositoryFromRemote(remote) {
  const match = remote.match(/github\.com[/:]([^/]+)\/([^/#]+?)(?:\.git)?$/i)
  if (!match) {
    throw new Error(`origin 不是 GitHub 仓库地址：${remote}`)
  }
  return `${match[1]}/${match[2]}`
}

function ensureCleanAndPushed(branch) {
  const changes = git(["status", "--porcelain"])
  if (changes) {
    throw new Error("当前工作区有未提交修改。请先提交并推送后再触发打包，避免打包到旧代码。")
  }

  const upstream = git(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"], true)
  if (!upstream) {
    throw new Error(`分支 ${branch} 没有 upstream。请先执行 git push -u origin ${branch}。`)
  }

  const head = git(["rev-parse", "HEAD"])
  const upstreamHead = git(["rev-parse", upstream])
  if (head !== upstreamHead) {
    throw new Error(`本地 ${branch} 与 ${upstream} 不一致。请先 push 或同步分支后再触发打包。`)
  }
}

async function main() {
  if (!process.stdin.isTTY || !process.stdout.isTTY) {
    throw new Error("版本号需要在交互式终端中输入；请直接运行 pnpm package:trigger。")
  }
  if (!existsSync(join(here, "..", ".github", "workflows", workflow))) {
    throw new Error(`未找到 workflow：.github/workflows/${workflow}`)
  }

  const remote = git(["config", "--get", "remote.origin.url"])
  const repository = process.env.GH_REPO?.trim() || repositoryFromRemote(remote)
  const branch = process.env.PACKAGE_REF?.trim() || git(["branch", "--show-current"])
  if (!branch) throw new Error("当前处于 detached HEAD，无法确定要打包的分支。")
  if (!process.env.PACKAGE_REF?.trim()) ensureCleanAndPushed(branch)

  const packageVersion = execFileSync("node", ["-e", "process.stdout.write(require('./package.json').version)"], { encoding: "utf8" }).trim()
  const prompt = readline.createInterface({ input: process.stdin, output: process.stdout })
  let input
  try {
    input = await prompt.question(`请输入版本号 [${packageVersion}]：`)
  } finally {
    prompt.close()
  }
  const version = normalizeVersion(input || packageVersion)

  const auth = spawnSync("gh", ["auth", "status", "--hostname", "github.com"], { stdio: "ignore" })
  if (auth.status !== 0) {
    throw new Error("GitHub CLI 未登录。请先执行 gh auth login。")
  }

  const result = spawnSync(
    "gh",
    ["workflow", "run", workflow, "--repo", repository, "--ref", branch, "--field", `version=${version}`],
    { stdio: "inherit" },
  )
  if (result.error) throw result.error
  if (result.status !== 0) throw new Error(`触发 GitHub Actions 失败，退出码：${result.status ?? "unknown"}`)

  console.log(`\n[ok] 已触发 ${repository} 的 ${version} 打包。`)
  console.log(`查看进度：https://github.com/${repository}/actions/workflows/${workflow}`)
  console.log(`完成后会发布 v${version}，包含 macOS M、macOS Intel 和 Windows x64 安装包。`)
}

main().catch((error) => {
  console.error(`[error] ${error.message}`)
  process.exitCode = 1
})
