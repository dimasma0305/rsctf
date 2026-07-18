import { Center, Loader, MantineProvider } from '@mantine/core'
import { DatesProvider } from '@mantine/dates'
import { emotionTransform, MantineEmotionProvider } from '@mantine/emotion'
import { ModalsProvider } from '@mantine/modals'
import { Notifications } from '@mantine/notifications'
import i18next from 'i18next'
import { FC, Suspense, useEffect, useRef, useState } from 'react'
import { ErrorBoundary } from 'react-error-boundary'
import { useTranslation } from 'react-i18next'
import { useLocation, useRoutes } from 'react-router'
import { SWRConfig } from 'swr'
import routes from '~react-pages'
import { ErrorFallback } from '@Components/ErrorFallback'
import { WsrxProvider } from '@Components/WsrxProvider'
import { shouldRedirectOnUnauthorized } from '@Utils/AuthState'
import { localCacheProvider } from '@Utils/Cache'
import { useLanguage } from '@Utils/I18n'
import { useCustomTheme } from '@Utils/ThemeOverride'
import { useBanner } from '@Hooks/useConfig'
import { fetcher as rawFetcher } from '@Api'
import '@mantine/core/styles.css'
import '@mantine/dates/styles.css'
import '@mantine/dropzone/styles.css'
import '@mantine/notifications/styles.css'
import './styles/App.css'

/**
 * Wraps the generated swagger fetcher so any 401 globally redirects
 * to /account/login?from=<current> and pops a "session expired" toast.
 * This replaces the old silent-empty-state behaviour after cookie
 * expiry.  Flagged on window so the interceptor fires at most once
 * per navigation.
 */
let authRedirectInFlight = false
const authAwareFetcher = async (args: Parameters<typeof rawFetcher>[0]) => {
  try {
    return await rawFetcher(args)
  } catch (e: unknown) {
    const status = (e as { status?: number } | undefined)?.status
    const path = typeof args === 'string' ? args : args[0]
    // Only a genuine session expiry redirects. An anonymous visitor on a public
    // page (e.g. a scoreboard that fires an optional [RequireUser] /details
    // fetch) must render the public view, not bounce to login.
    if (
      typeof window !== 'undefined' &&
      shouldRedirectOnUnauthorized({
        status,
        requestPath: path,
        pathname: window.location.pathname,
        redirectInFlight: authRedirectInFlight,
      })
    ) {
      authRedirectInFlight = true
      try {
        const { showNotification } = await import('@mantine/notifications')
        showNotification({
          id: 'session-expired',
          color: 'red',
          title: i18next.t('common.error.session_expired', 'Session expired'),
          message: i18next.t('common.content.relogin', 'Please log in again.'),
        })
      } catch {
        // notifications unavailable — skip toast, still redirect
      }
      const from = window.location.pathname + window.location.search
      window.location.href = `/account/login?from=${encodeURIComponent(from)}`
    }
    throw e
  }
}

const RouteAccessibility: FC = () => {
  const location = useLocation()
  const [announcement, setAnnouncement] = useState('')
  const lastMain = useRef<HTMLElement | null>(null)

  useEffect(() => {
    let observer: MutationObserver | undefined
    const focusNewMain = () => {
      const main = document.getElementById('main-content')
      if (!main || main === lastMain.current) return false

      main.focus({ preventScroll: true })
      lastMain.current = main
      setAnnouncement(document.title)
      observer?.disconnect()
      return true
    }

    const timeout = window.setTimeout(() => {
      if (focusNewMain()) return

      observer = new MutationObserver(focusNewMain)
      observer.observe(document.getElementById('root') ?? document.body, { childList: true, subtree: true })
    }, 0)
    const observerTimeout = window.setTimeout(() => observer?.disconnect(), 10_000)

    return () => {
      window.clearTimeout(timeout)
      window.clearTimeout(observerTimeout)
      observer?.disconnect()
    }
  }, [location.pathname])

  return (
    <div className="app-sr-only" role="status" aria-live="polite" aria-atomic="true">
      {announcement}
    </div>
  )
}

const ThemedApp: FC = () => {
  useBanner()

  const { t } = useTranslation()
  const { locale } = useLanguage()
  const { theme } = useCustomTheme()

  return (
    <MantineProvider theme={theme} defaultColorScheme="dark" deduplicateInlineStyles stylesTransform={emotionTransform}>
      <MantineEmotionProvider>
        <ErrorBoundary FallbackComponent={ErrorFallback}>
          <Notifications position="top-right" limit={5} zIndex={5000} />
          <DatesProvider settings={{ locale }}>
            <ModalsProvider labels={{ confirm: t('common.modal.confirm'), cancel: t('common.modal.cancel') }}>
              <WsrxProvider>
                <RouteAccessibility />
                <Suspense
                  fallback={
                    <Center h="100vh" w="100vw" role="status" aria-live="polite">
                      <Loader aria-label={t('common.content.loading', 'Loading')} />
                    </Center>
                  }
                >
                  {useRoutes(routes)}
                </Suspense>
              </WsrxProvider>
            </ModalsProvider>
          </DatesProvider>
        </ErrorBoundary>
      </MantineEmotionProvider>
    </MantineProvider>
  )
}

export const App: FC = () => (
  <SWRConfig
    value={{
      // Keep the theme/config hooks and every route on one cache. In particular,
      // the admin settings mutation must reach useCustomTheme immediately.
      refreshInterval: 60_000,
      keepPreviousData: true,
      provider: localCacheProvider,
      fetcher: authAwareFetcher,
    }}
  >
    <ThemedApp />
  </SWRConfig>
)
