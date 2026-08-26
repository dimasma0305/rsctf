import { encryptApiData } from '@Utils/Crypto'
import api, { GameJoinModel } from '@Api'

type Translate = Parameters<typeof encryptApiData>[0]

interface FingerprintOptions {
  enableBrowserFingerprint?: boolean
  apiPublicKey?: string | null
  t: Translate
}

interface GameEnrollmentOptions extends FingerprintOptions {
  gameId: number
  info: GameJoinModel
}

interface TeamEnrollmentOptions extends FingerprintOptions {
  code: string
}

const collectEnrollmentIdentity = async ({
  apiPublicKey,
  t,
}: FingerprintOptions): Promise<Pick<GameJoinModel, 'fingerprint' | 'fingerprintProof'>> => {
  const challengeResponse = await api.account.accountFingerprintChallenge()
  const challenge = challengeResponse.data.data
  if (!challenge?.nonce || !challenge.requiredSignals) {
    throw new Error('Invalid fingerprint challenge')
  }

  const { getFingerprintPayload } = await import('@Utils/BrowserFingerprint')
  const payload = await getFingerprintPayload({
    nonce: challenge.nonce,
    requiredSignals: challenge.requiredSignals,
  })

  return {
    fingerprint: await encryptApiData(t, payload.fingerprint, apiPublicKey),
    fingerprintProof: await encryptApiData(t, payload.proof, apiPublicKey),
  }
}

export const submitGameEnrollment = async ({ gameId, info, ...fingerprintOptions }: GameEnrollmentOptions) => {
  if (!Number.isSafeInteger(gameId) || gameId <= 0) throw new Error('Invalid game ID')

  const identity = fingerprintOptions.enableBrowserFingerprint
    ? await collectEnrollmentIdentity(fingerprintOptions)
    : {}
  await api.game.gameJoinGame(gameId, { ...info, ...identity })
}

export const submitTeamEnrollment = async ({ code, ...fingerprintOptions }: TeamEnrollmentOptions) => {
  const identity = fingerprintOptions.enableBrowserFingerprint
    ? await collectEnrollmentIdentity(fingerprintOptions)
    : {}
  await api.team.teamAccept({ code, ...identity })
}
