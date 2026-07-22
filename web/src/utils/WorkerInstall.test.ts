import assert from 'node:assert/strict'
import test from 'node:test'
import { workerInstallCommand } from './WorkerInstall'

test('worker install command contains only the public HTTPS origin', () => {
  assert.equal(
    workerInstallCommand('https://tcp.1pc.tf'),
    'curl -fsSL https://tcp.1pc.tf/install/worker | sudo bash -s -- --server-url https://tcp.1pc.tf'
  )
})

test('worker install command rejects credentials, paths, insecure origins, and shell syntax', () => {
  for (const origin of [
    'http://tcp.1pc.tf',
    'https://user@tcp.1pc.tf',
    'https://tcp.1pc.tf/path',
    'https://tcp.1pc.tf;touch-pwned',
  ]) {
    assert.throws(() => workerInstallCommand(origin))
  }
})
