import api, { type ControlJobModel } from '@Api'

const TERMINAL = new Set(['Succeeded', 'Failed', 'Cancelled'])
const BASE_DELAY_MS = 750
const MAX_DELAY_MS = 15_000
const MAX_WAIT_MS = 15 * 60_000

export const createOperationId = (): string => crypto.randomUUID()

export const startControlJob = async (
  operationId: string,
  start: () => Promise<{ data: ControlJobModel }>,
  signal?: AbortSignal
): Promise<ControlJobModel> => {
  try {
    return (await start()).data
  } catch (startError) {
    if (signal?.aborted) throw startError
    try {
      return (await api.eventSecurity.getControlJobByOperation(operationId, { signal })).data
    } catch {
      throw startError
    }
  }
}

const abortError = () => new DOMException('Control-job polling was cancelled', 'AbortError')

const delay = (milliseconds: number, signal?: AbortSignal): Promise<void> =>
  new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(abortError())
      return
    }
    let timer = 0
    const onAbort = () => {
      window.clearTimeout(timer)
      reject(abortError())
    }
    timer = window.setTimeout(() => {
      signal?.removeEventListener('abort', onAbort)
      resolve()
    }, milliseconds)
    signal?.addEventListener('abort', onAbort, { once: true })
  })

const waitUntilPollable = async (signal?: AbortSignal): Promise<void> => {
  while (document.visibilityState === 'hidden' || !navigator.onLine) {
    await delay(500, signal)
  }
}

/** Completion-scheduled recovery for a known durable operation. */
export const waitForControlJob = async (
  initial: ControlJobModel,
  signal?: AbortSignal,
  load: (jobId: string, signal?: AbortSignal) => Promise<ControlJobModel> = async (jobId, requestSignal) =>
    (await api.eventSecurity.getControlJob(jobId, { signal: requestSignal })).data
): Promise<ControlJobModel> => {
  let current = initial
  let delayMs = BASE_DELAY_MS
  const deadline = Date.now() + MAX_WAIT_MS
  while (!TERMINAL.has(current.status)) {
    if (Date.now() >= deadline) throw new Error('Control job is still running; its operation ID can be recovered later.')
    await waitUntilPollable(signal)
    await delay(delayMs, signal)
    try {
      current = await load(current.id, signal)
      delayMs = BASE_DELAY_MS
    } catch {
      if (signal?.aborted) throw abortError()
      delayMs = Math.min(MAX_DELAY_MS, Math.ceil(delayMs * 1.75))
      continue
    }
  }
  if (current.status !== 'Succeeded') {
    throw new Error(current.error || `Control job ended as ${current.status}.`)
  }
  return current
}

export const controlJobResultCount = (job: ControlJobModel, key: string): number => {
  const value = job.result?.[key]
  return typeof value === 'number' && Number.isFinite(value) ? value : 0
}
