import assert from 'node:assert/strict'
import { test } from 'node:test'
import { abbreviatedSha256, attachmentDownloadInfo } from './AttachmentDownload'

const HASH = 'c5a573e275a0fca6cf6929d324dcc0a6d20882bc922009f1ca0ca022d8e5709d'

test('extracts immutable metadata from the normal local asset route', () => {
  assert.deepEqual(attachmentDownloadInfo(`/assets/${HASH}/Rythme%20Client.exe`), {
    isLocal: true,
    filename: 'Rythme Client.exe',
    sha256: HASH,
  })
})

test('extracts metadata from the token-compatible local route', () => {
  assert.deepEqual(attachmentDownloadInfo(`/assets/${HASH}/s/token/challenge.zip?ignored=yes`), {
    isLocal: true,
    filename: 'challenge.zip',
    sha256: HASH,
  })
})

test('prefers a valid API hash and never treats external links as local assets', () => {
  assert.deepEqual(attachmentDownloadInfo('https://cdn.example/challenge.zip', HASH.toUpperCase()), {
    isLocal: false,
    filename: null,
    sha256: null,
  })
  assert.equal(attachmentDownloadInfo(`/assets/not-a-hash/file.zip`, HASH).sha256, HASH)
  assert.equal(attachmentDownloadInfo(`/assets/${HASH}/unexpected/file.zip`).isLocal, false)
})

test('abbreviates hashes without hiding their distinguishing suffix', () => {
  assert.equal(abbreviatedSha256(HASH), 'c5a573e275a0…d8e5709d')
})
