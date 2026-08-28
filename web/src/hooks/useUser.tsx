import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { useEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router'
import { useSWRConfig } from 'swr'
import { clearLegacyAdTokenStorage } from '@Utils/AdTokenMemory'
import { setAuthSession } from '@Utils/AuthState'
import { createProfileRetryTimers, profileErrorDisposition, profileRetryScheduleDelay } from '@Utils/ProfileRetry'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api from '@Api'

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

  // A user replacement and an unmount both invalidate retry ownership.
  useEffect(() => {
    retryTimers.current.cancel()
    handledTerminalError.current = null
    return () => retryTimers.current.cancel()
  }, [user?.userId])

  // SWR can retain the same profile while a transient revalidation fails.
  // Cancel its queued retry as soon as that same account recovers.
  useEffect(() => {
    if (user && !error) retryTimers.current.cancel()
  }, [user, error])

  // Terminal session effects cannot live in onErrorRetry: SWR deliberately
  // skips that callback when shouldRetryOnError rejects the status.
  useEffect(() => {
    const disposition = profileErrorDisposition(error)
    if ((disposition !== 'anonymous' && disposition !== 'banned') || handledTerminalError.current === error) return
    handledTerminalError.current = error
    retryTimers.current.cancel()
    setAuthSession(false)
    void mutate(undefined, { revalidate: false })

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
  }, [error, mutate, navigate, t])

  // Feed the global 401 interceptor's "is there a session?" belief. A loaded
  // profile means logged in; a 401 on the profile probe means anonymous (or
  // expired). This is what lets public pages render for logged-out visitors
  // instead of redirecting them to login on an optional [RequireUser] fetch.
  useEffect(() => {
    if (user) setAuthSession(true)
    else if (profileErrorDisposition(error) === 'anonymous') setAuthSession(false)
  }, [user, error])

  return { user, error, mutate }
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
    mutate: mutateTeamList,
  } = api.team.useTeamGetTeamsInfo(OnceSWRConfig)

  const mutate: typeof mutateTeamList = async (data, options) => {
    const result = await mutateTeamList(data, options)
    await mutateCache('/api/team/selector')
    return result
  }

  return { teams, error, mutate }
}

export const useTeamSelector = (enabled: boolean = true) => {
  const {
    data: teams,
    error,
    mutate,
  } = api.team.useTeamGetTeamSelector(OnceSWRConfig, enabled)
  return { teams, error, mutate }
}

export const useLogOut = () => {
  const navigate = useNavigate()
  const { mutate } = useSWRConfig()
  const { mutate: mutateProfile } = useUser()
  const { t } = useTranslation()

  return async () => {
    try {
      await api.account.accountLogOut()
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
      navigate('/')
      mutateProfile(undefined, { revalidate: false })
    } finally {
      clearLegacyAdTokenStorage()
    }
  }
}
