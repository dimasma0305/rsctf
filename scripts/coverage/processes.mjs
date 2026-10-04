import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { createWriteStream } from 'node:fs'
import { processGroupRunning, stopChildTree } from '../../tests/load/process-control.mjs'

// One owner for the already-compiled test processes. Compilation, fixture
// provisioning and coverage reporting are deliberately outside this helper.
export class CoverageProcesses {
  children = new Set()
  results = []
  error = null
  stopping = null

  cancel(reason) {
    this.error ??= reason instanceof Error ? reason : new Error(String(reason))
    this.stopping ??= Promise.allSettled([...this.children].map((child) =>
      stopChildTree(child, { processGroup: true, graceMs: 1000 }))).then((results) => {
      const failures = results.filter((result) => result.status === 'rejected')
      if (failures.length > 0) {
        this.error = new AggregateError([this.error, ...failures.map((result) => result.reason)],
          'coverage process cleanup failed')
      }
    })
    return this.stopping
  }

  async run(binary, args, { label, logPath, env = process.env, cwd = process.cwd(),
    timeoutMs = 900000, maxOutputBytes = 8 * 1024 ** 2 } = {}) {
    if (this.error) throw this.error
    assert.ok(Number.isSafeInteger(timeoutMs) && timeoutMs > 0)
    assert.ok(Number.isSafeInteger(maxOutputBytes) && maxOutputBytes > 0)
    const log = createWriteStream(logPath, { flags: 'wx' })
    // Wait for exclusive log creation before starting a child. Never overwrite
    // prior evidence or leave an unobserved child after a filesystem failure.
    try {
      await new Promise((resolve, reject) => {
        log.once('open', resolve)
        log.once('error', reject)
      })
    } catch {
      await this.cancel(new Error(`${label}: log creation failed`))
      throw this.error
    }
    if (this.error) {
      await new Promise((resolve) => log.end(resolve))
      throw this.error
    }
    const started = performance.now()
    const child = spawn(binary, args, { cwd, env, detached: true, stdio: ['ignore', 'pipe', 'pipe'] })
    if (Number.isSafeInteger(child.pid)) this.children.add(child)
    let stdout = ''
    let bytes = 0
    let spawnError = null
    const collect = (chunk, output) => {
      const remaining = maxOutputBytes - bytes
      if (remaining > 0) log.write(chunk.subarray(0, remaining))
      bytes += chunk.length
      if (bytes > maxOutputBytes) void this.cancel(new Error(`${label}: output limit exceeded`))
      else if (output) stdout += chunk.toString('utf8')
    }
    const onLogError = () => { void this.cancel(new Error(`${label}: log write failed`)) }
    log.on('error', onLogError)
    child.stdout.on('data', (chunk) => collect(chunk, true))
    child.stderr.on('data', (chunk) => collect(chunk, false))
    const timer = setTimeout(() => { void this.cancel(new Error(`${label}: timed out`)) }, timeoutMs)
    let result
    try {
      result = await new Promise((resolve) => {
        // A spawn error is followed by close. Do not settle while descendants
        // or their output streams can still be alive.
        child.once('error', (error) => { spawnError = error.code || error.message })
        child.once('close', (code, signal) => resolve({ code, signal }))
      })
      clearTimeout(timer)
      if (spawnError || result.code !== 0) {
        await this.cancel(new Error(`${label}: process failed (${spawnError || result.code || result.signal})`))
      } else if (processGroupRunning(child.pid)) {
        await this.cancel(new Error(`${label}: test left a live descendant`))
      }
    } finally {
      clearTimeout(timer)
      if (this.stopping) await this.stopping
      await new Promise((resolve) => log.end(resolve))
      log.off('error', onLogError)
      this.children.delete(child)
      this.results.push({ label, logPath, pid: child.pid, ...result, spawnError,
        elapsedSeconds: (performance.now() - started) / 1000, outputBytes: bytes })
    }
    if (this.error) throw this.error
    return stdout
  }
}
