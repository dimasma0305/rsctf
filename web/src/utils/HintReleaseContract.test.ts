import assert from 'node:assert/strict'
import fs from 'node:fs'
import test from 'node:test'

const api = fs.readFileSync('src/Api.ts', 'utf8')
const editor = fs.readFileSync(
  'src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx',
  'utf8'
)
const list = fs.readFileSync('src/components/HintList.tsx', 'utf8')

test('hint publication is an explicit revisioned organizer action', () => {
  assert.match(api, /releasedHintCount: number/)
  assert.match(api, /editReleaseNextChallengeHint/)
  assert.match(api, /expectedRevision: number/)
  assert.match(editor, /index !== challenge\.releasedHintCount/)
  assert.match(editor, /releaseDisabled=\{dirty\}/)
})

test('hint release controls expose textual state and accessible names', () => {
  assert.match(list, /Hint \{\{number\}\}/)
  assert.match(list, /Released/)
  assert.match(list, /Draft/)
  assert.match(list, /Release hint \{\{number\}\}/)
  assert.match(list, /Delete hint \{\{number\}\}/)
})
