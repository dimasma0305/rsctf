import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const source = readFileSync('src/components/admin/DivisionEditDrawer.tsx', 'utf8')

test('division edits send one dirty revisioned operation', () => {
  assert.match(source, /mutationOwner = useRef\(false\)/)
  assert.match(source, /operationId\.current \?\? crypto\.randomUUID\(\)/)
  assert.match(source, /expectedRevision: division\.revision/)
  assert.match(source, /if \(model\.name !== division\.name\) edit\.name/)
  assert.match(source, /if \(Object\.keys\(edit\)\.length === 2\)/)
})
