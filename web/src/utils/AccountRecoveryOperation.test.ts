import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import test from 'node:test'

test('account recovery owns one reload-recoverable mail intent and one native submit path', () => {
  const recovery = readFileSync(resolve(process.cwd(), 'src/pages/account/Recovery.tsx'), 'utf8')

  assert.match(recovery, /sessionStorage\.setItem\(RECOVERY_OPERATION_KEY/)
  assert.match(recovery, /operationId: operation\.operationId/)
  assert.match(recovery, /if \(inFlight\.current\) return/)
  assert.ok(
    recovery.indexOf('inFlight.current = true') < recovery.indexOf('await getToken()'),
    'the synchronous owner must be claimed before captcha awaits',
  )
  assert.match(recovery, /type="submit"/)
  assert.doesNotMatch(recovery, /onClick=\{onRecovery\}/)
})
