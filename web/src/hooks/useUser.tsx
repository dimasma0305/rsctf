import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { useCallback, useEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router'
import { type KeyedMutator, type MutatorCallback, type MutatorOptions, useSWRConfig } from 'swr'
import { setAuthSession } from '@Utils/AuthState'
import { clearLegacySensitiveBrowserStorage } from '@Utils/Cache'
import { invalidatePlayerReads, TEAM_SELECTOR_PATH } from '@Utils/PlayerReadCache'
import { createProfileRetryTimers, profileErrorDisposition, profileRetryScheduleDelay } from '@Utils/ProfileRetry'
import api, { type TeamInfoModel } from '@Api'

const handledBannedProfileErrors = new WeakSet<object>()

export const useUser = () => {
  const navigate = useNavigate()
  const { t } = useTranslation()
  const retryTimers = useRef(createProfileRetryTimers())
  const handledTerminalError = useRef<unknown>(null)

  const {
    data: user,
    error,
    mutate,
  } = api.account.useAccountProfile({
    refreshInterval: 0,
    refreshWhenHidden: false,
    refreshWhenOffline: false,
    shouldRetryOnError: (err) => profileErrorDisposition(err) === 'retry',
    revalidateOnFocus: false,
    onErrorRetry: (err, _key, _config, revalidate, { retryCount }) => {
      const delay = profileRetryScheduleDelay(err, retryCount)
      if (delay === null) return
      retryTimers.current.schedule(
        delay,
        () => revalidate({ retryCount }),
        () => (_config.refreshWhenHidden || _config.isVisible()) && (_config.refreshWhenOffline || _config.isOnline())
      )
    },
  })
  const disposition = profileErrorDisposition(error)
  const currentUser = disposition === 'anonymous' || disposition === 'banned' ? undefined : user

  // A user replacement and an unmount both invalidate retry ownership.
  useEffect(() => {
    retryTimers.current.cancel()
    handledTerminalError.current = null
    return () => retryTimers.current.cancel()
  }, [currentUser?.userId])

  // SWR can retain the same profile while a transient revalidation fails.
  // Cancel its queued retry as soon as that same account recovers.
  useEffect(() => {
    if (currentUser && !error) retryTimers.current.cancel()
  }, [currentUser, error])

  // Terminal session effects cannot live in onErrorRetry: SWR deliberately
  // skips that callback when shouldRetryOnError rejects the status.
  useEffect(() => {
    const disposition = profileErrorDisposition(error)
    if ((disposition !== 'anonymous' && disposition !== 'banned') || handledTerminalError.current === error) return
    handledTerminalError.current = error
    retryTimers.current.cancel()
    setAuthSession(false)
    // Keep the terminal error cached. Clearing it remounts the pending viewer
    // scope and turns one anonymous probe into an unbounded 401 loop.

    if (disposition !== 'banned' || !error || typeof error !== 'object') return
    if (handledBannedProfileErrors.has(error)) return
    handledBannedProfileErrors.add(error)

    void api.account
      .accountLogOut()
      .catch(() => undefined)
      .finally(() => {
        navigate('/')
        showNotification({
          color: 'red',
          message: t('account.notification.login.banned'),
          icon: <Icon path={mdiClose} size={1} />,
        })
      })
  }, [error, navigate, t])

  // Feed the global 401 interceptor's "is there a session?" belief. A loaded
  // profile means logged in; a 401 on the profile probe means anonymous (or
  // expired). This is what lets public pages render for logged-out visitors
  // instead of redirecting them to login on an optional [RequireUser] fetch.
  useEffect(() => {
    if (currentUser) setAuthSession(true)
    else if (disposition === 'anonymous') setAuthSession(false)
  }, [currentUser, disposition])

  return { user: currentUser, error, mutate }
}

export const useUserRole = () => {
  const { user, error } = useUser()
  return { role: user?.role, error }
}

export const useTeams = () => {
  const { mutate: mutateCache } = useSWRConfig()
  const {
    data: teams,
    error,
    mutate: mutateTeams,
  } = api.team.useTeamGetTeamsInfo({
    refreshInterval: 0,
    refreshWhenHidden: false,
    refreshWhenOffline: false,
    shouldRetryOnError: false,
    revalidateOnFocus: false,
    revalidateOnReconnect: false,
  })

  const mutate: KeyedMutator<TeamInfoModel[]> = useCallback(
    async <MutationData = TeamInfoModel[],>(
      data?: TeamInfoModel[] | Promise<TeamInfoModel[] | undefined> | MutatorCallback<TeamInfoModel[]>,
      options?: boolean | MutatorOptions<TeamInfoModel[], MutationData>
    ) => {
      const result = await mutateTeams<MutationData>(data, options)
      await invalidatePlayerReads(mutateCache, [TEAM_SELECTOR_PATH])
      return result
    },
    [mutateCache, mutateTeams]
  )

  return { teams, error, mutate }
}

/** Compact, one-shot team choices for event enrollment. */
export const useTeamSelector = (active = true) => {
  const query = api.team.useTeamGetSelector(
    {
      refreshInterval: 0,
      refreshWhenHidden: false,
      refreshWhenOffline: false,
      shouldRetryOnError: false,
      revalidateOnFocus: false,
      revalidateOnReconnect: false,
    },
    active
  )

  return { teams: query.data, error: query.error, mutate: query.mutate }
}

export const useLogOut = () => {
  const navigate = useNavigate()
  const { mutate } = useSWRConfig()
  const { mutate: mutateProfile } = useUser()
  const { t } = useTranslation()

  return async () => {
    try {
      await api.account.accountLogOut()
      clearLegacySensitiveBrowserStorage()
      navigate('/')
      mutate((key) => typeof key === 'string' && key.includes('game/'), undefined, {
        revalidate: false,
      })
      mutateProfile(undefined, { revalidate: false })
      showNotification({
        color: 'teal',
        message: t('account.notification.logout'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch {
      clearLegacySensitiveBrowserStorage()
      navigate('/')
      mutateProfile(undefined, { revalidate: false })
    }
  }
}
