import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { readFileSync } from 'node:fs'
import { BLOB_OPERATION_HEADER, retainBlobUploadOperation } from './BlobUploadOperations'

const source = (path: string) => readFileSync(path, 'utf8')

const apiMethod = (api: string, name: string): string => {
  const start = api.indexOf(`${name}: (`)
  assert.notEqual(start, -1, `${name} is missing from Api.ts`)
  const end = api.indexOf('\n\n    /**', start)
  return api.slice(start, end === -1 ? undefined : end)
}

test('blob upload retries retain identity and different files rotate it', () => {
  const browser = new Window()
  const first = new browser.File(['first'], 'proof.pdf', {
    type: 'application/pdf',
    lastModified: 10,
  }) as unknown as File
  const different = new browser.File(['different'], 'other.pdf', {
    type: 'application/pdf',
    lastModified: 11,
  }) as unknown as File
  let sequence = 0
  const createId = () => `operation-${++sequence}`

  const initial = retainBlobUploadOperation(null, first, createId)
  assert.equal(retainBlobUploadOperation(initial, first, createId), initial)
  assert.notEqual(retainBlobUploadOperation(initial, different, createId).id, initial.id)
  assert.equal(sequence, 2)
  assert.equal(BLOB_OPERATION_HEADER, 'X-RSCTF-Operation-Id')
})

test('every replayable blob API requires and owns its operation header', () => {
  const api = source('src/Api.ts')
  for (const name of ['accountAvatar', 'adminUpdateLogo', 'editUpdateGamePoster', 'gameSubmitWriteup']) {
    const method = apiMethod(api, name)
    assert.match(method, /operationId: string/)
    assert.match(method, /\.\.\.params,\s*headers: \{\s*\.\.\.params\.headers,/)
    assert.match(method, /"X-RSCTF-Operation-Id": operationId/)
  }
})

test('event poster upload retains one operation identity across retries', () => {
  const info = source('src/pages/admin/games/[id]/Info.tsx')
  assert.match(info, /posterOperation\.current = retainBlobUploadOperation\(posterOperation\.current, file\)/)
  assert.match(
    info,
    /editUpdateGamePoster\(game\.id!, \{ file \}, posterOperation\.current\.id\)/
  )
})
