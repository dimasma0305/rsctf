import type { ConfigEditModel, SettingsMutationResult } from '@Api'

const EDITABLE_SECTIONS = [
  'globalConfig',
  'accountPolicy',
  'containerPolicy',
  'containerProvider',
  'buildRegistry',
  'email',
  'captcha',
  'oAuth',
  'registry',
  'donations',
] as const satisfies readonly (keyof ConfigEditModel)[]

export interface SettingsOperationOwner {
  operationId: string
  expectedRevision: number
  signature: string
}

const sameValue = (left: unknown, right: unknown): boolean => JSON.stringify(left) === JSON.stringify(right)

/** Build the partial wire model; unchanged sections never reach the server. */
export const dirtySettingsSections = (
  baseline: ConfigEditModel,
  current: ConfigEditModel
): ConfigEditModel => {
  const dirty: ConfigEditModel = {}
  for (const section of EDITABLE_SECTIONS) {
    if (!sameValue(baseline[section], current[section])) {
      Object.assign(dirty, { [section]: current[section] })
    }
  }
  return dirty
}

export const settingsRequestSignature = (request: ConfigEditModel): string => JSON.stringify(request)

export const newSettingsOperationId = (): string => {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID()
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  bytes[6] = (bytes[6] & 0x0f) | 0x40
  bytes[8] = (bytes[8] & 0x3f) | 0x80
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}

export const ownsSettingsResult = (
  owner: SettingsOperationOwner,
  result: SettingsMutationResult
): boolean => result.operationId === owner.operationId && result.revision === owner.expectedRevision + 1
