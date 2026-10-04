import assert from 'node:assert/strict'
import test from 'node:test'
import { WRITEUP_MAX_BYTES, writeupFailureKind, writeupFileProblem, writeupUploadPercent } from './WriteupUpload'

test('writeup selection validates PDF format, non-empty content, and the server byte limit', () => {
  const pdf = { name: 'writeup.pdf', type: 'application/pdf', size: WRITEUP_MAX_BYTES }
  assert.equal(writeupFileProblem(pdf), null)
  assert.equal(writeupFileProblem({ ...pdf, type: '' }), null)
  assert.equal(writeupFileProblem({ ...pdf, name: 'writeup.exe' }), 'format')
  assert.equal(writeupFileProblem({ ...pdf, type: 'text/plain' }), 'format')
  assert.equal(writeupFileProblem({ ...pdf, size: 0 }), 'empty')
  assert.equal(writeupFileProblem({ ...pdf, size: WRITEUP_MAX_BYTES + 1 }), 'size')
})

test('writeup progress never invents a total or exceeds a percentage', () => {
  assert.equal(writeupUploadPercent(400), null)
  assert.equal(writeupUploadPercent(400, 0), null)
  assert.equal(writeupUploadPercent(400, Number.NaN), null)
  assert.equal(writeupUploadPercent(Number.POSITIVE_INFINITY, 100), null)
  assert.equal(writeupUploadPercent(100, 400), 25)
  assert.equal(writeupUploadPercent(399.9, 400), 99)
  assert.equal(writeupUploadPercent(800, 400), 100)
  assert.equal(writeupUploadPercent(-2, 100), 0)
})

test('writeup errors distinguish unknown delivery, capacity, session, and deadline failures', () => {
  assert.equal(writeupFailureKind(new Error('Network Error')), 'connection')
  for (const status of [408, 502, 504]) assert.equal(writeupFailureKind({ response: { status } }), 'connection')
  for (const status of [429, 503]) assert.equal(writeupFailureKind({ response: { status } }), 'busy')
  assert.equal(writeupFailureKind({ response: { status: 401 } }), 'session')
  assert.equal(writeupFailureKind({ response: { status: 413 } }), 'size')
  assert.equal(writeupFailureKind({ response: { status: 403 } }), 'other')
  assert.equal(
    writeupFailureKind({ response: { status: 409, data: { title: 'Writeup submission is no longer eligible' } } }),
    'deadline'
  )
})
