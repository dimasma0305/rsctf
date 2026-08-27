import type { HubConnection, IRetryPolicy, RetryContext } from '@microsoft/signalr'
import { httpErrorStatus, retryAfterMilliseconds } from '@Utils/ProfileRetry'

/** The server emits SignalR pings every 15 seconds. Two missed pings plus a
 * small scheduling allowance detects a dead transport without treating one
 * delayed frame as an outage. */
export const HUB_KEEPALIVE_MS = 15_000
export const HUB_SERVER_TIMEOUT_MS = 35_000

export const HUB_INITIAL_RETRY_LIMIT = 6
export const HUB_RETRY_BASE_MS = 750
export const HUB_RETRY_CAP_MS = 15_000
export const HUB_EXHAUSTED_RETRY_MS = 60_000
export const HUB_REVALIDATE_RETRY_LIMIT = 3
export const HUB_REVALIDATE_RETRY_AFTER_MAX_MS = 2_147_000_000
export const NOTICE_FALLBACK_POLL_MS = 60_000
export const OPERATOR_FALLBACK_POLL_MS = 30_000

type TimerHandle = ReturnType<typeof setTimeout>

export interface RecoveryTimers {
  setTimeout: (callback: () => void, milliseconds: number) => TimerHandle
  clearTimeout: (handle: TimerHandle) => void
}

const browserTimers: RecoveryTimers = {
  setTimeout: (callback, milliseconds) => setTimeout(callback, milliseconds),
  clearTimeout: (handle) => clearTimeout(handle),
}

/** Equal-jitter exponential backoff. Keeping half of each exponential slot as
 * a floor prevents a tight retry loop while the random upper half prevents a
 * recovering replica from receiving every browser handshake at once. */
export const cappedJitterDelay = (
  previousRetryCount: number,
  random: () => number = Math.random,
  baseMs = HUB_RETRY_BASE_MS,
  capMs = HUB_RETRY_CAP_MS
) => {
  const exponent = Math.max(0, Math.min(30, Math.trunc(previousRetryCount)))
  const ceiling = Math.min(capMs, baseMs * 2 ** exponent)
  const unit = Math.min(1, Math.max(0, random()))
  return Math.round(ceiling / 2 + (ceiling / 2) * unit)
}

export class CappedJitterRetryPolicy implements IRetryPolicy {
  constructor(
    private readonly maxAttempts = HUB_INITIAL_RETRY_LIMIT,
    private readonly random: () => number = Math.random
  ) {}

  nextRetryDelayInMilliseconds(context: RetryContext): number | null {
    if (!isRetryableHubFailure(context.retryReason) || context.previousRetryCount >= this.maxAttempts) return null
    return cappedJitterDelay(context.previousRetryCount, this.random)
  }
}

const statusFromUnknown = (error: unknown): number | undefined => {
  const responseStatus = httpErrorStatus(error)
  if (responseStatus !== null) return responseStatus
  if (typeof error === 'object' && error !== null) {
    for (const property of ['statusCode']) {
      const value = Reflect.get(error, property)
      if (typeof value === 'number') return value
    }
  }
  const message = error instanceof Error ? error.message : String(error ?? '')
  const match = message.match(/(?:status(?: code)?|response)\D{0,12}(\d{3})/i)
  return match ? Number(match[1]) : undefined
}

/** Authentication, authorization, malformed-scope, and missing-game failures
 * are server decisions and must not be bypassed with a retry storm. Admission
 * pressure (429), unavailable replicas (503), other 5xx responses, and
 * transport failures are transient and use the bounded recovery schedule. */
export const isRetryableHubFailure = (error: unknown) => {
  const status = statusFromUnknown(error)
  if (status === undefined) return true
  if ([400, 401, 403, 404].includes(status)) return false
  return status === 408 || status === 425 || status === 429 || status >= 500
}

/** Use the transport's capped jitter policy for HTTP reconciliation as well,
 * while respecting a server Retry-After and refusing an unbounded wait. */
export const hubRevalidationRetryDelay = (
  error: unknown,
  retryCount: number,
  random: () => number = Math.random,
  now: number = Date.now()
): number | null => {
  if (retryCount >= HUB_REVALIDATE_RETRY_LIMIT || !isRetryableHubFailure(error)) return null
  const retryAfter = retryAfterMilliseconds(error, now)
  if (retryAfter !== null && retryAfter > HUB_REVALIDATE_RETRY_AFTER_MAX_MS) return null
  return Math.max(cappedJitterDelay(retryCount, random), retryAfter ?? 0)
}

export type HubRecoveryState = 'idle' | 'connecting' | 'connected' | 'reconnecting' | 'exhausted' | 'stopped'

export interface RecoverableHubConnection {
  start: () => Promise<void>
  stop: () => Promise<void>
  onclose: (callback: (error?: Error) => void) => void
  onreconnecting: (callback: (error?: Error) => void) => void
  onreconnected: (callback: (connectionId?: string) => void) => void
}

export interface HubRecoveryOptions {
  revalidate: () => void | Promise<unknown>
  onConnected?: (generation: number, recovered: boolean) => void | Promise<unknown>
  onReconnecting?: (error?: Error) => void
  onExhausted?: (error?: unknown) => void
  onStateChange?: (state: HubRecoveryState) => void
  pollingIntervalMs?: number
  /** Feed connections resume an initial-start cycle at this slow cadence after
   * exhaustion. Exec consoles set it to null and expose an explicit Retry. */
  exhaustedRetryMs?: number | null
  initialRetryLimit?: number
  isPollingAllowed?: () => boolean
  random?: () => number
  timers?: RecoveryTimers
}

/** Own one HubConnection from mount to unmount. SignalR only retries an
 * established connection; this controller also retries failed initial starts,
 * restarts after automatic reconnect exhaustion, coalesces HTTP backfills, and
 * maintains one visibility-aware polling fallback. */
export class HubRecoveryController {
  private readonly timers: RecoveryTimers
  private readonly random: () => number
  private readonly initialRetryLimit: number
  private readonly exhaustedRetryMs: number | null
  private active = false
  private state: HubRecoveryState = 'idle'
  private startFailures = 0
  private startInFlight: Promise<void> | null = null
  private retryTimer: TimerHandle | null = null
  private pollTimer: TimerHandle | null = null
  private refreshInFlight: Promise<void> | null = null
  private refreshRetryTimer: TimerHandle | null = null
  private refreshRetryDeferred = false
  private refreshRetrySuppressed = false
  private refreshFailures = 0
  private transportGeneration = 0
  private reconnectWave = false
  private automaticRetryAllowed = true

  constructor(
    private readonly connection: RecoverableHubConnection,
    private readonly options: HubRecoveryOptions
  ) {
    this.timers = options.timers ?? browserTimers
    this.random = options.random ?? Math.random
    this.initialRetryLimit = Math.max(1, options.initialRetryLimit ?? HUB_INITIAL_RETRY_LIMIT)
    this.exhaustedRetryMs = options.exhaustedRetryMs === undefined ? HUB_EXHAUSTED_RETRY_MS : options.exhaustedRetryMs

    connection.onreconnecting((error) => {
      if (!this.active) return
      if (!this.reconnectWave) {
        this.reconnectWave = true
        this.setState('reconnecting')
        options.onReconnecting?.(error)
      }
    })
    connection.onreconnected(() => {
      if (!this.active || !this.reconnectWave) return
      this.reconnectWave = false
      this.startFailures = 0
      this.connected(true)
    })
    connection.onclose((error) => {
      if (!this.active) return
      this.reconnectWave = false
      this.automaticRetryAllowed = isRetryableHubFailure(error)
      this.setState('exhausted')
      options.onExhausted?.(error)
      this.scheduleExhaustedRestart(error)
    })
  }

  get currentState() {
    return this.state
  }

  get generation() {
    return this.transportGeneration
  }

  /** Permanent 4xx decisions stay terminal until an explicit user action.
   * Visibility/online listeners use this to avoid retrying server-denied
   * connections merely because the browser changed state. */
  get canRetryAutomatically() {
    return this.automaticRetryAllowed
  }

  start() {
    if (this.active) return
    this.active = true
    this.startFailures = 0
    this.schedulePoll()
    this.beginStart()
  }

  /** User-driven recovery for an exhausted terminal. It is harmless while an
   * attempt is already running and never creates concurrent start calls. */
  retryNow() {
    if (!this.active || this.startInFlight) return
    this.cancelRetry()
    this.startFailures = 0
    this.automaticRetryAllowed = true
    this.beginStart()
  }

  revalidateNow() {
    if (!this.active || !this.pollingAllowed()) return Promise.resolve()
    if (this.refreshRetryDeferred) this.refreshRetryDeferred = false
    return this.revalidate()
  }

  async stop() {
    if (!this.active) return
    this.active = false
    this.transportGeneration += 1
    this.reconnectWave = false
    this.cancelRetry()
    this.cancelRefreshRetry()
    if (this.pollTimer !== null) {
      this.timers.clearTimeout(this.pollTimer)
      this.pollTimer = null
    }
    this.setState('stopped')
    await this.connection.stop().catch(() => undefined)
  }

  private setState(state: HubRecoveryState) {
    if (this.state === state) return
    this.state = state
    this.options.onStateChange?.(state)
  }

  private pollingAllowed() {
    return this.options.isPollingAllowed?.() ?? true
  }

  private schedulePoll() {
    const interval = this.options.pollingIntervalMs ?? 0
    if (!this.active || interval <= 0) return
    const delay = Math.round(interval * (0.9 + Math.min(1, Math.max(0, this.random())) * 0.2))
    this.pollTimer = this.timers.setTimeout(() => {
      this.pollTimer = null
      if (!this.active) return
      if (this.pollingAllowed()) void this.revalidate()
      this.schedulePoll()
    }, delay)
  }

  private revalidate() {
    if (this.refreshInFlight) return this.refreshInFlight
    if (this.refreshRetryTimer !== null || this.refreshRetryDeferred || this.refreshRetrySuppressed) {
      return Promise.resolve()
    }
    const refresh = Promise.resolve()
      .then(() => this.options.revalidate())
      .then(() => {
        this.refreshFailures = 0
        this.refreshRetrySuppressed = false
      })
      .catch((error: unknown) => {
        if (!this.active) return
        const delay = hubRevalidationRetryDelay(error, this.refreshFailures, this.random)
        if (delay === null) {
          const retryAfter = retryAfterMilliseconds(error)
          this.refreshRetrySuppressed =
            isRetryableHubFailure(error) &&
            retryAfter !== null &&
            retryAfter > HUB_REVALIDATE_RETRY_AFTER_MAX_MS
          this.refreshFailures = 0
          return
        }
        this.refreshFailures += 1
        this.scheduleRefreshRetry(delay)
      })
      .then(() => undefined)
    this.refreshInFlight = refresh
    void refresh.finally(() => {
      if (this.refreshInFlight === refresh) this.refreshInFlight = null
    })
    return refresh
  }

  private beginStart() {
    if (!this.active || this.startInFlight) return
    this.setState('connecting')
    const attempt = this.connection
      .start()
      .then(async () => {
        if (!this.active) {
          await this.connection.stop().catch(() => undefined)
          return
        }
        this.startFailures = 0
        this.automaticRetryAllowed = true
        this.connected(this.transportGeneration > 0)
      })
      .catch((error: unknown) => {
        if (!this.active) return
        this.startFailures += 1
        const retryable = isRetryableHubFailure(error)
        this.automaticRetryAllowed = retryable
        if (!retryable || this.startFailures >= this.initialRetryLimit) {
          this.setState('exhausted')
          this.options.onExhausted?.(error)
          if (retryable) this.scheduleExhaustedRestart(error)
          return
        }
        this.scheduleRetry(cappedJitterDelay(this.startFailures - 1, this.random))
      })
      .then(() => undefined)
    this.startInFlight = attempt
    void attempt.finally(() => {
      if (this.startInFlight === attempt) this.startInFlight = null
    })
  }

  private connected(recovered: boolean) {
    this.transportGeneration += 1
    const generation = this.transportGeneration
    this.setState('connected')
    void Promise.resolve(this.options.onConnected?.(generation, recovered)).catch(() => undefined)
    // Reconcile after every successful handshake. This closes both the initial
    // HTTP→subscription race and any event gap accumulated during reconnect.
    void this.revalidate()
  }

  private scheduleRetry(delay: number) {
    this.cancelRetry()
    this.retryTimer = this.timers.setTimeout(() => {
      this.retryTimer = null
      this.beginStart()
    }, delay)
  }

  private scheduleExhaustedRestart(error: unknown) {
    if (!this.active || this.exhaustedRetryMs === null || !isRetryableHubFailure(error)) return
    const delay = Math.round(this.exhaustedRetryMs * (0.8 + Math.min(1, Math.max(0, this.random())) * 0.4))
    this.cancelRetry()
    this.retryTimer = this.timers.setTimeout(() => {
      this.retryTimer = null
      this.startFailures = 0
      this.beginStart()
    }, delay)
  }

  private cancelRetry() {
    if (this.retryTimer === null) return
    this.timers.clearTimeout(this.retryTimer)
    this.retryTimer = null
  }

  private scheduleRefreshRetry(delay: number) {
    if (this.refreshRetryTimer !== null) this.timers.clearTimeout(this.refreshRetryTimer)
    this.refreshRetryDeferred = false
    this.refreshRetryTimer = this.timers.setTimeout(() => {
      this.refreshRetryTimer = null
      if (!this.active) return
      if (!this.pollingAllowed()) {
        this.refreshRetryDeferred = true
        return
      }
      void this.revalidate()
    }, delay)
  }

  private cancelRefreshRetry() {
    if (this.refreshRetryTimer !== null) this.timers.clearTimeout(this.refreshRetryTimer)
    this.refreshRetryTimer = null
    this.refreshRetryDeferred = false
    this.refreshRetrySuppressed = false
    this.refreshFailures = 0
  }
}

export const configureHubTimeouts = (connection: HubConnection) => {
  connection.keepAliveIntervalInMilliseconds = HUB_KEEPALIVE_MS
  connection.serverTimeoutInMilliseconds = HUB_SERVER_TIMEOUT_MS
  return connection
}

/** Serialize a resource that must be recreated once for each successful
 * transport generation. A late result from an obsolete generation is disposed
 * rather than published; an explicit retry is allowed only after the current
 * attempt has failed or the previous resource has closed. */
export class GenerationBoundOpener<T> {
  private currentGeneration = -1
  private openedGeneration = -1
  private readonly inFlight = new Map<number, Promise<void>>()

  beginGeneration(generation: number) {
    this.currentGeneration = generation
    this.openedGeneration = -1
  }

  invalidate() {
    this.currentGeneration = -1
    this.openedGeneration = -1
  }

  retryCurrent() {
    if (this.currentGeneration > 0 && !this.inFlight.has(this.currentGeneration)) this.openedGeneration = -1
  }

  open(
    generation: number,
    create: () => Promise<T>,
    accept: (value: T) => void | Promise<unknown>,
    dispose: (value: T) => void | Promise<unknown>
  ) {
    if (generation !== this.currentGeneration) return Promise.resolve()
    const existing = this.inFlight.get(generation)
    if (existing) return existing
    if (this.openedGeneration === generation) return Promise.resolve()

    this.openedGeneration = generation
    const opening = Promise.resolve()
      .then(create)
      .then(async (value) => {
        if (generation !== this.currentGeneration) {
          await dispose(value)
          return
        }
        await accept(value)
      })
      .catch((error: unknown) => {
        if (generation === this.currentGeneration) this.openedGeneration = -1
        throw error
      })
      .then(() => undefined)
    this.inFlight.set(generation, opening)
    const cleanup = () => {
      if (this.inFlight.get(generation) === opening) this.inFlight.delete(generation)
    }
    void opening.then(cleanup, cleanup)
    return opening
  }
}
