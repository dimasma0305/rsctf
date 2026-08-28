import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const page = readFileSync('src/pages/admin/games/[id]/AdOps.tsx', 'utf8')
const panel = readFileSync('src/components/admin/KothOpsPanel.tsx', 'utf8')

test('KotH operations preserve semantic heading order without changing visual scale', () => {
  assert.match(page, /<Title order=\{2\} size="h4">/)
  assert.match(panel, /<Title order=\{3\} size="h5">/)
})

test('disabled hills stay readable and expose a textual state', () => {
  assert.doesNotMatch(panel, /opacity: hill\.isEnabled/)
  assert.match(panel, /!hill\.isEnabled[\s\S]*common\.content\.disabled/)
})
