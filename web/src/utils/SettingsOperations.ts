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
  createdAt: number
}

export interface SettingsOperationStorage {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
  removeItem(key: string): void
}

const SETTINGS_OPERATION_KEY = 'rsctf:admin:platform-settings-operation'
const SETTINGS_OPERATION_MAX_AGE_MS = 60 * 60 * 1_000
const SETTINGS_OPERATION_MAX_BYTES = 1_024
const OPERATION_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i

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

type RetainedSettingsOperation = Omit<SettingsOperationOwner, 'signature'>

const validSettingsOperation = (value: unknown, now: number): value is RetainedSettingsOperation => {
  if (!value || typeof value !== 'object') return false
  const owner = value as Partial<RetainedSettingsOperation>
  return (
    typeof owner.operationId === 'string' &&
    OPERATION_ID.test(owner.operationId) &&
    typeof owner.expectedRevision === 'number' &&
    Number.isSafeInteger(owner.expectedRevision) &&
    owner.expectedRevision >= 0 &&
    typeof owner.createdAt === 'number' &&
    Number.isFinite(owner.createdAt) &&
    owner.createdAt <= now &&
    now - owner.createdAt <= SETTINGS_OPERATION_MAX_AGE_MS
  )
}

export const readSettingsOperation = (
  storage: SettingsOperationStorage,
  now: number = Date.now()
): SettingsOperationOwner | null => {
  const encoded = storage.getItem(SETTINGS_OPERATION_KEY)
  if (!encoded || encoded.length > SETTINGS_OPERATION_MAX_BYTES) {
    if (encoded) storage.removeItem(SETTINGS_OPERATION_KEY)
    return null
  }
  try {
    const parsed: unknown = JSON.parse(encoded)
    if (validSettingsOperation(parsed, now)) {
      // Request signatures can contain write-only settings. They remain in
      // memory for same-mount retries and are deliberately not persisted.
      return { ...parsed, signature: '' }
    }
  } catch {
    // Malformed tab-local state is never a mutation identity.
  }
  storage.removeItem(SETTINGS_OPERATION_KEY)
  return null
}

export const retainSettingsOperation = (
  storage: SettingsOperationStorage,
  owner: SettingsOperationOwner
): void => {
  const encoded = JSON.stringify({
    operationId: owner.operationId,
    expectedRevision: owner.expectedRevision,
    createdAt: owner.createdAt,
  } satisfies RetainedSettingsOperation)
  if (encoded.length > SETTINGS_OPERATION_MAX_BYTES) {
    throw new Error('Platform settings operation is too large to retain safely')
  }
  storage.setItem(SETTINGS_OPERATION_KEY, encoded)
}

export const clearSettingsOperation = (
  storage: SettingsOperationStorage,
  operationId?: string
): void => {
  if (!operationId) {
    storage.removeItem(SETTINGS_OPERATION_KEY)
    return
  }
  const current = readSettingsOperation(storage)
  if (current?.operationId === operationId) storage.removeItem(SETTINGS_OPERATION_KEY)
}

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
