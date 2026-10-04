import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

const artifacts = ['rsctf-worker-agent-linux-amd64.tar.gz', 'rsctf-worker-agent-linux-arm64.tar.gz',
  'rsctf-worker-agent-windows-amd64.zip', 'install-worker.sh', 'install-worker.ps1',
  'install.sh', 'rsctf-deployment-bundle.tar.gz']
const workflow = readFileSync(new URL('../../../.github/workflows/worker-agent-release.yml', import.meta.url), 'utf8')
const block = workflow.split('      - name: Verify release artifact attestations\n')[1]
assert.ok(block)
const script = block.match(/run: \|\n((?:(?: {10}.*)?\n)+)/)?.[1].replace(/^ {10}/gm, '')
assert.ok(script)

function verify(change) {
  const directory = mkdtempSync(join(tmpdir(), 'rsctf attestation test '))
  try {
    mkdirSync(join(directory, 'dist'))
    const executable = join(directory, 'gh')
    copyFileSync(new URL('./fixtures/attestation-gh.mjs', import.meta.url), executable)
    chmodSync(executable, 0o700)
    for (const artifact of artifacts) writeFileSync(join(directory, 'dist', artifact), `fixture:${artifact}`)
    writeFileSync(join(directory, 'dist', 'rsctf-worker-agent-attestation.json'), 'signed-bundle-fixture')
    change?.(directory)
    const events = join(directory, 'events.jsonl')
    const result = spawnSync('bash', ['-c', script], { cwd: directory, encoding: 'utf8', timeout: 15000,
      env: { PATH: `${directory}:${process.env.PATH}`, ATTESTATION_TEST_EVENTS: events,
        GITHUB_REPOSITORY: 'example/rsctf', GITHUB_REF: 'refs/tags/v0.1.999' } })
    assert.equal(result.error, undefined)
    const recorded = readFileSync(events, 'utf8').trim().split('\n').map((line) => JSON.parse(line))
    const running = new Set()
    let peak = 0
    for (const event of recorded) {
      if (event.phase === 'start') {
        assert.ok(!running.has(event.pid))
        running.add(event.pid)
        peak = Math.max(peak, running.size)
      } else assert.ok(running.delete(event.pid))
    }
    assert.equal(running.size, 0, 'the step must settle every verification process')
    for (const phase of ['start', 'end']) assert.deepEqual(
      recorded.filter((event) => event.phase === phase).map((event) => event.artifact).sort(), [...artifacts].sort())
    assert.ok(peak >= 2 && peak <= 7, `verification concurrency must be bounded, observed ${peak}`)
    return result.status
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
}

test('the actual release step verifies every subject and trust constraint with bounded parallelism', () => {
  assert.equal(verify(), 0)
})

test('a missing artifact fails the whole parallel verification step', () => {
  assert.notEqual(verify((directory) => rmSync(join(directory, 'dist', 'install.sh'))), 0)
})

test('an altered artifact or bundle fails without skipping other subjects', () => {
  for (const artifact of ['rsctf-worker-agent-linux-arm64.tar.gz', 'rsctf-worker-agent-attestation.json']) {
    assert.notEqual(verify((directory) => writeFileSync(join(directory, 'dist', artifact), 'altered')), 0)
  }
})
