import { useCallback, useEffect, useRef, useState } from 'react'

type LinkWork = (signal: AbortSignal) => Promise<unknown>

/** Own one account-link request synchronously and ignore stale route responses. */
export const useAccountLinkSubmit = (linkIdentity: string) => {
  const owner = useRef(false)
  const generation = useRef(0)
  const controller = useRef<AbortController | null>(null)
  const [pending, setPending] = useState(false)

  useEffect(() => {
    generation.current += 1
    owner.current = false
    controller.current?.abort()
    controller.current = null
    setPending(false)
    return () => {
      generation.current += 1
      owner.current = false
      controller.current?.abort()
      controller.current = null
    }
  }, [linkIdentity])

  const run = useCallback(async (work: LinkWork, onSuccess: () => void, onFailure: () => void) => {
    if (owner.current) return
    owner.current = true
    const requestGeneration = ++generation.current
    const requestController = new AbortController()
    controller.current = requestController
    setPending(true)
    try {
      await work(requestController.signal)
      if (generation.current === requestGeneration && !requestController.signal.aborted) onSuccess()
    } catch (error) {
      if (generation.current === requestGeneration && !requestController.signal.aborted) onFailure()
    } finally {
      if (generation.current === requestGeneration) {
        owner.current = false
        controller.current = null
        setPending(false)
      }
    }
  }, [])

  return { pending, run }
}
