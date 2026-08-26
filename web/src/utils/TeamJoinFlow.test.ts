import assert from 'node:assert/strict'
import test from 'node:test'
import { settleTeamJoinAttempt } from './TeamJoinFlow'

test('guided team join preserves the form after rejection and completes only after success', async () => {
  const actions: string[] = []
  let attempts = 0
  const attempt = () =>
    settleTeamJoinAttempt({
      accept: async () => {
        attempts += 1
        if (attempts === 1) throw new Error('expired invite')
      },
      onAccepted: () => actions.push('clear-code', 'close', 'advance-guide'),
      onRejected: () => actions.push('show-error'),
    })

  assert.equal(await attempt(), false)
  assert.deepEqual(actions, ['show-error'])

  assert.equal(await attempt(), true)
  assert.deepEqual(actions, ['show-error', 'clear-code', 'close', 'advance-guide'])
})
