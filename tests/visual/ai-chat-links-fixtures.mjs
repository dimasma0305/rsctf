// Loopback-only fixtures for the AI chat links player card, admin provider
// registry, and monitor list. Built-in provider rules are read from the Rust
// source so screenshots and browser checks can never drift from the server.
import { readFileSync } from 'node:fs'

const RUST_SOURCE = new URL('../../src/services/ai_chat_links.rs', import.meta.url)

/** Parse `BUILTIN_PROVIDERS` (key, label, expanded pattern, examples) from Rust. */
export function builtinProviders(source = readFileSync(RUST_SOURCE, 'utf8')) {
  const query = String.raw`(?:\?[!-~]*)?`
  const start = source.indexOf('pub const BUILTIN_PROVIDERS')
  const array = source.slice(start, source.indexOf('\n];', start))
  const blocks = array.split('BuiltinProvider {').slice(1)
  return blocks.map((block) => {
    const key = block.match(/key: "([^"]+)"/)[1]
    const label = block.match(/label: "([^"]+)"/)[1]
    const raw = [...block.matchAll(/r"([^"]+)"/g)].map((match) => match[1])
    const pattern = raw.join('') + (block.includes('query!()') ? query : '')
    const examples = [...block.slice(block.indexOf('examples')).matchAll(/"(https:\/\/[^"]+)"/g)].map((m) => m[1])
    return { key, label, pattern, examples }
  })
}

export function aiChatFixture(now = Date.now()) {
  const builtins = builtinProviders()
  const profile = { userId: '11111111-1111-4111-8111-111111111111', role: 'Admin', userName: 'aria', email: 'aria@example.invalid' }
  const game = {
    id: 901, title: 'RSCTF Demo Finals', start: now - 3600000, end: now + 8000000, serverTime: now,
    status: 'Accepted', joined: true, teamCount: 48, userCount: 142, practiceMode: false, divisions: [],
    writeupRequired: false, aiChatLinksEnabled: true, aiChatLinksRequired: true, solverUploadsEnabled: true,
  }
  const titles = ['Heap Symphony', 'Cookie Jar', 'Cipher Garden', 'Minions', 'Signal Lost', 'Crown Hill']
  const categories = ['Pwn', 'Web', 'Crypto', 'Reverse', 'Misc', 'Misc']
  const challenges = titles.map((title, i) => ({
    id: 9001 + i, title, category: categories[i], type: i === 5 ? 'KingOfTheHill' : 'StaticAttachment',
    score: 500 - i * 25, solved: 12 - i, bloods: [], disableBloodBonus: true,
  }))
  const rank = {
    id: 7, name: 'Packet Pioneers', rank: 3, score: 1450, solvedCount: 3, lastSubmissionTime: now - 30000,
    solvedChallenges: [9001, 9002, 9003].map((id) => ({ id, type: 'Normal', score: 500, time: now - 30000 })),
  }
  const disabled = new Set(['grok'])
  const providerModels = [
    ...builtins.map(({ key, label, pattern, examples }) => ({
      key, label, pattern, builtin: true, enabled: !disabled.has(key), examples,
      updatedAt: disabled.has(key) ? now - 7200000 : null,
    })),
    {
      key: 'copilot', label: 'Microsoft Copilot', pattern: String.raw`https://copilot\.microsoft\.com/shares/[A-Za-z0-9_-]{6,128}`,
      builtin: false, enabled: true, examples: [], updatedAt: now - 3600000,
    },
  ]
  const example = (key) => builtins.find((provider) => provider.key === key).examples[0]
  const solvedAt = now - 30000 - 600000
  const baseState = {
    editable: true, solved: true, editableUntil: now + 8000000, maxLinks: 5,
    providers: providerModels.filter((p) => p.enabled).map(({ key, label, pattern }) => ({ key, label, pattern })),
    links: [], revision: 0, updatedAt: null, submittedBy: null,
    required: true, pending: false, declaredNoAi: false, solvedAt, firstDisclosedAt: null, editCount: 0,
  }
  // Solved: 9001 pending (nothing disclosed), 9002 links edited once, 9003 "No AI used".
  const states = {
    9001: { ...baseState, pending: true },
    9002: {
      ...baseState,
      links: [
        { url: example('chatgpt'), providerKey: 'chatgpt', providerLabel: 'ChatGPT' },
        { url: example('claude'), providerKey: 'claude', providerLabel: 'Claude' },
      ],
      revision: 2, updatedAt: now - 60000, submittedBy: 'aria', firstDisclosedAt: solvedAt + 240000, editCount: 1,
    },
    9003: { ...baseState, declaredNoAi: true, revision: 1, updatedAt: now - 300000, submittedBy: 'aria', firstDisclosedAt: solvedAt + 90000 },
  }
  const state = states[9002]
  const stateFor = (id) => states[id] ?? { ...baseState, solved: false, editable: false, solvedAt: null }
  const teams = ['Packet Pioneers', 'Stack Underflow', 'Null Pointers', 'Byte Bandits']
  const records = [
    { team: 0, challenge: 1, status: 'Links', links: [['chatgpt', 'ChatGPT'], ['claude', 'Claude']], by: 'aria', delay: 240, edits: 1, events: 4, revision: 1 },
    { team: 1, challenge: 0, status: 'Links', links: [['gemini', 'Gemini']], by: 'bima', delay: 95, edits: 0, events: 1 },
    { team: 2, challenge: 2, status: 'Links', links: [['grok', 'Grok'], ['deepseek', 'DeepSeek']], by: 'citra', delay: 5400, edits: 1, events: 2 },
    { team: 3, challenge: 1, status: 'NoAi', links: [], by: 'dewi', delay: 30, edits: 0, events: 1 },
    { team: 1, challenge: 2, status: 'Missing', links: [], by: null, delay: null, edits: 0, events: 0 },
    { team: 3, challenge: 0, status: 'Missing', links: [], by: null, delay: null, edits: 1, events: 2 },
  ].map((record, i) => {
    const recordSolvedAt = now - (i + 2) * 900000
    const disclosed = record.status !== 'Missing'
    return {
      participationId: 70 + record.team, teamId: 7 + record.team, teamName: teams[record.team],
      challengeId: challenges[record.challenge].id, challengeTitle: challenges[record.challenge].title,
      category: challenges[record.challenge].category,
      links: record.links.map(([key, label]) => ({ url: example(key), providerKey: key, providerLabel: label, providerActive: !disabled.has(key) })),
      submittedBy: record.by, updatedAt: disclosed ? now - (i + 1) * 420000 : null, revision: disclosed ? (record.revision ?? record.edits + 1) : 0,
      status: record.status, declaredNoAi: record.status === 'NoAi', solvedAt: recordSolvedAt,
      firstDisclosedAt: record.delay === null ? null : recordSolvedAt + record.delay * 1000, delaySeconds: record.delay,
      editCount: record.edits, eventCount: record.events,
    }
  })
  // Created -> Edited -> Cleared -> Created for Packet Pioneers on Cookie Jar.
  const history = records[0]
  const [gpt, claude] = history.links.map((link) => link.url)
  const events = [
    // Server semantics: a clear deletes the row (revision 0) and the next save starts at 1.
    { action: 'Created', userName: 'aria', previousLinks: [], links: [gpt], added: [gpt], removed: [], at: 240, revision: 1 },
    { action: 'Edited', userName: 'bima', previousLinks: [gpt], links: [gpt, claude], added: [claude], removed: [], at: 600, revision: 2 },
    { action: 'Cleared', userName: 'aria', previousLinks: [gpt, claude], links: [], added: [], removed: [gpt, claude], at: 900, revision: 0 },
    { action: 'Created', userName: 'aria', previousLinks: [], links: [gpt, claude], added: [gpt, claude], removed: [], at: 1260, revision: 1 },
  ].map(({ at, ...event }, i) => ({
    id: 500 + i, previousDeclaredNoAi: false, declaredNoAi: false,
    solvedAt: history.solvedAt, secondsSinceSolve: at, networkHint: i === 1 ? '9f2c4e81a0b7' : '3b7d10c2e4f9',
    occurredAt: history.solvedAt + at * 1000, ...event,
  }))
  const eventsPath = `/api/game/901/ai-chats/${history.participationId}/${history.challengeId}/events`
  // Solver uploads: Cookie Jar (9002) has two versions; every record keeps all versions.
  const sha = (seed) => seed.repeat(64 / seed.length)
  const solverVersion = (id, version, fileName, sizeBytes, uploadedBy, secondsSinceSolve, seed) => ({
    id, version, fileName, sizeBytes, sha256: sha(seed), uploadedBy, secondsSinceSolve,
    uploadedAt: solvedAt + secondsSinceSolve * 1000,
  })
  const solverStates = {
    9002: {
      editable: true, solved: true, editableUntil: now + 8000000, maxFileBytes: 1048576, maxVersions: 10,
      teamBytesUsed: 5530, teamBytesLimit: 16777216,
      versions: [
        solverVersion(802, 2, 'solve.py', 3412, 'bima', 1500, '7c1e'),
        solverVersion(801, 1, 'solve.py', 2118, 'aria', 320, 'a94f'),
      ],
    },
  }
  const solverStateFor = (id) =>
    solverStates[id] ?? {
      editable: id <= 9003, solved: id <= 9003, editableUntil: now + 8000000, maxFileBytes: 1048576, maxVersions: 10,
      teamBytesUsed: 5530, teamBytesLimit: 16777216, versions: [],
    }
  const solverRecords = [
    { team: 0, challenge: 1, versions: solverStates[9002].versions },
    { team: 1, challenge: 0, versions: [solverVersion(811, 1, 'exploit.py', 4096, 'bima', 210, 'e03b')] },
    { team: 2, challenge: 2, versions: [
      solverVersion(823, 3, 'solve.sage', 1880, 'citra', 7300, '51d2'),
      solverVersion(822, 2, 'solve.sage', 1792, 'citra', 6100, '0f8c'),
      solverVersion(821, 1, 'notes.md', 944, 'citra', 5500, 'b6a7'),
    ] },
  ].map((record) => ({
    participationId: 70 + record.team, teamId: 7 + record.team, teamName: teams[record.team],
    challengeId: challenges[record.challenge].id, challengeTitle: challenges[record.challenge].title,
    category: challenges[record.challenge].category, solvedAt, versions: record.versions,
  }))
  const config = {
    title: 'RSCTF', slogan: 'Capture the flag', portMapping: 'Default', allowRegister: true, allowPasswordRegistration: true,
    allowTeamCreation: true, emailConfirmationRequired: false, enableBrowserFingerprint: false,
  }
  const settings = {
    revision: 1, globalConfig: { title: 'RSCTF' },
    accountPolicy: { allowRegister: true, allowPasswordRegistration: true, allowTeamCreation: true },
    containerPolicy: { portMapping: 'Default', defaultLifetime: 120, extensionDuration: 120, renewalWindow: 10 },
    containerProvider: { type: 'Docker', name: 'Docker', available: true }, buildRegistry: {}, email: {},
    captcha: { provider: 'None' }, oAuth: {}, registry: {}, donations: { enabled: false },
    proxyTrust: { enabled: false, trustedNetworksCsv: '' },
  }
  const group = (items) => items.reduce((map, item) => ({ ...map, [item.category]: [...(map[item.category] ?? []), item] }), {})
  const responses = {
    '/api/account/profile': profile, '/api/config': config, '/api/captcha': { type: 'None' },
    '/api/game/901': game,
    '/api/game/901/details': { challenges: group(challenges), challengeCount: challenges.length, rank, teamToken: 'fixture' },
    '/api/game/901/details/participant': { rank },
    '/api/game/901/notices': [],
    '/api/game/901/scoreboard': { updateTimeUtc: now, bloodBonus: 0, challenges: group(challenges), challengeCount: challenges.length, items: [rank], timelines: [], divisions: [] },
    [eventsPath]: { items: events, truncated: false },
    '/api/admin/ai-chat-providers': { providers: providerModels, maxCustomProviders: 32 },
    '/api/admin/config': settings,
  }
  for (const c of challenges) {
    responses[`/api/game/901/challenges/${c.id}`] = {
      ...c, content: `Exploit **${c.title}** and submit the flag.`, context: { url: `/assets/${'a'.repeat(64)}/handout.zip`, fileSize: 20480 },
      attempts: 0, hints: [], userRating: 2, userComment: 'Clean heap layout, fun tcache trick.',
    }
    responses[`/api/game/901/challenges/${c.id}/solvers/page`] = { data: [], total: c.solved }
  }
  const pendingIds = () => Object.entries(states).filter(([, value]) => value.pending).map(([id]) => Number(id))
  const aiChatState = /^\/api\/game\/901\/challenges\/(\d+)\/ai-chats$/
  const solverState = /^\/api\/game\/901\/challenges\/(\d+)\/solver-uploads$/
  const writes = []
  const handle = (path, method = 'GET', body = '') => {
    const url = new URL(path, 'http://localhost')
    const p = url.pathname.toLowerCase()
    if (['GET', 'HEAD'].includes(method)) {
      if (aiChatState.test(p)) return { body: stateFor(Number(p.match(aiChatState)[1])) }
      if (solverState.test(p)) return { body: solverStateFor(Number(p.match(solverState)[1])) }
      if (p === '/api/game/901/solver-uploads') return { body: { total: solverRecords.length, items: solverRecords } }
      if (p === '/api/game/901/ai-chats/pending') return { body: { required: true, challengeIds: pendingIds() } }
      if (p === '/api/game/901/ai-chats') {
        const status = url.searchParams.get('status')
        const items = records.filter((record) => !status || record.status === status)
        return { body: { total: items.length, required: true, items } }
      }
      return responses[p] === undefined ? { unknown: p, body: [] } : { body: responses[p] }
    }
    writes.push({ path: p, method, body })
    if (aiChatState.test(p) && method === 'PUT') {
      const id = Number(p.match(aiChatState)[1])
      const current = stateFor(id)
      const model = JSON.parse(body)
      const disclosed = model.links.length > 0 || model.noAiUsed === true
      const wasDisclosed = current.links.length > 0 || current.declaredNoAi
      const provider = (url) => current.providers.find((rule) => new RegExp(`^(?:${rule.pattern})$`, 'u').test(url))
      states[id] = {
        ...current,
        revision: model.expectedRevision + 1, updatedAt: now, submittedBy: 'aria',
        links: model.links.map((url) => ({ url, providerKey: provider(url)?.key ?? 'claude', providerLabel: provider(url)?.label ?? 'Claude' })),
        declaredNoAi: model.noAiUsed === true,
        pending: current.required && !disclosed,
        firstDisclosedAt: current.firstDisclosedAt ?? (disclosed ? now : null),
        editCount: current.editCount + (wasDisclosed ? 1 : 0),
      }
      return { body: states[id] }
    }
    if (p.startsWith('/api/admin/ai-chat-providers/')) {
      const key = decodeURIComponent(p.split('/').pop())
      const model = JSON.parse(body || '{}')
      const current = providerModels.find((provider) => provider.key === key) ?? { key, builtin: false, examples: [], updatedAt: now }
      return { body: { ...current, ...model } }
    }
    return { status: 405, body: { title: 'Fixture mutation blocked', status: 405 } }
  }
  return { profile, builtins, providerModels, state, states, records, events, eventsPath, solverRecords, handle, writes }
}
