import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import test from 'node:test'

const pagePath = resolve('src/pages/admin/repo-bindings.tsx')

test('repo binding activity polling is disabled for every idle page', async () => {
  const source = await readFile(pagePath, 'utf8')
  assert.match(source, /collection\.items\.some\(\(binding\) => binding\.currentActivity\)\s*\?\s*3000\s*:\s*0/)
  assert.doesNotMatch(source, /refreshInterval:\s*3000/)
})

test('repo binding pages use one paginated collection request instead of per-binding reads', async () => {
  const source = await readFile(pagePath, 'utf8')
  assert.match(source, /\['\/api\/admin\/repobindings', bindingQuery\]/)
  assert.doesNotMatch(source, /bindings\.map\([\s\S]*useSWR/)
})
