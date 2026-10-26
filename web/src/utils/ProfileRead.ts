import type { Cache } from 'swr'
import type { ProfileUserInfoModel } from '@Api'
import { authSessionRevision } from './AuthState'

interface ProfileState {
  data?: ProfileUserInfoModel
  error?: unknown
}

/** SWR fences stale successful reads against mutations, but late 401 errors
 * can still overwrite them. Resolve that old anonymous read from the fresh
 * committed profile in the same canonical cache, so SWR discards it normally. */
export const readProfile = async (cache: Cache, read: () => Promise<ProfileUserInfoModel>) => {
  const revision = authSessionRevision()
  const initial = (cache.get('/api/account/profile') as ProfileState | undefined)?.data
  try {
    return await read()
  } catch (error) {
    const current = cache.get('/api/account/profile') as ProfileState | undefined
    if (
      revision !== authSessionRevision() &&
      (error as { status?: number } | null)?.status === 401 &&
      current?.data &&
      current.data !== initial &&
      current.error === undefined
    )
      return current.data
    throw error
  }
}
