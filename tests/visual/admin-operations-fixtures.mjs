import http from 'node:http'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export const uid = '11111111-1111-4111-8111-111111111111'
const now = Date.UTC(2026, 8, 7, 12)
export const builds = Array.from({ length: 30 }, (_, i) => ({
  id: i + 1,
  challengeId: 100 + i,
  gameId: 19,
  challengeTitle:
    i === 0
      ? 'Tower of Babel'
      : i === 1
        ? 'A very long challenge title that should remain readable on small screens'
        : 'Challenge ' + (i + 1),
  enqueuedAtUtc: new Date(now - i * 60000).toISOString(),
  trigger: 'Manual',
  kind: i % 3 === 0 ? 'Checker' : 'Challenge',
  attempt: (i % 2) + 1,
  status: ['Failed', 'Success', 'Queued', 'MissingDockerfile', 'NotApplicable'][i % 5],
  durationMs: 42500,
  imageRef: i % 5 === 1 ? 'registry.example.invalid/team/challenge:' + 'a'.repeat(64) : null,
  digest: i % 5 === 1 ? 'sha256:' + 'b'.repeat(64) : null,
  errorMessage:
    i % 5 === 0 ? 'Build failed: dependency resolution unavailable. Check the build output for details.' : null,
  logTail: i === 0 ? null : '#1 Load build context\n#2 Resolve dependencies\n#3 ' + 'build-output-'.repeat(30),
}))
const bindings = Array.from({ length: 22 }, (_, i) => ({
  id: i + 1,
  repoUrl:
    'https://github.com/example/' +
    (i === 0
      ? 'intechfest-2026'
      : i === 1
        ? 'very-long-repository-name-for-testing-narrow-admin-panels-and-wrapping'
        : 'challenges-' + (i + 1)),
  ref: 'main',
  createdAtUtc: new Date(now).toISOString(),
  lastScanUtc: new Date(now - 60000).toISOString(),
  nextScanUtc: new Date(now + 60000).toISOString(),
  intervalSeconds: 120,
  status: i % 3 === 1 ? 'Paused' : 'Active',
  lastCommitSha: 'a'.repeat(40),
  lastScanMessage:
    i === 1 ? 'clone/fetch failed: repository exceeds the configured size limit' : 'Repository is up to date.',
  hasGitHubToken: i !== 2,
  tokenStatus: i === 1 ? 'DecryptFailed' : 'Available',
  pushOnEdit: false,
  currentActivity: null,
  pushBacklog: i === 1 ? 2 : 0,
  pushLastError: i === 1 ? 'Upstream push is waiting for a valid credential.' : null,
  games: [{ id: 19, title: 'Intechfest · Main event', eventManifestPath: 'events/main-event/event.yml' }],
}))

// Always resolves API traffic locally. Unknown routes and all unhandled mutations fail closed.
export function fixture(path, method = 'GET', scenario = 'normal', role = 'Admin') {
  const url = new URL(path, 'http://127.0.0.1'),
    p = url.pathname.toLowerCase()
  if (method !== 'GET' && method !== 'HEAD') return { status: 405, body: { title: 'Fixture write blocked' } }
  if (p === '/api/account/profile')
    return { body: { userId: uid, userName: 'Organizer', role, email: 'organizer@example.invalid' } }
  if (p === '/api/config')
    return {
      body: {
        title: 'RSCTF',
        portMapping: 'Default',
        allowRegister: true,
        allowTeamCreation: true,
        emailConfirmationRequired: false,
        enableBrowserFingerprint: false,
      },
    }
  if (p === '/api/captcha') return { body: { type: 'None' } }
  if (p.startsWith('/api/admin/') && role !== 'Admin') return { status: 403, body: { title: 'forbidden' } }
  if (
    scenario === 'error' &&
    ['/api/admin/builds', '/api/admin/builds/inprogress', '/api/admin/repobindings'].includes(p)
  )
    return { status: 503, body: { title: 'Fixture temporarily unavailable' } }
  if (
    scenario === 'loading' &&
    ['/api/admin/builds', '/api/admin/builds/inprogress', '/api/admin/repobindings'].includes(p)
  )
    return { hold: true }
  if (p === '/api/admin/builds') return { body: scenario === 'empty' ? [] : builds }
  if (p === '/api/admin/builds/inprogress')
    return {
      body:
        scenario === 'active'
          ? [
              {
                auditId: 31,
                challengeId: 103,
                gameId: 19,
                slug: 'fixture-active-build-' + 'a'.repeat(100),
                attempt: 2,
                trigger: 'Manual',
                kind: 'Checker',
                startedAtUtc: new Date(now).toISOString(),
              },
            ]
          : [],
    }
  if (p === '/api/admin/repobindings') {
    const skip = Number(url.searchParams.get('skip') || 0),
      count = Number(url.searchParams.get('count') || 20)
    return {
      body: {
        data:
          scenario === 'empty'
            ? []
            : bindings
                .slice(skip, skip + count)
                .map((binding, i) =>
                  scenario === 'active' && i === 0
                    ? { ...binding, currentActivity: 'Importing challenge manifests' }
                    : binding
                ),
        total: scenario === 'empty' ? 0 : bindings.length,
      },
    }
  }
  if (/\/api\/admin\/repobindings\/\d+\/scans$/.test(p))
    return {
      body: {
        data: [
          {
            id: 1,
            ranAtUtc: new Date(now).toISOString(),
            commitSha: 'a'.repeat(40),
            gamesCreated: 0,
            gamesUpdated: 1,
            challengesImported: 2,
            challengesUpdated: 3,
            failures: 1,
            messages: 'Fixture scan: Dockerfile is missing.\n' + 'a'.repeat(250),
          },
        ],
        total: 1,
      },
    }
  if (scenario === 'images-error' && ['/api/admin/builds/images', '/api/admin/builds/storage'].includes(p))
    return { status: 503, body: { title: 'Fixture inventory unavailable' } }
  if (p === '/api/admin/builds/images')
    return {
      body: [
        {
          id: 'sha256:' + 'a'.repeat(64),
          tags: ['rsctf/challenge:' + 'b'.repeat(64)],
          sizeBytes: 135 * 1024 * 1024,
          createdUtc: new Date(now).toISOString(),
          referenced: true,
          referencedBy: ['Tower of Babel'],
          isChecker: false,
          retentionExpiresUtc: now + 3600000,
          inUse: true,
        },
      ],
    }
  if (p === '/api/admin/builds/storage')
    return {
      body: {
        filesystemTotalBytes: 200 * 1024 ** 3,
        filesystemAvailableBytes: 70 * 1024 ** 3,
        buildCacheBytes: 3 * 1024 ** 3,
        reclaimableBuildCacheBytes: 1024 ** 3,
        minimumFreeBytes: 5 * 1024 ** 3,
        lowStorage: false,
      },
    }
  return { status: 404, body: { title: 'Unknown fixture route' }, unknown: p }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url) && process.argv.includes('--serve')) {
  http
    .createServer(async (request, response) => {
      try {
        if (!['GET', 'HEAD'].includes(request.method)) {
          response.writeHead(405)
          return response.end()
        }
        if (request.url.toLowerCase().startsWith('/api/')) {
          const result = fixture(request.url, request.method)
          response.writeHead(result.status || 200, { 'Content-Type': 'application/json' })
          return response.end(JSON.stringify(result.body))
        }
        const result = await fetch('http://127.0.0.1:63017' + request.url)
        response.writeHead(result.status, {
          'Content-Type': result.headers.get('Content-Type') || 'application/octet-stream',
        })
        response.end(Buffer.from(await result.arrayBuffer()))
      } catch {
        response.writeHead(502)
        response.end('Fixture proxy failed')
      }
    })
    .listen(63018, '127.0.0.1')
}
