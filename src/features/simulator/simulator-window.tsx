import { DeviceCanvas } from "./device-canvas"

/** 从模拟器窗口 label（simulator-<hex(sessionId)>）解析会话 id（hex 可逆编码）。 */
export function extractSessionIdFromLabel(label: string): string | null {
  const PREFIX = "simulator-"
  if (!label.startsWith(PREFIX)) return null
  const hex = label.slice(PREFIX.length)
  if (!hex || hex.length % 2 !== 0 || !/^[0-9a-f]+$/i.test(hex)) return null
  const bytes = new Uint8Array(hex.length / 2)
  for (let i = 0; i < hex.length; i += 2) {
    bytes[i / 2] = Number.parseInt(hex.slice(i, i + 2), 16)
  }
  try {
    const out = new TextDecoder("utf-8", { fatal: true }).decode(bytes)
    return out || null
  } catch {
    return null
  }
}

/** 从模拟器 URL 路由解析会话 id；用于避免部分宿主初始时 label metadata 未就绪。 */
export function extractSessionIdFromSearch(search: string): string | null {
  const encoded = new URLSearchParams(search).get("simulator")
  if (!encoded) return null
  return extractSessionIdFromLabel(`simulator-${encoded}`)
}

/** 模拟器窗口：只显示某台设备的画面；标题栏关闭由 Rust 原生事件统一清理。 */
export function SimulatorWindow({ sessionId }: { sessionId: string }) {
  return (
    <div className="h-screen w-screen overflow-hidden bg-black">
      <DeviceCanvas sessionId={sessionId} />
    </div>
  )
}
