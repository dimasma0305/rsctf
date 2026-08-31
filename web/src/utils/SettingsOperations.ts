import { sha256 } from 'js-sha256'
import type { ConfigEditModel, SettingsMutationResult } from '@Api'
import { createUuid } from './Uuid'

const SETTINGS_OPERATION_STORAGE_KEY = 'rsctf:settings-operation:v1'

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
export const dirtySettingsSections = (baseline: ConfigEditModel, current: ConfigEditModel): ConfigEditModel => {
  const dirty: ConfigEditModel = {}
  for (const section of EDITABLE_SECTIONS) {
    if (!sameValue(baseline[section], current[section])) {
      Object.assign(dirty, { [section]: current[section] })
    }
  }
  return dirty
}

export const settingsBrandingDigest = async (branding: Blob | null): Promise<string | null> =>
  branding ? sha256(await branding.arrayBuffer()) : null

export const settingsRequestSignature = (request: ConfigEditModel, brandingDigest: string | null = null): string =>
  sha256(JSON.stringify([request, brandingDigest]))

export const newSettingsOperationId = (): string => createUuid()

export const loadSettingsOperation = (): SettingsOperationOwner | null => {
  try {
    const raw = globalThis.sessionStorage?.getItem(SETTINGS_OPERATION_STORAGE_KEY)
    if (!raw) return null
    const value = JSON.parse(raw) as Partial<SettingsOperationOwner>
    if (
      typeof value.operationId !== 'string' ||
      typeof value.signature !== 'string' ||
      !/^[0-9a-f-]{36}$/i.test(value.operationId) ||
      !/^[0-9a-f]{64}$/i.test(value.signature) ||
      !Number.isSafeInteger(value.expectedRevision) ||
      (value.expectedRevision ?? -1) < 0
    ) {
      globalThis.sessionStorage?.removeItem(SETTINGS_OPERATION_STORAGE_KEY)
      return null
    }
    return value as SettingsOperationOwner
  } catch {
    return null
  }
}

export const storeSettingsOperation = (owner: SettingsOperationOwner): void => {
  try {
    globalThis.sessionStorage?.setItem(SETTINGS_OPERATION_STORAGE_KEY, JSON.stringify(owner))
  } catch {
    // In-memory ownership remains authoritative when storage is unavailable.
  }
}

export const clearSettingsOperation = (): void => {
  try {
    globalThis.sessionStorage?.removeItem(SETTINGS_OPERATION_STORAGE_KEY)
  } catch {
    // A disabled storage backend must not turn a completed save into failure.
  }
}

export const ownsSettingsResult = (owner: SettingsOperationOwner, result: SettingsMutationResult): boolean =>
  result.operationId === owner.operationId && result.revision === owner.expectedRevision + 1
