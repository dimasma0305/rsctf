// Invented rendering data only. API and hub requests never reach a real server.
import http from 'node:http'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { fixture as commonFixture } from './admin-navigation-fixtures.mjs'

export const now = Date.now()
export const events = [
  ['Packet League', -3600000, 7200000, true],
  ['Intechfest — Main Event', 86400000, 108000000, false],
  ['An archived competition with a long name that must remain readable', -172800000, -86400000, true],
].map(([title, start, end, joined], i) => ({
  id: 901 + i, title, summary: 'Web, reverse engineering, and cryptography challenges.',
  start: now + start, end: now + end, serverTime: now, limit: 3,
  teamCount: 8, userCount: 24, joined, participationStatus: joined ? 'Accepted' : null, practiceMode: false,
  content: '', divisions: [], reviewCount: 0,
}))
export const posts = [
  { id: '00000001', title: 'Main event schedule and access', summary: 'Check your team before the event. See the [player guide](/guide) for connection instructions.', isPinned: true },
  { id: '00000002', title: 'Warmup results are available', summary: 'The warmup has ended. You can review the final standings from the event page.', isPinned: false },
].map((post, i) => ({ ...post, authorName: 'Organizers', time: now - i * 86400000, tags: ['announcement'] }))

export const guideSetup = `for (const id of ['guest', '${commonFixture('/api/account/profile').body.userId}']) localStorage.setItem('rsctf-player-guide:' + id, JSON.stringify({ interactiveEnabled: false, completedVersion: 5, seenFeatures: [], activeTourStep: null, tourPaused: true }));`

// CDP document scripts also run in child frames, including opaque sandbox origins.
export const topDocumentScript = (source, origin) =>
  `if (window === window.top && location.origin === ${JSON.stringify(origin)}) { ${source} }`

export function fixture(path, method = 'GET', scenario = 'normal', role = 'Admin') {
  if (!['GET', 'HEAD'].includes(method)) return { status: 405, body: { title: 'Fixture write blocked' } }
  const url = new URL(path, 'http://127.0.0.1')
  const p = url.pathname.toLowerCase()
  if (p.startsWith('/hub')) return { status: 404, body: {} }
  if (p === '/api/account/profile' && role === 'Guest') return { status: 401, body: { title: 'unauthorized' } }
  if (p.startsWith('/api/admin/') && role !== 'Admin') return { status: 403, body: { title: 'forbidden' } }
  const owned = ['/api/posts/latest', '/api/game', '/api/game/recent', '/api/admin/dashboard', '/api/admin/submissiontrend']
  if (owned.includes(p) && scenario === 'loading') return { hold: true }
  if (owned.includes(p) && scenario === 'error') return { status: 503, body: { title: 'Fixture unavailable' } }
  if (p === '/api/posts/latest') return { body: scenario === 'empty' ? [] : posts }
  if (p === '/api/game/recent') return { body: scenario === 'empty' ? [] : events }
  if (p === '/api/game') {
    const search = (url.searchParams.get('search') || '').toLowerCase()
    const membership = url.searchParams.get('membership')
    const rows = (scenario === 'empty' ? [] : events).filter(event =>
      (!search || `${event.id} ${event.title} ${event.summary}`.toLowerCase().includes(search)) &&
      (membership === 'joined' ? event.joined : membership === 'notJoined' ? !event.joined : true))
    const skip = Math.max(0, Number(url.searchParams.get('skip') || 0))
    const count = Math.min(12, Math.max(0, Number(url.searchParams.get('count') || 12)))
    return { body: { data: rows.slice(skip, skip + count), total: rows.length, length: rows.length } }
  }
  if (p === '/api/admin/dashboard') return { body: { systemStats: { userCount: 24, teamCount: 8, activeContainerCount: 6 }, topGames: scenario === 'empty' ? [] : events } }
  if (p === '/api/admin/submissiontrend') return { body: scenario === 'trend' ? [0, 1, 2, 3].map(i => ({ time: now - (3 - i) * 3600000, count: i * 4 })) : [] }
  return commonFixture(path, method, role)
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url) && process.argv.includes('--serve')) {
  http.createServer(async (req, res) => {
    try {
      if (!['GET', 'HEAD'].includes(req.method)) { res.writeHead(405); return res.end() }
      if (/^\/(?:api\/|hub)/i.test(req.url)) {
        const result = fixture(req.url, req.method)
        res.writeHead(result.status || 200, { 'Content-Type': 'application/json' })
        return res.end(JSON.stringify(result.body))
      }
      const url = new URL(req.url, 'http://127.0.0.1')
      const response = await fetch('http://127.0.0.1:63017' + url.pathname + url.search, { signal: AbortSignal.timeout(10000) })
      res.writeHead(response.status, { 'Content-Type': response.headers.get('Content-Type') || 'application/octet-stream' })
      const body = Buffer.from(await response.arrayBuffer())
      res.end(response.headers.get('Content-Type')?.includes('text/html')
        ? body.toString().replace('<head>', `<head><script>${guideSetup}</script>`)
        : body)
    } catch { res.writeHead(502); res.end('Fixture preview unavailable') }
  }).listen(63018, '127.0.0.1')
}
