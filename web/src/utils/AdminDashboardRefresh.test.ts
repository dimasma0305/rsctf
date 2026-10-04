import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { ADMIN_DASHBOARD_REFRESH_MS, startAdminDashboardRefresh } from './AdminDashboardRefresh'

test('dashboard refresh has one visible owner, pauses while inactive, and never overlaps', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  let active = true
  let listener: (() => void) | undefined
  let refreshes = 0
  let release: (() => void) | undefined

  const stop = startAdminDashboardRefresh({
    refresh: async () => {
      refreshes += 1
      await new Promise<void>((resolve) => {
        release = resolve
      })
    },
    isActive: () => active,
    subscribe: (next) => {
      listener = next
      return () => {
        listener = undefined
      }
    },
    setTimer: setTimeout,
    clearTimer: clearTimeout,
  })

  try {
    await context.mock.timers.tick(ADMIN_DASHBOARD_REFRESH_MS)
    assert.equal(refreshes, 1)

    await context.mock.timers.tick(ADMIN_DASHBOARD_REFRESH_MS * 3)
    assert.equal(refreshes, 1, 'an in-flight refresh must not overlap itself')

    active = false
    listener?.()
    release?.()
    await Promise.resolve()
    await context.mock.timers.tick(ADMIN_DASHBOARD_REFRESH_MS * 2)
    assert.equal(refreshes, 1, 'hidden or idle dashboard must not poll')

    active = true
    listener?.()
    assert.equal(refreshes, 2, 'returning to an active dashboard performs one catch-up refresh')
    listener?.()
    assert.equal(refreshes, 2, 'duplicate browser activity events share the same owner')
  } finally {
    release?.()
    stop()
    context.mock.timers.reset()
  }
})

test('dashboard fetches only the visible activity tab and overrides global polling', () => {
  const source = readFileSync('src/pages/admin/Dashboard.tsx', 'utf8')

  assert.match(source, /refreshInterval:\s*0/)
  assert.match(source, /activityTab === 'reviews' \? \['\/api\/admin\/reviews', reviewPage\] : null/)
  assert.match(source, /activityTab === 'writeups' \? \['\/api\/admin\/writeups', writeupPage\] : null/)
  assert.match(source, /activityTab === 'cheats' \? \['\/api\/admin\/cheat-reports', cheatPage\] : null/)
  assert.equal((source.match(/startAdminDashboardRefresh\(/g) ?? []).length, 1)
})

test('dashboard refresh reports a failed poll and keeps its bounded cadence', async (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  const failures: unknown[] = []
  let refreshes = 0
  const stop = startAdminDashboardRefresh({
    refresh: async () => {
      refreshes += 1
      throw new Error('offline')
    },
    onError: (error) => failures.push(error),
    isActive: () => true,
    subscribe: () => () => {},
    setTimer: setTimeout,
    clearTimer: clearTimeout,
    intervalMilliseconds: 100,
  })

  try {
    await context.mock.timers.tick(100)
    await Promise.resolve()
    assert.equal(refreshes, 1)
    assert.equal(failures.length, 1)

    await context.mock.timers.tick(100)
    await Promise.resolve()
    assert.equal(refreshes, 2)
    assert.equal(failures.length, 2)
  } finally {
    stop()
    context.mock.timers.reset()
  }
})
