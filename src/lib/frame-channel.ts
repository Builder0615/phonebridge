/**
 * 把 Tauri Channel 的原始消息规范化成 RGBA 字节。
 *
 * Rust 端发送 `InvokeResponseBody::Raw` 时，Tauri 2 在 JS 层交付的是
 * ArrayBuffer（无论消息走直接执行还是大消息 fetch 通道）；部分运行时/测试
 * 环境会交付 Uint8Array 或其它 TypedArray，因此这里集中兼容这些形态。
 */
export function frameBytesFromChannelMessage(message: unknown): Uint8Array | null {
  if (message instanceof ArrayBuffer) {
    return new Uint8Array(message)
  }
  if (message instanceof Uint8Array) {
    return message
  }
  if (ArrayBuffer.isView(message)) {
    return new Uint8Array(message.buffer, message.byteOffset, message.byteLength)
  }
  return null
}

export interface FramePacket {
  bytes: Uint8Array
  /** Rust ChannelSink 的帧序号；旧格式帧没有序号。 */
  sequence: number | null
}

const FRAME_MAGIC = [0x50, 0x42, 0x46, 0x52] // "PBFR"
const FRAME_HEADER_BYTES = 12

/** 解包有界帧通道消息，同时兼容旧的无头 RGBA 消息。 */
export function framePacketFromChannelMessage(message: unknown): FramePacket | null {
  const bytes = frameBytesFromChannelMessage(message)
  if (!bytes) return null
  if (
    bytes.byteLength >= FRAME_HEADER_BYTES &&
    FRAME_MAGIC.every((value, index) => bytes[index] === value)
  ) {
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
    const low = view.getUint32(4, true)
    const high = view.getUint32(8, true)
    return {
      bytes: bytes.subarray(FRAME_HEADER_BYTES),
      sequence: low + high * 2 ** 32,
    }
  }
  return { bytes, sequence: null }
}
