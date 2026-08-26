import { encryptApiData } from '@Utils/Crypto'
import {
  collectFingerprintIdentityWith,
  FingerprintIdentityDependencies,
  FingerprintIdentityOptions,
} from '@Utils/FingerprintIdentityCore'
import type { EncryptedFingerprintIdentity } from '@Utils/FingerprintIdentityCore'
import { FingerprintCollectionError, throwIfAborted } from '@Utils/FingerprintProbe'
import api from '@Api'

export type { EncryptedFingerprintIdentity, FingerprintIdentityOptions } from '@Utils/FingerprintIdentityCore'

const defaultDependencies = (options: FingerprintIdentityOptions): FingerprintIdentityDependencies => ({
  requestChallenge: async (signal) => {
    const response = await api.account.accountFingerprintChallenge({ signal })
    const challenge = response.data.data
    if (!challenge?.nonce || !Array.isArray(challenge.requiredSignals)) {
      throw new FingerprintCollectionError('fingerprint-unavailable')
    }
    return { nonce: challenge.nonce, requiredSignals: challenge.requiredSignals }
  },
  collectPayload: async (challenge, signal) => {
    const { getFingerprintPayload } = await import('@Utils/BrowserFingerprint')
    return getFingerprintPayload(challenge, { signal })
  },
  encrypt: (value) => encryptApiData(options.translate, value, options.apiPublicKey),
})

export const collectFingerprintIdentity = async (
  options: FingerprintIdentityOptions
): Promise<EncryptedFingerprintIdentity> => {
  throwIfAborted(options.signal)
  return collectFingerprintIdentityWith(options, defaultDependencies(options))
}
