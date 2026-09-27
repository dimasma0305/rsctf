import assert from 'node:assert/strict'
import test from 'node:test'
import { aiChatFixture, builtinProviders } from './ai-chat-links-fixtures.mjs'

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
