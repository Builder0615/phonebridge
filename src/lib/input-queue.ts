/**
 * Ordered input queue for a simulator canvas.
 *
 * Button/key/wheel operations are never reordered. Pointer moves are treated
 * as disposable state: relative moves accumulate, while absolute moves keep
 * only the newest point. At most one move operation is in flight, so a slow
 * WDA/BLE bridge cannot build an unbounded queue of stale coordinates.
 */

export interface QueuedMove {
  dx: number
  dy: number
  abs?: { x: number; y: number } | null
  /** Source dimensions captured with an absolute point. */
  source?: { width: number; height: number } | null
}

export type MoveMode = "relative" | "absolute"

interface MoveSegment {
  mode: MoveMode
  pending: QueuedMove | null
  closed: boolean
  done: boolean
  promise: Promise<void>
}

export class SerializedInputQueue {
  private tail: Promise<void> = Promise.resolve()
  private currentSegment: MoveSegment | null = null

  constructor(private readonly sendMove: (move: QueuedMove) => Promise<void>) {}

  /** Enqueue a non-disposable operation in strict arrival order. */
  enqueue<T>(operation: () => Promise<T>): Promise<T> {
    // Close the current move segment before appending a button/key/wheel.
    // Moves arriving after this synchronous boundary are scheduled after the
    // operation, so a continuously moving pointer cannot starve a click.
    const segment = this.currentSegment
    if (segment && !segment.done) {
      segment.closed = true
      this.currentSegment = null
    }
    const next = this.tail.then(operation)
    this.tail = next.then(
      () => undefined,
      () => undefined,
    )
    return next
  }

  /**
   * Add a pointer move. Relative motion is additive; absolute motion is
   * latest-wins. Starting the pump here also means hover/drag input does not
   * wait for the next React render.
   */
  queueMove(move: QueuedMove, mode: MoveMode): void {
    if (mode === "absolute" && !move.abs) return

    const segment = this.currentSegment
    if (segment && !segment.closed && !segment.done && segment.mode === mode) {
      segment.pending = mergeMove(segment.pending, move, mode)
      return
    }

    const previous = this.tail
    let resolveSegment!: () => void
    const promise = new Promise<void>((resolve) => {
      resolveSegment = resolve
    })
    const next: MoveSegment = {
      mode,
      pending: mergeMove(null, move, mode),
      closed: false,
      done: false,
      promise,
    }
    this.currentSegment = next
    void previous.then(async () => {
      try {
        while (next.pending) {
          const current = next.pending
          next.pending = null
          try {
            await this.sendMove(current)
          } catch {
            // A disconnected device must not prevent a later release/stop
            // operation from reaching the serialized tail.
          }
          // If the segment is still open, a move collected while the native
          // request was in flight is the newest state and is sent next. Once
          // closed by enqueue(), no later move can enter this segment.
        }
      } finally {
        next.done = true
        resolveSegment()
      }
    })
    this.tail = promise
  }

  /** Flush all moves known at this point before a button/key operation. */
  flushMoves(): Promise<void> {
    const segment = this.currentSegment
    return segment && !segment.done ? segment.promise : this.tail
  }

  /** Drop moves which are no longer valid after blur/stop. */
  clearMoves(): void {
    const segment = this.currentSegment
    if (!segment || segment.done) return
    segment.pending = null
    segment.closed = true
    this.currentSegment = null
  }
}

function mergeMove(
  previous: QueuedMove | null,
  move: QueuedMove,
  mode: MoveMode,
): QueuedMove {
  if (mode === "absolute") {
    const next: QueuedMove = {
      dx: 0,
      dy: 0,
      abs: move.abs ? { ...move.abs } : null,
    }
    if (move.source) next.source = { ...move.source }
    return next
  }
  return {
    dx: (previous?.dx ?? 0) + move.dx,
    dy: (previous?.dy ?? 0) + move.dy,
  }
}
