import { useCallback, useEffect, useRef, useState } from 'react'
import { isRetryableHttpError } from '@Utils/HttpError'

export interface DurableCreateIntent<T> {
  operationId: string
  payload: T
  createdAt: number
}

const RECOVERY_WINDOW_MS = 60 * 60_000
const MAX_INTENT_BYTES = 512 * 1024

const newOperationId = () => {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID()
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  bytes[6] = (bytes[6] & 0x0f) | 0x40
  bytes[8] = (bytes[8] & 0x3f) | 0x80
  const value = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
  return `${value.slice(0, 8)}-${value.slice(8, 12)}-${value.slice(12, 16)}-${value.slice(16, 20)}-${value.slice(20)}`
}

export const createIntentStorageKey = (kind: 'challenge' | 'team' | 'game' | 'post', scope = 'global') =>
  `create-intent:${kind}:${scope}`

export const readCreateIntent = <T>(
  storage: Pick<Storage, 'getItem' | 'removeItem'>,
  key: string,
  now: number = Date.now()
): DurableCreateIntent<T> | null => {
  try {
    const raw = storage.getItem(key)
    if (!raw || raw.length > MAX_INTENT_BYTES) {
      if (raw) storage.removeItem(key)
      return null
    }
    const parsed = JSON.parse(raw) as Partial<DurableCreateIntent<T>> | null
    if (
      !parsed ||
      typeof parsed.operationId !== 'string' ||
      !parsed.operationId ||
      typeof parsed.createdAt !== 'number' ||
      !Number.isFinite(parsed.createdAt) ||
      now - parsed.createdAt < 0 ||
      now - parsed.createdAt > RECOVERY_WINDOW_MS ||
      !('payload' in parsed)
    ) {
      storage.removeItem(key)
      return null
    }
    return parsed as DurableCreateIntent<T>
  } catch {
    storage.removeItem(key)
    return null
  }
}

export const writeCreateIntent = <T>(
  storage: Pick<Storage, 'setItem'>,
  key: string,
  payload: T,
  operationId: string = newOperationId(),
  now: number = Date.now()
): DurableCreateIntent<T> => {
  const intent = { operationId, payload, createdAt: now }
  const serialized = JSON.stringify(intent)
  if (serialized.length > MAX_INTENT_BYTES) throw new Error('Create request is too large to retain safely')
  storage.setItem(key, serialized)
  return intent
}

export const clearCreateIntent = (
  storage: Pick<Storage, 'getItem' | 'removeItem'>,
  key: string,
  operationId: string
) => {
  if (readCreateIntent(storage, key)?.operationId === operationId) storage.removeItem(key)
}

interface DurableCreateOptions<T, R> {
  storageKey: string
  enabled: boolean
  request: (payload: T, operationId: string, signal: AbortSignal) => Promise<R>
  onSuccess: (result: R, recovered: boolean) => void | Promise<void>
  onError: (error: unknown) => void
}

/**
 * Own one create flight synchronously and replay its exact persisted body once
 * after a reload. Only terminal success or a definitive client error clears it.
 */
export const useDurableCreateIntent = <T, R>({
  storageKey,
  enabled,
  request,
  onSuccess,
  onError,
}: DurableCreateOptions<T, R>) => {
  const callbacks = useRef({ request, onSuccess, onError })
  callbacks.current = { request, onSuccess, onError }
  const owner = useRef<AbortController | null>(null)
  const [busy, setBusy] = useState(false)

  const execute = useCallback(
    async (intent: DurableCreateIntent<T>, recovered: boolean) => {
      if (owner.current) return false
      const controller = new AbortController()
      owner.current = controller
      setBusy(true)
      try {
        const result = await callbacks.current.request(intent.payload, intent.operationId, controller.signal)
        if (owner.current !== controller) return false
        await callbacks.current.onSuccess(result, recovered)
        clearCreateIntent(window.sessionStorage, storageKey, intent.operationId)
        return true
      } catch (error) {
        if (owner.current !== controller || controller.signal.aborted) return false
        if (!isRetryableHttpError(error)) clearCreateIntent(window.sessionStorage, storageKey, intent.operationId)
        callbacks.current.onError(error)
        return false
      } finally {
        if (owner.current === controller) {
          owner.current = null
          setBusy(false)
        }
      }
    },
    [storageKey]
  )

  useEffect(() => {
    owner.current?.abort()
    owner.current = null
    setBusy(false)
    if (enabled) {
      const pending = readCreateIntent<T>(window.sessionStorage, storageKey)
      if (pending) void execute(pending, true)
    }
    return () => {
      owner.current?.abort()
      owner.current = null
    }
  }, [enabled, execute, storageKey])

  const submit = useCallback(
    (payload: T) => {
      if (owner.current) return Promise.resolve(false)
      try {
        // A retry must reconcile the known intent before a changed form can
        // become a second create. The server returns its retained identity if
        // the earlier response was lost after commit.
        const intent =
          readCreateIntent<T>(window.sessionStorage, storageKey) ??
          writeCreateIntent(window.sessionStorage, storageKey, payload)
        return execute(intent, false)
      } catch (error) {
        callbacks.current.onError(error)
        return Promise.resolve(false)
      }
    },
    [execute, storageKey]
  )

  return { busy, submit }
}
