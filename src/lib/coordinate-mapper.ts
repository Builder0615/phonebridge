/**
 * InputMapper：画布坐标 → 手机坐标的纯逻辑模块（Spec §6.1）。
 *
 * 规则（与 Spec 一致）：
 * 1. 先由视口与画面尺寸计算 contentRect（等比缩放 + 居中 + 黑边）；
 * 2. scale = min(viewportW / displayW, viewportH / displayH)；
 * 3. 鼠标点先减 contentRect 原点，再除以缩放，最后 clamp 到画面范围；
 * 4. 旋转或换源时清空旧的 hover/drag 状态，避免跳点；
 * 5. 相对位移按 HID 描述符上限拆分，避免单次报告越界；
 * 6. 移动事件可合并，按下/释放事件不可丢弃或重排。
 *
 * 本模块不依赖 DOM / Tauri，可用 vitest 直接测试。
 */

export interface Size {
  width: number
  height: number
}

export interface Point {
  x: number
  y: number
}

export interface Rect {
  x: number
  y: number
  width: number
  height: number
}

export type RotationDeg = 0 | 90 | 180 | 270

/** 单条 HID 相对鼠标报告的最大位移（int8，-127..127）。 */
export const HID_MAX_DELTA = 127

/** 移动事件：只能合并连续移动，按下/释放由按钮事件承载，不允许排序或丢弃。 */
export interface HidMoveChunk {
  dx: number
  dy: number
}

/** 旋转后的显示尺寸（90/270 时交换宽高）。 */
export function rotatedSize(frame: Size, rotation: RotationDeg): Size {
  if (rotation === 90 || rotation === 270) {
    return { width: frame.height, height: frame.width }
  }
  return { width: frame.width, height: frame.height }
}

/** 等比缩放 + 居中后的内容矩形；非法尺寸返回空矩形。 */
export function computeContentRect(viewport: Size, display: Size): Rect {
  if (
    viewport.width <= 0 ||
    viewport.height <= 0 ||
    display.width <= 0 ||
    display.height <= 0
  ) {
    return { x: 0, y: 0, width: 0, height: 0 }
  }
  const scale = Math.min(viewport.width / display.width, viewport.height / display.height)
  const width = display.width * scale
  const height = display.height * scale
  return {
    x: (viewport.width - width) / 2,
    y: (viewport.height - height) / 2,
    width,
    height,
  }
}

/**
 * 把「显示坐标」还原为「原始视频坐标」。
 * 约定：display = rotate(video, rotation)，这里给出逆变换。
 */
export function videoPointFromDisplay(
  displayPoint: Point,
  frame: Size,
  rotation: RotationDeg,
): Point {
  const { width: fw, height: fh } = frame
  const { x: u, y: v } = displayPoint
  switch (rotation) {
    case 90:
      // 顺时针旋转 90°：u = fh-1-y, v = x → x=v, y=fh-1-u
      return { x: v, y: fh - 1 - u }
    case 180:
      // u = fw-1-x, v = fh-1-y
      return { x: fw - 1 - u, y: fh - 1 - v }
    case 270:
      // 顺时针旋转 270°：u = y, v = fw-1-x → x=fw-1-v, y=u
      return { x: fw - 1 - v, y: u }
    default:
      return { x: u, y: v }
  }
}

const clamp = (v: number, max: number): number => Math.max(0, Math.min(max, Math.round(v)))

/**
 * 屏幕坐标 → 手机原始视频坐标。
 * 点在黑边（contentRect 之外）或画面非法时返回 null，调用方应忽略该事件。
 */
export function screenToPhone(
  point: Point,
  viewport: Size,
  frame: Size,
  rotation: RotationDeg,
): Point | null {
  if (frame.width <= 0 || frame.height <= 0 || viewport.width <= 0 || viewport.height <= 0) {
    return null
  }
  const display = rotatedSize(frame, rotation)
  const rect = computeContentRect(viewport, display)
  if (rect.width <= 0 || rect.height <= 0) return null

  const px = point.x - rect.x
  const py = point.y - rect.y
  // 黑边排除在可点击区域之外
  if (px < 0 || py < 0 || px > rect.width || py > rect.height) return null

  const scale = rect.width / display.width
  const displayPoint: Point = { x: px / scale, y: py / scale }
  const video = videoPointFromDisplay(displayPoint, frame, rotation)
  return { x: clamp(video.x, frame.width - 1), y: clamp(video.y, frame.height - 1) }
}

/**
 * 相对位移拆分：任意大位移拆分为不超过 HID_MAX_DELTA 的多条报告。
 * 保证总位移与输入一致，且上报为零的 chunk 会被剔除。
 */
export function toRelativeChunks(
  from: Point,
  to: Point,
  maxDelta: number = HID_MAX_DELTA,
): HidMoveChunk[] {
  const limit = Math.max(1, Math.floor(maxDelta))
  const totalDx = to.x - from.x
  const totalDy = to.y - from.y
  if (totalDx === 0 && totalDy === 0) return []

  const chunks: HidMoveChunk[] = []
  let dx = totalDx
  let dy = totalDy
  let guard = 0
  while ((dx !== 0 || dy !== 0) && guard < 65536) {
    const cdx = Math.abs(dx) > limit ? Math.sign(dx) * limit : dx
    const cdy = Math.abs(dy) > limit ? Math.sign(dy) * limit : dy
    chunks.push({ dx: cdx, dy: cdy })
    dx -= cdx
    dy -= cdy
    guard += 1
  }
  return chunks
}

/**
 * 将桌面指针的 CSS 位移量量化为整数 HID 位移，并保留小数余量。
 *
 * iOS 的 HOGP 鼠标是相对设备，不能接收“点击手机视频中的绝对坐标”。
 * 因此 iOS 应使用桌面指针的实际位移；WebView 的 client 坐标可能包含小数，
 * 余量必须跨事件保留，否则高 DPI/缩放场景会逐渐丢失移动量。
 */
export function cssDeltaToRelativeChunks(
  from: Point,
  to: Point,
  remainder: Point = { x: 0, y: 0 },
): { chunks: HidMoveChunk[]; remainder: Point } {
  const rawDx = to.x - from.x + remainder.x
  const rawDy = to.y - from.y + remainder.y
  const dx = Math.trunc(rawDx)
  const dy = Math.trunc(rawDy)
  return {
    chunks: dx !== 0 || dy !== 0 ? toRelativeChunks({ x: 0, y: 0 }, { x: dx, y: dy }) : [],
    remainder: { x: rawDx - dx, y: rawDy - dy },
  }
}

/**
 * 移动事件合并：连续移动可合并为一条，减少高频上报；
 * 按下/释放事件必须与其它事件分列，不允许移除或重排。
 */
export function mergeConsecutiveMoves(
  events: ReadonlyArray<{ kind: "move" | "press" | "release"; dx?: number; dy?: number }>,
): Array<{ kind: "move" | "press" | "release"; dx?: number; dy?: number }> {
  const out: Array<{ kind: "move" | "press" | "release"; dx?: number; dy?: number }> = []
  let pending: { dx: number; dy: number } | null = null

  const flush = () => {
    if (pending) {
      out.push({ kind: "move", ...pending })
      pending = null
    }
  }

  for (const e of events) {
    if (e.kind === "move") {
      pending = pending
        ? { dx: pending.dx + (e.dx ?? 0), dy: pending.dy + (e.dy ?? 0) }
        : { dx: e.dx ?? 0, dy: e.dy ?? 0 }
    } else {
      flush()
      out.push(e)
    }
  }
  flush()
  return out
}

/**
 * 会话内的有状态映射器：维护上一次目标点，输出相对 HID 报告序列。
 * setViewport / setSource 会清空 hover/drag 状态（消除旋转/换源跳点）。
 */
export class InputMapper {
  private lastPhone: Point | null = null
  private viewport: Size = { width: 0, height: 0 }
  private frame: Size = { width: 0, height: 0 }
  private rotation: RotationDeg = 0

  get hasView(): boolean {
    return this.frame.width > 0 && this.frame.height > 0
  }

  setViewport(viewport: Size): void {
    this.viewport = viewport
    this.lastPhone = null
  }

  /** frame 变化（含旋转）时清空上一次目标点。 */
  setSource(frame: Size, rotation: RotationDeg): void {
    this.frame = frame
    this.rotation = rotation
    this.lastPhone = null
  }

  /** 把当前鼠标屏幕位置转为手机坐标；黑边或非法返回 null。 */
  mapPoint(point: Point): Point | null {
    if (!this.hasView) return null
    return screenToPhone(point, this.viewport, this.frame, this.rotation)
  }

  /** 输出从上一个目标点到当前点的相对位移报告（自动拆分 + 零位移剔除）。 */
  moveTo(point: Point): HidMoveChunk[] {
    const phone = this.mapPoint(point)
    if (!phone) return []
    if (this.lastPhone === null) {
      this.lastPhone = phone
      return []
    }
    const chunks = toRelativeChunks(this.lastPhone, phone)
    this.lastPhone = phone
    return chunks
  }

  /** 供拖拽等场景直接给出绝对手机坐标的移动。 */
  moveToPhone(phone: Point): HidMoveChunk[] {
    if (this.lastPhone === null) {
      this.lastPhone = { ...phone }
      return []
    }
    const chunks = toRelativeChunks(this.lastPhone, phone)
    this.lastPhone = { ...phone }
    return chunks
  }

  reset(): void {
    this.lastPhone = null
  }
}
