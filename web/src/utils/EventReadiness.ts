import type { ChallengeInfoModel, GameInfoModel } from '@Api'
import { httpErrorStatus } from './HttpError'

export type ReadinessState = 'checked' | 'attention' | 'unverified' | 'info'
export type ReadinessCheck = {
  key: 'schedule' | 'freeze' | 'writeup' | 'challenges' | 'builds'
  state: ReadinessState
  reason: string
  count?: number
  section: 'info' | 'challenges'
}

const validTime = (value: unknown): value is number =>
  typeof value === 'number' && Number.isFinite(value) && Math.abs(value) <= 8.64e15

/** Saved-configuration advice only. Never infer runtime health or authorize a launch. */
export function eventReadiness(game: GameInfoModel, challenges: ChallengeInfoModel[]): ReadinessCheck[] {
  const scheduleValid = validTime(game.start) && validTime(game.end) && game.start < game.end
  const freezeValid = scheduleValid && validTime(game.freeze) && game.freeze > game.start && game.freeze < game.end
  const writeupValid = scheduleValid && validTime(game.writeupDeadline) && game.writeupDeadline >= game.end
  const enabled = challenges.filter((challenge) => challenge.isEnabled)
  const active = enabled.filter((challenge) => challenge.reviewStatus === 'Active')
  const unreviewed = enabled.length - active.length
  const failed = active.filter((challenge) =>
    ['Failed', 'MissingDockerfile'].includes(challenge.buildStatus ?? 'None')
  ).length
  const building = active.filter((challenge) => ['Queued', 'Building'].includes(challenge.buildStatus ?? 'None')).length
  const unknownBuilds = active.filter(
    (challenge) => !['Success', 'NotApplicable'].includes(challenge.buildStatus ?? 'None')
  ).length

  return [
    {
      key: 'schedule',
      section: 'info',
      state: scheduleValid ? 'checked' : 'attention',
      reason: scheduleValid ? 'schedule_ok' : 'schedule_invalid',
    },
    {
      key: 'freeze',
      section: 'info',
      state: game.freeze == null ? 'info' : freezeValid ? 'checked' : 'attention',
      reason: game.freeze == null ? 'freeze_off' : freezeValid ? 'freeze_ok' : 'freeze_invalid',
    },
    {
      key: 'writeup',
      section: 'info',
      state: !game.writeupRequired ? 'info' : writeupValid ? 'checked' : 'attention',
      reason: !game.writeupRequired ? 'writeup_off' : writeupValid ? 'writeup_ok' : 'writeup_review',
    },
    {
      key: 'challenges',
      section: 'challenges',
      state: unreviewed || !active.length ? 'attention' : 'checked',
      reason: unreviewed ? 'challenges_review' : !active.length ? 'challenges_empty' : 'challenges_ok',
      count: unreviewed || active.length,
    },
    {
      key: 'builds',
      section: 'challenges',
      state: failed ? 'attention' : building || unknownBuilds ? 'unverified' : 'info',
      reason: failed
        ? 'builds_failed'
        : building
          ? 'builds_running'
          : unknownBuilds
            ? 'builds_unknown'
            : 'builds_recorded',
      count: failed || building || unknownBuilds,
    },
  ]
}

export function readinessError(error: unknown) {
  const status = httpErrorStatus(error)
  if (status === 401) return 'session'
  if (status === 403) return 'permission'
  if (status === 404) return 'missing'
  if (status === 429) return 'busy'
  if (status !== null && status >= 500) return 'service'
  return 'request'
}

/** Never retain private cached content after access is denied or the event is removed. */
export const canShowReadinessCache = (errors: unknown[]) =>
  !errors.some((error) => ['session', 'permission', 'missing'].includes(readinessError(error)))

export type ManualReadinessKey = 'teams' | 'attachments' | 'services' | 'vpn'
export type ManualReadinessItem = { key: ManualReadinessKey; section: 'review' | 'challenges' | 'adops' | 'info' }

/** Runtime checks this page cannot perform; only the relevant ones are listed. */
export function manualReadinessItems(options: { competitiveServices: boolean; vpnRequired: boolean }) {
  const items: ManualReadinessItem[] = [
    { key: 'teams', section: 'review' },
    { key: 'attachments', section: 'challenges' },
  ]
  if (options.competitiveServices) items.push({ key: 'services', section: 'adops' })
  if (options.vpnRequired) items.push({ key: 'vpn', section: 'info' })
  return items
}

export function readinessCounts(checks: Pick<ReadinessCheck, 'state'>[]) {
  const counts: Record<ReadinessState, number> = { attention: 0, unverified: 0, checked: 0, info: 0 }
  for (const check of checks) counts[check.state] += 1
  return counts
}

const MANUAL_KEYS: readonly ManualReadinessKey[] = ['teams', 'attachments', 'services', 'vpn']
const manualStorageKey = (eventId: number) => `rsctf-readiness-manual:${eventId}`

type StorageLike = Pick<Storage, 'getItem' | 'setItem'>
const browserStorage = (): StorageLike | undefined => {
  try {
    return globalThis.localStorage
  } catch {
    return undefined
  }
}

/**
 * The organizer's own manual ticks for one event. Stored only in this browser as a
 * convenience; unavailable or corrupt storage simply yields an empty checklist.
 */
export function loadManualReadiness(eventId: number, storage = browserStorage()): ManualReadinessKey[] {
  try {
    const parsed: unknown = JSON.parse(storage?.getItem(manualStorageKey(eventId)) ?? '[]')
    if (!Array.isArray(parsed)) return []
    return MANUAL_KEYS.filter((key) => parsed.includes(key))
  } catch {
    return []
  }
}

export function saveManualReadiness(eventId: number, keys: ManualReadinessKey[], storage = browserStorage()) {
  try {
    storage?.setItem(manualStorageKey(eventId), JSON.stringify(MANUAL_KEYS.filter((key) => keys.includes(key))))
  } catch {
    // Private windows and blocked storage keep the in-memory ticks only.
  }
}

/** Whole days, hours, and minutes of a positive event window, for a compact label. */
export function readinessDuration(start: number, end: number) {
  const minutes = Math.max(0, Math.round((end - start) / 60_000))
  return { days: Math.floor(minutes / 1440), hours: Math.floor((minutes % 1440) / 60), minutes: minutes % 60 }
}

/** The headline for the overview: attention first, then unverified, else all clear. */
export function readinessVerdict(counts: Record<ReadinessState, number>) {
  if (counts.attention) return { state: 'attention' as const, count: counts.attention }
  if (counts.unverified) return { state: 'unverified' as const, count: counts.unverified }
  return { state: 'checked' as const, count: counts.checked + counts.info }
}
