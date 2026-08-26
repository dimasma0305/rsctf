import assert from 'node:assert/strict'
import test from 'node:test'
import { isValidTeamInviteCode } from './TeamInvite'

const token = '0123456789abcdef0123456789abcdef'

test('team invite readiness matches the format accepted by the join action', () => {
  assert.equal(isValidTeamInviteCode(`rookies:7:${token}`), true)
  assert.equal(isValidTeamInviteCode(`red:blue:42:${token}`), true)
  assert.equal(isValidTeamInviteCode(`rookies:7:${token.slice(0, -1)}`), false)
  assert.equal(isValidTeamInviteCode(`rookies:7:${token.toUpperCase()}`), false)
  assert.equal(isValidTeamInviteCode(`rookies:7:${token} `), false)
  assert.equal(isValidTeamInviteCode('rookies'), false)
})
