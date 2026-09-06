import { httpErrorStatus } from './HttpError'

export const WRITEUP_MAX_BYTES = 20 * 1024 * 1024

/** Client hints only; the server still enforces the file and event boundaries. */
export const writeupFileProblem = (file: Pick<File, 'name' | 'size' | 'type'>) => {
  if (!file.name.endsWith('.pdf') || (file.type !== '' && file.type !== 'application/pdf')) return 'format'
  if (file.size === 0) return 'empty'
  if (file.size > WRITEUP_MAX_BYTES) return 'size'
  return null
}

export const writeupUploadPercent = (loaded: number, total?: number): number | null => {
  if (!Number.isFinite(loaded) || !total || !Number.isFinite(total) || total <= 0) return null
  return Math.min(100, Math.max(0, Math.floor((loaded / total) * 100)))
}

export const isWriteupDeadlineError = (error: unknown): boolean => {
  const response = (error as { response?: { status?: unknown; data?: { status?: unknown; title?: unknown } } })
    ?.response
  const status = response?.data?.status ?? response?.status
  const title = response?.data?.title
  return (
    (status === 400 && title === 'Writeup deadline has passed') ||
    (status === 409 && title === 'Writeup submission is no longer eligible')
  )
}

export const writeupFailureKind = (error: unknown) => {
  if (isWriteupDeadlineError(error)) return 'deadline'
  const status = httpErrorStatus(error)
  if (status === 401) return 'session'
  if (status === 413) return 'size'
  if (status === 429 || status === 503) return 'busy'
  if (status === null || status === 408 || status === 502 || status === 504) return 'connection'
  return 'other'
}
