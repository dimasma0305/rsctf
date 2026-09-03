import { FC, useEffect } from 'react'
import { useLocation } from 'react-router'
import {
  connectionAllowsRoutePrefetch,
  createRouteModulePrefetcher,
  RoutePrefetchConnection,
  sameOriginRoutePath,
} from '@Utils/RoutePrefetchPolicy'

const routeModules = import.meta.glob('../pages/**/*.tsx')
const prefetchRouteModule = createRouteModulePrefetcher(routeModules)
const IDLE_PREFETCH_LIMIT = 2
const IDLE_PREFETCH_DELAY_MS = 1_200

const browserConnection = () => (navigator as Navigator & { connection?: RoutePrefetchConnection }).connection

const mayPrefetch = () =>
  document.visibilityState === 'visible' &&
  navigator.onLine !== false &&
  connectionAllowsRoutePrefetch(browserConnection())

const anchorRoute = (target: EventTarget | null) => {
  if (!(target instanceof Element)) return null
  const anchor = target.closest<HTMLAnchorElement>('a[href]')
  if (!anchor || anchor.download || (anchor.target && anchor.target !== '_self')) return null
  return sameOriginRoutePath(anchor.href, window.location.origin)
}

/**
 * Prime lazy route chunks from navigation intent and, on normal connections,
 * a bounded settled-page scan. This never fetches route data or crosses origins.
 */
export const RoutePrefetcher: FC = () => {
  const location = useLocation()

  useEffect(() => {
    const prefetchFromIntent = (event: Event) => {
      if (!mayPrefetch()) return
      const pathname = anchorRoute(event.target)
      if (pathname && pathname !== location.pathname) void prefetchRouteModule(pathname)
    }

    document.addEventListener('pointerover', prefetchFromIntent, { passive: true })
    document.addEventListener('focusin', prefetchFromIntent)
    document.addEventListener('touchstart', prefetchFromIntent, { passive: true })
    return () => {
      document.removeEventListener('pointerover', prefetchFromIntent)
      document.removeEventListener('focusin', prefetchFromIntent)
      document.removeEventListener('touchstart', prefetchFromIntent)
    }
  }, [location.pathname])

  useEffect(() => {
    let cancelled = false
    const prefetchVisibleLinks = async () => {
      if (!mayPrefetch()) return
      let scheduled = 0
      const pageLinks = document.querySelectorAll<HTMLAnchorElement>('[data-page-content] a[href]')
      const allLinks = document.querySelectorAll<HTMLAnchorElement>('a[href]')
      for (const anchor of new Set([...pageLinks, ...allLinks])) {
        if (cancelled) return
        const pathname = anchorRoute(anchor)
        if (!pathname || pathname === location.pathname) continue
        if ((await prefetchRouteModule(pathname)) && ++scheduled >= IDLE_PREFETCH_LIMIT) break
      }
    }

    // requestIdleCallback may remain starved while live competition pages keep
    // scheduling work. A small fixed delay is deterministic and still leaves
    // initial rendering and route data ahead of this bounded background work.
    const handle = setTimeout(() => void prefetchVisibleLinks(), IDLE_PREFETCH_DELAY_MS)
    return () => {
      cancelled = true
      clearTimeout(handle)
    }
  }, [location.pathname])

  return null
}
