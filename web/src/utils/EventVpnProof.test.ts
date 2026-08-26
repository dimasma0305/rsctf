import assert from 'node:assert/strict'
import test from 'node:test'
import { protectedEventGameId, protectedEventGamePathId } from './EventVpnProof'

const protectedPaths = [
  '/api/game/7/challenges/9',
  '/API/GAME/7/CHALLENGES/9',
  '/Api/GaMe/7/Challenges/9',
  '/api/game/7/ad/scoreboard',
  '/API/GAME/7/AD/SCOREBOARD',
  '/api/Game/7/Ad/Scoreboard',
  '/api/game/7/ad/koth/scoreboard',
  '/API/GAME/7/AD/KOTH/SCOREBOARD',
  '/api/Game/7/Ad/Koth/Scoreboard',
]

test('event VPN proof matching covers Jeopardy, A&D and KotH route casing', () => {
  for (const path of protectedPaths) {
    assert.equal(protectedEventGamePathId(path), 7, path)
  }
})

test('event VPN proof matching preserves intentional public routes', () => {
  for (const path of [
    '/api/game/7',
    '/API/GAME/7',
    '/api/Game/7/Check',
    '/API/game/7/VpN/challenge',
    '/api/game/recent',
    '/api/edit/games/7',
  ]) {
    assert.equal(protectedEventGamePathId(path), null, path)
  }
})

test('event VPN proof matching accepts only positive PostgreSQL game ids', () => {
  assert.equal(protectedEventGamePathId('/api/game/+7/details'), 7)
  for (const path of [
    '/api/game/0/details',
    '/api/game/-7/details',
    '/api/game/2147483648/details',
    '/api/game/not-a-game/details',
    '/api/games/7/details',
  ]) {
    assert.equal(protectedEventGamePathId(path), null, path)
  }
})

test('event VPN proof matching never attaches a proof to another origin', () => {
  assert.equal(protectedEventGameId('/api/Game/7/Ad/Scoreboard', 'https://arena.test'), 7)
  assert.equal(protectedEventGameId('https://attacker.test/api/Game/7/Ad/Scoreboard', 'https://arena.test'), null)
})
