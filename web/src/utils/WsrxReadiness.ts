export const WSRX_READINESS_POLL_MS = 1_500
export const WSRX_READINESS_WINDOW_MS = 8_000

interface WsrxReadinessSchedulerOptions {
  sync: () => Promise<void>
  onExpiredChange: (expired: ReadonlySet<string>) => void
  pollMs?: number
  windowMs?: number
}

export interface WsrxReadinessScheduler {
  /** Replace the provider's current set of tunnels whose latency is still unknown. */
  updatePending: (remotes: Iterable<string>) => void
  /** Let an explicit retry re-admit one expired tunnel into the current/new window. */
  retry: (remote: string) => void
  /** Suspend accelerated reads without forgetting the provider's current tunnels. */
  setEnabled: (enabled: boolean) => void
  /** Clear daemon-specific state after its options change. */
  reset: () => void
  /** Permanently cancel timers and ignore late in-flight completions. */
  dispose: () => void
}

interface ActiveWindow {
  generation: number
  deadline: number
  pollTimer?: ReturnType<typeof setTimeout>
  deadlineTimer: ReturnType<typeof setTimeout>
}

const sameSet = (left: ReadonlySet<string>, right: ReadonlySet<string>) => {
  if (left.size !== right.size) return false
  for (const value of left) {
    if (!right.has(value)) return false
  }
  return true
}

/**
 * Owns the short, accelerated readiness window layered on top of WSRX's normal
 * 15-second synchronization. There is one controller per provider, not per
 * rendered instance, so adding more challenge entries cannot multiply daemon
 * requests.
 */
export const createWsrxReadinessScheduler = ({
  sync,
  onExpiredChange,
  pollMs = WSRX_READINESS_POLL_MS,
  windowMs = WSRX_READINESS_WINDOW_MS,
}: WsrxReadinessSchedulerOptions): WsrxReadinessScheduler => {
  let active: ActiveWindow | undefined
  let disposed = false
  let enabled = false
  let generation = 0
  let pending = new Set<string>()
  let expired = new Set<string>()

  const publishExpired = (next: Set<string>) => {
    if (sameSet(expired, next)) return
    expired = next
    onExpiredChange(new Set(expired))
  }

  const clearWindow = () => {
    if (!active) return
    if (active.pollTimer !== undefined) clearTimeout(active.pollTimer)
    clearTimeout(active.deadlineTimer)
    active = undefined
    generation += 1
  }

  const hasEligiblePending = () => {
    for (const remote of pending) {
      if (!expired.has(remote)) return true
    }
    return false
  }

  const expireWindow = (expectedGeneration: number) => {
    if (!active || active.generation !== expectedGeneration) return
    clearWindow()
    const next = new Set(expired)
    for (const remote of pending) next.add(remote)
    publishExpired(next)
  }

  const scheduleNext = (expectedGeneration: number) => {
    if (!active || active.generation !== expectedGeneration || disposed || !enabled) return
    const remaining = active.deadline - Date.now()
    if (remaining <= 0) {
      expireWindow(expectedGeneration)
      return
    }

    active.pollTimer = setTimeout(
      () => {
        void poll(expectedGeneration)
      },
      Math.min(pollMs, remaining)
    )
  }

  const poll = async (expectedGeneration: number) => {
    if (!active || active.generation !== expectedGeneration || disposed || !enabled) return
    active.pollTimer = undefined
    if (Date.now() >= active.deadline) {
      expireWindow(expectedGeneration)
      return
    }

    try {
      // No next timer exists while this settles: slow daemon responses cannot
      // overlap another GET /pool.
      await sync()
    } catch {
      // A failed accelerated read ends this window. WSRX retains its normal
      // 15-second synchronization, while an explicit retry/reconnect can open
      // one fresh bounded window.
      expireWindow(expectedGeneration)
      return
    }

    if (!active || active.generation !== expectedGeneration || disposed || !enabled) return
    if (!hasEligiblePending()) {
      clearWindow()
      return
    }
    if (Date.now() >= active.deadline) {
      expireWindow(expectedGeneration)
      return
    }
    scheduleNext(expectedGeneration)
  }

  const startWindow = () => {
    if (active || disposed || !enabled || !hasEligiblePending()) return
    const windowGeneration = ++generation
    const deadline = Date.now() + windowMs
    active = {
      generation: windowGeneration,
      deadline,
      deadlineTimer: setTimeout(() => expireWindow(windowGeneration), windowMs),
    }
    scheduleNext(windowGeneration)
  }

  return {
    updatePending(remotes) {
      if (disposed) return
      const nextPending = new Set(remotes)
      pending = nextPending

      const retainedExpired = new Set([...expired].filter((remote) => nextPending.has(remote)))
      publishExpired(retainedExpired)

      if (!hasEligiblePending()) {
        clearWindow()
        return
      }
      startWindow()
    },
    retry(remote) {
      if (disposed || !pending.has(remote)) return
      if (expired.has(remote)) {
        const next = new Set(expired)
        next.delete(remote)
        publishExpired(next)
      }
      startWindow()
    },
    setEnabled(nextEnabled) {
      if (disposed || enabled === nextEnabled) return
      enabled = nextEnabled
      if (!enabled) {
        clearWindow()
        return
      }
      startWindow()
    },
    reset() {
      if (disposed) return
      enabled = false
      clearWindow()
      pending = new Set()
      publishExpired(new Set())
    },
    dispose() {
      if (disposed) return
      disposed = true
      enabled = false
      clearWindow()
      pending = new Set()
      expired = new Set()
    },
  }
}
