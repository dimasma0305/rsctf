// Invented, read-only player data. No request reaches a real game API.
import http from 'node:http'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export function createCompetitionFixture() {
  const now = Date.now()
  const profile = { userId: '11111111-1111-4111-8111-111111111111', role: 'User', userName: 'Dimas', email: 'player@example.invalid' }
  const game = { id: 901, title: 'Intechfest 2026', start: now - 3600000, end: now + 8000000, serverTime: now, status: 'Accepted', joined: true, teamCount: 50, userCount: 150, practiceMode: false, divisions: [], writeupRequired: false }
  const names = ['Ret2win', 'Cookie Jar', 'Cipher Garden', 'Minions in 32K', 'Signal Lost']
  const challenges = Array.from({ length: 100 }, (_, i) => ({ id: 9001 + i, title: i < 5 ? names[i] : `Challenge ${String(i + 1).padStart(3, '0')}`, category: ['Pwn', 'Web', 'Crypto', 'Reverse', 'Misc'][i % 5], type: 'StaticAttachment', score: 500 - (i % 5) * 50, solved: i % 13, bloods: [], disableBloodBonus: true }))
  const rank = { id: 7, name: 'TCP1P', rank: 7, score: 3250, solvedCount: 12, lastSubmissionTime: now - 30000, solvedChallenges: challenges.slice(1, 13).map(challenge => ({ id: challenge.id, type: 'Normal', score: challenge.score, time: now - 30000 })) }
  rank.solvedChallenges.push({ id: 9001, type: 'Unaccepted', score: 0, time: now - 15000 })
  const config = { title: 'RSCTF', slogan: 'Capture the flag', portMapping: 'Default', allowRegister: true, allowPasswordRegistration: true, allowTeamCreation: true, emailConfirmationRequired: false, enableBrowserFingerprint: false }
  const responses = {
    '/api/account/profile': profile, '/api/config': config, '/api/captcha': { type: 'None' },
    '/api/game/901': game,
    '/api/game/902': { ...game, id: 902 },
    '/api/game/902/notices': [],
    '/api/game/901/details': { challenges: Object.groupBy(challenges, item => item.category), challengeCount: 100, rank, teamToken: 'fixture-only-not-a-credential' },
    '/api/game/901/details/participant': { rank },
    '/api/game/901/notices': [{ id: 1, type: 'FirstBlood', time: now - 30000, publishTimeUtc: now - 30000, values: ['TCP1P', 'Cipher Garden'] }],
  }
  for (const challenge of challenges) {
    responses[`/api/game/901/challenges/${challenge.id}`] = { ...challenge, content: `Challenge files: ${challenge.title}. Download the challenge files and submit your flag.`, context: { url: `/assets/${'a'.repeat(64)}/ret2win.zip`, fileSize: 2048 }, attempts: 0, hints: [] }
    responses[`/api/game/901/challenges/${challenge.id}/solvers/page`] = { data: [], total: challenge.solved }
  }
  const fixture = (path, method = 'GET') => {
    const pathname = new URL(path, 'http://127.0.0.1').pathname.toLowerCase()
    // End the read-only live-update handshake locally; never open a real hub.
    if (method === 'POST' && pathname === '/hub/user/negotiate') {
      return { body: { error: 'Live updates are unavailable in the read-only fixture' } }
    }
    if (!['GET', 'HEAD'].includes(method)) return { status: 405, body: { title: 'Fixture write blocked' } }
    if (pathname.startsWith('/hub')) return { body: [] }
    return pathname in responses ? { body: responses[pathname] } : { status: 404, body: { title: 'Unknown fixture request' } }
  }
  return { now, profile, game, challenges, rank, config, responses, fixture }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url) && process.argv.includes('--serve')) {
  const { game, profile, fixture } = createCompetitionFixture()
  game.title = 'INTECHFEST CTF 2023'
  game.practiceMode = true
  const setup = `localStorage.setItem('language', JSON.stringify('en-US'));localStorage.setItem('mantine-color-scheme-value','dark');localStorage.setItem('rsctf-player-guide:${profile.userId}',JSON.stringify({interactiveEnabled:false,completedVersion:1,seenFeatures:[]}));`
  http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url, 'http://127.0.0.1')
      if (/^\/(?:api\/|hub)/i.test(url.pathname)) {
        const result = fixture(req.url, req.method)
        res.writeHead(result.status || 200, { 'Content-Type': 'application/json' })
        return res.end(JSON.stringify(result.body))
      }
      if (!['GET', 'HEAD'].includes(req.method)) { res.writeHead(405); return res.end() }
      const response = await fetch('http://127.0.0.1:63017' + url.pathname + url.search, { signal: AbortSignal.timeout(10000) })
      const contentType = response.headers.get('Content-Type') || 'application/octet-stream'
      res.writeHead(response.status, { 'Content-Type': contentType })
      if (contentType.includes('text/html')) res.end((await response.text()).replace('<head>', `<head><script>${setup}</script>`))
      else res.end(Buffer.from(await response.arrayBuffer()))
    } catch { res.writeHead(502); res.end('Fixture preview unavailable') }
  }).listen(63018, '127.0.0.1')
}
