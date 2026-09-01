import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const login = readFileSync('src/pages/account/Login.tsx', 'utf8')
const register = readFileSync('src/pages/account/Register.tsx', 'utf8')
const teamJoin = readFileSync('src/components/TeamJoinModal.tsx', 'utf8')
const eventJoin = readFileSync('src/components/GameJoinModal.tsx', 'utf8')

test('login and registration carry one operation across explicit consent', () => {
  for (const source of [login, register]) {
    assert.match(source, /OperationRef = useRef/)
    assert.match(source, /consentGranted = false/)
    assert.match(source, /void execute(?:Login|Register)\(true\)/)
    assert.match(source, /collectEncryptedFingerprintIdentity/)
  }
})

test('native form submission is the only account button activation path', () => {
  assert.match(login, /<Button type="submit" fullWidth disabled=\{disabled\}>/)
  assert.match(register, /<Button type="submit" fullWidth disabled=\{disabled\}>/)
  assert.doesNotMatch(register, /type="submit"[^>]+onClick=/)
  assert.doesNotMatch(login, /type="submit"[^>]+onClick=/)
})

test('team and event enrollment share abortable single-flight collection', () => {
  assert.match(teamJoin, /joinOperationRef = useRef<AbortController/)
  assert.match(teamJoin, /collectEncryptedFingerprintIdentity/)
  assert.match(eventJoin, /submitController = useRef<AbortController/)
  assert.match(eventJoin, /onSubmitJoin: \(info: GameJoinModel, signal: AbortSignal\)/)
})
