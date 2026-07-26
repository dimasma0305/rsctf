import { readdirSync, statSync } from 'node:fs'
import { dirname, join, relative, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
export const repositoryRoot = join(here, '..', '..')
export const pagesRoot = join(repositoryRoot, 'web', 'src', 'pages')
export const viewportCatalog = Object.freeze({
  ultrawide: { width: 3440, height: 1440, mobile: false },
  wide: { width: 1920, height: 1080, mobile: false },
  desktop: { width: 1440, height: 1100, mobile: false },
  laptop: { width: 1024, height: 768, mobile: false },
  tablet: { width: 768, height: 1024, mobile: true },
  mobile: { width: 390, height: 844, mobile: true },
  compact: { width: 320, height: 568, mobile: true },
})

export function parseRouteShard(value) {
  const match = /^([1-9]\d*)\/([1-9]\d*)$/.exec(value ?? '')
  if (!match) throw new Error('visual route shard must use INDEX/TOTAL with positive integers')
  const shard = { index: Number(match[1]), total: Number(match[2]) }
  if (shard.index > shard.total) throw new Error('visual route shard index cannot exceed its total')
  return shard
}

export function selectRouteShard(routes, shard) {
  if (!shard) return routes
  const start = Math.floor(((shard.index - 1) * routes.length) / shard.total)
  const end = Math.floor((shard.index * routes.length) / shard.total)
  return routes.slice(start, end)
}

function pageFiles(directory, files = []) {
  for (const entry of readdirSync(directory).sort()) {
    const path = join(directory, entry)
    if (statSync(path).isDirectory()) pageFiles(path, files)
    else if (entry.endsWith('.tsx')) files.push(path)
  }
  return files
}

function contextSegment(segment, context) {
  if (segment === '[...all]') return 'visual-audit-not-found'
  if (segment === '[id]') return String(context.gameId ?? '')
  if (segment === '[chalId]') return String(context.challengeId ?? '')
  if (segment === '[postId]') return String(context.postId ?? '')
  return segment.toLowerCase()
}

function queryFor(sourceFile) {
  if (!['account/Confirm.tsx', 'account/Reset.tsx', 'account/Verify.tsx'].includes(sourceFile)) return ''
  const email = Buffer.from('visual-audit@example.invalid').toString('base64')
  return `?email=${encodeURIComponent(email)}&token=visual-audit-invalid-token`
}

function authFor(path) {
  if (path.startsWith('/admin/') || path.includes('/monitor/') || /^\/posts\/[^/]+\/edit$/.test(path)) {
    return 'admin'
  }
  if (
    path === '/teams' ||
    path === '/account/profile' ||
    path === '/account/stats' ||
    /^\/games\/[^/]+(?:\/(?:attack|challenges|scoreboard|submit))?$/.test(path)
  ) {
    return 'player'
  }
  return 'anonymous'
}

function expectedPathFor(sourceFile, path) {
  if (sourceFile === 'account/Stats.tsx') return '/account/profile'
  return path
}

function layoutGroupFor(path) {
  return /^\/games\/[^/]+\/(?:challenges|scoreboard|submit|monitor\/[^/]+)$/.test(path) ? 'game-workspace' : undefined
}

export function discoverPageRoutes(context) {
  return pageFiles(pagesRoot).map((file) => {
    const sourceFile = relative(pagesRoot, file).split(sep).join('/')
    const sourceSegments = sourceFile.replace(/\.tsx$/, '').split('/')
    if (sourceSegments.at(-1)?.toLowerCase() === 'index') sourceSegments.pop()
    const segments = sourceSegments.map((segment) => contextSegment(segment, context))
    const missingContext = []
    if (sourceFile.includes('[id]') && !context.gameId) missingContext.push('gameId')
    if (sourceFile.includes('[chalId]') && !context.challengeId) missingContext.push('challengeId')
    if (sourceFile.includes('[postId]') && !context.postId) missingContext.push('postId')
    const path = `/${segments.filter(Boolean).join('/')}`.replace(/\/+$/, '') || '/'
    const name = sourceFile
      .replace(/\.tsx$/, '')
      .replaceAll('[...all]', 'not-found')
      .replaceAll('[id]', 'game')
      .replaceAll('[chalId]', 'challenge')
      .replaceAll('[postId]', 'post')
      .replaceAll('/', '--')
      .toLowerCase()

    return {
      name,
      sourceFile,
      path,
      expectedPath: expectedPathFor(sourceFile, path),
      urlPath: `${path}${queryFor(sourceFile)}`,
      auth: authFor(path),
      layoutGroup: layoutGroupFor(path),
      missingContext,
    }
  })
}

export function validatePageRoutes(routes) {
  const problems = []
  const sources = new Set()
  const paths = new Set()

  for (const route of routes) {
    if (sources.has(route.sourceFile)) problems.push(`duplicate source file: ${route.sourceFile}`)
    sources.add(route.sourceFile)
    if (paths.has(route.path)) problems.push(`duplicate route path: ${route.path}`)
    paths.add(route.path)
    if (route.missingContext.length) {
      problems.push(`${route.sourceFile} needs ${route.missingContext.join(', ')}`)
    }
  }

  return problems
}
