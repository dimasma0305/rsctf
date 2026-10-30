import assert from 'node:assert/strict'
import test from 'node:test'
import { isValidTeamInviteCode, teamInviteFromInput, isTeamInviteDestination } from './TeamInvite'

const token = '0123456789abcdef0123456789abcdef'

test('team invite readiness matches the format accepted by the join action', () => {
  assert.equal(isValidTeamInviteCode(`rookies:7:${token}`), true)
  assert.equal(isValidTeamInviteCode(`red:blue:42:${token}`), true)
  assert.equal(isValidTeamInviteCode(`rookies:7:${token.slice(0, -1)}`), false)
  assert.equal(isValidTeamInviteCode(`rookies:7:${token.toUpperCase()}`), false)
  assert.equal(isValidTeamInviteCode(`rookies:7:${token} `), false)
  assert.equal(isValidTeamInviteCode('rookies'), false)
})

test('pasted invitation links stay on this platform and survive account detours', () => {
  const code = `rookies:7:${token}`
  const path = `/teams?join=${encodeURIComponent(code)}`
  assert.equal(teamInviteFromInput(`https://ctf.test${path}`, 'https://ctf.test'), code)
  assert.equal(teamInviteFromInput(path, 'https://ctf.test'), code)
  assert.equal(isTeamInviteDestination(path), true)
  assert.equal(isTeamInviteDestination('/teams?join=wrong'), false)
  assert.equal(isTeamInviteDestination('//evil.test/teams?join=' + code), false)
  assert.notEqual(teamInviteFromInput(`https://evil.test${path}`, 'https://ctf.test'), code)
})
