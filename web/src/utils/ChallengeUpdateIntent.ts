import { useCallback, useEffect, useRef, useState } from 'react'
import { httpErrorStatus, isRetryableHttpError } from '@Utils/HttpError'
import type { ChallengeUpdateOperationResult } from '@Api'

export interface ChallengeUpdateIntent<T> {
  operationId: string
  expectedRevision: number
  payload: T
  createdAt: number
}

const RECOVERY_WINDOW_MS = 60 * 60_000
const MAX_INTENT_BYTES = 1024 * 1024

const newOperationId = () => crypto.randomUUID()

export const challengeUpdateIntentKey = (gameId: number, challengeId: number) =>
  `challenge-update-intent:${gameId}:${challengeId}`

export const readChallengeUpdateIntent = <T>(
  storage: Pick<Storage, 'getItem' | 'removeItem'>,
  key: string,
  now: number = Date.now()
): ChallengeUpdateIntent<T> | null => {
  try {
    const raw = storage.getItem(key)
    if (!raw || raw.length > MAX_INTENT_BYTES) {
      if (raw) storage.removeItem(key)
      return null
    }
    const parsed = JSON.parse(raw) as Partial<ChallengeUpdateIntent<T>> | null
    if (
      !parsed ||
      typeof parsed.operationId !== 'string' ||
      !parsed.operationId ||
      typeof parsed.expectedRevision !== 'number' ||
      typeof parsed.createdAt !== 'number' ||
      now - parsed.createdAt < 0 ||
      now - parsed.createdAt > RECOVERY_WINDOW_MS ||
      !('payload' in parsed)
    ) {
      storage.removeItem(key)
      return null
    }
    return parsed as ChallengeUpdateIntent<T>
  } catch {
    storage.removeItem(key)
    return null
  }
}

export const writeChallengeUpdateIntent = <T>(
  storage: Pick<Storage, 'setItem'>,
  key: string,
  expectedRevision: number,
  payload: T,
  operationId: string = newOperationId(),
  now: number = Date.now()
): ChallengeUpdateIntent<T> => {
  const intent = { operationId, expectedRevision, payload, createdAt: now }
  const serialized = JSON.stringify(intent)
  if (serialized.length > MAX_INTENT_BYTES) throw new Error('Challenge update is too large to retain safely')
  storage.setItem(key, serialized)
  return intent
}

const clearOwned = (storage: Pick<Storage, 'getItem' | 'removeItem'>, key: string, operationId: string) => {
  if (readChallengeUpdateIntent(storage, key)?.operationId === operationId) storage.removeItem(key)
}

interface Options<T, R> {
  storageKey: string
  enabled: boolean
  request: (intent: ChallengeUpdateIntent<T>, signal: AbortSignal) => Promise<R>
  recover: (operationId: string, signal: AbortSignal) => Promise<ChallengeUpdateOperationResult>
  onSuccess: (result: R, intent: ChallengeUpdateIntent<T>) => void | Promise<void>
  onRecovered: (result: ChallengeUpdateOperationResult, intent: ChallengeUpdateIntent<T>) => R | Promise<R>
  onError: (error: unknown) => void
}

export const useChallengeUpdateIntent = <T, R>({
  storageKey,
  enabled,
  request,
  recover,
  onSuccess,
  onRecovered,
  onError,
}: Options<T, R>) => {
  const callbacks = useRef({ request, recover, onSuccess, onRecovered, onError })
  callbacks.current = { request, recover, onSuccess, onRecovered, onError }
  const owner = useRef<AbortController | null>(null)
  const [busy, setBusy] = useState(false)

  const acceptRecovered = useCallback(
    async (intent: ChallengeUpdateIntent<T>, result: ChallengeUpdateOperationResult) => {
      if (result.operationId !== intent.operationId || result.revision !== intent.expectedRevision + 1) {
        throw new Error('Challenge update recovery returned an unexpected operation result')
      }
      const recovered = await callbacks.current.onRecovered(result, intent)
      clearOwned(window.sessionStorage, storageKey, intent.operationId)
      return recovered
    },
    [storageKey]
  )

  const execute = useCallback(
    async (intent: ChallengeUpdateIntent<T>, reconcileFirst: boolean) => {
      if (owner.current) return null
      const controller = new AbortController()
      owner.current = controller
      setBusy(true)
      try {
        if (reconcileFirst) {
          try {
            const result = await callbacks.current.recover(intent.operationId, controller.signal)
            if (owner.current !== controller) return null
            return await acceptRecovered(intent, result)
          } catch (error) {
            if (controller.signal.aborted || httpErrorStatus(error) !== 404) throw error
          }
        }
        try {
          const result = await callbacks.current.request(intent, controller.signal)
          if (owner.current !== controller) return null
          await callbacks.current.onSuccess(result, intent)
          clearOwned(window.sessionStorage, storageKey, intent.operationId)
          return result
        } catch (error) {
          if (controller.signal.aborted || !isRetryableHttpError(error)) throw error
          try {
            const recovered = await callbacks.current.recover(intent.operationId, controller.signal)
            if (owner.current !== controller) return null
            return await acceptRecovered(intent, recovered)
          } catch (recoveryError) {
            if (httpErrorStatus(recoveryError) !== 404) throw recoveryError
            throw error
          }
        }
      } catch (error) {
        if (owner.current !== controller || controller.signal.aborted) return null
        if (!isRetryableHttpError(error)) clearOwned(window.sessionStorage, storageKey, intent.operationId)
        callbacks.current.onError(error)
        return null
      } finally {
        if (owner.current === controller) {
          owner.current = null
          setBusy(false)
        }
      }
    },
    [acceptRecovered, storageKey]
  )

  useEffect(() => {
    owner.current?.abort()
    owner.current = null
    setBusy(false)
    if (enabled) {
      const pending = readChallengeUpdateIntent<T>(window.sessionStorage, storageKey)
      if (pending) void execute(pending, true)
    }
    return () => {
      owner.current?.abort()
      owner.current = null
    }
  }, [enabled, execute, storageKey])

  const submit = useCallback(
    (expectedRevision: number, payload: T) => {
      if (owner.current) return Promise.resolve(null)
      try {
        const intent = writeChallengeUpdateIntent(window.sessionStorage, storageKey, expectedRevision, payload)
        return execute(intent, false)
      } catch (error) {
        callbacks.current.onError(error)
        return Promise.resolve(null)
      }
    },
    [execute, storageKey]
  )

  return { busy, submit }
}
