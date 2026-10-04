import { interpolateGlobeRotation, normalizeGlobeAngle, type GlobeRotation } from '@Components/competition/model'

export const arenaHome = { yaw: 0.24, pitch: -0.18 }

/** One interruptible camera, advanced by the arena's existing frame loop. */
export function createArenaCamera() {
  let current = { ...arenaHome }
  let transition: { from: GlobeRotation; to: GlobeRotation; elapsed: number } | null = null
  const set = (value: GlobeRotation) => {
    transition = null
    current = { yaw: normalizeGlobeAngle(value.yaw), pitch: normalizeGlobeAngle(value.pitch) }
  }
  const focus = (to: GlobeRotation, motion: boolean) => {
    if (!motion) set(to)
    else transition = { from: current, to, elapsed: 0 }
  }
  return {
    get current() {
      return current
    },
    get moving() {
      return transition !== null
    },
    set,
    focus,
    stop() {
      transition = null
    },
    settle() {
      if (transition) set(transition.to)
    },
    nudge(dx: number, dy: number, motion: boolean) {
      const target = transition?.to || current
      focus({ yaw: target.yaw + dx, pitch: target.pitch + dy }, motion)
    },
    step(dt: number, auto: boolean) {
      // No catch-up jump after a suspended tab or a slow frame.
      const elapsed = Math.min(Math.max(dt, 0), 0.05)
      if (transition) {
        transition.elapsed += elapsed
        current = interpolateGlobeRotation(transition.from, transition.to, transition.elapsed / 0.42)
        if (transition.elapsed >= 0.42) transition = null
        return true
      }
      if (auto && elapsed > 0) {
        set({ ...current, yaw: current.yaw + elapsed * 0.075 })
        return true
      }
      return false
    },
  }
}
