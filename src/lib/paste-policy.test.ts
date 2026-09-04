import { describe, expect, it } from "vitest"
import {
  DEFAULT_MAX_PASTE_BYTES,
  evaluatePaste,
  sensitiveHints,
  utf8ByteLength,
} from "./paste-policy"

describe("utf8ByteLength", () => {
  it("ASCII 一字符一字节", () => {
    expect(utf8ByteLength("hello")).toBe(5)
  })
  it("中文按 UTF-8 3 字节/字", () => {
    expect(utf8ByteLength("你好")).toBe(6)
  })
})

describe("evaluatePaste", () => {
  it("空文本被拒绝", () => {
    const v = evaluatePaste("", DEFAULT_MAX_PASTE_BYTES)
    expect(v).toEqual({ decision: "rejected", code: "empty", charCount: 0, byteCount: 0 })
  })

  it("32 KiB 之内放行并返回字节数", () => {
    const v = evaluatePaste("hello", DEFAULT_MAX_PASTE_BYTES)
    expect(v.decision).toBe("proceed")
    if (v.decision === "proceed") {
      expect(v.byteCount).toBe(5)
      expect(v.charCount).toBe(5)
    }
  })

  it("超过 maxBytes 被拒绝且不静默截断", () => {
    const v = evaluatePaste("x".repeat(40 * 1024), DEFAULT_MAX_PASTE_BYTES)
    expect(v.decision).toBe("rejected")
    if (v.decision === "rejected" && v.code === "oversized") {
      expect(v.maxBytes).toBe(DEFAULT_MAX_PASTE_BYTES)
    }
  })

  it("边界值：恰好 maxBytes 放行", () => {
    const v = evaluatePaste("x".repeat(DEFAULT_MAX_PASTE_BYTES), DEFAULT_MAX_PASTE_BYTES)
    expect(v.decision).toBe("proceed")
  })
})

describe("sensitiveHints", () => {
  it("长数字串给出提示", () => {
    const hints = sensitiveHints("13800138000\n13800138001")
    expect(hints.length).toBeGreaterThan(0)
  })

  it("口令关键字给出提示", () => {
    const hints = sensitiveHints("password=secret")
    expect(hints.some((h) => h.includes("口令"))).toBe(true)
  })

  it("普通文本无提示", () => {
    expect(sensitiveHints("hello world")).toEqual([])
  })
})