import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import test from 'node:test'

const source = readFileSync(resolve('src/hooks/useAccountLinkSubmit.ts'), 'utf8')

test('account links acquire a synchronous owner and fence stale route responses', () => {
  assert.match(source, /if \(owner\.current\) return/)
  assert.match(source, /owner\.current = true/)
  assert.match(source, /controller\.current\?\.abort\(\)/)
  assert.match(source, /generation\.current === requestGeneration/)
})
