// Invented account responses. No request is forwarded to an application API.
import http from 'node:http'
import { existsSync, readFileSync, statSync } from 'node:fs'
import { extname, resolve, sep } from 'node:path'
import { createCompetitionFixture } from './competition-fixtures.mjs'
import { fixture as overview } from './overview-pages-fixtures.mjs'

export function accountAccessFixture() {
  const player = createCompetitionFixture()
  const profile = { ...player.profile, userName: 'Account Regression Player' }
  let state
  const gate = () => {
    let release
    const promise = new Promise((resolve) => {
      release = resolve
    })
    return { promise, release }
  }
  const releaseAll = () => {
    if (state) Object.values(state.gates).forEach((gate) => gate.release())
  }
  const reset = (options = {}) => {
    releaseAll()
    state = {
      options,
      authenticated: false,
      calls: [],
      unknown: [],
      blockedWrites: [],
      profiles: 0,
      gates: {
        initialProfile: gate(),
        freshProfile: gate(),
        login: gate(),
        config: gate()
      }
    }
    return state
  }
  reset()
  const response = async (url, method) => {
    const owner = state
    const path = url.pathname
    const call = { path, method, settled: false }
    owner.calls.push(call)
    let result
    if (path === '/api/config') {
      if (owner.options.holdConfig) await owner.gates.config.promise
      if (owner.options.configDelay)
        await new Promise((r) => setTimeout(r, owner.options.configDelay))
      result = owner.options.configError
        ? { status: 503, body: {} }
        : {
            body: {
              ...player.config,
              apiPublicKey: null,
              enableDiscordAuth: true,
              enableGoogleAuth: false,
              ...owner.options.config
            }
          }
    } else if (path === '/api/captcha') result = { body: { type: 'None' } }
    else if (path === '/api/account/profile') {
      result = owner.authenticated
        ? { body: profile }
        : { status: 401, body: { title: 'unauthorized', status: 401 } }
      const initial = owner.profiles++ === 0
      if (initial && owner.options.holdInitialProfile)
        await owner.gates.initialProfile.promise
      if (!initial && owner.options.holdFreshProfile)
        await owner.gates.freshProfile.promise
      const delay = initial
        ? owner.options.initialProfileDelay
        : owner.options.profileDelay
      if (delay) await new Promise((r) => setTimeout(r, delay))
    } else if (path === '/api/account/login' && method === 'POST') {
      if (owner.options.holdLogin) await owner.gates.login.promise
      if (owner.options.loginDelay)
        await new Promise((r) => setTimeout(r, owner.options.loginDelay))
      owner.authenticated = true
      result = { body: { status: 200 } }
    } else if (path === '/api/account/register' && method === 'POST') {
      const status = owner.options.registerStatus || 'LoggedIn'
      owner.authenticated = status === 'LoggedIn'
      result = { body: { status: 200, data: status } }
    } else if (path.startsWith('/api/oauth/'))
      result = { body: { audit: 'OAuth navigation intercepted' } }
    else if (path.startsWith('/hub'))
      result = { status: 503, body: { error: 'Read-only fixture' } }
    else if (!['GET', 'HEAD'].includes(method)) {
      owner.blockedWrites.push(path)
      result = { status: 405, body: {} }
    } else {
      result = player.fixture(url.pathname + url.search, method)
      if (result.status === 404)
        result = overview(
          url.pathname + url.search,
          method,
          'normal',
          owner.authenticated ? 'User' : 'Guest'
        )
      if (result.status === 404) owner.unknown.push(path)
    }
    call.status = result.status || 200
    call.settled = true
    return result
  }
  return {
    reset,
    response,
    release: (name) => state.gates[name].release(),
    releaseAll,
    get state() {
      return state
    },
    profile
  }
}

export async function serveAccountBuild(buildRoot) {
  const root = resolve(buildRoot)
  if (!existsSync(resolve(root, 'index.html')))
    throw new Error('Build the web client before running account regressions')
  const unhandled = []
  const types = {
    '.html': 'text/html',
    '.js': 'application/javascript',
    '.css': 'text/css',
    '.json': 'application/json',
    '.svg': 'image/svg+xml',
    '.png': 'image/png',
    '.woff': 'font/woff',
    '.woff2': 'font/woff2',
    '.wasm': 'application/wasm'
  }
  const server = http.createServer((req, res) => {
    const path = new URL(req.url, 'http://localhost').pathname
    if (
      !['GET', 'HEAD'].includes(req.method) ||
      path.startsWith('/api/') ||
      path.startsWith('/hub')
    ) {
      unhandled.push({ method: req.method, path })
      res.writeHead(503)
      res.end()
      return
    }
    try {
      let file = resolve(root, '.' + decodeURIComponent(path))
      if (file !== root && !file.startsWith(root + sep)) {
        res.writeHead(403)
        res.end()
        return
      }
      if (!existsSync(file) || !statSync(file).isFile())
        file = resolve(root, 'index.html')
      res.writeHead(200, {
        'Content-Type': types[extname(file)] || 'application/octet-stream'
      })
      res.end(req.method === 'HEAD' ? undefined : readFileSync(file))
    } catch {
      res.writeHead(400)
      res.end()
    }
  })
  await new Promise((accept, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', accept)
  })
  return {
    origin: `http://127.0.0.1:${server.address().port}`,
    unhandled,
    close: () =>
      new Promise((accept, reject) =>
        server.close((error) => (error ? reject(error) : accept()))
      )
  }
}
