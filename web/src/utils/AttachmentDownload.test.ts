import assert from 'node:assert/strict'
import { test } from 'node:test'
import { abbreviatedSha256, attachmentDownloadInfo, attachmentDownloadMode } from './AttachmentDownload'

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

test('local attachments mint a grant only for VPN-required events with a known hash and event', () => {
  const local = attachmentDownloadInfo(`/assets/${HASH}/challenge.zip`)
  const external = attachmentDownloadInfo('https://cdn.example/challenge.zip')
  assert.equal(attachmentDownloadMode(local, {}), 'direct')
  assert.equal(attachmentDownloadMode(local, { eventVpnRequired: false, gameId: 19 }), 'direct')
  assert.equal(attachmentDownloadMode(local, { eventVpnRequired: true, gameId: 19 }), 'granted')
  // The catalog and editor previews have no event or supply their own download.
  assert.equal(attachmentDownloadMode(local, { eventVpnRequired: true }), 'direct')
  assert.equal(attachmentDownloadMode(local, { eventVpnRequired: true, gameId: 0 }), 'direct')
  assert.equal(attachmentDownloadMode(local, { eventVpnRequired: true, gameId: 19, hasCustomDownload: true }), 'custom')
  // Off-origin links never carry a grant; nothing about them changes.
  assert.equal(attachmentDownloadMode(external, { eventVpnRequired: true, gameId: 19 }), 'external')
  assert.equal(
    attachmentDownloadMode(attachmentDownloadInfo('/assets/not-a-hash/file.zip'), { eventVpnRequired: true, gameId: 19 }),
    'direct'
  )
})
