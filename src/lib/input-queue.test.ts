import { describe, expect, it } from "vitest"
import { SerializedInputQueue, type QueuedMove } from "./input-queue"

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((r) => {
    resolve = r
  })
  return { promise, resolve }
}

describe("SerializedInputQueue", () => {
  it("keeps only the newest absolute move while one request is in flight", async () => {
    const sent: QueuedMove[] = []
    const first = deferred<void>()
    let calls = 0
    const queue = new SerializedInputQueue((move) => {
      sent.push(move)
      calls += 1
      return calls === 1 ? first.promise : Promise.resolve()
    })

    queue.queueMove({ dx: 0, dy: 0, abs: { x: 10, y: 10 } }, "absolute")
    await Promise.resolve()
    queue.queueMove({ dx: 0, dy: 0, abs: { x: 20, y: 20 } }, "absolute")
    queue.queueMove({ dx: 0, dy: 0, abs: { x: 30, y: 30 } }, "absolute")
    first.resolve()
    await queue.flushMoves()

    expect(sent).toEqual([
      { dx: 0, dy: 0, abs: { x: 10, y: 10 } },
      { dx: 0, dy: 0, abs: { x: 30, y: 30 } },
    ])
  })

  it("accumulates relative moves and preserves button ordering", async () => {
    const events: string[] = []
    const first = deferred<void>()
    let calls = 0
    const queue = new SerializedInputQueue((move) => {
      events.push(`move:${move.dx},${move.dy}`)
      calls += 1
      return calls === 1 ? first.promise : Promise.resolve()
    })

    queue.queueMove({ dx: 2, dy: 3 }, "relative")
    await Promise.resolve()
    queue.queueMove({ dx: 4, dy: -1 }, "relative")
    const button = queue.enqueue(async () => {
      events.push("button")
    })
    first.resolve()
    await queue.flushMoves()
    await button

    expect(events).toEqual(["move:2,3", "move:4,-1", "button"])
  })

  it("flushes a pending move before the next operation", async () => {
    const events: string[] = []
    const queue = new SerializedInputQueue(async (move) => {
      events.push(`move:${move.dx},${move.dy}`)
    })
    queue.queueMove({ dx: 1, dy: 0 }, "relative")
    await queue.flushMoves()
    await queue.enqueue(async () => {
      events.push("up")
    })
    expect(events).toEqual(["move:1,0", "up"])
  })

  it("does not let moves after a button boundary overtake the button", async () => {
    const events: string[] = []
    const queue = new SerializedInputQueue(async (move) => {
      events.push(`move:${move.dx},${move.dy}`)
    })

    queue.queueMove({ dx: 1, dy: 0 }, "relative")
    const down = queue.enqueue(async () => {
      events.push("down")
    })
    queue.queueMove({ dx: 2, dy: 0 }, "relative")
    await down
    await queue.flushMoves()

    expect(events).toEqual(["move:1,0", "down", "move:2,0"])
  })
})
