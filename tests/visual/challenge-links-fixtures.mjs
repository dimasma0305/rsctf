// Invented catalog data for read-only browser checks; no real event credentials.
import http from 'node:http'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createCompetitionFixture } from './competition-fixtures.mjs'

export function createChallengeLinksFixture() {
  const player = createCompetitionFixture()
  const catalog = player.challenges.map(challenge => ({
    ...challenge, solveCount: challenge.solved,
    solved: player.rank.solvedChallenges.some(solve => solve.id === challenge.id && solve.type === 'Normal'),
    gameId: player.game.id, gameTitle: player.game.title, gameStart: player.game.start, gameEnd: player.game.end,
  }))
  const fixture = (path, method = 'GET') => {
    const url = new URL(path, 'http://127.0.0.1')
    if (['GET', 'HEAD'].includes(method) && url.pathname === '/api/game/challenges') {
      const params = url.searchParams
      const exact = params.get('challengeId'), search = params.get('search')?.toLowerCase()
      const matches = catalog.filter(item => (!exact || item.id === Number(exact)) &&
        (!search || item.title.toLowerCase().includes(search) || String(item.id) === search))
      const skip = Number(params.get('skip') || 0), count = Number(params.get('count') || 24)
      return { body: { data: matches.slice(skip, skip + count), total: matches.length } }
    }
    return player.fixture(path, method)
  }
  return { ...player, catalog, fixture }
}

// The general visual/Axe auditor can use the same fixture through this proxy.
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url) && process.argv.includes('--serve')) {
  const { fixture, profile } = createChallengeLinksFixture()
  const origin = process.env.RSCTF_CHALLENGE_LINK_TARGET || 'http://127.0.0.1:18080'
  const setup = `localStorage.setItem('language', JSON.stringify('en-US'));localStorage.setItem('mantine-color-scheme-value','dark');localStorage.setItem('rsctf-player-guide:${profile.userId}',JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true}));`
  http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url, 'http://127.0.0.1')
      if (/^\/(?:api\/|hub)/i.test(url.pathname)) {
        const response = fixture(req.url, req.method)
        res.writeHead(response.status || 200, { 'Content-Type': 'application/json' })
        return res.end(JSON.stringify(response.body))
      }
      if (!['GET', 'HEAD'].includes(req.method)) { res.writeHead(405); return res.end() }
      const response = await fetch(new URL(url.pathname + url.search, origin), { signal: AbortSignal.timeout(10000) })
      const type = response.headers.get('Content-Type') || 'application/octet-stream'
      res.writeHead(response.status, { 'Content-Type': type })
      if (type.includes('text/html')) res.end((await response.text()).replace('<head>', `<head><script>${setup}</script>`))
      else res.end(Buffer.from(await response.arrayBuffer()))
    } catch { res.writeHead(502); res.end('Read-only fixture unavailable') }
  }).listen(63019, '127.0.0.1')
}
