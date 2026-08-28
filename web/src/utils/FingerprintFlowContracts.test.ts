import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const login = readFileSync('src/pages/account/Login.tsx', 'utf8')
const register = readFileSync('src/pages/account/Register.tsx', 'utf8')
const teamJoin = readFileSync('src/components/TeamJoinModal.tsx', 'utf8')
const eventJoin = readFileSync('src/pages/games/[id]/Index.tsx', 'utf8')
const eventJoinModal = readFileSync('src/components/GameJoinModal.tsx', 'utf8')
const enrollment = readFileSync('src/utils/EnrollmentFlow.ts', 'utf8')

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

test('all identity callers share the abortable fingerprint collection path', () => {
  for (const source of [login, register, enrollment]) {
    assert.match(source, /collectFingerprintIdentity\(\{/)
    assert.match(source, /signal,/)
    assert.doesNotMatch(source, /accountFingerprintChallenge|getFingerprintPayload/)
  }
  assert.match(teamJoin, /attemptAbort\.current\?\.abort\(\)/)
  assert.match(teamJoin, /submitTeamEnrollment\(\{[\s\S]*signal: controller\.signal/)
  assert.match(eventJoin, /submitGameEnrollment\(\{[\s\S]*signal,/)
  assert.match(eventJoinModal, /submissionAbort\.current\?\.abort\(\)/)
  assert.match(eventJoinModal, /onSubmitJoin\([\s\S]*controller\.signal/)
  assert.match(eventJoinModal, /component="form"[\s\S]*type="submit"/)
})
