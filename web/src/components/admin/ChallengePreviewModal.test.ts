import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const preview = readFileSync('src/components/admin/ChallengePreviewModal.tsx', 'utf8')
const editor = readFileSync('src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx', 'utf8')
const modal = readFileSync('src/components/ChallengeModal.tsx', 'utf8')

test('admin preview uses the persisted attachment and real test-container lifecycle', () => {
  assert.doesNotMatch(preview, /localhost:2333|\/assets\/attachment\.zip|FakeContext/)
  assert.match(editor, /challenge\?\.attachment\?\.url/)
  assert.match(editor, /instanceId: challenge\?\.testContainer\?\.id/)
  assert.match(editor, /challenge\?\.testContainer\?\.entry/)
  assert.match(editor, /onCreateInstance=\{onToggleTestContainer\}/)
  assert.match(editor, /onDestroyInstance=\{onToggleTestContainer\}/)
})

test('admin preview selects the no-instance proxy without simulating extension', () => {
  assert.match(preview, /testInstance/)
  assert.match(modal, /test=\{testInstance\}/)
  assert.doesNotMatch(preview, /onExtend|add\(10, 'm'\)/)
})
