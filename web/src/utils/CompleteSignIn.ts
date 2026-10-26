import type { ProfileUserInfoModel } from '@Api'
import { commitAuthSession } from './AuthState'

interface SignInCompletion {
  signal: AbortSignal
  isCurrent: () => boolean
  loadProfile: (signal: AbortSignal) => Promise<ProfileUserInfoModel>
  publishProfile: (profile: ProfileUserInfoModel) => Promise<unknown>
  navigate: () => void
}

/** Finish both password login and immediate registration from a fresh profile. */
export const completeSignIn = async ({
  signal,
  isCurrent,
  loadProfile,
  publishProfile,
  navigate,
}: SignInCompletion) => {
  if (signal.aborted || !isCurrent()) return false
  const profile = await loadProfile(signal)
  if (signal.aborted || !isCurrent()) return false

  // SWR synchronously publishes a resolved model with revalidate:false. Start
  // navigation in the same turn: publishing our own identity can unmount the
  // form and cancel its request owner, just like an ordinary account switch.
  const publication = publishProfile(profile)
  commitAuthSession()
  navigate()
  await publication
  return true
}
