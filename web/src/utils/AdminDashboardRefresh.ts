export const ADMIN_DASHBOARD_REFRESH_MS = 60_000

export interface AdminDashboardRefreshEnvironment {
  refresh: () => Promise<unknown> | unknown
  onError?: (error: unknown) => void
  isActive: () => boolean
  subscribe: (listener: () => void) => () => void
  setTimer: (callback: () => void, delayMilliseconds: number) => ReturnType<typeof setTimeout>
  clearTimer: (timer: ReturnType<typeof setTimeout>) => void
  intervalMilliseconds?: number
}

/**
 * Own one non-overlapping dashboard cadence. Initial data remains SWR-owned;
 * this owner refreshes only while the tab is visible, focused, and online.
 */
export const startAdminDashboardRefresh = ({
  refresh,
  onError,
  isActive,
  subscribe,
  setTimer,
  clearTimer,
  intervalMilliseconds = ADMIN_DASHBOARD_REFRESH_MS,
}: AdminDashboardRefreshEnvironment) => {
  let timer: ReturnType<typeof setTimeout> | undefined
  let running = false
  let stopped = false

  const cancelTimer = () => {
    if (timer === undefined) return
    clearTimer(timer)
    timer = undefined
  }

  const schedule = () => {
    cancelTimer()
    if (stopped || running || !isActive()) return
    timer = setTimer(() => {
      timer = undefined
      void run()
    }, intervalMilliseconds)
  }

  const run = async () => {
    if (stopped || running || !isActive()) return
    running = true
    try {
      await refresh()
    } catch (error) {
      onError?.(error)
    } finally {
      running = false
      schedule()
    }
  }

  const activityChanged = () => {
    if (!isActive()) {
      cancelTimer()
      return
    }
    if (!running && timer === undefined) void run()
  }

  const unsubscribe = subscribe(activityChanged)
  schedule()

  return () => {
    stopped = true
    cancelTimer()
    unsubscribe()
  }
}
