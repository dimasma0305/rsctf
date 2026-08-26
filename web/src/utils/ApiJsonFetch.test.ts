import axios, { AxiosError, AxiosHeaders, type AxiosRequestConfig, type AxiosResponse } from 'axios'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { createApiJsonFetch } from './ApiJsonFetch'
import { installEventVpnProof } from './EventVpnProof'

const jsonResult = (config: AxiosRequestConfig, status: number, data: unknown): AxiosResponse => ({
  config: { ...config, headers: AxiosHeaders.from(config.headers) },
  data,
  headers: {},
  status,
  statusText: String(status),
})

const header = (config: AxiosRequestConfig, name: string): string | undefined =>
  AxiosHeaders.from(config.headers).get(name)?.toString()

const adapterResult = (config: AxiosRequestConfig, status: number, data: unknown): AxiosResponse => {
  const response = jsonResult(config, status, data)
  if (status >= 200 && status < 300) return response
  throw new AxiosError(
    `Request failed with status code ${status}`,
    AxiosError.ERR_BAD_REQUEST,
    response.config,
    undefined,
    response
  )
}

test('VPN-required arena reads use one accepted proof across Jeopardy, A&D and KotH casing', async () => {
  const gameId = 17_001
  const acceptedProof = 'accepted-route-proof'
  const protectedPaths = [
    `/api/game/${gameId}/scoreboard`,
    `/api/Game/${gameId}/Ad/Scoreboard`,
    `/api/game/${gameId}/ad/koth/scoreboard`,
    `/api/Game/${gameId}/Ad/Koth/Scoreboard`,
  ]
  let proofMints = 0
  const instance = axios.create({
    adapter: async (config) => {
      const path = new URL(config.url ?? '', 'https://arena.test').pathname
      if (path === `/api/game/${gameId}/vpn/challenge`) {
        return adapterResult(config, 200, {
          challenge: 'vpn-challenge',
          proofUrl: 'https://arena.test/vpn-proof',
          proofHeader: 'x-rsctf-vpn-proof',
          expiresAtUtc: Date.now() + 60_000,
        })
      }
      if (path === '/vpn-proof') {
        proofMints += 1
        return adapterResult(config, 200, {
          proof: acceptedProof,
          proofHeader: 'x-rsctf-vpn-proof',
          expiresAtUtc: Date.now() + 30_000,
        })
      }
      const accepted = header(config, 'x-rsctf-vpn-proof') === acceptedProof
      return adapterResult(config, accepted ? 200 : 401, { path })
    },
  })
  installEventVpnProof(instance, 'https://arena.test')
  const fetchJson = createApiJsonFetch(instance)

  for (const path of protectedPaths) {
    const response = await fetchJson(path, { headers: { Accept: 'application/json' } })
    assert.equal(response.status, 200, path)
    assert.equal(response.ok, true, path)
    assert.deepEqual(await response.json(), { path })
    assert.deepEqual(Object.keys(response).sort(), ['json', 'ok', 'status'])
  }
  assert.equal(proofMints, 1)
})

test('arena adapter preserves 401 when a VPN proof is absent or rejected', async () => {
  for (const [gameId, proof, label] of [
    [17_002, null, 'absent'],
    [17_003, 'wrong-route-proof', 'wrong'],
  ] as const) {
    const instance = axios.create({
      adapter: async (config) => {
        const path = new URL(config.url ?? '', 'https://arena.test').pathname
        if (path === `/api/game/${gameId}/vpn/challenge`) {
          if (proof === null) return adapterResult(config, 401, {})
          return adapterResult(config, 200, {
            challenge: 'vpn-challenge',
            proofUrl: 'https://arena.test/vpn-proof',
            proofHeader: 'x-rsctf-vpn-proof',
            expiresAtUtc: Date.now() + 60_000,
          })
        }
        if (path === '/vpn-proof') {
          return adapterResult(config, 200, {
            proof,
            proofHeader: 'x-rsctf-vpn-proof',
            expiresAtUtc: Date.now() + 30_000,
          })
        }
        return adapterResult(config, 401, {})
      },
    })
    installEventVpnProof(instance, 'https://arena.test')

    for (const path of [
      `/api/game/${gameId}/scoreboard`,
      `/api/Game/${gameId}/Ad/Scoreboard`,
      `/api/Game/${gameId}/Ad/Koth/Scoreboard`,
    ]) {
      const response = await createApiJsonFetch(instance)(path)
      assert.equal(response.status, 401, `${label} ${path}`)
      assert.equal(response.ok, false, `${label} ${path}`)
    }
  }
})

test('arena adapter rejects cross-origin targets before Axios can attach credentials', async () => {
  let requests = 0
  const fetchJson = createApiJsonFetch(
    axios.create({
      adapter: async (config) => {
        requests += 1
        return adapterResult(config, 200, {})
      },
    })
  )

  for (const target of [
    'https://attacker.test/api/game/7/scoreboard',
    '//attacker.test/api/game/7',
    '/\\attacker.test/api/game/7',
  ]) {
    await assert.rejects(fetchJson(target), /same-origin absolute path/)
  }
  assert.equal(requests, 0)
})

test('live arena shadows only its JSON fetch with the configured proof-aware adapter', () => {
  const source = readFileSync('src/pages/games/[id]/Attack.tsx', 'utf8')
  assert.match(source, /import \{ apiJsonFetch as fetch \} from '@Utils\/ApiJsonFetch'/)
  assert.equal((source.match(/\bfetch\(/g) ?? []).length, 1)
})
