import assert from 'node:assert/strict'
import test from 'node:test'
import {
  clearDestroyedInstanceContext,
  destroyReconciledInstance,
  isInstanceExtensionWindowOpen,
  mergeInstanceContext,
  runInstanceExtension,
} from './InstanceLifecycle'

test('extension success is published only after the authoritative request succeeds', async () => {
  let successes = 0
  await assert.rejects(
    runInstanceExtension(
      async () => {
        throw new Error('extension rejected')
      },
      () => {
        successes += 1
      }
    ),
    /extension rejected/
  )
  assert.equal(successes, 0)

  await runInstanceExtension(
    async () => undefined,
    () => {
      successes += 1
    }
  )
  assert.equal(successes, 1)
})

test('extension availability uses corrected server time under browser clock skew', () => {
  const serverNow = 2_000_000_000_000
  const closeTime = serverNow + 30 * 60_000

  assert.equal(isInstanceExtensionWindowOpen(closeTime, 10, serverNow), false)
  assert.equal(isInstanceExtensionWindowOpen(closeTime, 10, serverNow + 2 * 60 * 60_000), true)
  assert.equal(isInstanceExtensionWindowOpen(serverNow + 9 * 60_000, 10, serverNow), true)
})

test('instance responses merge into the newest cache without reverting concurrent fields', () => {
  const refreshed = {
    attempts: 4,
    hints: ['new hint'],
    solved: true,
    context: { closeTime: 100, instanceEntry: 'new-entry' },
  }

  assert.deepEqual(mergeInstanceContext(refreshed, { closeTime: 250 }), {
    attempts: 4,
    hints: ['new hint'],
    solved: true,
    context: { closeTime: 250, instanceEntry: 'new-entry' },
  })
  assert.equal(mergeInstanceContext(undefined, { closeTime: 250 }), undefined)
})

test('destroy preserves a replacement instance published while deletion is in flight', async () => {
  const deleted = { context: { closeTime: 100, instanceEntry: 'old-entry' } }
  let cached = deleted
  let refreshes = 0

  const result = await destroyReconciledInstance({
    refresh: async () => {
      refreshes += 1
      if (refreshes > 1) throw new Error('confirmation unavailable')
      return deleted
    },
    hasInstance: (value) => Boolean(value?.context.instanceEntry),
    destroy: async () => {
      cached = { context: { closeTime: 300, instanceEntry: 'replacement-entry' } }
    },
    publishAbsent: async (latest) => {
      cached = clearDestroyedInstanceContext(cached, latest) ?? cached
    },
  })

  assert.equal(result, 'destroyed')
  assert.deepEqual(cached, { context: { closeTime: 300, instanceEntry: 'replacement-entry' } })
  assert.deepEqual(clearDestroyedInstanceContext(deleted, deleted), {
    context: { closeTime: null, instanceEntry: null },
  })
})

test('destroy uses the refreshed instance and revalidates after success', async () => {
  const snapshots = [{ active: true }, { active: false }]
  let destroys = 0
  let published = 0
  const result = await destroyReconciledInstance({
    refresh: async () => snapshots.shift(),
    hasInstance: (value) => value?.active === true,
    destroy: async () => {
      destroys += 1
    },
    publishAbsent: async (latest) => {
      assert.equal(latest.active, true)
      published += 1
    },
  })

  assert.equal(result, 'destroyed')
  assert.equal(destroys, 1)
  assert.equal(published, 1)
  assert.equal(snapshots.length, 0)
})

test('destroy treats an already-removed runtime as converged but preserves real failures', async () => {
  let destroys = 0
  const absent = await destroyReconciledInstance({
    refresh: async () => ({ active: false }),
    hasInstance: (value) => value?.active === true,
    destroy: async () => {
      destroys += 1
    },
    publishAbsent: async () => undefined,
  })
  assert.equal(absent, 'alreadyAbsent')
  assert.equal(destroys, 0)

  const racedSnapshots = [{ active: true }, { active: false }]
  const raced = await destroyReconciledInstance({
    refresh: async () => racedSnapshots.shift(),
    hasInstance: (value) => value?.active === true,
    destroy: async () => {
      throw new Error('already removed elsewhere')
    },
    publishAbsent: async () => undefined,
  })
  assert.equal(raced, 'alreadyAbsent')

  await assert.rejects(
    destroyReconciledInstance({
      refresh: async () => ({ active: true }),
      hasInstance: (value) => value?.active === true,
      destroy: async () => {
        throw new Error('backend unavailable')
      },
      publishAbsent: async () => undefined,
    }),
    /backend unavailable/
  )

  let refreshes = 0
  await assert.rejects(
    destroyReconciledInstance({
      refresh: async () => {
        refreshes += 1
        if (refreshes > 1) throw new Error('refresh unavailable')
        return { active: true }
      },
      hasInstance: (value) => value?.active === true,
      destroy: async () => {
        throw new Error('destroy unavailable')
      },
      publishAbsent: async () => undefined,
    }),
    /destroy unavailable/
  )
})

test('destroy always performs its final authoritative refresh when local publication fails', async () => {
  let refreshes = 0

  await assert.rejects(
    destroyReconciledInstance({
      refresh: async () => {
        refreshes += 1
        return { active: refreshes === 1 }
      },
      hasInstance: (value) => value?.active === true,
      destroy: async () => undefined,
      publishAbsent: async () => {
        throw new Error('local cache rejected')
      },
    }),
    /local cache rejected/
  )

  assert.equal(refreshes, 2)
})

test('destroy remains successful when only its post-delete confirmation refresh fails', async () => {
  let refreshes = 0
  const result = await destroyReconciledInstance({
    refresh: async () => {
      refreshes += 1
      if (refreshes > 1) throw new Error('confirmation unavailable')
      return { active: true }
    },
    hasInstance: (value) => value?.active === true,
    destroy: async () => undefined,
    publishAbsent: async () => undefined,
  })

  assert.equal(result, 'destroyed')
  assert.equal(refreshes, 2)
})
