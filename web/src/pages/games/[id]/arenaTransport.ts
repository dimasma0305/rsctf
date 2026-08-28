export interface ArenaCycleResult {
  readonly success: boolean
  readonly retryAfterMs?: number
}

export interface ArenaCycleOptions {
  readonly successDelayMs: number
  readonly failureBaseDelayMs: number
  readonly maximumDelayMs: number
  readonly requestTimeoutMs: number
  readonly random?: () => number
}

const MINIMUM_RETRY_DELAY_MS = 250
const MAXIMUM_RETRY_AFTER_MS = 5 * 60_000

/** Parse the standard Retry-After seconds/date forms without accepting unbounded sleeps. */
export const retryAfterMilliseconds = (
  value: string | null | undefined,
  now: number = Date.now()
): number | undefined => {
  if (!value) return undefined
  const seconds = Number(value)
  const delay = Number.isFinite(seconds) && seconds >= 0 ? seconds * 1000 : Date.parse(value) - now
  if (!Number.isFinite(delay) || delay < 0) return undefined
  return Math.min(MAXIMUM_RETRY_AFTER_MS, Math.ceil(delay))
}

/** Capped exponential backoff with full jitter and a small anti-spin floor. */
export const arenaRetryDelay = (
  attempt: number,
  baseDelayMs: number,
  maximumDelayMs: number,
  random: () => number = Math.random
): number => {
  const exponent = Math.max(0, Math.min(20, Math.trunc(attempt) - 1))
  const ceiling = Math.min(maximumDelayMs, baseDelayMs * 2 ** exponent)
  const sample = Math.max(0, Math.min(0.999_999, random()))
  return Math.min(maximumDelayMs, Math.max(MINIMUM_RETRY_DELAY_MS, Math.floor(ceiling * sample)))
}

/**
 * Own one completion-scheduled request cycle.
 *
 * A suspended/stopped owner aborts the current request and clears its sole timer.
 * The next request is scheduled only after the prior promise settles, so a slow
 * response can never accumulate overlapping arena polls.
 */
export class CompletionScheduledArenaCycle {
  private active = false
  private suspended = false
  private inFlight = false
  private failures = 0
  private timer: ReturnType<typeof setTimeout> | undefined
  private timeout: ReturnType<typeof setTimeout> | undefined
  private controller: AbortController | undefined

  constructor(
    private readonly run: (signal: AbortSignal) => Promise<ArenaCycleResult>,
    private readonly options: ArenaCycleOptions
  ) {}

  start(): void {
    if (this.active) return
    this.active = true
    this.schedule(0)
  }

  suspend(): void {
    if (!this.active || this.suspended) return
    this.suspended = true
    this.clearTimer()
    this.controller?.abort()
  }

  resume(): void {
    if (!this.active || !this.suspended) return
    this.suspended = false
    if (!this.inFlight) this.schedule(0)
  }

  stop(): void {
    this.active = false
    this.suspended = false
    this.clearTimer()
    this.clearTimeout()
    this.controller?.abort()
  }

  private schedule(delayMs: number): void {
    if (!this.active || this.suspended || this.inFlight || this.timer !== undefined) return
    this.timer = setTimeout(() => {
      this.timer = undefined
      void this.runOnce()
    }, delayMs)
  }

  private async runOnce(): Promise<void> {
    if (!this.active || this.suspended || this.inFlight) return
    this.inFlight = true
    const controller = new AbortController()
    this.controller = controller
    this.timeout = setTimeout(() => controller.abort(), this.options.requestTimeoutMs)

    let result: ArenaCycleResult = { success: false }
    try {
      result = await this.run(controller.signal)
    } catch {
      result = { success: false }
    } finally {
      this.clearTimeout()
      if (this.controller === controller) this.controller = undefined
      this.inFlight = false
    }

    if (!this.active || this.suspended) return
    if (result.success) {
      this.failures = 0
      this.schedule(this.options.successDelayMs)
      return
    }

    this.failures += 1
    const backoff = arenaRetryDelay(
      this.failures,
      this.options.failureBaseDelayMs,
      this.options.maximumDelayMs,
      this.options.random
    )
    this.schedule(Math.max(backoff, result.retryAfterMs ?? 0))
  }

  private clearTimer(): void {
    if (this.timer === undefined) return
    clearTimeout(this.timer)
    this.timer = undefined
  }

  private clearTimeout(): void {
    if (this.timeout === undefined) return
    clearTimeout(this.timeout)
    this.timeout = undefined
  }
}
