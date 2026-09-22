import assert from 'node:assert/strict'
import { test } from 'node:test'
import { attachmentGrantErrorMessage, downloadGrantedAttachment } from './AttachmentGrant'
import { EventVpnAccessError } from './EventVpnProof'

const HASH = 'c5a573e275a0fca6cf6929d324dcc0a6d20882bc922009f1ca0ca022d8e5709d'
const t = (key: string, fallback: string) => `${key}|${fallback}`

test('a granted download mints the grant before navigating the unchanged asset URL', async () => {
  const calls: string[] = []
  await downloadGrantedAttachment(19, HASH, `/assets/${HASH}/challenge.zip`, 'challenge.zip', {
    grant: async (gameId, hash) => {
      calls.push(`grant:${gameId}:${hash}`)
      return { hash, granted: true, expiresAtUtc: 1 }
    },
    start: (href, filename) => {
      calls.push(`start:${href}:${filename}`)
    },
  })
  assert.deepEqual(calls, [`grant:19:${HASH}`, `start:/assets/${HASH}/challenge.zip:challenge.zip`])
})

test('a failed grant never starts the download and surfaces the failure', async () => {
  let started = 0
  const failure = new EventVpnAccessError('disconnected', 'Connect to the event VPN, then retry this request.', 0)
  await assert.rejects(
    downloadGrantedAttachment(19, HASH, `/assets/${HASH}/challenge.zip`, 'challenge.zip', {
      grant: async () => {
        throw failure
      },
      start: () => {
        started += 1
      },
    }),
    failure
  )
  assert.equal(started, 0)
})

test('grant failures reuse the distinct Event-VPN copy and separate session expiry', () => {
  assert.match(
    attachmentGrantErrorMessage(new EventVpnAccessError('disconnected', 'x', 0), t),
    /^challenge\.error\.vpn_disconnected\|Connect to the event VPN/
  )
  assert.match(
    attachmentGrantErrorMessage(new EventVpnAccessError('rate-limited', 'x', 0), t),
    /^challenge\.error\.vpn_rate_limited\|/
  )
  assert.match(
    attachmentGrantErrorMessage(new EventVpnAccessError('unavailable', 'x', 0), t),
    /^challenge\.error\.vpn_unavailable\|/
  )
  assert.match(attachmentGrantErrorMessage({ response: { status: 401 } }, t), /^challenge\.error\.unauthorized\|/)
  assert.match(attachmentGrantErrorMessage({ response: { status: 403 } }, t), /^challenge\.error\.forbidden\|/)
  assert.match(attachmentGrantErrorMessage({ response: { status: 429 } }, t), /^challenge\.error\.vpn_rate_limited\|/)
  assert.match(attachmentGrantErrorMessage(new Error('network'), t), /^challenge\.error\.attachment_grant\|/)
})
