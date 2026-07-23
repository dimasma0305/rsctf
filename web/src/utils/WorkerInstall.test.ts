import assert from 'node:assert/strict'
import test from 'node:test'
import {
  workerInstallCommand,
  workerUninstallCommand,
  workerWindowsInstallCommand,
  workerWindowsUninstallCommand,
} from './WorkerInstall'

test('worker install command contains only the public HTTPS origin', () => {
  assert.equal(
    workerInstallCommand('https://tcp.1pc.tf'),
    `(t=$(mktemp) || exit 1; trap 'rm -f "$t"' 0 HUP INT TERM; wget -q -T 30 -O "$t" https://tcp.1pc.tf/install/worker && sh "$t" --server-url https://tcp.1pc.tf)`
  )
})

test('Windows worker command contains only the public HTTPS origin', () => {
  assert.equal(
    workerWindowsInstallCommand('https://tcp.1pc.tf'),
    '& ([scriptblock]::Create((Invoke-RestMethod https://tcp.1pc.tf/install/worker.ps1))) -ServerUrl https://tcp.1pc.tf'
  )
})

test('worker uninstall commands contain only the public HTTPS origin', () => {
  assert.equal(
    workerUninstallCommand('https://tcp.1pc.tf'),
    `(t=$(mktemp) || exit 1; trap 'rm -f "$t"' 0 HUP INT TERM; wget -q -T 30 -O "$t" https://tcp.1pc.tf/install/worker && sh "$t" --uninstall)`
  )
  assert.equal(
    workerWindowsUninstallCommand('https://tcp.1pc.tf'),
    '& ([scriptblock]::Create((Invoke-RestMethod https://tcp.1pc.tf/install/worker.ps1))) -Uninstall'
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
    assert.throws(() => workerWindowsInstallCommand(origin))
    assert.throws(() => workerUninstallCommand(origin))
    assert.throws(() => workerWindowsUninstallCommand(origin))
  }
})
