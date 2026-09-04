/* global Buffer, console */
// 生成「快投屏」应用图标（1024x1024 PNG），供 `tauri icon` 派生各平台图标。
// 纯 Node + zlib 实现，零依赖；图形在小尺寸下仍保持清晰可辨。
import { deflateSync } from "node:zlib"
import { writeFileSync, mkdirSync } from "node:fs"
import { fileURLToPath } from "node:url"
import path from "node:path"

const SIZE = 1024
const px = Buffer.alloc(SIZE * SIZE * 4)

const setPixel = (x, y, color) => {
  if (x < 0 || x >= SIZE || y < 0 || y >= SIZE) return
  const i = (y * SIZE + x) * 4
  px[i] = color[0]
  px[i + 1] = color[1]
  px[i + 2] = color[2]
  px[i + 3] = 255
}

const mix = (a, b, t) => Math.round(a + (b - a) * t)

// 渐变背景（靛蓝 → 蓝青），表达设备画面与无线投屏的流动感。
for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    const t = (x * 0.35 + y * 0.65) / SIZE
    const glow = Math.max(0, 1 - Math.hypot(x - 760, y - 180) / 780)
    setPixel(x, y, [
      Math.round(14 + 24 * t + 8 * glow),
      Math.round(22 + 74 * t + 46 * glow),
      Math.round(44 + 182 * t + 52 * glow),
    ])
  }
}

const inRoundedRect = (x, y, rect) => {
  if (x < rect.x0 || x > rect.x1 || y < rect.y0 || y > rect.y1) return false
  const cx = x < rect.x0 + rect.r ? rect.x0 + rect.r : x > rect.x1 - rect.r ? rect.x1 - rect.r : x
  const cy = y < rect.y0 + rect.r ? rect.y0 + rect.r : y > rect.y1 - rect.r ? rect.y1 - rect.r : y
  return (x - cx) ** 2 + (y - cy) ** 2 <= rect.r ** 2 ||
    (x >= rect.x0 + rect.r && x <= rect.x1 - rect.r) ||
    (y >= rect.y0 + rect.r && y <= rect.y1 - rect.r)
}

const paintRoundedRect = (rect, colorAt) => {
  for (let y = rect.y0; y <= rect.y1; y++) {
    for (let x = rect.x0; x <= rect.x1; x++) {
      if (inRoundedRect(x, y, rect)) setPixel(x, y, colorAt(x, y))
    }
  }
}

const drawDisk = (cx, cy, radius, color) => {
  const radiusSquared = radius ** 2
  for (let y = cy - radius; y <= cy + radius; y++) {
    for (let x = cx - radius; x <= cx + radius; x++) {
      if ((x - cx) ** 2 + (y - cy) ** 2 <= radiusSquared) setPixel(x, y, color)
    }
  }
}

const drawArc = (cx, cy, radius, start, end, color, width) => {
  const steps = Math.ceil((end - start) * radius / 2)
  for (let i = 0; i <= steps; i++) {
    const angle = start + (end - start) * (i / steps)
    drawDisk(Math.round(cx + Math.cos(angle) * radius), Math.round(cy + Math.sin(angle) * radius), width / 2, color)
  }
}

// 显示器轮廓：白色设备面 + 深色画面，直观表达“把手机画面投到屏幕上”。
const frame = { x0: 164, y0: 148, x1: 860, y1: 744, r: 126 }
paintRoundedRect(frame, (_x, y) => {
  const t = (y - frame.y0) / (frame.y1 - frame.y0)
  return [mix(255, 238, t), mix(255, 249, t), 255]
})

const display = { x0: 224, y0: 208, x1: 800, y1: 626, r: 70 }
paintRoundedRect(display, (x, y) => {
  const t = ((x - display.x0) * 0.35 + (y - display.y0) * 0.65) /
    ((display.x1 - display.x0) * 0.35 + (display.y1 - display.y0) * 0.65)
  return [mix(15, 37, t), mix(23, 99, t), mix(42, 235, t)]
})

// 标准投屏波纹：左下角圆点向右上方扩散，缩小到 32px 时仍可识别。
const signal = [248, 250, 252]
const signalCenter = { x: 310, y: 510 }
drawDisk(signalCenter.x, signalCenter.y, 28, signal)
drawArc(signalCenter.x, signalCenter.y, 126, -Math.PI / 2, 0, signal, 30)
drawArc(signalCenter.x, signalCenter.y, 218, -Math.PI / 2, 0, signal, 30)

// 显示器支架与底座让图形从“播放按钮”中区分出来。
paintRoundedRect({ x0: 468, y0: 696, x1: 556, y1: 796, r: 28 }, () => [248, 250, 252])
paintRoundedRect({ x0: 348, y0: 764, x1: 676, y1: 846, r: 42 }, () => [248, 250, 252])

// PNG 编码
const chunk = (type, data) => {
  const len = Buffer.alloc(4)
  len.writeUInt32BE(data.length)
  const body = Buffer.concat([Buffer.from(type, "ascii"), data])
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(body) >>> 0)
  return Buffer.concat([len, body, crc])
}

const crcTable = []
for (let n = 0; n < 256; n++) {
  let c = n
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
  crcTable[n] = c >>> 0
}
function crc32(buf) {
  let c = 0xffffffff
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8)
  return (c ^ 0xffffffff) >>> 0
}

const ihdr = Buffer.alloc(13)
ihdr.writeUInt32BE(SIZE, 0)
ihdr.writeUInt32BE(SIZE, 4)
ihdr[8] = 8 // bit depth
ihdr[9] = 6 // color type RGBA

const raw = Buffer.alloc((SIZE * 4 + 1) * SIZE)
for (let y = 0; y < SIZE; y++) {
  raw[y * (SIZE * 4 + 1)] = 0 // filter none
  px.copy(raw, y * (SIZE * 4 + 1) + 1, y * SIZE * 4, (y + 1) * SIZE * 4)
}

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
])

const here = path.dirname(fileURLToPath(import.meta.url))
const outDir = path.join(here, "..", "assets")
mkdirSync(outDir, { recursive: true })
const out = path.join(outDir, "app-icon.png")
writeFileSync(out, png)
console.log(`wrote ${out} (${png.length} bytes)`)
