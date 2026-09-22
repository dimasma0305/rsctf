import type { ControlJobModel, ImagePreflightModel, ImagePreflightResultModel } from '@Api'

/** Poll cadence while a preflight job is queued or running. */
export const PREFLIGHT_POLL_MS = 2_000

export type PreflightRowState = 'pending' | 'running' | 'succeeded' | 'failed' | 'skipped'

export const PREFLIGHT_STATE_COLOR: Record<PreflightRowState, string> = {
  pending: 'gray',
  running: 'blue',
  succeeded: 'teal',
  failed: 'red',
  skipped: 'gray',
}

export const isPreflightActive = (job: ControlJobModel | null | undefined): boolean =>
  job?.status === 'Queued' || job?.status === 'Running'

/**
 * Completion-poll delay for the preflight read: keep polling only while the
 * latest job is still queued or running; `null` stops the timer owner.
 */
export const preflightPollDelay = (data: ImagePreflightModel | undefined): number | null =>
  data && isPreflightActive(data.job) ? PREFLIGHT_POLL_MS : null

/**
 * One state per challenge row: any failed step fails the row, a successful
 * start succeeds it, a row the planner skipped stays skipped, and running
 * work shows before pending work.
 */
export const preflightRowState = (
  row: Pick<ImagePreflightResultModel, 'pullStatus' | 'startStatus'>
): PreflightRowState => {
  if (row.pullStatus === 'Failed' || row.startStatus === 'Failed') return 'failed'
  if (row.startStatus === 'Succeeded') return 'succeeded'
  if (row.pullStatus === 'Skipped' && row.startStatus === 'Skipped') return 'skipped'
  if (row.pullStatus === 'Running' || row.startStatus === 'Running') return 'running'
  return 'pending'
}

export const formatPreflightCpu = (cpuMillis: number): string => {
  if (!Number.isFinite(cpuMillis) || cpuMillis <= 0) return '0 vCPU'
  const cores = cpuMillis / 1000
  return `${Number.isInteger(cores) ? cores : cores.toFixed(1)} vCPU`
}

export const formatPreflightBytes = (bytes: number): string => {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 MiB'
  const gibibytes = bytes / 1024 ** 3
  if (gibibytes >= 1) return `${gibibytes >= 10 ? Math.round(gibibytes) : gibibytes.toFixed(1)} GiB`
  return `${Math.round(bytes / 1024 ** 2)} MiB`
}

export const formatPreflightDuration = (milliseconds: number): string => {
  if (!Number.isFinite(milliseconds) || milliseconds <= 0) return '—'
  if (milliseconds < 1000) return `${Math.round(milliseconds)}ms`
  if (milliseconds < 60_000) return `${(milliseconds / 1000).toFixed(1)}s`
  return `${Math.floor(milliseconds / 60_000)}m ${Math.floor((milliseconds % 60_000) / 1000)}s`
}
