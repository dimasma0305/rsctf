import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { mkdir, readFile, writeFile, rm } from 'node:fs/promises'
import path from 'node:path'

const execute = promisify(execFile)
export async function blobFixture(directory, id, roots, backend, settings = process.env) {
  assert.ok(['local', 's3'].includes(backend))
  assert.match(id, /^[a-f0-9]{16}$/)
  const prefix = root => {
    const index = roots.indexOf(root)
    assert.ok(index >= 0, 'only owned storage roots are allowed')
    return `restore-drill-${id}-${index}`
  }
  const key = hash => {
    assert.match(hash, /^[a-f0-9]{64}$/)
    return `${hash.slice(0, 2)}/${hash.slice(2, 4)}/${hash}`
  }
  const localPath = (root, hash) => { prefix(root); return path.join(root, key(hash)) }
  const config = {}
  let credentialFile
  if (backend === 's3') {
    const endpoint = new URL(settings.RSCTF_S3_ENDPOINT)
    assert.equal(endpoint.hostname, '127.0.0.1', 'restore S3 fixture must use loopback')
    assert.equal(endpoint.protocol, 'http:')
    assert.equal(endpoint.pathname, '/')
    assert.ok(!endpoint.username && !endpoint.password && !endpoint.search && !endpoint.hash)
    for (const name of ['RSCTF_S3_BUCKET', 'RSCTF_S3_ACCESS_KEY', 'RSCTF_S3_SECRET_KEY']) {
      const value = settings[name]
      assert.ok(value && !/[\r\n]/.test(value), `${name} is required`)
      config[name] = value
    }
    assert.match(config.RSCTF_S3_BUCKET, /^[a-z0-9][a-z0-9.-]+$/)
    config.RSCTF_S3_ENDPOINT = endpoint.origin
    config.RSCTF_S3_REGION = 'us-east-1'
    credentialFile = path.join(directory, 'curl-credentials')
    await writeFile(credentialFile,
      'user = ' + JSON.stringify(config.RSCTF_S3_ACCESS_KEY + ':' + config.RSCTF_S3_SECRET_KEY) + '\n',
      { mode: 0o600 })
  }
  async function request(root, object, method, bytes) {
    const input = path.join(directory, 'object-input')
    if (bytes) await writeFile(input, bytes, { mode: 0o600 })
    let output
    try {
      output = (await execute('curl', [
        '--silent', '--show-error', '--max-time', '10', '--config', credentialFile,
        '--aws-sigv4', 'aws:amz:us-east-1:s3', '--request', method,
        '--write-out', '%{http_code}', ...(bytes ? ['--data-binary', '@' + input] : []),
        `${config.RSCTF_S3_ENDPOINT}/${config.RSCTF_S3_BUCKET}/${prefix(root)}/${object}`,
      ], { encoding: 'buffer', timeout: 15_000, maxBuffer: 2 * 1024 * 1024 })).stdout
    } catch { throw new Error('disposable S3 fixture request failed') }
    const status = Number(output.subarray(-3).toString())
    if (status === 404) throw Object.assign(new Error('restored object is missing'), { code: 'ENOENT' })
    assert.ok(status >= 200 && status < 300, `S3 fixture HTTP ${status}`)
    return output.subarray(0, -3)
  }
  const seen = new Set()
  return {
    config(root) { return { ...config, RSCTF_STORAGE_BACKEND: backend,
      ...(backend === 's3' ? { RSCTF_S3_PREFIX: prefix(root) } : {}) } },
    async read(root, hash) {
      return backend === 's3' ? request(root, key(hash), 'GET') : readFile(localPath(root, hash))
    },
    async write(root, hash, bytes) {
      seen.add(hash)
      if (backend === 's3') return request(root, key(hash), 'PUT', bytes)
      const target = localPath(root, hash)
      await mkdir(path.dirname(target), { recursive: true })
      await writeFile(target, bytes)
    },
    async remove(root, hash) {
      if (backend === 's3') return request(root, key(hash), 'DELETE')
      await rm(localPath(root, hash))
    },
    async cleanup() {
      if (backend !== 's3') return
      for (const root of roots) {
        for (const hash of seen) await request(root, key(hash), 'DELETE')
        await request(root, '.rsctf-health', 'DELETE')
      }
    },
  }
}
