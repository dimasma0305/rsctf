export type RouteModuleLoader = () => Promise<unknown>

export type RouteModuleMap = Record<string, RouteModuleLoader>

interface CompiledRouteModule {
  modulePath: string
  pattern: RegExp
  specificity: number
  load: RouteModuleLoader
}

const escapePattern = (value: string) => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')

const compileRouteModule = (modulePath: string, load: RouteModuleLoader): CompiledRouteModule | null => {
  const marker = '/pages/'
  const markerIndex = modulePath.lastIndexOf(marker)
  if (markerIndex < 0 || !modulePath.endsWith('.tsx')) return null

  const segments = modulePath.slice(markerIndex + marker.length, -4).split('/')
  if (segments.at(-1)?.toLowerCase() === 'index') segments.pop()
  if (segments.some((segment) => /^\[\.\.\..+\]$/.test(segment))) return null

  let specificity = segments.length
  const routeSegments = segments.map((segment) => {
    if (/^\[[^/]+\]$/.test(segment)) {
      specificity += 10
      return '[^/]+'
    }
    specificity += 100
    return escapePattern(segment)
  })
  const route = routeSegments.length === 0 ? '' : `/${routeSegments.join('/')}`
  return {
    modulePath,
    pattern: new RegExp(route ? `^${route}/?$` : '^/?$', 'i'),
    specificity,
    load,
  }
}

const compileRouteModules = (modules: RouteModuleMap) =>
  Object.entries(modules)
    .map(([modulePath, load]) => compileRouteModule(modulePath, load))
    .filter((route): route is CompiledRouteModule => route !== null)
    .sort((left, right) => right.specificity - left.specificity)

/**
 * Load a route's existing Vite chunk before React.lazy needs it. Requests are
 * bounded to one per module; a failed speculative request may be retried.
 */
export const createRouteModulePrefetcher = (modules: RouteModuleMap) => {
  const routes = compileRouteModules(modules)
  const requested = new Set<string>()

  return async (pathname: string) => {
    if (!pathname.startsWith('/')) return false
    const route = routes.find((candidate) => candidate.pattern.test(pathname))
    if (!route || requested.has(route.modulePath)) return false

    requested.add(route.modulePath)
    try {
      await route.load()
      return true
    } catch {
      requested.delete(route.modulePath)
      return false
    }
  }
}

export interface RoutePrefetchConnection {
  effectiveType?: string
  saveData?: boolean
}

/** Avoid background bandwidth when the browser or connection asks for it. */
export const connectionAllowsRoutePrefetch = (connection?: RoutePrefetchConnection) =>
  !connection?.saveData && !/^(slow-)?2g$/i.test(connection?.effectiveType ?? '')

export const sameOriginRoutePath = (href: string, origin: string) => {
  try {
    const url = new URL(href, origin)
    if (url.origin !== origin || url.username || url.password) return null
    return url.pathname
  } catch {
    return null
  }
}
