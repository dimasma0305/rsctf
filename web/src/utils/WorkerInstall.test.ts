import assert from 'node:assert/strict'
import test from 'node:test'
import {
  workerInstallCommand,
  workerInstallCommandsForOrigin,
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

test('local HTTP development renders without generating an insecure command', () => {
  for (const origin of ['http://localhost:63000', 'http://127.0.0.1:63000', 'http://[::1]:63000']) {
    assert.equal(workerInstallCommandsForOrigin(origin, true), null)
    assert.throws(() => workerInstallCommandsForOrigin(origin, false), /requires one exact HTTPS origin/)
  }
})

test('development allowance cannot suppress non-local or malformed origin failures', () => {
  for (const origin of ['http://tcp.1pc.tf', 'http://localhost:63000/path', 'https://user@tcp.1pc.tf']) {
    assert.throws(() => workerInstallCommandsForOrigin(origin, true))
  }
})

test('HTTPS origins keep the same verified commands in development', () => {
  assert.deepEqual(workerInstallCommandsForOrigin('https://tcp.1pc.tf', true), {
    linux: workerInstallCommand('https://tcp.1pc.tf'),
    windows: workerWindowsInstallCommand('https://tcp.1pc.tf'),
    linuxUninstall: workerUninstallCommand('https://tcp.1pc.tf'),
    windowsUninstall: workerWindowsUninstallCommand('https://tcp.1pc.tf'),
  })
})
