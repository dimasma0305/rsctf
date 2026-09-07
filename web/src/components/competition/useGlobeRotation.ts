import { useCallback, useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react'
import { normalizeGlobeYaw } from './model'

// Input-driven only: coalesce each burst into one frame and retain its latest position.
// There is no auto-spin or inertia, including when reduced motion is enabled.
export const useGlobeRotation = (scope: string) => {
  const stage = useRef<HTMLDivElement>(null)
  const [yaw, setYaw] = useState(0)
  const currentYaw = useRef(0)
  const pendingFrame = useRef<number | null>(null)
  const drag = useRef<{ id: number; x: number; yaw: number; width: number } | null>(null)
  const update = useCallback((next: number) => {
    currentYaw.current = normalizeGlobeYaw(next)
    if (pendingFrame.current !== null) return
    pendingFrame.current = requestAnimationFrame(() => {
      pendingFrame.current = null
      setYaw(currentYaw.current)
    })
  }, [])
  const rotate = useCallback((delta: number) => update(currentYaw.current + delta), [update])
  const reset = useCallback(() => {
    drag.current = null
    update(0)
  }, [update])

  useEffect(() => {
    reset()
    return () => {
      if (pendingFrame.current !== null) cancelAnimationFrame(pendingFrame.current)
      pendingFrame.current = null
      drag.current = null
    }
  }, [scope, reset])

  useEffect(() => {
    const element = stage.current
    if (!element) return
    const wheel = (event: WheelEvent) => {
      // Leave browser zoom and an active pointer gesture alone.
      if (event.ctrlKey || event.metaKey || drag.current) return
      const delta = Math.abs(event.deltaX) > Math.abs(event.deltaY) ? event.deltaX : event.deltaY
      if (!delta) return
      event.preventDefault()
      const width = Math.max(240, element.clientWidth)
      const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? width : 1
      if (element !== document.activeElement && element.contains(document.activeElement)) {
        element.focus({ preventScroll: true })
      }
      rotate((delta * unit * Math.PI) / width)
    }
    // React wheel listeners are passive; cancellation must be local to this surface.
    element.addEventListener('wheel', wheel, { passive: false })
    return () => element.removeEventListener('wheel', wheel)
  }, [rotate])

  const endDrag = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current?.id !== event.pointerId) return
    drag.current = null
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId)
    }
  }

  return {
    yaw,
    rotate,
    reset,
    stageProps: {
      ref: stage,
      onPointerDown: (event: PointerEvent<HTMLDivElement>) => {
        if (!event.isPrimary || event.button !== 0 || drag.current) return
        if ((event.target as Element).closest('button')) return
        drag.current = {
          id: event.pointerId,
          x: event.clientX,
          yaw: currentYaw.current,
          width: Math.max(240, event.currentTarget.clientWidth),
        }
        event.currentTarget.focus({ preventScroll: true })
        event.currentTarget.setPointerCapture(event.pointerId)
      },
      onPointerMove: (event: PointerEvent<HTMLDivElement>) => {
        if (drag.current?.id !== event.pointerId) return
        update(drag.current.yaw + ((event.clientX - drag.current.x) * Math.PI) / drag.current.width)
      },
      onPointerUp: (event: PointerEvent<HTMLDivElement>) => {
        if (drag.current?.id !== event.pointerId) return
        update(drag.current.yaw + ((event.clientX - drag.current.x) * Math.PI) / drag.current.width)
        endDrag(event)
      },
      onPointerCancel: endDrag,
      onLostPointerCapture: endDrag,
      onKeyDown: (event: KeyboardEvent<HTMLDivElement>) => {
        if (event.target !== event.currentTarget || event.altKey || event.ctrlKey || event.metaKey) return
        if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
          event.preventDefault()
          rotate((event.key === 'ArrowRight' ? 1 : -1) * (Math.PI / 12))
        } else if (event.key === 'Home') {
          event.preventDefault()
          reset()
        }
      },
    },
  }
}
