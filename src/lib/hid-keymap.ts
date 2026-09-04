/**
 * 键盘事件归一化：把 DOM KeyboardEvent 转为发给 Rust 的结构化 HidKeyStroke。
 *
 * Rust 端是 HID usage 映射的唯一事实来源；本模块只负责「事件 → 结构化按键」，
 * 过滤掉纯修饰键按下（避免空报告），并保留 repeat 语义。
 */

import type { HidKeyStroke } from "./types"

/** 修饰键自身的 code，按下时不应产生独立按键报告。 */
const MODIFIER_CODES = new Set([
  "ControlLeft",
  "ControlRight",
  "ShiftLeft",
  "ShiftRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
  "CapsLock",
])

export interface KeyEventLike {
  code: string
  key: string
  ctrlKey: boolean
  shiftKey: boolean
  altKey: boolean
  metaKey: boolean
  repeat: boolean
}

/** 是否为纯修饰键事件（应被忽略）。 */
export function isModifierEvent(e: Pick<KeyEventLike, "code">): boolean {
  return MODIFIER_CODES.has(e.code)
}

export function normalizeKeyEvent(e: KeyEventLike): HidKeyStroke {
  return {
    code: e.code,
    key: e.key,
    modifiers: {
      ctrl: e.ctrlKey,
      shift: e.shiftKey,
      alt: e.altKey,
      meta: e.metaKey,
    },
    repeat: e.repeat,
  }
}

/**
 * `Ctrl+V` 之外的特殊快捷键判定（Spec §4.3）：
 * - Esc：退出全屏或停止拖拽（UI 层处理，不转发）；
 * - Ctrl+Alt+Pause：暂停/恢复输入转发；
 * - Ctrl+Alt+Q：释放全部按键/鼠标按钮。
 */
export interface ShortcutMatch {
  type: "escape" | "pause_toggle" | "release_all" | null
}

export function matchAppShortcut(e: KeyEventLike): ShortcutMatch {
  if (e.code === "Escape") return { type: "escape" }
  if (e.ctrlKey && e.altKey && e.code === "Pause") return { type: "pause_toggle" }
  if (e.ctrlKey && e.altKey && e.code === "KeyQ") return { type: "release_all" }
  return { type: null }
}