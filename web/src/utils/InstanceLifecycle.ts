import { isRetryableHttpError } from './HttpError'
import { retryAfterMilliseconds } from './ProfileRetry'

export const runInstanceExtension = async (extend: () => void | Promise<void>, onSuccess: () => void) => {
  await extend()
  onSuccess()
}

export const isInstanceExtensionWindowOpen = (
  closeTime: number | null | undefined,
  renewalWindowMinutes: number,
  nowMilliseconds: number
) => {
  const normalizedCloseTime = closeTime ?? 0
  if (
    !Number.isFinite(normalizedCloseTime) ||
    !Number.isFinite(renewalWindowMinutes) ||
    !Number.isFinite(nowMilliseconds)
  )
    return false
  return normalizedCloseTime - nowMilliseconds < renewalWindowMinutes * 60_000
}

type InstanceContext = {
  closeTime?: number | null
  instanceId?: string | null
  instanceEntry?: string | null
}

type InstanceRuntimeResponse = {
  id?: string | null
  entry?: string | null
  expectStopAt?: number | null
}

interface ExtensionReconciliation<T> {
  refresh: () => Promise<T | undefined>
  extend: (expectedContainerId: string) => Promise<InstanceRuntimeResponse>
  publish: (extension: InstanceRuntimeResponse) => Promise<void>
}

/** Merge a runtime response into the newest SWR value, never a render-time snapshot. */
export const mergeInstanceContext = <T extends { context?: InstanceContext }>(
  latest: T | undefined,
  patch: Partial<InstanceContext>
): T | undefined => {
  if (!latest) return latest
  return { ...latest, context: { ...latest.context, ...patch } }
}

/**
 * Revalidate exactly once after create and trust only the authoritative runtime ID.
 * The refresh itself publishes the current server state, so a delayed create response
 * can never repopulate a cache that passed through a create/destroy cycle.
 */
export const confirmCreatedInstance = async <T extends { context?: InstanceContext }>(
  created: InstanceRuntimeResponse,
  refresh: () => Promise<T | undefined>
): Promise<boolean> => {
  if (!created.id) return false
  const latest = await refresh()
  return latest?.context?.instanceId === created.id
}

export const INSTANCE_CREATE_RETRY_BUDGET_MS = 10 * 60_000
const INSTANCE_CREATE_MIN_RETRY_MS = 2_000
const INSTANCE_CREATE_MAX_BACKOFF_MS = 15_000

export type InstanceCreateOutcome = 'created' | 'present' | 'unconfirmed' | 'abandoned'

interface CreateReconciliation<T> {
  /** Sends the same durable operation identity on every attempt. */
  create: () => Promise<InstanceRuntimeResponse>
  refresh: () => Promise<T | undefined>
  /** False once the surface that owns this start has closed or moved on. */
  shouldContinue: () => boolean
  onRetryScheduled?: (delayMs: number) => void
  sleep?: (milliseconds: number) => Promise<void>
  now?: () => number
  budgetMs?: number
}

const defaultSleep = (milliseconds: number) => new Promise<void>((resolve) => setTimeout(resolve, milliseconds))

/** Honour Retry-After, otherwise back off from 2 s to 15 s between attempts. */
export const instanceCreateRetryDelay = (error: unknown, attempt: number, now: number): number => {
  const backoff = Math.min(INSTANCE_CREATE_MAX_BACKOFF_MS, INSTANCE_CREATE_MIN_RETRY_MS * 2 ** attempt)
  return Math.max(backoff, retryAfterMilliseconds(error, now) ?? 0)
}

/**
 * Keep one start alive across transient failures. An image build or slow
 * launch can outlive a proxy, browser, or server wait deadline while the
 * server-side owner keeps working; re-sending the same operation identity
 * rejoins that owner instead of reporting a transient failure as a dead
 * instance. Only a terminal rejection or an exhausted budget surfaces.
 */
export const createReconciledInstance = async <T extends { context?: InstanceContext }>({
  create,
  refresh,
  shouldContinue,
  onRetryScheduled,
  sleep = defaultSleep,
  now = Date.now,
  budgetMs = INSTANCE_CREATE_RETRY_BUDGET_MS,
}: CreateReconciliation<T>): Promise<InstanceCreateOutcome> => {
  const startedAt = now()
  for (let attempt = 0; ; attempt += 1) {
    let created: InstanceRuntimeResponse
    try {
      created = await create()
    } catch (error) {
      if (!isRetryableHttpError(error)) throw error
      const delay = instanceCreateRetryDelay(error, attempt, now())
      if (now() - startedAt + delay > budgetMs) throw error
      onRetryScheduled?.(delay)
      await sleep(delay)
      if (!shouldContinue()) return 'abandoned'
      // The owner may have finished while this attempt failed in transit.
      const latest = await refresh().catch(() => undefined)
      if (latest?.context?.instanceId) return 'present'
      if (!shouldContinue()) return 'abandoned'
      continue
    }
    return (await confirmCreatedInstance(created, refresh)) ? 'created' : 'unconfirmed'
  }
}

/** Apply an extension only while the cache still names the immutable runtime ID. */
export const mergeExtendedInstanceContext = <T extends { context?: InstanceContext }>(
  latest: T | undefined,
  extension: InstanceRuntimeResponse
): T | undefined => {
  if (!extension.id || typeof extension.expectStopAt !== 'number' || latest?.context?.instanceId !== extension.id)
    return latest
  return mergeInstanceContext(latest, { closeTime: extension.expectStopAt })
}

/** Extend the authoritative runtime snapshot and carry its immutable ID to the server. */
export const extendReconciledInstance = async <T extends { context?: InstanceContext }>({
  refresh,
  extend,
  publish,
}: ExtensionReconciliation<T>): Promise<void> => {
  const latest = await refresh()
  const expectedContainerId = latest?.context?.instanceId
  if (!expectedContainerId) throw new Error('The refreshed challenge response is missing its instance identity.')

  const extension = await extend(expectedContainerId)
  await publish(extension)
}

/** Clear only the runtime identity that the completed delete actually removed. */
export const clearDestroyedInstanceContext = <T extends { context?: InstanceContext }>(
  current: T | undefined,
  deleted: T
): T | undefined => {
  const deletedId = deleted.context?.instanceId
  if (!deletedId || current?.context?.instanceId !== deletedId) return current
  return mergeInstanceContext(current, { closeTime: null, instanceId: null, instanceEntry: null })
}

interface DestroyReconciliation<T> {
  refresh: () => Promise<T | undefined>
  hasInstance: (value: T | undefined) => boolean
  destroy: (latest: T) => Promise<void>
  publishAbsent: (latest: T) => Promise<void>
}

/** Destroy from the latest server snapshot and converge when another caller won the race. */
export const destroyReconciledInstance = async <T>({
  refresh,
  hasInstance,
  destroy,
  publishAbsent,
}: DestroyReconciliation<T>): Promise<'destroyed' | 'alreadyAbsent'> => {
  const latest = await refresh()
  if (!latest || !hasInstance(latest)) return 'alreadyAbsent'

  try {
    await destroy(latest)
  } catch (error) {
    try {
      const reconciled = await refresh()
      if (!hasInstance(reconciled)) return 'alreadyAbsent'
    } catch {
      // A failed refresh cannot prove convergence. Preserve the operation error
      // that prompted reconciliation so the player sees the actionable cause.
    }
    throw error
  }

  try {
    await publishAbsent(latest)
  } catch (error) {
    // Confirmation is still useful after a local publication failure, but it
    // must not replace the cache error that the caller can act on.
    await refresh().catch(() => undefined)
    throw error
  }

  // The delete is authoritative and the cache is already absent. A transient
  // confirmation read must not turn that successful operation into an error.
  await refresh().catch(() => undefined)
  return 'destroyed'
}
