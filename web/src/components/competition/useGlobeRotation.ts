import { useCallback, useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react'
import { focusHorizonNode, interpolateGlobeRotation, normalizeGlobeAngle } from './model'

// Input-driven only: coalesce dragging and use one bounded transition for target focus.
// There is no idle auto-spin or inertia.
export const useGlobeRotation = (scope: string) => {
  const stage = useRef<HTMLDivElement>(null)
  const [rotation, setRotation] = useState({ yaw: 0, pitch: 0 })
  const currentRotation = useRef(rotation)
  const pendingFrame = useRef<number | null>(null)
  const focusFrame = useRef<number | null>(null)
  const drag = useRef<{ id: number; x: number; y: number; yaw: number; pitch: number; width: number } | null>(null)
  const stopFocus = useCallback(() => {
    if (focusFrame.current !== null) cancelAnimationFrame(focusFrame.current)
    focusFrame.current = null
  }, [])
  const update = useCallback(
    (yaw: number, pitch: number) => {
      stopFocus()
      currentRotation.current = { yaw: normalizeGlobeAngle(yaw), pitch: normalizeGlobeAngle(pitch) }
      if (pendingFrame.current !== null) return
      pendingFrame.current = requestAnimationFrame(() => {
        pendingFrame.current = null
        setRotation(currentRotation.current)
      })
    },
    [stopFocus]
  )
  const rotate = useCallback(
    (yawDelta: number, pitchDelta = 0) =>
      update(currentRotation.current.yaw + yawDelta, currentRotation.current.pitch + pitchDelta),
    [update]
  )
  const reset = useCallback(() => {
    drag.current = null
    update(0, 0)
  }, [update])

  const focus = useCallback(
    (index: number) => {
      stopFocus()
      if (pendingFrame.current !== null) cancelAnimationFrame(pendingFrame.current)
      pendingFrame.current = null
      drag.current = null
      const target = focusHorizonNode(index)
      const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)')
      if (reducedMotion.matches) {
        update(target.yaw, target.pitch)
        return
      }
      const from = currentRotation.current
      const started = performance.now()
      const frame = (now: number) => {
        focusFrame.current = null
        const progress = reducedMotion.matches ? 1 : Math.min(1, (now - started) / 320)
        const next = interpolateGlobeRotation(from, target, progress)
        currentRotation.current = next
        setRotation(next)
        if (progress < 1) focusFrame.current = requestAnimationFrame(frame)
      }
      focusFrame.current = requestAnimationFrame(frame)
    },
    [stopFocus, update]
  )

  useEffect(() => {
    reset()
    return () => {
      stopFocus()
      if (pendingFrame.current !== null) cancelAnimationFrame(pendingFrame.current)
      pendingFrame.current = null
      drag.current = null
    }
  }, [scope, reset, stopFocus])

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

  const moveDrag = (event: PointerEvent<HTMLDivElement>) => {
    if (drag.current?.id !== event.pointerId) return
    const { x, y, yaw, pitch, width } = drag.current
    update(yaw + ((event.clientX - x) * Math.PI) / width, pitch - ((event.clientY - y) * Math.PI) / width)
  }

  return {
    ...rotation,
    rotate,
    reset,
    focus,
    stageProps: {
      ref: stage,
      onPointerDown: (event: PointerEvent<HTMLDivElement>) => {
        if (!event.isPrimary || event.button !== 0 || drag.current) return
        if ((event.target as Element).closest('button')) return
        stopFocus()
        drag.current = {
          id: event.pointerId,
          x: event.clientX,
          y: event.clientY,
          ...currentRotation.current,
          width: Math.max(240, event.currentTarget.clientWidth),
        }
        event.currentTarget.focus({ preventScroll: true })
        event.currentTarget.setPointerCapture(event.pointerId)
      },
      onPointerMove: moveDrag,
      onPointerUp: (event: PointerEvent<HTMLDivElement>) => {
        if (drag.current?.id !== event.pointerId) return
        moveDrag(event)
        endDrag(event)
      },
      onPointerCancel: endDrag,
      onLostPointerCapture: endDrag,
      onKeyDown: (event: KeyboardEvent<HTMLDivElement>) => {
        if (event.target !== event.currentTarget || event.altKey || event.ctrlKey || event.metaKey) return
        if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
          event.preventDefault()
          rotate((event.key === 'ArrowRight' ? 1 : -1) * (Math.PI / 12))
        } else if (event.key === 'ArrowUp' || event.key === 'ArrowDown') {
          event.preventDefault()
          rotate(0, (event.key === 'ArrowUp' ? 1 : -1) * (Math.PI / 12))
        } else if (event.key === 'Home') {
          event.preventDefault()
          reset()
        }
      },
    },
  }
}
