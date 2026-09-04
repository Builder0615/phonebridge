import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type MouseEvent as ReactMouseEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type ClipboardEvent as ReactClipboardEvent,
  type WheelEvent as ReactWheelEvent,
} from "react"
import * as api from "@/lib/api"
import {
  InputMapper,
  cssDeltaToRelativeChunks,
  computeContentRect,
  rotatedSize,
  toRelativeChunks,
  type Point,
} from "@/lib/coordinate-mapper"
import { isModifierEvent, matchAppShortcut, normalizeKeyEvent } from "@/lib/hid-keymap"
import { SerializedInputQueue } from "@/lib/input-queue"
import { useSession } from "@/lib/session-context"
import type { PointerButtonEvent } from "@/lib/types"
import { WebGLFrameRenderer } from "@/lib/webgl-frame-renderer"

interface PendingMove {
  dx: number
  dy: number
  abs?: Point | null
  source?: { width: number; height: number } | null
}
interface Viewport {
  width: number
  height: number
}

/** 模拟器画布：显示某会话的帧画面并转发输入（Spec v1.2 §4.2）。 */
export function DeviceCanvas({ sessionId }: { sessionId: string }) {
  const { details, prefs, paste, releaseAll } = useSession()
  const detail = details[sessionId] ?? { state: null, hid: null, mirror: null }
  const mirror = detail.mirror
  // 状态事件和 HID 事件是两条独立的事件流。新打开的模拟器窗口可能
  // 先收到 state(control_ready)，再收到 hid(status)，两者都应允许输入。
  const controlConnected =
    detail.hid?.control === "connected" || detail.state?.state === "control_ready"
  const controlTransport = detail.hid?.controlTransport || (sessionId.startsWith("android") ? "scrcpy" : "ble")
  const isIosBle = !sessionId.startsWith("android") && controlTransport !== "usb_wda"
  const isIosUsb = !sessionId.startsWith("android") && controlTransport === "usb_wda"

  const containerRef = useRef<HTMLDivElement>(null)
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const mapperRef = useRef(new InputMapper())
  const [viewport, setViewport] = useState<Viewport>({ width: 0, height: 0 })
  const [paused, setPausedState] = useState(false)
  const [frameSize, setFrameSize] = useState({ width: 640, height: 480 })
  const [frameActive, setFrameActive] = useState(false)

  const rafRef = useRef<number | null>(null)
  const pendingRef = useRef<PendingMove[]>([])
  const absRef = useRef<Point | null>(null)
  const dragRef = useRef(false)
  const frameBufRef = useRef<{ bytes: Uint8Array; w: number; h: number } | null>(null)
  const frameSizeRef = useRef(frameSize)
  frameSizeRef.current = frameSize
  const focusedRef = useRef(false)
  const activePointerRef = useRef<number | null>(null)
  const lastPasteTriggerRef = useRef(0)
  // iOS HOGP exposes a relative mouse, not an absolute touchscreen. Keep the
  // last CSS pointer position and fractional remainder so the report units
  // follow the pointer's actual movement instead of being derived from the
  // downscaled video pixels. The latter made a 480px video frame amplify a
  // normal desktop movement and caused the phone pointer to drift away from
  // the cursor shown in the simulator.
  const relativePointerRef = useRef<Point | null>(null)
  const relativeRemainderRef = useRef<Point>({ x: 0, y: 0 })
  const inputQueue = useMemo(
    () => new SerializedInputQueue(async (move) => {
      const source = move.source ?? frameSizeRef.current
      if (move.abs) {
        await api.hidPointerMove(sessionId, {
          dx: 0,
          dy: 0,
          absX: move.abs.x,
          absY: move.abs.y,
          sourceWidth: source.width,
          sourceHeight: source.height,
        })
        return
      }
      // The browser can coalesce many pointer events while the native bridge
      // is busy. Never let a large accumulated BLE delta be clamped away by
      // the int8 HID report; preserve the complete displacement as an
      // ordered sequence of bounded reports.
      for (const chunk of toRelativeChunks({ x: 0, y: 0 }, { x: move.dx, y: move.dy })) {
        await api.hidPointerMove(sessionId, {
          dx: chunk.dx,
          dy: chunk.dy,
          absX: null,
          absY: null,
          sourceWidth: source.width,
          sourceHeight: source.height,
        })
      }
    }),
    [sessionId],
  )
  const webglRendererRef = useRef<WebGLFrameRenderer | null>(null)
  const canvas2dContextRef = useRef<CanvasRenderingContext2D | null>(null)

  useEffect(() => {
    const el = containerRef.current
    if (!el) return
    const ro = new ResizeObserver(() => {
      const rect = el.getBoundingClientRect()
      const size = { width: rect.width, height: rect.height }
      setViewport(size)
      mapperRef.current.setViewport(size)
      relativePointerRef.current = null
      relativeRemainderRef.current = { x: 0, y: 0 }
    })
    ro.observe(el)
    const rect = el.getBoundingClientRect()
    setViewport({ width: rect.width, height: rect.height })
    mapperRef.current.setViewport({ width: rect.width, height: rect.height })
    return () => ro.disconnect()
  }, [])

  useEffect(() => {
    if (!controlConnected || paused) {
      relativePointerRef.current = null
      relativeRemainderRef.current = { x: 0, y: 0 }
    }
  }, [controlConnected, paused])

  // Pointer Lock keeps the host cursor inside the simulator while iOS is
  // controlled through its relative BLE mouse. Without it, leaving the
  // canvas discards the host-side baseline while the iPhone cursor remains
  // elsewhere, so the next entry can never be position-consistent.
  useEffect(() => {
    const onPointerLockChange = () => {
      if (document.pointerLockElement !== containerRef.current) {
        relativePointerRef.current = null
        relativeRemainderRef.current = { x: 0, y: 0 }
      }
    }
    document.addEventListener("pointerlockchange", onPointerLockChange)
    return () => document.removeEventListener("pointerlockchange", onPointerLockChange)
  }, [])

  useEffect(() => {
    // Changing the gain must not carry a fractional remainder calculated with
    // the previous gain into the next report.
    relativePointerRef.current = null
    relativeRemainderRef.current = { x: 0, y: 0 }
  }, [prefs.iosPointerScale])

  useEffect(() => {
    if (mirror) {
      mapperRef.current.setSource({ width: mirror.width, height: mirror.height }, mirror.rotation)
    }
  }, [mirror])

  useEffect(() => {
    let disposed = false
    let dispose: (() => void) | undefined
    void api
      .attachFrameChannel(sessionId, {
        onFrame: (bytes) => {
          if (disposed) return
          const size = frameSizeRef.current
          frameBufRef.current = { bytes, w: size.width, h: size.height }
          setFrameActive(true)
        },
        onSize: (width, height) => {
          if (disposed) return
          frameSizeRef.current = { width, height }
          setFrameSize({ width, height })
          mapperRef.current.setSource({ width, height }, 0)
        },
      })
      .then((d) => {
        dispose = d
      })
    return () => {
      disposed = true
      dispose?.()
    }
  }, [sessionId])

  // 绘制循环：RGBA 帧直接写入 2D canvas。Android 管道已将最长边限制为
  // 1080px，保留足够清晰度，同时避免把大尺寸原始帧直接交给 WebView。
  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas) return
    const renderer = WebGLFrameRenderer.tryCreate(canvas)
    if (renderer) {
      webglRendererRef.current = renderer
    } else {
      canvas2dContextRef.current = canvas.getContext("2d", {
        alpha: false,
        desynchronized: true,
      })
    }
    return () => {
      renderer?.dispose()
      webglRendererRef.current = null
      canvas2dContextRef.current = null
    }
  }, [])

  useEffect(() => {
    let raf = 0
    const draw = () => {
      const cv = canvasRef.current
      const buf = frameBufRef.current
      if (cv && buf) {
        const need = buf.w * buf.h * 4
        if (buf.bytes.byteLength >= need) {
          if (cv.width !== buf.w || cv.height !== buf.h) {
            cv.width = buf.w
            cv.height = buf.h
          }
          const renderedByWebgl = webglRendererRef.current?.render(buf) ?? false
          const ctx = canvas2dContextRef.current
          if (!renderedByWebgl && ctx) {
            const view = new Uint8ClampedArray(
              buf.bytes.buffer as ArrayBuffer,
              buf.bytes.byteOffset,
              need,
            )
            ctx.putImageData(new ImageData(view, buf.w, buf.h), 0, 0)
            setFrameActive(true)
          }
        }
        frameBufRef.current = null
      }
      raf = requestAnimationFrame(draw)
    }
    raf = requestAnimationFrame(draw)
    return () => {
      cancelAnimationFrame(raf)
    }
  }, [])

  /** 所有输入共用一条有序队列；移动由队列内部做 latest-wins/累加合并。 */
  const enqueueInput = useCallback(
    <T,>(operation: () => Promise<T>): Promise<T> => inputQueue.enqueue(operation),
    [inputQueue],
  )

  const queueMove = useCallback(
    (move: PendingMove, mode: "relative" | "absolute") => {
      inputQueue.queueMove(move, mode)
    },
    [inputQueue],
  )

  const flushInputMoves = useCallback(() => inputQueue.flushMoves(), [inputQueue])

  const flushMoves = useCallback(() => {
    rafRef.current = null
    const batch = pendingRef.current
    pendingRef.current = []
    if (batch.length === 0) return
    const absolute = sessionId.startsWith("android") || isIosUsb
    if (absolute) {
      // 触摸只需要当前目标点；旧视频帧对应的中间点没有保留价值。
      const move = batch.at(-1)
      if (move?.abs) queueMove(move, "absolute")
      return
    }
    // BLE 相对鼠标需要保留总位移，但只发送一个合并后的动作，避免
    // Tauri invoke 在途时积压一串过期 HID 报告。
    queueMove(
      batch.reduce(
        (sum, move) => ({ dx: sum.dx + move.dx, dy: sum.dy + move.dy }),
        { dx: 0, dy: 0 },
      ),
      "relative",
    )
  }, [isIosUsb, queueMove, sessionId])

  const localPoint = (e: ReactPointerEvent) => {
    const rect = containerRef.current?.getBoundingClientRect()
    if (!rect) return null
    return { x: e.clientX - rect.left, y: e.clientY - rect.top }
  }

  const handleMove = useCallback(
    (e: ReactPointerEvent) => {
      // The simulator canvas is the only surface that can receive this
      // pointer event, so mouse movement must not wait for keyboard focus.
      // Otherwise the first pointerdown focuses the canvas, but all hover
      // movement that led to the click has already been discarded and the
      // iOS relative pointer remains at an unrelated position.
      if (!controlConnected || paused) return

      const isPointerLocked = isIosBle && document.pointerLockElement === containerRef.current
      if (isPointerLocked) {
        // Pointer Lock exposes movementX/movementY even after the native
        // cursor reaches the window edge. They are the only host-side values
        // that remain continuous while the cursor is captured.
        const result = cssDeltaToRelativeChunks(
          { x: 0, y: 0 },
          {
            x: e.movementX * prefs.iosPointerScale,
            y: e.movementY * prefs.iosPointerScale,
          },
          relativeRemainderRef.current,
        )
        relativeRemainderRef.current = result.remainder
        for (const c of result.chunks) pendingRef.current.push({ dx: c.dx, dy: c.dy })
        if (result.chunks.length > 0 && rafRef.current === null) {
          rafRef.current = requestAnimationFrame(flushMoves)
        }
        return
      }

      const pt = localPoint(e)
      if (!pt) return
      const phone = mapperRef.current.mapPoint(pt)
      if (!phone) {
        // Black bars are not part of the phone surface. Reset the relative
        // baseline so re-entering the image does not create a jump.
        if (isIosBle) {
          relativePointerRef.current = null
          relativeRemainderRef.current = { x: 0, y: 0 }
        }
        return
      }
      if (sessionId.startsWith("android") || isIosUsb) {
        absRef.current = phone
        if (dragRef.current) {
          pendingRef.current.push({
            dx: 0,
            dy: 0,
            abs: phone,
            source: { ...frameSizeRef.current },
          })
        }
      } else {
        const previous = relativePointerRef.current
        relativePointerRef.current = pt
        if (previous) {
          const result = cssDeltaToRelativeChunks(
            previous,
            {
              x: previous.x + (pt.x - previous.x) * prefs.iosPointerScale,
              y: previous.y + (pt.y - previous.y) * prefs.iosPointerScale,
            },
            relativeRemainderRef.current,
          )
          relativeRemainderRef.current = result.remainder
          for (const c of result.chunks) pendingRef.current.push({ dx: c.dx, dy: c.dy })
        }
      }
      if (rafRef.current === null) rafRef.current = requestAnimationFrame(flushMoves)
    },
    [controlConnected, flushMoves, isIosBle, isIosUsb, paused, prefs.iosPointerScale, sessionId],
  )

  const handleDown = useCallback(
    async (e: ReactPointerEvent, button: "left" | "middle" | "right") => {
      if (!controlConnected || paused) return
      e.preventDefault()
      // Capture the event's coordinate space before awaiting the move queue.
      // A frame-size/rotation update during that await must not remap this
      // click against a different source rectangle.
      const absolute = sessionId.startsWith("android") || isIosUsb
      const source = { ...frameSizeRef.current }
      const eventPoint = absolute ? localPoint(e) : null
      const eventPhone = eventPoint ? mapperRef.current.mapPoint(eventPoint) : null
      if (eventPhone) absRef.current = eventPhone
      dragRef.current = true
      // A RAF callback may still own the last move. Collect it and wait for
      // the coalescing pump before sending pointerDown; otherwise a slow WDA
      // request could make a tap arrive before its final move.
      flushMoves()
      await flushInputMoves()
      let payload: PointerButtonEvent
      if (absolute) {
        payload = {
          button,
          pressed: true,
          x: eventPhone?.x ?? absRef.current?.x ?? null,
          y: eventPhone?.y ?? absRef.current?.y ?? null,
          sourceWidth: source.width,
          sourceHeight: source.height,
        }
      } else {
        payload = { button, pressed: true }
      }
      try {
        await enqueueInput(() => api.hidPointerButton(sessionId, payload))
      } catch {
        dragRef.current = false
      }
    },
    [controlConnected, enqueueInput, flushInputMoves, flushMoves, isIosUsb, paused, sessionId],
  )

  const handleUp = useCallback(
    async (e: ReactPointerEvent) => {
      if (activePointerRef.current !== e.pointerId) return
      activePointerRef.current = null
      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId)
      }
      const wasDragging = dragRef.current
      dragRef.current = false
      if (!wasDragging) return
      // See handleDown: keep the release in the same source coordinate space
      // as the pointer event, even when a new frame arrived while the queue
      // was draining.
      const source = { ...frameSizeRef.current }
      const pt = localPoint(e)
      const phone = pt ? mapperRef.current.mapPoint(pt) : null
      if (phone) absRef.current = phone
      try {
        flushMoves()
        await flushInputMoves()
        await enqueueInput(() => api.hidPointerButton(sessionId, {
          button: "left",
          pressed: false,
          x: phone?.x ?? absRef.current?.x ?? null,
          y: phone?.y ?? absRef.current?.y ?? null,
          sourceWidth: source.width,
          sourceHeight: source.height,
        }))
      } catch {
        // 设备断开时后端已经执行 ReleaseAll；这里不再制造未处理异常。
      }
    },
    [enqueueInput, flushInputMoves, flushMoves, sessionId],
  )

  const handleWheel = useCallback(
    (e: ReactWheelEvent) => {
      e.preventDefault()
      if (!controlConnected || paused) return
      // A wheel/key/button event is an ordering boundary. Drain a move still
      // waiting for RAF before putting the non-disposable operation on the
      // serialized queue.
      flushMoves()
      void enqueueInput(() => api.hidWheel(sessionId, { deltaY: Math.round(e.deltaY) })).catch(() => undefined)
    },
    [controlConnected, enqueueInput, flushMoves, paused, sessionId],
  )

  const doPaste = useCallback(async () => {
    // The native command reads the system clipboard only for this explicit
    // user action. Its result is also recorded as a redacted diagnostic when
    // iOS has no subscribed keyboard report, instead of silently disappearing.
    flushMoves()
    await flushInputMoves()
    await enqueueInput(() => paste(sessionId))
  }, [enqueueInput, flushInputMoves, flushMoves, paste, sessionId])

  const triggerPaste = useCallback(() => {
    // Depending on the WebView and whether the action came from Cmd/Ctrl+V or
    // the native Edit menu, both keydown and paste can be delivered. Treat
    // them as one user action so a clipboard is never sent twice.
    const now = performance.now()
    if (now - lastPasteTriggerRef.current < 250) return
    lastPasteTriggerRef.current = now
    void doPaste()
  }, [doPaste])

  const handlePaste = useCallback(
    (e: ReactClipboardEvent<HTMLDivElement>) => {
      if (!prefs.pasteShortcutEnabled) return
      e.preventDefault()
      triggerPaste()
    },
    [prefs.pasteShortcutEnabled, triggerPaste],
  )

  const handleKeyDown = useCallback(
    (e: ReactKeyboardEvent) => {
      const sc = matchAppShortcut(e)
      if (sc.type === "release_all") {
        e.preventDefault()
        void enqueueInput(() => releaseAll(sessionId)).catch(() => undefined)
        return
      }
      if (sc.type === "pause_toggle") {
        e.preventDefault()
        setPausedState((v) => {
          const next = !v
          if (next) void releaseAll(sessionId)
          return next
        })
        return
      }
      if (sc.type === "escape") {
        e.preventDefault()
        if (dragRef.current) dragRef.current = false
        if (document.fullscreenElement) void document.exitFullscreen()
        if (document.pointerLockElement === containerRef.current) document.exitPointerLock()
        return
      }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "v") {
        if (prefs.pasteShortcutEnabled) {
          e.preventDefault()
          triggerPaste()
        }
        return
      }
      // Do not use a render-time readiness snapshot here: onFocus mutates the
      // ref synchronously and does not itself cause a render.
      if (!controlConnected || !focusedRef.current || paused) return
      const stroke = normalizeKeyEvent(e)
      if (isModifierEvent(stroke)) return
      flushMoves()
      void enqueueInput(() => api.hidKeyStroke(sessionId, stroke)).catch(() => undefined)
    },
    [controlConnected, enqueueInput, flushMoves, paused, prefs.pasteShortcutEnabled, releaseAll, sessionId, triggerPaste],
  )

  const handleContextMenu = useCallback(
    (e: ReactMouseEvent) => {
      e.preventDefault()
      if (controlConnected && !paused) {
        flushMoves()
        void enqueueInput(() => api.hidPointerButton(sessionId, { button: "right", pressed: true }))
          .then(() => enqueueInput(() => api.hidPointerButton(sessionId, { button: "right", pressed: false })))
          .catch(() => undefined)
      }
    },
    [controlConnected, enqueueInput, flushMoves, paused, sessionId],
  )

  const handleBlur = useCallback(() => {
    focusedRef.current = false
    if (dragRef.current) dragRef.current = false
    activePointerRef.current = null
    relativePointerRef.current = null
    relativeRemainderRef.current = { x: 0, y: 0 }
    pendingRef.current = []
    inputQueue.clearMoves()
    if (document.pointerLockElement === containerRef.current) document.exitPointerLock()
    void enqueueInput(() => releaseAll(sessionId)).catch(() => undefined)
  }, [enqueueInput, inputQueue, releaseAll, sessionId])

  useEffect(() => () => {
    if (rafRef.current !== null) cancelAnimationFrame(rafRef.current)
    rafRef.current = null
    pendingRef.current = []
    inputQueue.clearMoves()
  }, [inputQueue])

  const displaySize = useMemo(
    () =>
      mirror
        ? rotatedSize(mirror, mirror.rotation)
        : frameActive
          ? { width: frameSize.width, height: frameSize.height }
          : null,
    [frameActive, frameSize.height, frameSize.width, mirror],
  )
  const contentRect = useMemo(
    () =>
      displaySize && viewport.width > 0
        ? computeContentRect(viewport, displaySize)
        : { x: 0, y: 0, width: 0, height: 0 },
    [displaySize, viewport],
  )

  const handlePointerDown = useCallback(
    (e: ReactPointerEvent<HTMLDivElement>) => {
      e.currentTarget.focus()
      focusedRef.current = true

      const pt = localPoint(e)
      const insideContent = Boolean(
        pt &&
          contentRect.width > 0 &&
          pt.x >= contentRect.x &&
          pt.x <= contentRect.x + contentRect.width &&
          pt.y >= contentRect.y &&
          pt.y <= contentRect.y + contentRect.height,
      )
      if (!insideContent || !controlConnected || paused) return

      if (isIosBle && pt) {
        relativePointerRef.current = pt
        relativeRemainderRef.current = { x: 0, y: 0 }
      }

      e.preventDefault()
      activePointerRef.current = e.pointerId
      e.currentTarget.setPointerCapture(e.pointerId)
      if (isIosBle) {
        // A locked relative pointer cannot leave the simulator content. The
        // API is optional on older WebViews; the existing local-delta path is
        // retained as a safe fallback when the request is rejected.
        try {
          const result = e.currentTarget.requestPointerLock()
          if (result && typeof result.catch === "function") void result.catch(() => undefined)
        } catch {
          // Pointer Lock is an enhancement, not a prerequisite for input.
        }
      }
      void handleDown(e, "left")
    },
    [contentRect, controlConnected, handleDown, isIosBle, paused],
  )

  return (
    <div className="relative h-full w-full overflow-hidden bg-black">
      <div
        ref={containerRef}
        tabIndex={0}
        role="application"
        aria-label={`设备 ${sessionId} 画布`}
        className="absolute inset-0 touch-none select-none outline-none"
        onPointerMove={handleMove}
        onPointerDown={handlePointerDown}
        onPointerUp={handleUp}
        onPointerCancel={handleUp}
        onPointerLeave={() => {
          if (
            activePointerRef.current === null &&
            document.pointerLockElement !== containerRef.current
          ) {
            if (isIosBle) {
              relativePointerRef.current = null
              relativeRemainderRef.current = { x: 0, y: 0 }
            }
          }
        }}
        onWheel={handleWheel}
        onKeyDown={handleKeyDown}
        onPaste={handlePaste}
        onFocus={() => {
          focusedRef.current = true
        }}
        onBlur={handleBlur}
        onContextMenu={handleContextMenu}
      >
        <canvas
          ref={canvasRef}
          className="pointer-events-none absolute"
          style={{
            left: contentRect.x,
            top: contentRect.y,
            width: contentRect.width,
            height: contentRect.height,
            display: frameActive ? "block" : "none",
          }}
        />
      </div>
    </div>
  )
}
