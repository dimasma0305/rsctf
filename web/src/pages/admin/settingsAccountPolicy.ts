import type { AccountPolicy } from '@Api'

export type AccountUniquenessStatus = 'configured' | 'attention'

export interface AccountUniquenessState {
  fingerprintCollectionEnabled: boolean
  hasEffectiveUniquenessPolicy: boolean
  hasIneffectiveFingerprintPolicy: boolean
  status: AccountUniquenessStatus
}

export const getAccountUniquenessState = (accountPolicy: AccountPolicy | null | undefined): AccountUniquenessState => {
  const fingerprintCollectionEnabled = accountPolicy?.enableBrowserFingerprint ?? false
  const hasIpUniquenessPolicy = Boolean(
    accountPolicy?.requireUniqueIpPerTeamUser || accountPolicy?.requireUniqueIpGlobal
  )
  const hasFingerprintUniquenessPolicy = Boolean(
    accountPolicy?.requireUniqueFingerprintPerTeamUser || accountPolicy?.requireUniqueFingerprintGlobal
  )
  const hasIneffectiveFingerprintPolicy = hasFingerprintUniquenessPolicy && !fingerprintCollectionEnabled
  const hasEffectiveUniquenessPolicy =
    hasIpUniquenessPolicy || (fingerprintCollectionEnabled && hasFingerprintUniquenessPolicy)

  return {
    fingerprintCollectionEnabled,
    hasEffectiveUniquenessPolicy,
    hasIneffectiveFingerprintPolicy,
    status: hasIneffectiveFingerprintPolicy || !hasEffectiveUniquenessPolicy ? 'attention' : 'configured',
  }
}

export const setBrowserFingerprintCollection = (
  accountPolicy: AccountPolicy | null | undefined,
  enabled: boolean
): AccountPolicy => ({
  ...accountPolicy,
  enableBrowserFingerprint: enabled,
  ...(enabled
    ? {}
    : {
        requireUniqueFingerprintPerTeamUser: false,
        requireUniqueFingerprintGlobal: false,
      }),
})
