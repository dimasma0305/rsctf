// Read-only fixtures for the full-content audit. Never forwards API or hub calls.
import http from 'node:http'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { fixture as readinessFixture } from './event-readiness-fixtures.mjs'

export function fixture(path, method = 'GET') {
  if (!['GET', 'HEAD'].includes(method)) return { status: 405, body: { title: 'Fixture write blocked' } }
  const url = new URL(path, 'http://127.0.0.1')
  if (url.pathname.toLowerCase() === '/api/posts/page') {
    return { body: { data: [{
      id: '00000001', title: 'Competition schedule and player access',
      summary: 'Read the **player guide** and check your team before joining the event.',
      authorName: 'Organizers', time: Date.UTC(2026, 8, 9), tags: ['announcement'], isPinned: true,
    }], total: 1 } }
  }
  return readinessFixture(path, method)
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url) && process.argv.includes('--serve')) {
  http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url, 'http://127.0.0.1')
      if (!['GET', 'HEAD'].includes(req.method)) { res.writeHead(405); return res.end() }
      if (/^\/(?:api\/|hub)/i.test(url.pathname)) {
        const result = fixture(req.url, req.method)
        res.writeHead(result.status || 200, { 'Content-Type': 'application/json' })
        return res.end(JSON.stringify(result.body))
      }
      const response = await fetch('http://127.0.0.1:63017' + url.pathname + url.search, { signal: AbortSignal.timeout(10000) })
      res.writeHead(response.status, { 'Content-Type': response.headers.get('Content-Type') || 'application/octet-stream' })
      res.end(Buffer.from(await response.arrayBuffer()))
    } catch { res.writeHead(502); res.end('Fixture preview unavailable') }
  }).listen(63018, '127.0.0.1')
}
