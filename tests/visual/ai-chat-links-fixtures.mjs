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
    writeupRequired: false, aiChatLinksEnabled: true,
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
  const state = {
    editable: true, solved: true, editableUntil: now + 8000000, maxLinks: 5,
    providers: providerModels.filter((p) => p.enabled).map(({ key, label, pattern }) => ({ key, label, pattern })),
    links: [
      { url: example('chatgpt'), providerKey: 'chatgpt', providerLabel: 'ChatGPT' },
      { url: example('claude'), providerKey: 'claude', providerLabel: 'Claude' },
    ],
    revision: 2, updatedAt: now - 60000, submittedBy: 'aria',
  }
  const teams = ['Packet Pioneers', 'Stack Underflow', 'Null Pointers', 'Byte Bandits']
  const records = [
    { team: 0, challenge: 1, links: [['chatgpt', 'ChatGPT'], ['claude', 'Claude']], by: 'aria' },
    { team: 1, challenge: 0, links: [['gemini', 'Gemini']], by: 'bima' },
    { team: 2, challenge: 2, links: [['grok', 'Grok'], ['deepseek', 'DeepSeek']], by: 'citra' },
    { team: 3, challenge: 1, links: [['perplexity', 'Perplexity']], by: 'dewi' },
  ].map((record, i) => ({
    participationId: 70 + i, teamId: 7 + i, teamName: teams[record.team],
    challengeId: challenges[record.challenge].id, challengeTitle: challenges[record.challenge].title,
    category: challenges[record.challenge].category,
    links: record.links.map(([key, label]) => ({ url: example(key), providerKey: key, providerLabel: label, providerActive: !disabled.has(key) })),
    submittedBy: record.by, updatedAt: now - (i + 1) * 420000, revision: 1,
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
    '/api/game/901/ai-chats': { total: records.length, items: records },
    '/api/admin/ai-chat-providers': { providers: providerModels, maxCustomProviders: 32 },
    '/api/admin/config': settings,
  }
  for (const c of challenges) {
    responses[`/api/game/901/challenges/${c.id}`] = {
      ...c, content: `Exploit **${c.title}** and submit the flag.`, context: { url: `/assets/${'a'.repeat(64)}/handout.zip`, fileSize: 20480 },
      attempts: 0, hints: [], userRating: 2, userComment: 'Clean heap layout, fun tcache trick.',
    }
    responses[`/api/game/901/challenges/${c.id}/solvers/page`] = { data: [], total: c.solved }
    responses[`/api/game/901/challenges/${c.id}/ai-chats`] = state
  }
  const writes = []
  const handle = (path, method = 'GET', body = '') => {
    const p = new URL(path, 'http://localhost').pathname.toLowerCase()
    if (['GET', 'HEAD'].includes(method)) {
      return responses[p] === undefined ? { unknown: p, body: [] } : { body: responses[p] }
    }
    writes.push({ path: p, method, body })
    if (/^\/api\/game\/901\/challenges\/\d+\/ai-chats$/.test(p) && method === 'PUT') {
      const model = JSON.parse(body)
      return { body: { ...state, revision: model.expectedRevision + 1, links: model.links.map((url) => ({ url, providerKey: 'claude', providerLabel: 'Claude' })) } }
    }
    if (p.startsWith('/api/admin/ai-chat-providers/')) {
      const key = decodeURIComponent(p.split('/').pop())
      const model = JSON.parse(body || '{}')
      const current = providerModels.find((provider) => provider.key === key) ?? { key, builtin: false, examples: [], updatedAt: now }
      return { body: { ...current, ...model } }
    }
    return { status: 405, body: { title: 'Fixture mutation blocked', status: 405 } }
  }
  return { profile, builtins, providerModels, state, records, handle, writes }
}
