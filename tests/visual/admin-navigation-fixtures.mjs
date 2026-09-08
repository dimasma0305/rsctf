import http from 'node:http'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { fixture as operatorFixture } from './ad-operations-fixtures.mjs'

const now = Date.now()
const game = { id: 19, title: 'Intechfest · Navigation fixture', summary: 'Read-only navigation audit', content: '', start: now + 3600000, end: now + 7200000, serverTime: now, limit: 3, teamCount: 6, userCount: 18, divisions: [], revision: 1, practiceMode: false }
const challenges = [74, 75, 76].map((id) => ({ id, title: `Navigation challenge ${id}`, category: 'Misc', type: 'StaticAttachment', content: 'Fixture challenge, no real flag or service.', isEnabled: false, revision: 1, controlRevision: 1, flags: [], hints: [], score: 500, difficulty: 4, minScoreRate: 0.25, submissionLimit: 20, attachment: null, acceptedCount: 0 }))
const settings = { revision: 1, globalConfig: { title: 'RSCTF' }, accountPolicy: { allowRegister: true, allowPasswordRegistration: true, allowTeamCreation: true }, containerPolicy: { portMapping: 'Default', defaultLifetime: 120, extensionDuration: 120, renewalWindow: 10 }, containerProvider: { type: 'Docker', name: 'Docker', available: true }, buildRegistry: {}, email: {}, captcha: { provider: 'None' }, oAuth: {}, registry: {}, donations: { enabled: false }, proxyTrust: { enabled: false, trustedNetworksCsv: '' } }

export function fixture(path, method = 'GET', role = 'Admin') {
  if (!['GET', 'HEAD'].includes(method)) return { status: 405, body: { title: 'Fixture write blocked' } }
  const p = new URL(path, 'http://localhost').pathname.toLowerCase()
  if (p === '/api/account/profile') {
    const r = operatorFixture(path, method)
    return { body: { ...r.body, role: role === 'Manager' ? 'User' : role, hasManagedGames: role === 'Manager' } }
  }
  if (p.startsWith('/api/admin/') && role !== 'Admin') return { status: 403, body: { title: 'forbidden' } }
  if (p.startsWith('/api/edit/') && !['Admin', 'Manager'].includes(role)) return { status: 403, body: { title: 'forbidden' } }
  const responses = {
    '/api/admin/config': settings,
    '/api/admin/users': { data: [], total: 0, length: 0 },
    '/api/admin/teams': [],
    '/api/admin/dashboard': { systemStats: { userCount: 18, teamCount: 6, activeContainerCount: 0 }, topGames: [game] },
    '/api/admin/submissiontrend': [], '/api/admin/reviews': [], '/api/admin/writeups': [], '/api/admin/cheat-reports': [],
    '/api/edit/games': { data: [game], total: 1, length: 1 },
    '/api/edit/games/19': game,
    '/api/edit/games/19/challenges': challenges,
    '/api/edit/games/19/notices': [],
    '/api/edit/games/19/challenges/pending': [],
    '/api/edit/games/19/challengereviews': [],
  }
  if (p in responses) return { body: responses[p] }
  const detail = p.match(/^\/api\/edit\/games\/19\/challenges\/(\d+)$/)
  if (detail) return { body: challenges.find((challenge) => challenge.id === Number(detail[1])) }
  if (/^\/api\/edit\/games\/19\/challenges\/\d+\/flags$/.test(p)) return { body: [] }
  return operatorFixture(path, method)
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url) && process.argv.includes('--serve')) {
  http.createServer(async (req, res) => {
    try {
      if (!['GET', 'HEAD'].includes(req.method)) { res.writeHead(405); return res.end() }
      if (req.url.toLowerCase().startsWith('/api/')) {
        const r = fixture(req.url, req.method)
        res.writeHead(r.status || 200, { 'Content-Type': 'application/json' })
        return res.end(JSON.stringify(r.body))
      }
      const response = await fetch('http://127.0.0.1:63017' + req.url)
      res.writeHead(response.status, { 'Content-Type': response.headers.get('Content-Type') || 'application/octet-stream' })
      res.end(Buffer.from(await response.arrayBuffer()))
    } catch { res.writeHead(502); res.end('Fixture proxy failed') }
  }).listen(63018, '127.0.0.1')
}
