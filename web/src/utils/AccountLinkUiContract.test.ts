import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import test from 'node:test'

const load = (relative: string) => readFileSync(resolve('src/utils', relative), 'utf8')

test('account link pages synchronously own one request and bind it to the route token', () => {
  for (const source of [load('../pages/account/Confirm.tsx'), load('../pages/account/Verify.tsx')]) {
    assert.match(source, /useRef\(new RetryableMutationOwner\(\)\)/)
    assert.match(source, /owner\.current\.claim\(JSON\.stringify\(\{ token, email \}\)\)/)
    assert.match(source, /if \(!lease\) return/)
    assert.match(source, /\{ signal: lease\.signal \}/)
    assert.match(source, /owner\.current\.settle\(lease, true\)/)
    assert.match(source, /\[token, email\]/)
    assert.match(source, /return \(\) => owner\.current\.cancel\(\)/)
  }
})
