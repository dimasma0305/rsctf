import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router'
import { useSWRConfig } from 'swr'
import { setAuthSession } from '@Utils/AuthState'
import api from '@Api'

export const useUser = () => {
  const navigate = useNavigate()
  const { t } = useTranslation()

  const {
    data: user,
    error,
    mutate,
  } = api.account.useAccountProfile({
    refreshInterval: 0,
    shouldRetryOnError: false,
    revalidateOnFocus: false,
    onErrorRetry: async (err, _key, _config, revalidate, { retryCount }) => {
      if (err?.status === 403) {
        await api.account.accountLogOut()
        navigate('/')
        showNotification({
          color: 'red',
          message: t('account.notification.login.banned'),
          icon: <Icon path={mdiClose} size={1} />,
        })
        return
      }

      if (err?.status === 401 || retryCount >= 5) {
        mutate(undefined, false)
        return
      }

      setTimeout(() => revalidate({ retryCount: retryCount }), 10000)
    },
  })

  // Feed the global 401 interceptor's "is there a session?" belief. A loaded
  // profile means logged in; a 401 on the profile probe means anonymous (or
  // expired). This is what lets public pages render for logged-out visitors
  // instead of redirecting them to login on an optional [RequireUser] fetch.
  useEffect(() => {
    if (user) setAuthSession(true)
    else if (error?.status === 401) setAuthSession(false)
  }, [user, error])

  return { user, error, mutate }
}

export const useUserRole = () => {
  const { user, error } = useUser()
  return { role: user?.role, error }
}

export const useTeams = () => {
  const {
    data: teams,
    error,
    mutate,
  } = api.team.useTeamGetTeamsInfo({
    refreshInterval: 120000,
    shouldRetryOnError: false,
    revalidateOnFocus: false,
  })

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
    }
  }
}
