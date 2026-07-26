import { execFileSync, spawn } from 'node:child_process'
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds))

function which(command) {
  try {
    return execFileSync('which', [command], { encoding: 'utf8' }).trim()
  } catch {
    return ''
  }
}

export function resolveChromium() {
  const candidates = [
    process.env.CHROME_BIN,
    '/usr/bin/google-chrome',
    '/usr/bin/google-chrome-stable',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
    which('google-chrome'),
    which('google-chrome-stable'),
    which('chromium'),
    which('chromium-browser'),
  ].filter(Boolean)
  const executable = candidates.find((candidate) => existsSync(candidate))
  if (!executable) {
    throw new Error('Chromium was not found; set CHROME_BIN to a Chrome/Chromium executable')
  }
  return executable
}

export class CdpClient {
  constructor(url) {
    this.url = url
    this.nextId = 1
    this.pending = new Map()
    this.listeners = new Map()
    this.socket = undefined
  }

  async connect() {
    this.socket = new WebSocket(this.url)
    await new Promise((resolve, reject) => {
      this.socket.addEventListener('open', resolve, { once: true })
      this.socket.addEventListener('error', reject, { once: true })
    })
    this.socket.addEventListener('message', ({ data }) => {
      const message = JSON.parse(data)
      if (message.id) {
        const pending = this.pending.get(message.id)
        if (!pending) return
        this.pending.delete(message.id)
        if (message.error) pending.reject(new Error(`${pending.method}: ${message.error.message}`))
        else pending.resolve(message.result)
        return
      }
      for (const listener of this.listeners.get(message.method) ?? []) listener(message.params)
    })
  }

  on(method, listener) {
    const listeners = this.listeners.get(method) ?? []
    listeners.push(listener)
    this.listeners.set(method, listeners)
  }

  send(method, params = {}) {
    if (!this.socket) throw new Error('CDP client is not connected')
    const id = this.nextId++
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject, method })
      this.socket.send(JSON.stringify({ id, method, params }))
    })
  }

  close() {
    this.socket?.close()
  }
}

export async function launchBrowser() {
  const executable = resolveChromium()
  const profile = mkdtempSync(join(tmpdir(), 'rsctf-visual-chromium-'))
  const errors = []
  const child = spawn(
    executable,
    [
      '--headless=new',
      '--no-sandbox',
      '--disable-gpu',
      '--disable-dev-shm-usage',
      '--disable-background-networking',
      '--disable-component-update',
      '--disable-default-apps',
      '--disable-extensions',
      '--disable-sync',
      '--metrics-recording-only',
      '--no-first-run',
      '--remote-debugging-address=127.0.0.1',
      '--remote-debugging-port=0',
      `--user-data-dir=${profile}`,
      'about:blank',
    ],
    {
      detached: process.platform !== 'win32',
      stdio: ['ignore', 'ignore', 'pipe'],
    }
  )
  child.stderr.setEncoding('utf8')
  child.stderr.on('data', (chunk) => {
    errors.push(chunk)
    if (errors.length > 20) errors.shift()
  })

  const childExited = () => child.exitCode !== null || child.signalCode !== null
  const signalBrowser = (signal) => {
    if (childExited()) return
    try {
      if (process.platform === 'win32') child.kill(signal)
      else process.kill(-child.pid, signal)
    } catch {
      try {
        child.kill(signal)
      } catch {
        // The browser exited between the state check and signal.
      }
    }
  }
  const waitForExit = (timeoutMilliseconds) => {
    if (childExited()) return Promise.resolve(true)
    return new Promise((resolve) => {
      let timeout
      const onExit = () => {
        clearTimeout(timeout)
        resolve(true)
      }
      child.once('exit', onExit)
      timeout = setTimeout(() => {
        child.off('exit', onExit)
        resolve(childExited())
      }, timeoutMilliseconds)
    })
  }
  const stopBrowser = async () => {
    signalBrowser('SIGTERM')
    if (!(await waitForExit(2_000))) {
      signalBrowser('SIGKILL')
      await waitForExit(2_000)
    }
  }
  const removeProfile = () => rmSync(profile, { recursive: true, force: true })

  try {
    const activePortFile = join(profile, 'DevToolsActivePort')
    const deadline = Date.now() + 20_000
    while (!existsSync(activePortFile) && Date.now() < deadline) {
      if (childExited()) break
      await sleep(100)
    }
    if (!existsSync(activePortFile)) {
      throw new Error(`Chromium did not start: ${errors.join('').slice(-2_000)}`)
    }

    const [port] = readFileSync(activePortFile, 'utf8').trim().split(/\r?\n/)
    const base = `http://127.0.0.1:${port}`
    const page = await fetch(`${base}/json/new?about:blank`, {
      method: 'PUT',
    }).then(async (response) => {
      if (!response.ok) throw new Error(`could not create Chromium target: ${response.status}`)
      return response.json()
    })
    const cdp = new CdpClient(page.webSocketDebuggerUrl)
    await cdp.connect()

    let closed = false
    const close = async () => {
      if (closed) return
      closed = true
      cdp.close()
      try {
        await fetch(`${base}/json/close/${page.id}`)
      } catch {
        // The browser may already be gone after a failed navigation.
      } finally {
        await stopBrowser()
        removeProfile()
      }
    }

    return { cdp, close, executable }
  } catch (error) {
    await stopBrowser()
    removeProfile()
    throw error
  }
}
