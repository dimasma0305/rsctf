import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const editorPath = 'src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx'

test('container editor warns when the active backend cannot enforce storage quotas', () => {
  const editor = readFileSync(editorPath, 'utf8')

  assert.match(editor, /isContainerType && challenge\?\.storageQuotaEnforced === false/)
  assert.match(editor, /storage_limit\.unbounded_title/)
  assert.match(editor, /storage_limit\.unbounded_warning/)
})

test('quota warning explains the unbounded fallback in English and Indonesian', () => {
  const english = JSON.parse(readFileSync('src/locales/en-US/admin.json', 'utf8'))
  const indonesian = JSON.parse(readFileSync('src/locales/id-ID/admin.json', 'utf8'))
  const englishCopy = english.content.games.challenges.storage_limit
  const indonesianCopy = indonesian.content.games.challenges.storage_limit

  assert.match(englishCopy.unbounded_warning, /not applied to instances/i)
  assert.match(englishCopy.unbounded_warning, /monitor free disk space/i)
  assert.match(indonesianCopy.unbounded_warning, /tidak diterapkan pada instance/i)
  assert.match(indonesianCopy.unbounded_warning, /pantau ruang disk kosong/i)
})

test('generated client exposes the runtime quota capability', () => {
  const api = readFileSync('src/Api.ts', 'utf8')

  assert.match(api, /storageQuotaEnforced\?: boolean \| null/)
})
