import { describe, expect, it } from "vitest"
import {
  computeContentRect,
  cssDeltaToRelativeChunks,
  HID_MAX_DELTA,
  InputMapper,
  mergeConsecutiveMoves,
  rotatedSize,
  screenToPhone,
  toRelativeChunks,
  videoPointFromDisplay,
  type RotationDeg,
} from "./coordinate-mapper"

describe("rotatedSize", () => {
  it("旋转 90/270 时交换宽高", () => {
    expect(rotatedSize({ width: 1080, height: 1920 }, 0)).toEqual({ width: 1080, height: 1920 })
    expect(rotatedSize({ width: 1080, height: 1920 }, 90)).toEqual({ width: 1920, height: 1080 })
    expect(rotatedSize({ width: 1080, height: 1920 }, 180)).toEqual({ width: 1080, height: 1920 })
    expect(rotatedSize({ width: 1080, height: 1920 }, 270)).toEqual({ width: 1920, height: 1080 })
  })
})

describe("computeContentRect", () => {
  it("等比缩放 + 居中（宽适配）", () => {
    const rect = computeContentRect({ width: 1200, height: 900 }, { width: 400, height: 800 })
    // scale = min(1200/400, 900/800) = 1.125 → w=450, h=900
    expect(rect.width).toBeCloseTo(450)
    expect(rect.height).toBeCloseTo(900)
    expect(rect.x).toBeCloseTo((1200 - 450) / 2)
    expect(rect.y).toBeCloseTo(0)
  })

  it("非法尺寸返回空矩形", () => {
    expect(computeContentRect({ width: 0, height: 100 }, { width: 100, height: 100 })).toEqual({
      x: 0,
      y: 0,
      width: 0,
      height: 0,
    })
    expect(computeContentRect({ width: 100, height: 100 }, { width: 0, height: 100 })).toEqual({
      x: 0,
      y: 0,
      width: 0,
      height: 0,
    })
  })
})

describe("videoPointFromDisplay", () => {
  const frame = { width: 100, height: 200 }

  it("0° 恒等", () => {
    expect(videoPointFromDisplay({ x: 10, y: 20 }, frame, 0)).toEqual({ x: 10, y: 20 })
  })

  it("90° 逆变换：display(10, 30) → video(30, 200-1-10)", () => {
    expect(videoPointFromDisplay({ x: 10, y: 30 }, frame, 90)).toEqual({ x: 30, y: 189 })
  })

  it("180° 逆变换", () => {
    expect(videoPointFromDisplay({ x: 10, y: 20 }, frame, 180)).toEqual({ x: 89, y: 179 })
  })

  it("270° 逆变换：display(10, 20) → video(100-1-20, 10)", () => {
    expect(videoPointFromDisplay({ x: 10, y: 20 }, frame, 270)).toEqual({ x: 79, y: 10 })
  })

  it("逆变换与标准旋转互为往返", () => {
    // forward 为标准旋转公式（display = rotate(video)）
    const forward = (p: { x: number; y: number }, r: RotationDeg): { x: number; y: number } => {
      switch (r) {
        case 90:
          return { x: frame.height - 1 - p.y, y: p.x }
        case 180:
          return { x: frame.width - 1 - p.x, y: frame.height - 1 - p.y }
        case 270:
          return { x: p.y, y: frame.width - 1 - p.x }
        default:
          return { x: p.x, y: p.y }
      }
    }
    for (const r of [0, 90, 180, 270] as const) {
      const display = rotatedSize(frame, r)
      const mid = { x: Math.floor(display.width / 2), y: Math.floor(display.height / 2) }
      const video = videoPointFromDisplay(mid, frame, r)
      expect(forward(video, r)).toEqual(mid)
    }
  })
})

describe("screenToPhone", () => {
  // 视口 1000x800，画面 500x500 → scale=1.6，内容区 800x800，左右各 100px 黑边
  const viewport = { width: 1000, height: 800 }
  const frame = { width: 500, height: 500 }

  it("黑边之外返回 null（不可点击区域）", () => {
    expect(screenToPhone({ x: 50, y: 400 }, viewport, frame, 0)).toBeNull()
    expect(screenToPhone({ x: 950, y: 400 }, viewport, frame, 0)).toBeNull()
    expect(screenToPhone({ x: 0, y: 0 }, viewport, frame, 0)).toBeNull()
  })

  it("内容区中心映射到画面中心附近", () => {
    const p = screenToPhone({ x: 500, y: 400 }, viewport, frame, 0)
    expect(p).not.toBeNull()
    expect(p!.x).toBeGreaterThan(240)
    expect(p!.y).toBeGreaterThan(240)
  })

  it("坐标被 clamp 到 [0, width-1]×[0, height-1]", () => {
    // 内容区右边缘（900,400）→ u=500 → clamp 499
    const p = screenToPhone({ x: 900, y: 400 }, viewport, frame, 0)!
    expect(p.x).toBeLessThanOrEqual(499)
    expect(p.y).toBeLessThanOrEqual(499)
    // 内容区底边缘（500,800）→ v=500 → clamp 499
    const q = screenToPhone({ x: 500, y: 800 }, viewport, frame, 0)!
    expect(q.x).toBeLessThanOrEqual(499)
    expect(q.y).toBeLessThanOrEqual(499)
  })

  it("旋转偏移后的映射仍然落在画面内", () => {
    for (const r of [0, 90, 180, 270] as const) {
      const p = screenToPhone({ x: 500, y: 400 }, viewport, frame, r)
      expect(p).not.toBeNull()
      expect(p!.x).toBeGreaterThanOrEqual(0)
      expect(p!.x).toBeLessThan(500)
      expect(p!.y).toBeGreaterThanOrEqual(0)
      expect(p!.y).toBeLessThan(500)
    }
  })

  it("非法画面尺寸返回 null", () => {
    expect(screenToPhone({ x: 500, y: 400 }, viewport, { width: 0, height: 0 }, 0)).toBeNull()
  })
})

describe("toRelativeChunks", () => {
  it("零位移返回空数组", () => {
    expect(toRelativeChunks({ x: 5, y: 5 }, { x: 5, y: 5 })).toEqual([])
  })

  it("小位移单条返回", () => {
    expect(toRelativeChunks({ x: 0, y: 0 }, { x: 10, y: -5 })).toEqual([{ dx: 10, dy: -5 }])
  })

  it("大位移按 HID_MAX_DELTA 拆分且方向正确", () => {
    const chunks = toRelativeChunks({ x: 0, y: 0 }, { x: 300, y: -300 })
    expect(chunks.length).toBe(3)
    expect(chunks[0]).toEqual({ dx: 127, dy: -127 })
    expect(chunks[1]).toEqual({ dx: 127, dy: -127 })
    expect(chunks[2]).toEqual({ dx: 46, dy: -46 })
    const sum = chunks.reduce((a, c) => ({ dx: a.dx + c.dx, dy: a.dy + c.dy }), { dx: 0, dy: 0 })
    expect(sum).toEqual({ dx: 300, dy: -300 })
  })

  it("每块都不超过上限", () => {
    const chunks = toRelativeChunks({ x: 0, y: 0 }, { x: 10000, y: 0 })
    for (const c of chunks) {
      expect(Math.abs(c.dx)).toBeLessThanOrEqual(HID_MAX_DELTA)
      expect(Math.abs(c.dy)).toBeLessThanOrEqual(HID_MAX_DELTA)
    }
  })
})

describe("cssDeltaToRelativeChunks", () => {
  it("uses desktop CSS movement directly and preserves fractional remainder", () => {
    const first = cssDeltaToRelativeChunks({ x: 10, y: 10 }, { x: 10.6, y: 9.6 })
    expect(first.chunks).toEqual([])
    expect(first.remainder.x).toBeCloseTo(0.6)
    expect(first.remainder.y).toBeCloseTo(-0.4)

    const second = cssDeltaToRelativeChunks(
      { x: 10.6, y: 9.6 },
      { x: 11.1, y: 8.9 },
      first.remainder,
    )
    expect(second.chunks).toEqual([{ dx: 1, dy: -1 }])
    expect(second.remainder.x).toBeCloseTo(0.1)
    expect(second.remainder.y).toBeCloseTo(-0.1)
  })
})

describe("mergeConsecutiveMoves", () => {
  it("连续移动被合并，按下/释放不被重排或丢弃", () => {
    const merged = mergeConsecutiveMoves([
      { kind: "move", dx: 1, dy: 0 },
      { kind: "move", dx: 2, dy: 3 },
      { kind: "press" },
      { kind: "move", dx: 5, dy: 5 },
      { kind: "release" },
    ])
    expect(merged).toEqual([
      { kind: "move", dx: 3, dy: 3 },
      { kind: "press" },
      { kind: "move", dx: 5, dy: 5 },
      { kind: "release" },
    ])
  })

  it("空输入返回空", () => {
    expect(mergeConsecutiveMoves([])).toEqual([])
  })
})

describe("InputMapper", () => {
  it("首次移动不产生报告（仅建立基准点）", () => {
    const m = new InputMapper()
    m.setViewport({ width: 1000, height: 1000 })
    m.setSource({ width: 500, height: 500 }, 0)
    expect(m.moveTo({ x: 500, y: 500 })).toEqual([])
  })

  it("后续移动产生相对报告并累积基准", () => {
    const m = new InputMapper()
    m.setViewport({ width: 1000, height: 1000 })
    m.setSource({ width: 1000, height: 1000 }, 0)
    m.moveTo({ x: 400, y: 400 })
    const chunks = m.moveTo({ x: 500, y: 400 })
    expect(chunks).toEqual([{ dx: 100, dy: 0 }])
    expect(m.moveTo({ x: 500, y: 400 })).toEqual([])
  })

  it("换源（旋转）后清空基准，避免跳点", () => {
    const m = new InputMapper()
    m.setViewport({ width: 1000, height: 1000 })
    m.setSource({ width: 500, height: 500 }, 0)
    m.moveTo({ x: 500, y: 500 })
    m.setSource({ width: 500, height: 500 }, 90)
    expect(m.moveTo({ x: 500, y: 500 })).toEqual([])
  })

  it("黑边移动不产生报告也不污染基准", () => {
    const m = new InputMapper()
    m.setViewport({ width: 1000, height: 800 })
    m.setSource({ width: 500, height: 500 }, 0)
    // 内容区中心（500,400）建立基准
    expect(m.moveTo({ x: 500, y: 400 })).toEqual([])
    // 左、右黑边内移动 → 无报告
    expect(m.moveTo({ x: 2, y: 400 })).toEqual([])
    expect(m.moveTo({ x: 998, y: 400 })).toEqual([])
    // 回到内容区 → 产生相对报告（基准未被黑边移动污染）
    const chunks = m.moveTo({ x: 600, y: 400 })
    expect(chunks.length).toBeGreaterThan(0)
  })

  it("moveToPhone 直接消费绝对手机坐标", () => {
    const m = new InputMapper()
    m.setViewport({ width: 1000, height: 1000 })
    m.setSource({ width: 1000, height: 1000 }, 0)
    m.moveToPhone({ x: 10, y: 10 })
    const chunks = m.moveToPhone({ x: 12, y: 14 })
    expect(chunks).toEqual([{ dx: 2, dy: 4 }])
  })
})
