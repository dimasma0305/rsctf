import axios, { AxiosError, AxiosHeaders, type AxiosResponse, type InternalAxiosRequestConfig } from 'axios'
import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { installTestDom } from '../test/installDom'
import {
  eventVpnFetch,
  eventVpnProofTestApi,
  installEventVpnProof,
  isEventVpnAccessError,
  protectedEventGameId,
} from './EventVpnProof'

const response = <T>(config: InternalAxiosRequestConfig, status: number, data: T): AxiosResponse<T> => ({
  config,
  data,
  headers: new AxiosHeaders({ 'content-type': 'application/json' }),
  status,
  statusText: String(status),
})

const failure = (config: InternalAxiosRequestConfig, status: number) =>
  new AxiosError(
    `request failed with ${status}`,
    AxiosError.ERR_BAD_RESPONSE,
    config,
    undefined,
    response(config, status, { status, title: 'failed' })
  )

test('proof-aware Axios requests coalesce minting and renew an expired proof', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games/19/challenges' })
  const restoreDom = installTestDom(browser)
  const now = 2_000_100_000_000
  context.mock.timers.enable({ apis: ['Date'], now: new Date(now) })
  const client = axios.create()
  let challenges = 0
  let proofs = 0
  let protectedCalls = 0

  client.defaults.adapter = async (config) => {
    const url = config.url ?? ''
    if (url === '/api/game/19/vpn/challenge') {
      challenges += 1
      return response(config, 200, {
        challenge: `challenge-${challenges}`,
        proofHeader: 'x-rsctf-vpn-proof',
        proofUrl: 'https://event-vpn.test/proof',
        expiresAtUtc: Date.now() + 60_000,
      })
    }
    if (url === 'https://event-vpn.test/proof') {
      proofs += 1
      return response(config, 200, {
        proof: `proof-${proofs}`,
        proofHeader: 'x-rsctf-vpn-proof',
        expiresAtUtc: Date.now() + 2_000,
      })
    }
    if (url === '/api/game/19/challenges/7') {
      protectedCalls += 1
      const proof = AxiosHeaders.from(config.headers).get('x-rsctf-vpn-proof')
      if (!proof) throw failure(config, 401)
      return response(config, 200, { id: 7, proof })
    }
    throw new Error(`unexpected request ${url}`)
  }

  try {
    eventVpnProofTestApi.reset()
    installEventVpnProof(client)
    const [first, second] = await Promise.all([
      client.get('/api/game/19/challenges/7'),
      client.get('/api/game/19/challenges/7'),
    ])
    assert.equal(first.data.proof, 'proof-1')
    assert.equal(second.data.proof, 'proof-1')
    assert.equal(challenges, 1)
    assert.equal(proofs, 1)
    assert.equal(eventVpnProofTestApi.cachedGames(), 1)

    context.mock.timers.tick(2_100)
    const renewed = await client.get('/api/game/19/challenges/7')
    assert.equal(renewed.data.proof, 'proof-2')
    assert.equal(challenges, 2)
    assert.equal(proofs, 2)
    assert.equal(protectedCalls, 6)
  } finally {
    eventVpnProofTestApi.reset()
    context.mock.timers.reset()
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('VPN disconnects do not become login expiry and failed minting is circuit-bounded', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/19/challenges' })
  const restoreDom = installTestDom(browser)
  const client = axios.create()
  let challenges = 0
  let proofs = 0

  client.defaults.adapter = async (config) => {
    const url = config.url ?? ''
    if (url === '/api/game/19/challenges/7') throw failure(config, 401)
    if (url === '/api/game/19/vpn/challenge') {
      challenges += 1
      return response(config, 200, {
        challenge: 'challenge',
        proofHeader: 'x-rsctf-vpn-proof',
        proofUrl: 'https://event-vpn.test/proof',
        expiresAtUtc: Date.now() + 60_000,
      })
    }
    if (url === 'https://event-vpn.test/proof') {
      proofs += 1
      throw failure(config, 403)
    }
    throw new Error(`unexpected request ${url}`)
  }

  try {
    eventVpnProofTestApi.reset()
    installEventVpnProof(client)
    for (let attempt = 0; attempt < 2; attempt += 1) {
      await assert.rejects(client.get('/api/game/19/challenges/7'), (error: unknown) => {
        assert.equal(isEventVpnAccessError(error), true)
        if (!isEventVpnAccessError(error)) return false
        assert.equal(error.kind, 'disconnected')
        assert.equal('status' in error, false)
        return true
      })
    }
    assert.equal(challenges, 1)
    assert.equal(proofs, 1)
    assert.equal(eventVpnProofTestApi.failedGames(), 1)
  } finally {
    eventVpnProofTestApi.reset()
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('challenge-endpoint 401 remains session expiry while successful monitor reads bypass minting', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/19/challenges' })
  const restoreDom = installTestDom(browser)
  const client = axios.create()
  let challenges = 0

  client.defaults.adapter = async (config) => {
    const url = config.url ?? ''
    if (url === '/api/game/19/cheatinfo') return response(config, 200, { events: [] })
    if (url === '/api/game/19/challenges/7') throw failure(config, 401)
    if (url === '/api/game/19/vpn/challenge') {
      challenges += 1
      throw failure(config, 401)
    }
    throw new Error(`unexpected request ${url}`)
  }

  try {
    eventVpnProofTestApi.reset()
    installEventVpnProof(client)
    assert.deepEqual((await client.get('/api/game/19/cheatinfo')).data, { events: [] })
    assert.equal(challenges, 0)
    await assert.rejects(client.get('/api/game/19/challenges/7'), (error: unknown) => {
      assert.equal(isEventVpnAccessError(error), false)
      assert.equal((error as AxiosError).response?.status, 401)
      return true
    })
    assert.equal(challenges, 1)
    assert.equal(eventVpnProofTestApi.failedGames(), 0)
  } finally {
    eventVpnProofTestApi.reset()
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('native fetch uses the same cached proof exchange and ignores foreign origins', async () => {
  const browser = new Window({ url: 'https://rsctf.test/games/19/attack' })
  const restoreDom = installTestDom(browser)
  const previousFetch = Object.getOwnPropertyDescriptor(globalThis, 'fetch')
  const client = axios.create()
  let protectedCalls = 0
  let foreignCalls = 0

  client.defaults.adapter = async (config) => {
    if (config.url === '/api/game/19/vpn/challenge') {
      return response(config, 200, {
        challenge: 'challenge',
        proofHeader: 'x-rsctf-vpn-proof',
        proofUrl: 'https://event-vpn.test/proof',
        expiresAtUtc: Date.now() + 60_000,
      })
    }
    if (config.url === 'https://event-vpn.test/proof') {
      return response(config, 200, {
        proof: 'native-proof',
        proofHeader: 'x-rsctf-vpn-proof',
        expiresAtUtc: Date.now() + 60_000,
      })
    }
    throw new Error(`unexpected request ${config.url}`)
  }
  Object.defineProperty(globalThis, 'fetch', {
    configurable: true,
    writable: true,
    value: async (request: Request) => {
      if (request.url.startsWith('https://foreign.test/')) {
        foreignCalls += 1
        return new Response('{}', { status: 200 })
      }
      protectedCalls += 1
      return request.headers.get('x-rsctf-vpn-proof') === 'native-proof'
        ? new Response('{"ok":true}', { status: 200 })
        : new Response('{"status":401}', { status: 401 })
    },
  })

  try {
    eventVpnProofTestApi.reset()
    installEventVpnProof(client)
    assert.equal(protectedEventGameId('https://foreign.test/api/game/19/challenges/7'), null)
    assert.equal((await eventVpnFetch('/api/game/19/scoreboard')).status, 200)
    assert.equal(protectedCalls, 2)
    assert.equal((await eventVpnFetch('https://foreign.test/api/game/19/challenges/7')).status, 200)
    assert.equal(foreignCalls, 1)
  } finally {
    eventVpnProofTestApi.reset()
    if (previousFetch) Object.defineProperty(globalThis, 'fetch', previousFetch)
    else delete (globalThis as typeof globalThis & { fetch?: typeof fetch }).fetch
    await browser.happyDOM.close()
    restoreDom()
  }
})
