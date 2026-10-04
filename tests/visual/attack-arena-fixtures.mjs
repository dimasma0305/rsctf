// Invented spectator data. Every API and WebSocket is local and read-only.
import http from 'node:http'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createCompetitionFixture } from './competition-fixtures.mjs'

export const arenaBrowserSetup = `
localStorage.setItem('language', JSON.stringify('en-US'));
localStorage.setItem('mantine-color-scheme-value', 'dark');
window.arenaSockets = [];
window.WebSocket = class extends EventTarget {
  static CONNECTING = 0; static OPEN = 1; static CLOSED = 3;
  readyState = 0;
  constructor(url) { super(); this.url = url; arenaSockets.push(this); setTimeout(() => { if (this.readyState === 0) { this.readyState = 1; this.onopen?.({}); } }, 20); }
  send() {}
  close() { this.readyState = 3; this.onclose?.({}); }
  frame(value) { this.onmessage?.({data: JSON.stringify(value)}); }
};`

export function createArenaFixture(mode = 'jeopardy', count = 49) {
  const { config, now } = createCompetitionFixture()
  // Exercise a managed accent as well as the default theme. Focus-ring text
  // over this red accent surface previously failed contrast in light mode.
  config.customTheme = '#e33f3d'
  const names = Array.from({ length: count }, (_, i) => i === 0 ? 'Northern Lights / Long Team Name' : `Team ${String(i + 1).padStart(2, '0')}`)
  const teams = names.map((teamName, i) => ({ participationId: i + 1, teamName, rank: i + 1, settledTotal: 5000 - i * 70, projectedTotal: 5010 - i * 70, offenseRate: .8, defenseRate: .9, slaRate: 1 }))
  const ad = { teams: mode === 'mixed' ? teams : [], challenges: mode === 'mixed' ? [{ challengeId: 101, title: 'Relay', category: 'Web' }] : [], epochTicks: mode === 'mixed' ? 10 : 0, currentEpoch: 2, latestRound: 20, currentRoundEndsAt: now + 60000, tickSeconds: 60, evidence: { acceptedCaptures: 12 }, fullySettled: false, started: true, isFrozenView: false }
  const koth = { teams: mode === 'mixed' || mode === 'koth' ? teams : [], hills: mode === 'mixed' || mode === 'koth' ? [{ challengeId: 102, title: 'Beacon', lastCheckStatus: 'Up', currentHolderTeamName: names[0] }] : [], epochTicks: mode === 'jeopardy' ? 0 : 10, latestRound: 20, currentRoundEndsAt: now + 60000, fullySettled: false }
  const scoreboard = { items: names.map((name, i) => ({ id: i + 1, name, score: 8000 - i * 100, solvedCount: 24 - i % 20 })), challenges: Object.fromEntries(['Web', 'Crypto', 'Reverse', 'Pwn'].map((category, ci) => [category, Array.from({ length: 3 }, (_, i) => ({ id: 1000 + ci * 10 + i, title: `${category} challenge ${i + 1}`, score: 500, solved: 0, bloods: [], type: 'StaticAttachment' }))])) }
  const responses = {
    '/api/config': config, '/api/captcha': { type: 'None' },
    '/api/game/901': { id: 901, title: 'INTECHFEST · Globe spectator fixture', start: now - 3600000, end: now + 3600000 },
    '/api/game/901/ad/scoreboard': ad, '/api/game/901/ad/koth/scoreboard': koth, '/api/game/901/scoreboard': scoreboard,
  }
  return { names, responses, ad, koth, scoreboard, fixture(path, method = 'GET') {
    path = new URL(path, 'http://127.0.0.1').pathname.toLowerCase()
    if (!['GET', 'HEAD'].includes(method)) return { status: 405, body: { title: 'Fixture write blocked' } }
    if (path === '/api/account/profile') return { status: 401, body: {} }
    return path in responses ? { status: 200, body: responses[path] } : { status: 404, body: { title: 'Unknown fixture request' } }
  } }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url) && process.argv.includes('--serve')) {
  const { fixture } = createArenaFixture()
  http.createServer(async (req, res) => {
    try {
      if (/^\/(?:api\/|hub)/i.test(req.url)) {
        const result = fixture(req.url, req.method)
        res.writeHead(result.status, { 'Content-Type': 'application/json' }); return res.end(JSON.stringify(result.body))
      }
      if (!['GET', 'HEAD'].includes(req.method)) { res.writeHead(405); return res.end() }
      const upstream = await fetch('http://127.0.0.1:18080' + req.url, { signal: AbortSignal.timeout(10000) })
      const contentType = upstream.headers.get('content-type') || 'application/octet-stream'
      res.writeHead(upstream.status, { 'Content-Type': contentType })
      res.end(contentType.includes('text/html') ? (await upstream.text()).replace('<head>', `<head><script>${arenaBrowserSetup}</script>`) : Buffer.from(await upstream.arrayBuffer()))
    } catch { res.writeHead(502); res.end('Fixture unavailable') }
  }).listen(63020, '127.0.0.1')
}
