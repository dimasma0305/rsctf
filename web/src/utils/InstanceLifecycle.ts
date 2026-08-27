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

/** Apply an extension only while the cache still names the immutable runtime ID. */
export const mergeExtendedInstanceContext = <T extends { context?: InstanceContext }>(
  latest: T | undefined,
  extension: InstanceRuntimeResponse
): T | undefined => {
  if (!extension.id || typeof extension.expectStopAt !== 'number' || latest?.context?.instanceId !== extension.id)
    return latest
  return mergeInstanceContext(latest, { closeTime: extension.expectStopAt })
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
  destroy: () => Promise<void>
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
    await destroy()
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
