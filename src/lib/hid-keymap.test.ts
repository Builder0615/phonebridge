import { describe, expect, it } from "vitest"
import { isModifierEvent, matchAppShortcut, normalizeKeyEvent } from "./hid-keymap"

describe("normalizeKeyEvent", () => {
  it("普通按键保留 code/key/修饰键/repeat", () => {
    const stroke = normalizeKeyEvent({
      code: "KeyA",
      key: "a",
      ctrlKey: true,
      shiftKey: false,
      altKey: false,
      metaKey: false,
      repeat: false,
    })
    expect(stroke).toEqual({
      code: "KeyA",
      key: "a",
      modifiers: { ctrl: true, shift: false, alt: false, meta: false },
      repeat: false,
    })
  })

  it("箭头键与回车", () => {
    const up = normalizeKeyEvent({
      code: "ArrowUp",
      key: "ArrowUp",
      ctrlKey: false,
      shiftKey: false,
      altKey: false,
      metaKey: false,
      repeat: true,
    })
    expect(up.code).toBe("ArrowUp")
    expect(up.repeat).toBe(true)
  })
})

describe("isModifierEvent", () => {
  it("修饰键自身事件应被忽略", () => {
    for (const code of ["ControlLeft", "ShiftRight", "MetaLeft", "AltRight", "CapsLock"]) {
      expect(isModifierEvent({ code })).toBe(true)
    }
    expect(isModifierEvent({ code: "KeyB" })).toBe(false)
  })
})

describe("matchAppShortcut", () => {
  const base = {
    code: "",
    key: "",
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    metaKey: false,
    repeat: false,
  }

  it("Esc 退出全屏/停止拖拽", () => {
    expect(matchAppShortcut({ ...base, code: "Escape" })).toEqual({ type: "escape" })
  })

  it("Ctrl+Alt+Pause 暂停/恢复输入", () => {
    expect(
      matchAppShortcut({ ...base, code: "Pause", ctrlKey: true, altKey: true }),
    ).toEqual({ type: "pause_toggle" })
  })

  it("Ctrl+Alt+Q 紧急释放", () => {
    expect(matchAppShortcut({ ...base, code: "KeyQ", ctrlKey: true, altKey: true })).toEqual({
      type: "release_all",
    })
  })

  it("未匹配返回 null", () => {
    expect(matchAppShortcut({ ...base, code: "KeyV", ctrlKey: true })).toEqual({ type: null })
  })
})