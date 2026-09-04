import { describe, expect, it } from "vitest"
import { frameBytesFromChannelMessage, framePacketFromChannelMessage } from "./frame-channel"

describe("frameBytesFromChannelMessage", () => {
  it("converts Tauri Raw ArrayBuffer messages", () => {
    const buffer = new Uint8Array([1, 2, 3, 4]).buffer
    expect(frameBytesFromChannelMessage(buffer)).toEqual(new Uint8Array([1, 2, 3, 4]))
  })

  it("preserves a typed-array view and its offset", () => {
    const buffer = new Uint8Array([9, 5, 6, 8])
    expect(frameBytesFromChannelMessage(buffer.subarray(1, 3))).toEqual(new Uint8Array([5, 6]))
  })

  it("rejects metadata and unrelated values", () => {
    expect(frameBytesFromChannelMessage({ width: 480, height: 1040 })).toBeNull()
    expect(frameBytesFromChannelMessage(null)).toBeNull()
  })

  it("unwraps a bounded frame packet and keeps its sequence", () => {
    const packet = new Uint8Array(14)
    packet.set([0x50, 0x42, 0x46, 0x52])
    const view = new DataView(packet.buffer)
    view.setUint32(4, 7, true)
    view.setUint32(8, 1, true)
    packet.set([9, 10], 12)
    expect(framePacketFromChannelMessage(packet)).toEqual({
      bytes: new Uint8Array([9, 10]),
      sequence: 1 * 2 ** 32 + 7,
    })
  })
})
