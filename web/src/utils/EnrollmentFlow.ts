import { collectFingerprintIdentity, FingerprintIdentityOptions } from '@Utils/FingerprintIdentity'
import { throwIfAborted } from '@Utils/FingerprintProbe'
import api, { GameJoinModel } from '@Api'

interface FingerprintOptions extends Pick<FingerprintIdentityOptions, 'apiPublicKey' | 'signal'> {
  enableBrowserFingerprint?: boolean
  t: FingerprintIdentityOptions['translate']
}

interface GameEnrollmentOptions extends FingerprintOptions {
  gameId: number
  info: GameJoinModel
}

interface TeamEnrollmentOptions extends FingerprintOptions {
  code: string
}

const collectEnrollmentIdentity = ({
  apiPublicKey,
  enableBrowserFingerprint,
  signal,
  t,
}: FingerprintOptions): Promise<Pick<GameJoinModel, 'fingerprint' | 'fingerprintProof'>> =>
  collectFingerprintIdentity({
    enabled: enableBrowserFingerprint,
    apiPublicKey,
    signal,
    translate: t,
  })

export const submitGameEnrollment = async ({ gameId, info, ...fingerprintOptions }: GameEnrollmentOptions) => {
  if (!Number.isSafeInteger(gameId) || gameId <= 0) throw new Error('Invalid game ID')

  const identity = await collectEnrollmentIdentity(fingerprintOptions)
  throwIfAborted(fingerprintOptions.signal)
  await api.game.gameJoinGame(gameId, { ...info, ...identity }, { signal: fingerprintOptions.signal })
  throwIfAborted(fingerprintOptions.signal)
}

export const submitTeamEnrollment = async ({ code, ...fingerprintOptions }: TeamEnrollmentOptions) => {
  const identity = await collectEnrollmentIdentity(fingerprintOptions)
  throwIfAborted(fingerprintOptions.signal)
  await api.team.teamAccept({ code, ...identity }, { signal: fingerprintOptions.signal })
  throwIfAborted(fingerprintOptions.signal)
}
