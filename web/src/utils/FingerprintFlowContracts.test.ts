import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const login = readFileSync('src/pages/account/Login.tsx', 'utf8')
const register = readFileSync('src/pages/account/Register.tsx', 'utf8')
const teamJoin = readFileSync('src/components/TeamJoinModal.tsx', 'utf8')
const eventJoin = readFileSync('src/pages/games/[id]/Index.tsx', 'utf8')
const eventJoinModal = readFileSync('src/components/GameJoinModal.tsx', 'utf8')

test('login and registration use one native form-submit path with a consent single-flight', () => {
  for (const source of [login, register]) {
    assert.match(source, /onSubmit=\{on(?:Login|Register)\}/)
    assert.match(source, /useConsentSingleFlight\(/)
    assert.match(source, /type="submit"/)
    assert.doesNotMatch(source, /onClick=\{on(?:Login|Register)\}/)
    assert.match(source, /acceptConsent\(\)/)
    assert.match(source, /rejectConsent\(\)/)
  }
})

test('all four identity callers share the abortable fingerprint collection path', () => {
  for (const source of [login, register, teamJoin, eventJoin]) {
    assert.match(source, /collectFingerprintIdentity\(\{/)
    assert.match(source, /signal,/)
    assert.doesNotMatch(source, /accountFingerprintChallenge|getFingerprintPayload/)
  }
  assert.match(teamJoin, /useAbortableSingleFlight\(executeJoinTeam\)/)
  assert.match(teamJoin, /joinOperation\.cancel\(\)[\s\S]*setJoining\(false\)[\s\S]*modalProps\.onClose\(\)/)
  assert.match(eventJoinModal, /useAbortableSingleFlight\(executeJoinGame\)/)
  assert.match(eventJoinModal, /joinOperation\.cancel\(\)[\s\S]*setDisabled\(false\)[\s\S]*modalProps\.onClose\(\)/)
  assert.match(eventJoinModal, /component="form"[\s\S]*type="submit"/)
})
