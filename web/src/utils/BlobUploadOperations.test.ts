import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { BLOB_OPERATION_HEADER, retainBlobUploadOperation } from './BlobUploadOperations'

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
