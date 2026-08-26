import { useEffect, useRef } from 'react'
import { throwIfAborted } from '@Utils/FingerprintProbe'

type AbortableOperation<Args extends unknown[], Result> = (signal: AbortSignal, ...args: Args) => Promise<Result>

interface OperationOwner<Result> {
  controller: AbortController
  promise: Promise<Result>
}

export interface SingleFlightOperation<Args extends unknown[], Result> {
  run: (...args: Args) => Promise<Result>
  cancel: () => void
  dispose: () => void
}

export const createSingleFlightOperation = <Args extends unknown[], Result>(
  operation: AbortableOperation<Args, Result>
): SingleFlightOperation<Args, Result> => {
  let owner: OperationOwner<Result> | null = null
  let disposed = false

  const run = (...args: Args): Promise<Result> => {
    if (disposed) return Promise.reject(new DOMException('The operation was aborted.', 'AbortError'))
    if (owner) return owner.promise

    const controller = new AbortController()
    const current = {} as OperationOwner<Result>
    const promise = Promise.resolve()
      .then(() => {
        throwIfAborted(controller.signal)
        return operation(controller.signal, ...args)
      })
      .finally(() => {
        if (owner === current) owner = null
      })
    current.controller = controller
    current.promise = promise
    owner = current
    return promise
  }

  const cancel = () => owner?.controller.abort()
  const dispose = () => {
    disposed = true
    cancel()
  }

  return { run, cancel, dispose }
}

interface ConsentSingleFlightOptions<Result> {
  requiresConsent: () => boolean
  requestConsent: () => void
  operation: (signal: AbortSignal, consentGranted: boolean) => Promise<Result>
}

export interface ConsentSingleFlightOperation<Result> extends SingleFlightOperation<[], Result | undefined> {
  acceptConsent: () => void
  rejectConsent: () => void
}

export const createConsentSingleFlightOperation = <Result>({
  requiresConsent,
  requestConsent,
  operation,
}: ConsentSingleFlightOptions<Result>): ConsentSingleFlightOperation<Result> => {
  let accepted = false
  let resolveConsent: ((accepted: boolean) => void) | null = null

  const waitForConsent = (signal: AbortSignal): Promise<boolean> =>
    new Promise<boolean>((resolve, reject) => {
      const onAbort = () => {
        resolveConsent = null
        reject(
          signal.reason instanceof Error ? signal.reason : new DOMException('The operation was aborted.', 'AbortError')
        )
      }
      resolveConsent = (granted) => {
        signal.removeEventListener('abort', onAbort)
        resolveConsent = null
        resolve(granted)
      }
      signal.addEventListener('abort', onAbort, { once: true })
    })

  const owner = createSingleFlightOperation(async (signal) => {
    const consentRequired = requiresConsent()
    let granted = accepted
    if (consentRequired && !granted) {
      requestConsent()
      granted = await waitForConsent(signal)
    }
    throwIfAborted(signal)
    if (consentRequired && !granted) return undefined

    if (consentRequired) accepted = true
    return operation(signal, consentRequired && granted)
  })

  const acceptConsent = () => {
    accepted = true
    resolveConsent?.(true)
  }
  const rejectConsent = () => resolveConsent?.(false)
  const dispose = () => {
    rejectConsent()
    owner.dispose()
  }

  return { ...owner, acceptConsent, rejectConsent, dispose }
}

export const useAbortableSingleFlight = <Args extends unknown[], Result>(
  operation: AbortableOperation<Args, Result>
): SingleFlightOperation<Args, Result> => {
  const operationRef = useRef(operation)
  operationRef.current = operation
  const ownerRef = useRef<SingleFlightOperation<Args, Result> | null>(null)
  if (!ownerRef.current) {
    ownerRef.current = createSingleFlightOperation((signal, ...args) => operationRef.current(signal, ...args))
  }

  useEffect(() => () => ownerRef.current?.cancel(), [])
  return ownerRef.current
}

interface UseConsentSingleFlightOptions<Result> {
  requiresConsent: boolean
  onConsentRequired: () => void
  operation: (signal: AbortSignal, consentGranted: boolean) => Promise<Result>
}

export const useConsentSingleFlight = <Result>({
  requiresConsent,
  onConsentRequired,
  operation,
}: UseConsentSingleFlightOptions<Result>) => {
  const optionsRef = useRef({ requiresConsent, onConsentRequired, operation })
  optionsRef.current = { requiresConsent, onConsentRequired, operation }
  const ownerRef = useRef<ConsentSingleFlightOperation<Result> | null>(null)
  if (!ownerRef.current) {
    ownerRef.current = createConsentSingleFlightOperation({
      requiresConsent: () => optionsRef.current.requiresConsent,
      requestConsent: () => optionsRef.current.onConsentRequired(),
      operation: (signal, granted) => optionsRef.current.operation(signal, granted),
    })
  }

  useEffect(() => () => ownerRef.current?.cancel(), [])
  return ownerRef.current
}
