import type { FingerprintChallenge, FingerprintPayload } from '@Utils/BrowserFingerprint'
import { throwIfAborted } from '@Utils/FingerprintProbe'

export interface EncryptedFingerprintIdentity {
  fingerprint?: string
  fingerprintProof?: string
}

export interface FingerprintIdentityOptions {
  enabled?: boolean
  apiPublicKey?: string | null
  signal?: AbortSignal
  translate: (key: string) => string
}

export interface FingerprintIdentityDependencies {
  requestChallenge: (signal?: AbortSignal) => Promise<FingerprintChallenge>
  collectPayload: (challenge: FingerprintChallenge, signal?: AbortSignal) => Promise<FingerprintPayload>
  encrypt: (value: string) => Promise<string>
}

export const collectFingerprintIdentityWith = async (
  options: FingerprintIdentityOptions,
  dependencies: FingerprintIdentityDependencies
): Promise<EncryptedFingerprintIdentity> => {
  if (!options.enabled) return {}

  throwIfAborted(options.signal)
  const challenge = await dependencies.requestChallenge(options.signal)
  throwIfAborted(options.signal)
  const payload = await dependencies.collectPayload(challenge, options.signal)
  throwIfAborted(options.signal)
  const [fingerprint, fingerprintProof] = await Promise.all([
    dependencies.encrypt(payload.fingerprint),
    dependencies.encrypt(payload.proof),
  ])
  throwIfAborted(options.signal)
  return { fingerprint, fingerprintProof }
}
