// Fixed-rate component comparison; HTTP and durable-reference gates are separate.
import { execFile, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import { readFile, writeFile, readdir } from 'node:fs/promises'
import { createHash, randomUUID } from 'node:crypto'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { setTimeout as pause } from 'node:timers/promises'

const execute = promisify(execFile)
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const { stdout: common } = await execute('git', ['rev-parse', '--path-format=absolute', '--git-common-dir'], { cwd: root })
if (process.env.RSCTF_STORAGE_AUDIT_LOCKED !== '1') {
  const child = spawn('flock', [path.join(common.trim(), 'rsctf-build.lock'), process.execPath, fileURLToPath(import.meta.url)], {
    cwd: root, stdio: 'inherit', env: { ...process.env, RSCTF_STORAGE_AUDIT_LOCKED: '1' },
  })
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve) })
  process.exit(code ?? 1)
}
const binary = process.env.STORAGE_AUDIT_BIN ?? path.join(common.trim(), 'rsctf-target/release/rsctf-storage-audit')
const imageName = process.env.STORAGE_AUDIT_S3_IMAGE ?? 'rustfs/rustfs:latest'
const { stdout: imageId } = await execute('docker', ['image', 'inspect', '--format', '{{.Id}}', imageName])
const bucket = 'rsctf-storage-audit'
const access = 'architecture-audit'
const secret = randomUUID()
let endpoint = new URL('http://127.0.0.1:1')
let ownedContainer = null
async function stopStore() {
  if (!ownedContainer) return
  await execute('docker', ['stop', '--time', '5', ownedContainer])
  await execute('docker', ['rm', '--volumes', ownedContainer])
  ownedContainer = null
}
async function freshStore() {
  await stopStore()
  const name = `rsctf-storage-audit-${randomUUID()}`
  await execute('docker', ['run', '-d', '--name', name, '--label', 'rsctf.test=architecture-storage-audit',
    '--memory', '1g', '--cpus', '1', '-p', '127.0.0.1::9000',
    '-e', `RUSTFS_ACCESS_KEY=${access}`, '-e', `RUSTFS_SECRET_KEY=${secret}`, imageId.trim()])
  ownedContainer = name
  const { stdout } = await execute('docker', ['port', name, '9000/tcp'])
  endpoint = new URL(`http://${stdout.trim()}`)
  let error
  for (let attempt = 0; attempt < 60; attempt += 1) {
    try {
      await execute('curl', ['--silent', '--show-error', '--fail', '--max-time', '2',
        '--aws-sigv4', 'aws:amz:us-east-1:s3', '--user', `${access}:${secret}`, '-X', 'PUT', `${endpoint}${bucket}`])
      return
    } catch (failure) { error = failure; await pause(100) }
  }
  throw error
}
let accounting = null
const proxy = http.createServer((request, response) => {
  const url = new URL(request.url, 'http://local')
  const kind = url.searchParams.has('uploads') ? `${request.method}:initiate`
    : url.searchParams.has('partNumber') ? `${request.method}:part`
      : url.searchParams.has('uploadId') ? `${request.method}:multipart` : request.method
  // Capture the current window, so response bytes stay attached to the request.
  const current = accounting
  if (current) current.requests[kind] = (current.requests[kind] ?? 0) + 1
  request.on('data', chunk => { if (current) current.uploadBytes += chunk.length })
  const upstream = http.request({ hostname: endpoint.hostname, port: endpoint.port,
    path: request.url, method: request.method, headers: request.headers }, incoming => {
    if (current) {
      current.responses[incoming.statusCode] = (current.responses[incoming.statusCode] ?? 0) + 1
      if (incoming.statusCode >= 400) current.errorResponses += 1
    }
    response.writeHead(incoming.statusCode, incoming.headers)
    incoming.on('data', chunk => { if (current) current.downloadBytes += chunk.length })
    incoming.pipe(response)
  })
  upstream.on('error', error => response.destroy(error))
  request.on('error', error => upstream.destroy(error))
  request.pipe(upstream)
})
await new Promise(resolve => proxy.listen(0, '127.0.0.1', resolve))
const samples = []
try {
  for (const backend of ['local', 's3']) {
    for (const scenario of ['cold', 'warm']) {
      for (const order of [['buffered', 'file'], ['file', 'buffered'], ['buffered', 'file']]) {
        for (const mode of order) {
          // Isolate each sample's server state and reclaim its anonymous data
          // volume. Deleted objects can otherwise await the store's own GC.
          if (backend === 's3') await freshStore()
          let network = null
          let result = null
          let pending = ''
          const child = spawn(binary, [mode, backend, scenario], { cwd: root,
            env: { ...process.env, RSCTF_S3_ENDPOINT: `http://127.0.0.1:${proxy.address().port}`,
              RSCTF_S3_BUCKET: bucket, RSCTF_S3_ACCESS_KEY: access,
              RSCTF_S3_SECRET_KEY: secret, RSCTF_S3_REGION: 'us-east-1',
              RSCTF_S3_PREFIX: `storage-audit/${randomUUID()}` }, stdio: ['ignore', 'pipe', 'inherit'],
          })
          child.stdout.setEncoding('utf8')
          child.stdout.on('data', chunk => {
            pending += chunk
            let end
            while ((end = pending.indexOf('\n')) >= 0) {
              const event = JSON.parse(pending.slice(0, end))
              pending = pending.slice(end + 1)
              if (event.event === 'start') {
                network = { requests: {}, responses: {}, uploadBytes: 0, downloadBytes: 0, errorResponses: 0 }
                accounting = network
              } else if (event.event === 'stop') accounting = null
              else if (event.event === 'result') result = event
            }
          })
          const timer = setTimeout(() => child.kill('SIGTERM'), 120_000)
          let code
          try { code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve) }) }
          finally { clearTimeout(timer); accounting = null }
          if (code !== 0 || !result || result.integrity_errors !== 0) throw new Error(`Failed ${backend}/${scenario}/${mode}: ${code}`)
          samples.push({ ...result, network: backend === 's3' ? network : null })
          if (backend === 's3') await stopStore()
          console.error(`${backend}/${scenario}/${mode}: p95 ${result.service.p95_ms.toFixed(1)} ms, CPU ${result.cpu_percent.toFixed(1)}%, RSS ${result.peak_rss_mib.toFixed(1)} MiB`)
        }
      }
    }
  }
} finally {
  await stopStore()
  proxy.closeAllConnections()
  await new Promise(resolve => proxy.close(resolve))
}
const { stdout: sourceSha } = await execute('git', ['rev-parse', 'HEAD'], { cwd: root })
async function hashes(directory) {
  const result = {}
  for (const entry of await readdir(path.join(root, directory), { withFileTypes: true })) {
    const name = `${directory}/${entry.name}`
    if (entry.isDirectory()) Object.assign(result, await hashes(name))
    else if (entry.isFile()) result[name] = createHash('sha256').update(await readFile(path.join(root, name))).digest('hex')
  }
  return result
}
const result = { date: new Date().toISOString(), sourceSha: sourceSha.trim(), kernel: os.release(), cpu: os.cpus()[0]?.model,
  s3Image: imageId.trim(), s3MemoryLimit: '1 GiB', s3CpuLimit: 1,
  binarySha256: createHash('sha256').update(await readFile(binary)).digest('hex'),
  sourceHashes: { ...await hashes('src/storage'), ...await hashes('tests/load/storage-audit') },
  profile: 'release opt-level=3 lto=thin codegen-units=1', workers: 2,
  fixture: 'Sparse files, unique 16-byte headers, otherwise zeros; uncompressed transfers. Size is recorded per sample.',
  notes: ['Same production store implementations; error-only shim', 'Sparse source files; cold means absent destination key',
    'Local candidate also fsyncs file/directories; buffered baseline does not',
    'S3 request counts include expected cold HEAD 404s; client errors fail the run',
    'CPU/RSS are client-process measurements; S3/proxy CPU and disk are excluded',
    'RSS is process high-water, including fixture setup; timing/IO exclude setup and readback',
    'Each uploaded object is fully streamed, hashed, and deleted after timing'], samples }
if (process.env.SUMMARY_JSON) await writeFile(process.env.SUMMARY_JSON, `${JSON.stringify(result, null, 2)}\n`)
else console.log(JSON.stringify(result, null, 2))
