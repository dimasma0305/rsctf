import { createContext, FC, Fragment, PropsWithChildren, useContext, useLayoutEffect, useMemo, useRef } from 'react'
import { useLocation } from 'react-router'
import { type Key, type Middleware, useSWRConfig } from 'swr'
import { profileErrorDisposition } from '@Utils/ProfileRetry'
import { useUser } from '@Hooks/useUser'

const VIEWER_SCOPE_MARKER = 'rsctf-viewer-scope'

type ViewerScopedKey = readonly [typeof VIEWER_SCOPE_MARKER, string, Key]

interface ViewerIdentity {
  /**
   * Null only outside the application provider (principally isolated hook
   * tests). Inside the application every authenticated, anonymous, and
   * unresolved session has a separate cache namespace.
   */
  scope: string | null
}

const ViewerIdentityContext = createContext<ViewerIdentity>({ scope: null })

const viewerScope = (userId?: string, role?: string, anonymous = false) => {
  if (userId) return `user:${userId}:${role ?? 'unknown'}`
  return anonymous ? 'anonymous' : 'session-pending'
}

export const ViewerIdentityProvider: FC<PropsWithChildren> = ({ children }) => {
  const { user, error } = useUser()
  const { mutate } = useSWRConfig()
  const anonymous = ['anonymous', 'banned'].includes(profileErrorDisposition(error))
  const scope = viewerScope(user?.userId, user?.role, anonymous)
  const previousScope = useRef<string | null>(null)

  useLayoutEffect(() => {
    const previous = previousScope.current
    previousScope.current = scope
    if (!previous || previous === scope) return

    // Retire the old namespace as the account changes. This bounds persistent
    // browser storage and fences late old-session requests through SWR's mutate
    // timestamp instead of leaving dormant account copies indefinitely.
    void mutate((key) => isViewerScopedKey(key) && key[1] === previous, undefined, { revalidate: false })
  }, [mutate, scope])

  return <ViewerIdentityScope scope={scope}>{children}</ViewerIdentityScope>
}

export const ViewerIdentityScope: FC<PropsWithChildren<{ scope: string | null }>> = ({ scope, children }) => {
  const value = useMemo(() => ({ scope }), [scope])
  return <ViewerIdentityContext.Provider value={value}>{children}</ViewerIdentityContext.Provider>
}

export const useViewerIdentity = () => useContext(ViewerIdentityContext)

export const viewerScopedKey = (key: Key, scope: string | null): Key =>
  key && scope ? ([VIEWER_SCOPE_MARKER, scope, key] as const) : key

const isViewerScopedKey = (key: unknown): key is ViewerScopedKey =>
  Array.isArray(key) && key.length === 3 && key[0] === VIEWER_SCOPE_MARKER && typeof key[1] === 'string'

export const unwrapViewerScopedKey = (key: Key): Key => (isViewerScopedKey(key) ? key[2] : key)

export const swrRequestPath = (key: Key): string | null => {
  const requestKey = unwrapViewerScopedKey(key)
  if (typeof requestKey === 'string') return requestKey
  if (Array.isArray(requestKey) && typeof requestKey[0] === 'string') return requestKey[0]
  return null
}

/** Cache entries whose response can differ with identity or authorization. */
export const isViewerScopedRequest = (key: Key) => {
  const path = swrRequestPath(key)
  if (!path) return false
  const normalized = path.toLowerCase()
  return (
    normalized === '/api/game' ||
    normalized.startsWith('/api/game/') ||
    normalized === '/api/team' ||
    normalized.startsWith('/api/team/') ||
    normalized === '/api/admin' ||
    normalized.startsWith('/api/admin/') ||
    normalized === '/api/edit' ||
    normalized.startsWith('/api/edit/') ||
    normalized === '/api/account' ||
    (normalized.startsWith('/api/account/') && normalized !== '/api/account/profile')
  )
}

/**
 * Scope identity-sensitive SWR keys without changing their HTTP request. The
 * wrapper fetcher removes the cache-only scope marker before delegating to the
 * configured fetcher. This keeps backend authorization authoritative while
 * making account replacement a hard cache boundary.
 */
export const viewerIdentityMiddleware: Middleware = (useSWRNext) =>
  function useViewerScopedSWR(key, fetcher, config) {
    const { scope } = useViewerIdentity()
    const shouldScope = scope !== null && isViewerScopedRequest(key)
    const scopedKey = shouldScope ? viewerScopedKey(key, scope) : key
    const scopedFetcher =
      shouldScope && fetcher ? (requestKey: ViewerScopedKey) => fetcher(unwrapViewerScopedKey(requestKey)) : fetcher

    return useSWRNext(scopedKey, scopedFetcher, {
      ...config,
      // Authorization and identity responses must become empty/loading when
      // their game, query, or viewer key changes.
      keepPreviousData: shouldScope ? false : config.keepPreviousData,
    })
  }

export const routeLifecycleKey = (pathname: string, search: string, scope: string | null) =>
  `${scope ?? 'unscoped'}\u0000${pathname}\u0000${search}`

export const RouteLifecycleBoundary: FC<PropsWithChildren> = ({ children }) => {
  const location = useLocation()
  const { scope } = useViewerIdentity()
  return <Fragment key={routeLifecycleKey(location.pathname, location.search, scope)}>{children}</Fragment>
}
