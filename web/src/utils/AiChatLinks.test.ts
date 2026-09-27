import assert from 'node:assert/strict'
import test from 'node:test'
import {
  aiChatHostname,
  aiChatMatchOrder,
  compileAiChatPattern,
  matchAiChatProvider,
  normalizeAiChatUrl,
  safeAiChatHref,
  validateAiChatPattern,
  validateAiChatProviderKey,
} from './AiChatLinks'

// Test-only copy of the contract's built-in list. At runtime the enabled list
// always comes from the server.
const BUILTINS = [
  {
    key: 'chatgpt',
    label: 'ChatGPT',
    pattern: String.raw`https://(?:chatgpt\.com|chat\.openai\.com)/share/[A-Za-z0-9-]{8,128}(?:\?[!-~]*)?`,
  },
  { key: 'claude', label: 'Claude', pattern: String.raw`https://claude\.ai/share/[A-Za-z0-9-]{8,128}(?:\?[!-~]*)?` },
  {
    key: 'gemini',
    label: 'Gemini',
    pattern: String.raw`https://(?:gemini\.google\.com/share|g\.co/gemini/share)/[A-Za-z0-9_-]{6,128}(?:\?[!-~]*)?`,
  },
  { key: 'grok', label: 'Grok', pattern: String.raw`https://grok\.com/share/[A-Za-z0-9_-]{8,160}(?:\?[!-~]*)?` },
  {
    key: 'deepseek',
    label: 'DeepSeek',
    pattern: String.raw`https://chat\.deepseek\.com/share/[A-Za-z0-9_-]{6,128}(?:\?[!-~]*)?`,
  },
  {
    key: 'perplexity',
    label: 'Perplexity',
    pattern: String.raw`https://(?:www\.)?perplexity\.ai/search/[A-Za-z0-9._~%-]{6,256}(?:\?[!-~]*)?`,
  },
  {
    key: 'kimi',
    label: 'Kimi',
    pattern: String.raw`https://(?:www\.)?kimi\.com/share/[A-Za-z0-9_-]{6,128}(?:\?[!-~]*)?`,
  },
  {
    key: 'huggingchat',
    label: 'HuggingChat',
    pattern: String.raw`https://(?:hf\.co|huggingface\.co)/chat/r/[A-Za-z0-9_-]{4,64}(?:\?[!-~]*)?`,
  },
]

const match = (input: string) => {
  const normalized = normalizeAiChatUrl(input)
  assert.equal(normalized.ok, true, `${input} should normalize`)
  return normalized.ok ? (matchAiChatProvider(normalized.url, BUILTINS)?.key ?? null) : null
}

test('every built-in sample share link matches its own provider', () => {
  const samples: [string, string][] = [
    ['https://chatgpt.com/share/6711a2b3-1234-8000-abcd-0123456789ab', 'chatgpt'],
    ['https://chat.openai.com/share/abcd1234efgh', 'chatgpt'],
    ['https://claude.ai/share/0f6b1c2d-3e4f-5a6b-7c8d-9e0f1a2b3c4d', 'claude'],
    ['https://gemini.google.com/share/a1b2c3d4e5f6', 'gemini'],
    ['https://g.co/gemini/share/AbC_12-x', 'gemini'],
    ['https://grok.com/share/bGVnYWN5_abc123XYZ', 'grok'],
    ['https://chat.deepseek.com/share/abc123def456', 'deepseek'],
    ['https://www.perplexity.ai/search/how-to-solve-rsa-AbC.123~x', 'perplexity'],
    ['https://perplexity.ai/search/abcdef', 'perplexity'],
    ['https://www.kimi.com/share/d1abc2def3', 'kimi'],
    ['https://kimi.com/share/d1abc2def3', 'kimi'],
    ['https://huggingface.co/chat/r/AbCd12', 'huggingchat'],
    ['https://hf.co/chat/r/xyz9', 'huggingchat'],
    ['https://claude.ai/share/0f6b1c2d-3e4f?utm_source=copy', 'claude'],
  ]
  for (const [url, provider] of samples) assert.equal(match(url), provider, url)
})

test('normalization keeps the contract form and rejects unsafe URLs', () => {
  assert.deepEqual(normalizeAiChatUrl('  https://CLAUDE.ai/share/abcdefgh#section  '), {
    ok: true,
    url: 'https://claude.ai/share/abcdefgh',
  })
  // A default port is not an explicit port under WHATWG or Rust `url`.
  assert.deepEqual(normalizeAiChatUrl('https://claude.ai:443/share/abcdefgh'), {
    ok: true,
    url: 'https://claude.ai/share/abcdefgh',
  })
  // `URL.href` keeps a bare trailing `?`; the server uses the same form.
  assert.deepEqual(normalizeAiChatUrl('https://claude.ai/share/abcdefgh?'), {
    ok: true,
    url: 'https://claude.ai/share/abcdefgh?',
  })
  assert.deepEqual(normalizeAiChatUrl(''), { ok: false, reason: 'empty' })
  assert.deepEqual(normalizeAiChatUrl('claude.ai/share/abcdefgh'), { ok: false, reason: 'invalid' })
  assert.deepEqual(normalizeAiChatUrl('http://chatgpt.com/share/abcdefgh1234'), {
    ok: false,
    reason: 'https_required',
  })
  assert.deepEqual(normalizeAiChatUrl('javascript:alert(1)'), { ok: false, reason: 'https_required' })
  assert.deepEqual(normalizeAiChatUrl('https://user:pw@claude.ai/share/abcdefgh'), {
    ok: false,
    reason: 'credentials',
  })
  assert.deepEqual(normalizeAiChatUrl('https://user@claude.ai/share/abcdefgh'), { ok: false, reason: 'credentials' })
  assert.deepEqual(normalizeAiChatUrl('https://claude.ai:8443/share/abcdefgh'), { ok: false, reason: 'port' })
  assert.deepEqual(normalizeAiChatUrl(`https://claude.ai/share/${'a'.repeat(2100)}`), {
    ok: false,
    reason: 'too_long',
  })
})

test('near-miss links do not match any built-in provider', () => {
  assert.equal(match('https://chatgpt.com.evil.test/share/abcdefgh1234'), null)
  assert.equal(match('https://evilchatgpt.com/share/abcdefgh1234'), null)
  assert.equal(match('https://chatgpt.com/c/abcdefgh1234'), null)
  assert.equal(match('https://chatgpt.com/share/abc'), null)
  assert.equal(match('https://claude.ai/share/abcdefgh/extra'), null)
  assert.equal(match('https://claude.ai/chat/abcdefgh1234'), null)
  // The fragment is stripped before matching rather than being matched against.
  assert.equal(match('https://claude.ai/share/abcdefgh#../../evil'), 'claude')
})

test('the first matching provider wins and invalid patterns are skipped', () => {
  const providers = [
    { key: 'broken', label: 'Broken', pattern: 'https://(' },
    { key: 'first', label: 'First', pattern: String.raw`https://example\.test/.*` },
    { key: 'second', label: 'Second', pattern: String.raw`https://example\.test/share/.*` },
  ]
  assert.equal(compileAiChatPattern('https://('), null)
  assert.equal(matchAiChatProvider('https://example.test/share/x', providers)?.key, 'first')
  assert.equal(matchAiChatProvider('https://other.test/share/x', providers), null)
})

test('match order puts enabled built-ins first, then enabled custom providers by key', () => {
  const ordered = aiChatMatchOrder([
    { key: 'zeta', label: 'Z', pattern: 'https://z', builtin: false, enabled: true },
    { key: 'claude', label: 'Claude', pattern: 'https://c', builtin: true, enabled: true },
    { key: 'alpha', label: 'A', pattern: 'https://a', builtin: false, enabled: true },
    { key: 'grok', label: 'Grok', pattern: 'https://g', builtin: true, enabled: false },
    { key: 'chatgpt', label: 'ChatGPT', pattern: 'https://o', builtin: true, enabled: true },
  ])
  assert.deepEqual(
    ordered.map((provider) => provider.key),
    ['claude', 'chatgpt', 'alpha', 'zeta']
  )
})

test('rendered links and hostnames only come from normalized https URLs', () => {
  assert.equal(safeAiChatHref('javascript:alert(1)'), null)
  assert.equal(safeAiChatHref('data:text/html,hi'), null)
  assert.equal(safeAiChatHref('https://claude.ai/share/abcdefgh'), 'https://claude.ai/share/abcdefgh')
  assert.equal(aiChatHostname('https://www.perplexity.ai/search/abcdef'), 'www.perplexity.ai')
  assert.equal(aiChatHostname('not a url'), '')
})

test('admin pattern and key validation mirror the provider rules', () => {
  assert.equal(validateAiChatPattern(String.raw`https://copilot\.microsoft\.com/shares/[A-Za-z0-9]{8,64}`), null)
  assert.equal(validateAiChatPattern(''), 'empty')
  assert.equal(validateAiChatPattern(String.raw`http://copilot\.microsoft\.com/shares/.*`), 'https_prefix')
  assert.equal(validateAiChatPattern(String.raw`https://poe\.com/s/x|https://.*`), 'alternation')
  assert.equal(validateAiChatPattern(String.raw`https://poe\.com/(?:s|share)/[a-z|]{6,64}`), null)
  assert.equal(validateAiChatPattern('https://example\\.test/(unclosed'), 'syntax')
  assert.equal(validateAiChatPattern(`https://${'a'.repeat(520)}`), 'too_long')

  const existing = [
    { key: 'claude', builtin: true },
    { key: 'copilot', builtin: false },
  ]
  assert.equal(validateAiChatProviderKey('mistral-le-chat', existing), null)
  assert.equal(validateAiChatProviderKey('Mistral', existing), 'format')
  assert.equal(validateAiChatProviderKey('-leading', existing), 'format')
  assert.equal(validateAiChatProviderKey('a'.repeat(41), existing), 'format')
  assert.equal(validateAiChatProviderKey('claude', existing), 'builtin')
  assert.equal(validateAiChatProviderKey('copilot', existing), 'duplicate')
})
