import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { agentSignatureKeyProblem } from './AgentSignatures'

const signatures = [
  { key: 'claude-code-scratchpad', builtin: true },
  { key: 'my-agent', builtin: false },
]

test('agent signature keys follow the server key rule and cannot shadow existing keys', () => {
  assert.equal(agentSignatureKeyProblem('new-agent-2', signatures), null)
  for (const bad of ['', '-x', 'Upper', 'under_score', 'a'.repeat(41)]) {
    assert.equal(agentSignatureKeyProblem(bad, signatures), 'format', bad)
  }
  assert.equal(agentSignatureKeyProblem('claude-code-scratchpad', signatures), 'builtin')
  assert.equal(agentSignatureKeyProblem('my-agent', signatures), 'duplicate')
})

test('agent signatures are a settings section and rescans are admin-only', () => {
  const navigation = readFileSync('src/components/admin/navigation.ts', 'utf8')
  const settings = readFileSync('src/pages/admin/Settings.tsx', 'utf8')
  const cheat = readFileSync('src/pages/games/[id]/monitor/CheatCheck.tsx', 'utf8')
  assert.match(navigation, /key: 'agent_signatures',\s*icon: mdiFingerprint/)
  assert.match(settings, /activeSection === 'agent_signatures' && <AgentSignaturesSettings \/>/)
  assert.match(settings, /agent_signatures: agentSignaturesConfigured \? 'configured' : 'inactive'/)
  assert.match(cheat, /\{user\?\.role === Role\.Admin && \(\s*<Button[\s\S]*?data-agent-rescan/)
  assert.match(cheat, /api\.admin\.adminRescanAgentArtifacts\(numId\)/)
})

test('every agent signature settings string exists in English and Chinese', () => {
  const source = readFileSync('src/components/admin/AgentSignaturesSettings.tsx', 'utf8')
  const keys = new Set(
    [...source.matchAll(/'admin\.content\.settings\.agent_signatures\.([a-z_]+)'/g)].map((match) => match[1])
  )
  assert.ok(keys.size > 20)
  for (const language of ['en-US', 'zh-CN']) {
    const admin = JSON.parse(readFileSync(`src/locales/${language}/admin.json`, 'utf8'))
    const section = admin.content.settings.agent_signatures
    assert.equal(typeof admin.content.settings.nav.agent_signatures, 'string', language)
    for (const key of keys) assert.equal(typeof section[key], 'string', `${language} ${key}`)
  }
})
