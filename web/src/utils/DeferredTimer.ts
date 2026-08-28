export interface DeferredTimerOwner {
  schedule: (action: () => void, delay: number) => ReturnType<typeof setTimeout> | null
  cancel: (timer: ReturnType<typeof setTimeout> | null) => void
  cancelAll: () => void
  pending: () => number
}

/** Owns deferred callbacks so route changes and unmounts cannot run stale work. */
export const createDeferredTimerOwner = (): DeferredTimerOwner => {
  const timers = new Set<ReturnType<typeof setTimeout>>()
  let stopped = false

  const cancel = (timer: ReturnType<typeof setTimeout> | null) => {
    if (timer === null) return
    clearTimeout(timer)
    timers.delete(timer)
  }

  return {
    schedule(action, delay) {
      if (stopped) return null
      const timer = setTimeout(
        () => {
          timers.delete(timer)
          if (!stopped) action()
        },
        Math.max(0, delay)
      )
      timers.add(timer)
      return timer
    },
    cancel,
    cancelAll() {
      stopped = true
      timers.forEach(clearTimeout)
      timers.clear()
    },
    pending: () => timers.size,
  }
}
