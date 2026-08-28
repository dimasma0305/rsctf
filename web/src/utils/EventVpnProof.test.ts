import axios, { AxiosError, AxiosHeaders, type AxiosRequestConfig, type AxiosResponse } from 'axios'
import assert from 'node:assert/strict'
import test from 'node:test'
import {
  EVENT_VPN_AUTH_REASON_HEADER,
  eventVpnMintRetryDelay,
  installEventVpnProof,
  isEventVpnUnauthorized,
  protectedEventGameId,
  protectedEventGamePathId,
  resetEventVpnProofForTests,
} from './EventVpnProof'

const adapterError = (config: AxiosRequestConfig, status: number, headers: Record<string, string> = {}): never => {
  const response: AxiosResponse = {
    config: { ...config, headers: AxiosHeaders.from(config.headers) },
    data: {},
    headers,
    status,
    statusText: String(status),
  }
  throw new AxiosError('request failed', AxiosError.ERR_BAD_REQUEST, response.config, undefined, response)
}

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

test('only the server-labelled VPN 401 is treated as a proof bootstrap', () => {
  assert.equal(
    isEventVpnUnauthorized({
      response: { status: 401, headers: { [EVENT_VPN_AUTH_REASON_HEADER]: 'event-vpn' } },
    }),
    true
  )
  assert.equal(isEventVpnUnauthorized({ response: { status: 401, headers: {} } }), false)
  assert.equal(
    isEventVpnUnauthorized({
      response: { status: 403, headers: { [EVENT_VPN_AUTH_REASON_HEADER]: 'event-vpn' } },
    }),
    false
  )
})

test('mint backoff is capped, jittered, and honors Retry-After', () => {
  assert.equal(
    eventVpnMintRetryDelay({}, 1, () => 0),
    750
  )
  assert.equal(
    eventVpnMintRetryDelay({}, 1, () => 1),
    1_250
  )
  assert.equal(
    eventVpnMintRetryDelay({}, 20, () => 1),
    75_000
  )
  assert.equal(
    eventVpnMintRetryDelay({ response: { headers: { 'retry-after': '90' } } }, 1, () => 0),
    90_000
  )
  assert.equal(
    eventVpnMintRetryDelay({ response: { headers: { 'retry-after': '9999' } } }, 1, () => 0),
    300_000
  )
})

test('a genuine session 401 never starts a VPN proof exchange', async () => {
  resetEventVpnProofForTests()
  let challengeRequests = 0
  const instance = axios.create({
    adapter: async (config) => {
      if (config.url?.endsWith('/vpn/challenge')) challengeRequests += 1
      return adapterError(config, 401)
    },
  })
  installEventVpnProof(instance, 'https://arena.test')
  await assert.rejects(instance.get('/api/game/7/details'), (error: AxiosError) => error.response?.status === 401)
  assert.equal(challengeRequests, 0)
})

test('a failed mint opens one shared circuit for repeated protected reads', async () => {
  resetEventVpnProofForTests()
  let challengeRequests = 0
  const instance = axios.create({
    adapter: async (config) => {
      if (config.url?.endsWith('/vpn/challenge')) {
        challengeRequests += 1
        return adapterError(config, 503)
      }
      return adapterError(config, 401, { [EVENT_VPN_AUTH_REASON_HEADER]: 'event-vpn' })
    },
  })
  installEventVpnProof(instance, 'https://arena.test')

  for (const path of ['/api/game/7/details', '/api/game/7/scoreboard']) {
    await assert.rejects(instance.get(path), (error: AxiosError) => error.response?.status === 401)
  }
  assert.equal(challengeRequests, 1)
})

test('expired proofs remint once and failed mint circuits reopen only after bounded time', async (context) => {
  context.mock.timers.enable({ apis: ['Date'], now: new Date('2026-08-28T00:00:00Z') })
  resetEventVpnProofForTests()
  let challengeRequests = 0
  let proofMints = 0
  let failChallenges = false
  const instance = axios.create({
    adapter: async (config) => {
      const path = new URL(config.url ?? '', 'https://arena.test').pathname
      if (path === '/api/game/7/vpn/challenge') {
        challengeRequests += 1
        if (failChallenges) return adapterError(config, 503)
        return {
          config: { ...config, headers: AxiosHeaders.from(config.headers) },
          data: {
            challenge: `challenge-${challengeRequests}`,
            proofUrl: 'https://arena.test/vpn-proof',
            proofHeader: 'x-rsctf-vpn-proof',
            expiresAtUtc: Date.now() + 60_000,
          },
          headers: {},
          status: 200,
          statusText: '200',
        }
      }
      if (path === '/vpn-proof') {
        proofMints += 1
        return {
          config: { ...config, headers: AxiosHeaders.from(config.headers) },
          data: {
            proof: `proof-${proofMints}`,
            proofHeader: 'x-rsctf-vpn-proof',
            expiresAtUtc: Date.now() + 30_000,
          },
          headers: {},
          status: 200,
          statusText: '200',
        }
      }
      const proof = AxiosHeaders.from(config.headers).get('x-rsctf-vpn-proof')?.toString()
      if (proof === `proof-${proofMints}` && proofMints > 0) {
        return {
          config: { ...config, headers: AxiosHeaders.from(config.headers) },
          data: { connected: true },
          headers: {},
          status: 200,
          statusText: '200',
        }
      }
      return adapterError(config, 401, { [EVENT_VPN_AUTH_REASON_HEADER]: 'event-vpn' })
    },
  })
  installEventVpnProof(instance, 'https://arena.test')

  try {
    assert.equal((await instance.get('/api/game/7/details')).status, 200)
    assert.equal(challengeRequests, 1)
    assert.equal(proofMints, 1)

    context.mock.timers.tick(31_000)
    assert.equal((await instance.get('/api/game/7/details')).status, 200)
    assert.equal(challengeRequests, 2)
    assert.equal(proofMints, 2)

    context.mock.timers.tick(31_000)
    failChallenges = true
    await assert.rejects(instance.get('/api/game/7/details'), (error: AxiosError) => error.response?.status === 401)
    await assert.rejects(instance.get('/api/game/7/scoreboard'), (error: AxiosError) => error.response?.status === 401)
    assert.equal(challengeRequests, 3, 'the open circuit suppresses repeated protected polls')

    context.mock.timers.tick(2_000)
    await assert.rejects(instance.get('/api/game/7/details'), (error: AxiosError) => error.response?.status === 401)
    assert.equal(challengeRequests, 4, 'the bounded circuit permits an explicit retry after backoff')
  } finally {
    context.mock.timers.reset()
    resetEventVpnProofForTests()
  }
})
