import assert from 'node:assert/strict'
import { test } from 'node:test'
import { FlagSubmitAttemptOwner, FlagSubmitAttemptPayload } from './FlagSubmitAttempt'

const input = {
  gameId: 7,
  challengeId: 11,
  flag: 'flag{answer}',
  proof: 'one-use-proof',
}

test('double dispatch has one ref owner and one request', async () => {
  const owner = new FlagSubmitAttemptOwner(() => '00000000-0000-4000-8000-000000000001')
  let prepareCalls = 0
  let sendCalls = 0
  let release!: (submissionId: number) => void
  const blocked = new Promise<number>((resolve) => {
    release = resolve
  })
  const prepare = async (attemptId: string): Promise<FlagSubmitAttemptPayload> => {
    prepareCalls += 1
    return { attemptId, flag: 'encrypted-answer', proof: input.proof }
  }
  const send = async () => {
    sendCalls += 1
    return blocked
  }

  const first = owner.begin(input, prepare, send)
  const duplicate = owner.begin(input, prepare, send)
  assert.equal(first.owner, true)
  assert.equal(duplicate.owner, false)
  assert.equal(first.attemptId, duplicate.attemptId)
  release(41)
  assert.deepEqual(await first.result, {
    attemptId: first.attemptId,
    submissionId: 41,
    firstAcknowledgement: true,
  })
  assert.equal((await duplicate.result).submissionId, 41)
  assert.equal(prepareCalls, 1)
  assert.equal(sendCalls, 1)
})

test('lost response retries the exact attempt and encrypted payload until terminal recovery', async () => {
  const ids = [
    '00000000-0000-4000-8000-000000000001',
    '00000000-0000-4000-8000-000000000002',
  ]
  const owner = new FlagSubmitAttemptOwner(() => ids.shift()!)
  let prepareCalls = 0
  const sent: FlagSubmitAttemptPayload[] = []
  const prepare = async (attemptId: string): Promise<FlagSubmitAttemptPayload> => {
    prepareCalls += 1
    return { attemptId, flag: 'same-encrypted-wire-value', proof: input.proof }
  }
  const send = async (payload: FlagSubmitAttemptPayload) => {
    sent.push(payload)
    if (sent.length === 1) throw new Error('response lost after server commit')
    return 73
  }

  const first = owner.begin(input, prepare, send)
  await assert.rejects(first.result, /response lost/)
  const retry = owner.begin(input, prepare, send)
  const recovered = await retry.result
  assert.equal(retry.attemptId, first.attemptId)
  assert.equal(recovered.submissionId, 73)
  assert.equal(recovered.firstAcknowledgement, true)
  assert.equal(prepareCalls, 1)
  assert.deepEqual(sent[0], sent[1])

  // Once the POST response is known, another status-recovery click reuses the
  // known ID without dispatching or accounting for another attempt.
  const statusRetry = owner.begin(input, prepare, send)
  assert.deepEqual(await statusRetry.result, {
    attemptId: first.attemptId,
    submissionId: 73,
    firstAcknowledgement: false,
  })
  assert.equal(sent.length, 2)

  owner.complete(input.gameId, input.challengeId, first.attemptId)
  const nextSemanticAttempt = owner.begin(input, prepare, async () => 74)
  assert.notEqual(nextSemanticAttempt.attemptId, first.attemptId)
  assert.equal((await nextSemanticAttempt.result).submissionId, 74)
})

test('editing the flag after a failed request creates a new semantic attempt', async () => {
  const ids = [
    '00000000-0000-4000-8000-000000000001',
    '00000000-0000-4000-8000-000000000002',
  ]
  const owner = new FlagSubmitAttemptOwner(() => ids.shift()!)
  const prepare = async (attemptId: string): Promise<FlagSubmitAttemptPayload> => ({
    attemptId,
    flag: `encrypted-for-${attemptId}`,
  })
  const failed = owner.begin(input, prepare, async () => {
    throw new Error('offline')
  })
  await assert.rejects(failed.result, /offline/)

  const changed = owner.begin({ ...input, flag: 'flag{changed}' }, prepare, async () => 88)
  assert.notEqual(changed.attemptId, failed.attemptId)
  assert.equal((await changed.result).submissionId, 88)
})
