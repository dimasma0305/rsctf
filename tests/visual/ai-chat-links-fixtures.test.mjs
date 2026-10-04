import assert from 'node:assert/strict'
import test from 'node:test'
import { aiChatFixture, builtinAgentSignatures, builtinProviders } from './ai-chat-links-fixtures.mjs'

const matches = (pattern, url) => new RegExp(`^(?:${pattern})$`, 'u').test(url)

test('built-in providers are parsed from the Rust source with working examples', () => {
  const providers = builtinProviders()
  assert.deepEqual(
    providers.map((provider) => provider.key),
    ['chatgpt', 'claude', 'gemini', 'grok', 'deepseek', 'perplexity', 'kimi', 'huggingchat']
  )
  for (const provider of providers) {
    assert.ok(provider.pattern.startsWith('https://'), provider.key)
    assert.ok(provider.examples.length > 0, provider.key)
    for (const example of provider.examples) {
      assert.ok(matches(provider.pattern, example), `${provider.key} ${example}`)
      const winner = providers.find((candidate) => matches(candidate.pattern, example))
      assert.equal(winner.key, provider.key, `${example} is claimed by an earlier provider`)
    }
    assert.equal(matches(provider.pattern, provider.examples[0].replace('https://', 'http://')), false)
  }
})

test('fixture links, disabled providers, and mutations stay consistent', () => {
  const fixture = aiChatFixture(1_790_000_000_000)
  const enabled = fixture.state.providers
  assert.ok(!enabled.some((provider) => provider.key === 'grok'), 'the disabled built-in is not offered')
  for (const link of fixture.state.links) {
    assert.ok(enabled.some((provider) => provider.key === link.providerKey && matches(provider.pattern, link.url)))
  }
  const blocked = fixture.records.flatMap((record) => record.links).filter((link) => !link.providerActive)
  assert.deepEqual([...new Set(blocked.map((link) => link.providerKey))], ['grok'])
  assert.equal(fixture.handle('/api/game/901/challenges/9002/ai-chats').body.revision, 2)
  assert.equal(fixture.handle('/api/unknown').unknown, '/api/unknown')
  assert.equal(fixture.handle('/api/game/901/challenges/9002', 'POST', '{}').status, 405)
  const saved = fixture.handle('/api/game/901/challenges/9002/ai-chats', 'PUT', JSON.stringify({ links: [], expectedRevision: 2 }))
  assert.equal(saved.body.revision, 3)
  assert.equal(fixture.writes.length, 2)
})

test('disclosure fixtures cover pending, links, no-AI, missing, and a full history', () => {
  const fixture = aiChatFixture(1_790_000_000_000)
  const get = (path) => fixture.handle(path).body
  assert.equal(get('/api/game/901').aiChatLinksRequired, true)
  assert.deepEqual(get('/api/game/901/ai-chats/pending'), { required: true, challengeIds: [9001] })

  const pending = get('/api/game/901/challenges/9001/ai-chats')
  assert.equal(pending.pending, true)
  assert.deepEqual(pending.links, [])
  const edited = get('/api/game/901/challenges/9002/ai-chats')
  assert.equal(edited.editCount, 1)
  assert.ok(edited.firstDisclosedAt > edited.solvedAt)
  assert.equal(get('/api/game/901/challenges/9003/ai-chats').declaredNoAi, true)
  assert.equal(get('/api/game/901/challenges/9004/ai-chats').solved, false)

  const all = get('/api/game/901/ai-chats?count=50&skip=0')
  assert.equal(all.required, true)
  assert.deepEqual([...new Set(all.items.map((record) => record.status))], ['Links', 'NoAi', 'Missing'])
  const missing = get('/api/game/901/ai-chats?count=50&skip=0&status=Missing')
  assert.equal(missing.total, missing.items.length)
  for (const record of missing.items) {
    assert.deepEqual(record.links, [])
    assert.equal(record.updatedAt, null)
    assert.equal(record.submittedBy, null)
    assert.equal(record.revision, 0)
  }

  const history = get(fixture.eventsPath)
  assert.deepEqual(history.items.map((event) => event.action), ['Created', 'Edited', 'Cleared', 'Created'])
  assert.deepEqual(history.items.map((event) => event.revision), [1, 2, 0, 1], 'a clear restarts revisions')
  const record = all.items.find((item) => item.status === 'Links' && item.eventCount === history.items.length)
  assert.equal(record.editCount, history.items.filter((event) => event.action === 'Edited').length)
  assert.equal(record.revision, history.items.at(-1).revision)
  assert.ok(history.items.some((event) => event.added.length > 0))
  assert.ok(history.items.some((event) => event.removed.length > 0))
  for (const event of history.items) {
    assert.match(event.networkHint, /^[0-9a-f]{12}$/)
    assert.deepEqual(
      [...event.previousLinks.filter((url) => !event.removed.includes(url)), ...event.added].sort(),
      [...event.links].sort()
    )
  }

  // Declaring "No AI used" releases the pending challenge.
  const declared = fixture.handle(
    '/api/game/901/challenges/9001/ai-chats',
    'PUT',
    JSON.stringify({ links: [], expectedRevision: 0, noAiUsed: true })
  ).body
  assert.equal(declared.declaredNoAi, true)
  assert.equal(declared.pending, false)
  assert.deepEqual(get('/api/game/901/ai-chats/pending').challengeIds, [])
  assert.equal(get('/api/game/901/challenges/9001/ai-chats').revision, 1)
})

test('solver fixtures keep versions newest first and every record reviewable', () => {
  const fixture = aiChatFixture(1_790_000_000_000)
  const state = fixture.handle('/api/game/901/challenges/9002/solver-uploads').body
  assert.deepEqual(state.versions.map((version) => version.version), [2, 1])
  assert.equal(state.teamBytesUsed, state.versions.reduce((sum, version) => sum + version.sizeBytes, 0))
  assert.equal(fixture.handle('/api/game/901/challenges/9004/solver-uploads').body.solved, false)
  const page = fixture.handle('/api/game/901/solver-uploads?count=50&skip=0').body
  assert.equal(page.total, page.items.length)
  for (const record of page.items) {
    const versions = record.versions.map((version) => version.version)
    assert.deepEqual(versions, [...versions].sort((a, b) => b - a))
    for (const version of record.versions) assert.match(version.sha256, /^[0-9a-f]{64}$/)
  }
})

test('agent signature built-ins are parsed from the Rust source with matching examples', () => {
  const signatures = builtinAgentSignatures()
  assert.deepEqual(
    signatures.map((signature) => signature.key),
    ['claude-code-scratchpad', 'claude-code-trailer', 'claude-code-projects', 'codex-home', 'cursor-projects']
  )
  for (const signature of signatures) {
    // Rust byte regexes: strip the ASCII-mode group, which JavaScript lacks.
    const pattern = new RegExp(signature.pattern.replace('(?-u:[^/\\s])', '[^/\\s]'))
    for (const example of signature.examples) assert.ok(pattern.test(example), `${signature.key} ${example}`)
  }
  const fixture = aiChatFixture(1_790_000_000_000)
  assert.equal(fixture.handle('/api/admin/agent-signatures').body.signatures.length, fixture.agentSignatureCount)
})
