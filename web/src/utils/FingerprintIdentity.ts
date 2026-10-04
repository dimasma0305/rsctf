import type { TFunction } from 'i18next'
import api from '@Api'
import { encryptApiData } from '@Utils/Crypto'

export interface EncryptedFingerprintIdentity {
  fingerprint: string
  fingerprintProof: string
}

/**
 * One cancellation-aware browser-identity path shared by every account/team/
 * event enrollment flow. Optional probes are isolated inside the collector;
 * challenge-required evidence fails visibly instead of being fabricated.
 */
export const collectEncryptedFingerprintIdentity = async (
  t: TFunction,
  apiPublicKey: string | null | undefined,
  signal?: AbortSignal
): Promise<EncryptedFingerprintIdentity> => {
  if (signal?.aborted) throw new DOMException('Browser identity collection was cancelled', 'AbortError')
  const challengeResponse = await api.account.accountFingerprintChallenge({ signal })
  const challenge = challengeResponse.data.data
  if (!challenge?.nonce || !Array.isArray(challenge.requiredSignals)) {
    throw new Error('Invalid fingerprint challenge')
  }

  const { getFingerprintPayload } = await import('@Utils/BrowserFingerprint')
  const payload = await getFingerprintPayload(
    { nonce: challenge.nonce, requiredSignals: challenge.requiredSignals },
    { signal }
  )
  return {
    fingerprint: await encryptApiData(t, payload.fingerprint, apiPublicKey),
    fingerprintProof: await encryptApiData(t, payload.proof, apiPublicKey),
  }
}
