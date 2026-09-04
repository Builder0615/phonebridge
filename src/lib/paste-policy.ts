/**
 * 粘贴策略（纯逻辑）：大小限制、空文本、敏感内容提示。
 *
 * Spec FR-CLP-004：MVP 处理不超过 maxBytes（默认 32 KiB）的纯文本；
 * 超出时拒绝并提示，绝不静默截断后继续发送。
 * Spec §2.2：粘贴流经「大小、字符和敏感信息策略检查」后才发送。
 * 本模块不读取、不记录剪贴板内容；只对已取到的文本做决策。
 */

export interface PastePolicyConfig {
  maxBytes: number
}

export const DEFAULT_MAX_PASTE_BYTES = 32 * 1024

export type PasteVerdict =
  | { decision: "rejected"; code: "empty"; charCount: 0; byteCount: 0 }
  | { decision: "rejected"; code: "oversized"; charCount: number; byteCount: number; maxBytes: number }
  | { decision: "proceed"; charCount: number; byteCount: number; hints: string[] }

/** UTF-8 字节数（与 Rust 端 TextEncoder 语义一致）。 */
export function utf8ByteLength(text: string): number {
  return new TextEncoder().encode(text).length
}

/** 敏感内容提示（非阻断）：供 UI 在发送前展示确认。 */
export function sensitiveHints(text: string): string[] {
  const hints: string[] = []
  const compact = text.replace(/\s/g, "")
  if (compact.length >= 8 && /^\d{8,}$/.test(compact)) {
    hints.push("内容看起来是长数字串（电话/证件号等），发送前请确认目标输入框")
  }
  if (text.includes("-----BEGIN") || /(password|passwd|token|secret|api[_-]?key)\s*[:=]/i.test(text)) {
    hints.push("内容可能包含口令或令牌，发送前请确认目标输入框")
  }
  return hints
}

export function evaluatePaste(text: string, maxBytes: number): PasteVerdict {
  if (text.length === 0) {
    return { decision: "rejected", code: "empty", charCount: 0, byteCount: 0 }
  }
  const byteCount = utf8ByteLength(text)
  if (byteCount > maxBytes) {
    return {
      decision: "rejected",
      code: "oversized",
      charCount: text.length,
      byteCount,
      maxBytes,
    }
  }
  return {
    decision: "proceed",
    charCount: text.length,
    byteCount,
    hints: sensitiveHints(text),
  }
}