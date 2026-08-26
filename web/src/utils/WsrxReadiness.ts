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
  /** Give one explicit retry a fresh window; repeated signals in that window coalesce. */
  retry: (remote: string) => void
  /** Suspend accelerated reads without forgetting the provider's current tunnels. */
  setEnabled: (enabled: boolean) => void
  /** Clear daemon-specific state after its options change. */
  reset: () => void
  /** Permanently cancel timers and ignore late in-flight completions. */
  dispose: () => void
}

interface ActiveScheduler {
  generation: number
  pollTimer?: ReturnType<typeof setTimeout>
  deadlineTimer?: ReturnType<typeof setTimeout>
}

const sameSet = (left: ReadonlySet<string>, right: ReadonlySet<string>) => {
  if (left.size !== right.size) return false
  for (const value of left) {
    if (!right.has(value)) return false
  }
  return true
}

/**
 * Owns the short, accelerated readiness windows layered on top of WSRX's
 * normal 15-second synchronization. Each pending tunnel gets a complete
 * bounded window, while one provider-level completion scheduler serializes
 * every daemon read regardless of how many entries are rendered.
 */
export const createWsrxReadinessScheduler = ({
  sync,
  onExpiredChange,
  pollMs = WSRX_READINESS_POLL_MS,
  windowMs = WSRX_READINESS_WINDOW_MS,
}: WsrxReadinessSchedulerOptions): WsrxReadinessScheduler => {
  let active: ActiveScheduler | undefined
  let disposed = false
  let enabled = false
  let generation = 0
  let inFlight = false
  let pending = new Set<string>()
  let expired = new Set<string>()
  const deadlines = new Map<string, number>()
  const retryWindows = new Set<string>()

  const publishExpired = (next: Set<string>) => {
    if (sameSet(expired, next)) return
    expired = next
    onExpiredChange(new Set(expired))
  }

  const clearScheduler = () => {
    if (!active) return
    if (active.pollTimer !== undefined) clearTimeout(active.pollTimer)
    if (active.deadlineTimer !== undefined) clearTimeout(active.deadlineTimer)
    active = undefined
    generation += 1
  }

  const expireDue = (now: number) => {
    const next = new Set(expired)
    for (const [remote, deadline] of deadlines) {
      if (deadline > now) continue
      deadlines.delete(remote)
      retryWindows.delete(remote)
      if (pending.has(remote)) next.add(remote)
    }
    publishExpired(next)
  }

  const expireEligible = () => {
    const next = new Set(expired)
    for (const remote of deadlines.keys()) {
      if (pending.has(remote)) next.add(remote)
    }
    deadlines.clear()
    retryWindows.clear()
    publishExpired(next)
    clearScheduler()
  }

  const earliestDeadline = () => {
    let earliest: number | undefined
    for (const deadline of deadlines.values()) {
      if (earliest === undefined || deadline < earliest) earliest = deadline
    }
    return earliest
  }

  const armDeadline = (expectedGeneration: number) => {
    if (!active || active.generation !== expectedGeneration || disposed || !enabled) return
    if (active.deadlineTimer !== undefined) clearTimeout(active.deadlineTimer)
    const deadline = earliestDeadline()
    if (deadline === undefined) {
      clearScheduler()
      return
    }

    active.deadlineTimer = setTimeout(
      () => {
        if (!active || active.generation !== expectedGeneration) return
        active.deadlineTimer = undefined
        expireDue(Date.now())
        if (deadlines.size === 0) clearScheduler()
        else armDeadline(expectedGeneration)
      },
      Math.max(0, deadline - Date.now())
    )
  }

  const scheduleNext = (expectedGeneration: number) => {
    if (
      !active ||
      active.generation !== expectedGeneration ||
      active.pollTimer !== undefined ||
      inFlight ||
      disposed ||
      !enabled ||
      deadlines.size === 0
    )
      return

    active.pollTimer = setTimeout(() => void poll(expectedGeneration), pollMs)
  }

  const poll = async (expectedGeneration: number) => {
    if (!active || active.generation !== expectedGeneration || inFlight || disposed || !enabled) return
    active.pollTimer = undefined
    expireDue(Date.now())
    if (!active || active.generation !== expectedGeneration || deadlines.size === 0) {
      clearScheduler()
      return
    }

    inFlight = true
    let failed = false
    try {
      // No next timer exists while this settles: slow daemon responses cannot
      // overlap another GET /pool, even when a new window starts meanwhile.
      await sync()
    } catch {
      failed = true
    } finally {
      inFlight = false
    }

    if (disposed) return
    if (!active || active.generation !== expectedGeneration) {
      if (active && enabled && deadlines.size > 0) scheduleNext(active.generation)
      return
    }
    if (failed) {
      // A failed accelerated read ends the currently eligible windows. WSRX
      // retains its normal 15-second synchronization; a later explicit retry
      // can open one fresh bounded window.
      expireEligible()
      return
    }

    expireDue(Date.now())
    if (!active || active.generation !== expectedGeneration || deadlines.size === 0) {
      clearScheduler()
      return
    }
    armDeadline(expectedGeneration)
    scheduleNext(expectedGeneration)
  }

  const startScheduler = () => {
    if (active || disposed || !enabled || deadlines.size === 0) return
    const schedulerGeneration = ++generation
    active = { generation: schedulerGeneration }
    armDeadline(schedulerGeneration)
    scheduleNext(schedulerGeneration)
  }

  const reconcileScheduler = () => {
    if (!enabled || disposed) return
    expireDue(Date.now())
    if (deadlines.size === 0) {
      clearScheduler()
      return
    }
    if (!active) startScheduler()
    else armDeadline(active.generation)
  }

  return {
    updatePending(remotes) {
      if (disposed) return
      const previousPending = pending
      const nextPending = new Set(remotes)
      const nextExpired = new Set([...expired].filter((remote) => nextPending.has(remote)))

      for (const remote of deadlines.keys()) {
        if (!nextPending.has(remote)) deadlines.delete(remote)
      }
      for (const remote of retryWindows) {
        if (!nextPending.has(remote)) retryWindows.delete(remote)
      }

      const now = Date.now()
      for (const remote of nextPending) {
        if (!previousPending.has(remote)) {
          nextExpired.delete(remote)
          retryWindows.delete(remote)
          if (enabled) deadlines.set(remote, now + windowMs)
        } else if (enabled && !nextExpired.has(remote) && !deadlines.has(remote)) {
          deadlines.set(remote, now + windowMs)
        }
      }

      pending = nextPending
      publishExpired(nextExpired)
      reconcileScheduler()
    },
    retry(remote) {
      if (disposed || !enabled || !pending.has(remote)) return
      // One user action may surface through multiple matching entries/effects.
      // Once admitted, those repeats cannot move this retry's deadline.
      if (retryWindows.has(remote) && deadlines.has(remote)) return
      retryWindows.add(remote)
      deadlines.set(remote, Date.now() + windowMs)
      if (expired.has(remote)) {
        const next = new Set(expired)
        next.delete(remote)
        publishExpired(next)
      }
      reconcileScheduler()
    },
    setEnabled(nextEnabled) {
      if (disposed || enabled === nextEnabled) return
      enabled = nextEnabled
      if (!enabled) {
        clearScheduler()
        deadlines.clear()
        retryWindows.clear()
        return
      }

      const deadline = Date.now() + windowMs
      for (const remote of pending) {
        if (!expired.has(remote)) deadlines.set(remote, deadline)
      }
      startScheduler()
    },
    reset() {
      if (disposed) return
      enabled = false
      clearScheduler()
      pending = new Set()
      deadlines.clear()
      retryWindows.clear()
      publishExpired(new Set())
    },
    dispose() {
      if (disposed) return
      disposed = true
      enabled = false
      clearScheduler()
      pending = new Set()
      expired = new Set()
      deadlines.clear()
      retryWindows.clear()
    },
  }
}
