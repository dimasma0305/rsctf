import assert from 'node:assert/strict'
import test from 'node:test'
import {
  clearDestroyedInstanceContext,
  confirmCreatedInstance,
  destroyReconciledInstance,
  isInstanceExtensionWindowOpen,
  mergeExtendedInstanceContext,
  mergeInstanceContext,
  runInstanceExtension,
} from './InstanceLifecycle'

const ORIGINAL_ID = '11111111-1111-4111-8111-111111111111'
const REPLACEMENT_ID = '22222222-2222-4222-8222-222222222222'
const REUSED_DIRECT_ENTRY = '203.0.113.17:31337'
type ChallengeSnapshot = {
  attempts: number
  context: {
    closeTime: number | null
    instanceId: string | null
    instanceEntry: string | null
  }
}

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
    context: { closeTime: 100, instanceId: ORIGINAL_ID, instanceEntry: 'new-entry' },
  }

  assert.deepEqual(mergeInstanceContext(refreshed, { closeTime: 250 }), {
    attempts: 4,
    hints: ['new hint'],
    solved: true,
    context: { closeTime: 250, instanceId: ORIGINAL_ID, instanceEntry: 'new-entry' },
  })
  assert.equal(mergeInstanceContext(undefined, { closeTime: 250 }), undefined)
})

test('an initial create is published by one authoritative same-ID refresh', async () => {
  const created = { id: ORIGINAL_ID, entry: 'created-entry', expectStopAt: 200 }
  let cached: ChallengeSnapshot = {
    attempts: 2,
    context: { closeTime: null, instanceId: null, instanceEntry: null },
  }
  const authoritative: ChallengeSnapshot = {
    attempts: 3,
    context: { closeTime: 100, instanceId: ORIGINAL_ID, instanceEntry: 'created-entry' },
  }
  let refreshes = 0

  const confirmed = await confirmCreatedInstance(created, async () => {
    refreshes += 1
    cached = authoritative
    return cached
  })

  assert.equal(confirmed, true)
  assert.equal(refreshes, 1)
  assert.equal(cached, authoritative)

  let malformedRefreshes = 0
  assert.equal(
    await confirmCreatedInstance({ entry: 'created-entry', expectStopAt: 200 }, async () => {
      malformedRefreshes += 1
      return authoritative
    }),
    false
  )
  assert.equal(malformedRefreshes, 0)
})

test('a delayed create response cannot replace a destroyed and recreated runtime', async () => {
  const originalResponse = { id: ORIGINAL_ID, entry: 'old-entry', expectStopAt: 100 }
  let cached: ChallengeSnapshot = {
    attempts: 4,
    context: { closeTime: null, instanceId: null, instanceEntry: null },
  }
  const original: ChallengeSnapshot = {
    attempts: 4,
    context: { closeTime: 100, instanceId: ORIGINAL_ID, instanceEntry: 'old-entry' },
  }
  const replacement: ChallengeSnapshot = {
    attempts: 4,
    context: { closeTime: 300, instanceId: REPLACEMENT_ID, instanceEntry: 'replacement-entry' },
  }
  let authoritative = original
  let resolveCreate: ((created: typeof originalResponse) => void) | undefined
  const delayedCreate = new Promise<typeof originalResponse>((resolve) => {
    resolveCreate = resolve
  })
  const confirmCreate = delayedCreate.then((created) => {
    return confirmCreatedInstance(created, async () => {
      cached = authoritative
      return cached
    })
  })

  authoritative = replacement

  resolveCreate?.(originalResponse)
  assert.equal(await confirmCreate, false)

  assert.equal(cached, replacement)
})

test('a delayed create cannot republish a runtime destroyed before authoritative confirmation', async () => {
  const created = { id: ORIGINAL_ID, entry: REUSED_DIRECT_ENTRY, expectStopAt: 100 }
  const absent: ChallengeSnapshot = {
    attempts: 4,
    context: { closeTime: null, instanceId: null, instanceEntry: null },
  }
  const active: ChallengeSnapshot = {
    attempts: 4,
    context: { closeTime: 100, instanceId: ORIGINAL_ID, instanceEntry: REUSED_DIRECT_ENTRY },
  }
  let cached = absent
  let authoritative = active
  let refreshes = 0
  let resolveCreate: ((response: typeof created) => void) | undefined
  const delayedCreate = new Promise<typeof created>((resolve) => {
    resolveCreate = resolve
  })
  const confirmCreate = delayedCreate.then((response) => {
    return confirmCreatedInstance(response, async () => {
      refreshes += 1
      cached = authoritative
      return cached
    })
  })

  authoritative = absent
  resolveCreate?.(created)

  assert.equal(await confirmCreate, false)
  assert.equal(refreshes, 1)
  assert.equal(cached, absent)
})

test('a delayed extension response cannot stamp a destroyed and recreated runtime', async () => {
  const original = { context: { closeTime: 100, instanceId: ORIGINAL_ID, instanceEntry: 'old-entry' } }
  let cached = original
  let resolveExtension: ((extension: { id: string; entry: string; expectStopAt: number }) => void) | undefined
  const delayedExtension = new Promise<{ id: string; entry: string; expectStopAt: number }>((resolve) => {
    resolveExtension = resolve
  })
  const publishExtension = delayedExtension.then((extension) => {
    cached = mergeExtendedInstanceContext(cached, extension) ?? cached
  })

  cached = clearDestroyedInstanceContext(cached, original) ?? cached
  cached =
    mergeInstanceContext(cached, {
      closeTime: 300,
      instanceId: REPLACEMENT_ID,
      instanceEntry: 'replacement-entry',
    }) ?? cached

  resolveExtension?.({ id: ORIGINAL_ID, entry: 'old-entry', expectStopAt: 200 })
  await publishExtension

  assert.deepEqual(cached, {
    context: { closeTime: 300, instanceId: REPLACEMENT_ID, instanceEntry: 'replacement-entry' },
  })
  assert.deepEqual(
    mergeExtendedInstanceContext(cached, {
      id: REPLACEMENT_ID,
      entry: 'replacement-entry',
      expectStopAt: 400,
    }),
    { context: { closeTime: 400, instanceId: REPLACEMENT_ID, instanceEntry: 'replacement-entry' } }
  )
})

test('direct-port reuse fences stale create and extension responses by container ID', async () => {
  const replacement = {
    context: {
      closeTime: 300,
      instanceId: REPLACEMENT_ID,
      instanceEntry: REUSED_DIRECT_ENTRY,
    },
  }
  const staleResponse = {
    id: ORIGINAL_ID,
    entry: REUSED_DIRECT_ENTRY,
    expectStopAt: 900,
  }

  let refreshes = 0
  assert.equal(
    await confirmCreatedInstance(staleResponse, async () => {
      refreshes += 1
      return replacement
    }),
    false
  )
  assert.equal(refreshes, 1)
  assert.equal(mergeExtendedInstanceContext(replacement, staleResponse), replacement)
})

test('a stale destroy completion cannot clear a replacement that reused its direct port', async () => {
  const deleted = {
    context: { closeTime: 100, instanceId: ORIGINAL_ID, instanceEntry: REUSED_DIRECT_ENTRY },
  }
  const replacement = {
    context: { closeTime: 300, instanceId: REPLACEMENT_ID, instanceEntry: REUSED_DIRECT_ENTRY },
  }
  let cached = deleted
  let refreshes = 0

  const result = await destroyReconciledInstance({
    refresh: async () => {
      refreshes += 1
      return refreshes === 1 ? deleted : cached
    },
    hasInstance: (value) => Boolean(value?.context.instanceEntry),
    destroy: async () => {
      cached = replacement
    },
    publishAbsent: async (latest) => {
      cached = clearDestroyedInstanceContext(cached, latest) ?? cached
    },
  })

  assert.equal(result, 'destroyed')
  assert.equal(cached, replacement)
})

test('a conditional delete conflict refreshes the replacement and preserves the failure', async () => {
  const original = {
    context: { closeTime: 100, instanceId: ORIGINAL_ID, instanceEntry: REUSED_DIRECT_ENTRY },
  }
  const replacement = {
    context: { closeTime: 300, instanceId: REPLACEMENT_ID, instanceEntry: REUSED_DIRECT_ENTRY },
  }
  let cached = original
  let refreshes = 0
  let published = 0
  let requestedId: string | null | undefined

  await assert.rejects(
    destroyReconciledInstance({
      refresh: async () => {
        refreshes += 1
        cached = refreshes === 1 ? original : replacement
        return cached
      },
      hasInstance: (value) => Boolean(value?.context.instanceId),
      destroy: async (latest) => {
        requestedId = latest.context.instanceId
        throw new Error('409 runtime changed')
      },
      publishAbsent: async () => {
        published += 1
      },
    }),
    /runtime changed/
  )

  assert.equal(requestedId, ORIGINAL_ID)
  assert.equal(refreshes, 2)
  assert.equal(published, 0)
  assert.equal(cached, replacement)
})

test('destroy preserves a replacement instance published while deletion is in flight', async () => {
  const deleted = { context: { closeTime: 100, instanceId: ORIGINAL_ID, instanceEntry: 'old-entry' } }
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
      cached = {
        context: { closeTime: 300, instanceId: REPLACEMENT_ID, instanceEntry: 'replacement-entry' },
      }
    },
    publishAbsent: async (latest) => {
      cached = clearDestroyedInstanceContext(cached, latest) ?? cached
    },
  })

  assert.equal(result, 'destroyed')
  assert.deepEqual(cached, {
    context: { closeTime: 300, instanceId: REPLACEMENT_ID, instanceEntry: 'replacement-entry' },
  })
  assert.deepEqual(clearDestroyedInstanceContext(deleted, deleted), {
    context: { closeTime: null, instanceId: null, instanceEntry: null },
  })
})

test('destroy uses the refreshed instance and revalidates after success', async () => {
  const snapshots = [{ active: true }, { active: false }]
  let destroys = 0
  let published = 0
  const result = await destroyReconciledInstance({
    refresh: async () => snapshots.shift(),
    hasInstance: (value) => value?.active === true,
    destroy: async (latest) => {
      assert.equal(latest.active, true)
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
