import assert from 'node:assert/strict'
import test from 'node:test'
import {
  ADMIN_INSTANCE_FILTER_OPTIONS_CONFIG,
  ADMIN_INSTANCE_STATS_CADENCE_MS,
  adminInstanceRetryDelay,
  createAdminInstancePollingConfig,
} from './AdminInstancePolling'

test('page and option requests never retain stale data across filter keys', () => {
  assert.equal(createAdminInstancePollingConfig(true).config.keepPreviousData, false)
  assert.equal(ADMIN_INSTANCE_FILTER_OPTIONS_CONFIG.keepPreviousData, false)
  assert.equal(ADMIN_INSTANCE_FILTER_OPTIONS_CONFIG.shouldRetryOnError, false)
})

test('admin instance polling stops permanent errors and honors Retry-After', () => {
  assert.equal(adminInstanceRetryDelay({ response: { status: 404 } }, ADMIN_INSTANCE_STATS_CADENCE_MS), null)
  assert.equal(adminInstanceRetryDelay({ response: { status: 403 } }, ADMIN_INSTANCE_STATS_CADENCE_MS), null)
  assert.equal(
    adminInstanceRetryDelay(
      { response: { status: 429, headers: { 'retry-after': '45' } } },
      ADMIN_INSTANCE_STATS_CADENCE_MS
    ),
    45_000
  )
  assert.equal(
    adminInstanceRetryDelay({ response: { status: 503 } }, ADMIN_INSTANCE_STATS_CADENCE_MS),
    ADMIN_INSTANCE_STATS_CADENCE_MS
  )
})

test('admin instance polling owns one cancellable recovery timer', (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  let polls = 0
  const owner = createAdminInstancePollingConfig(true, () => true)
  const retry = owner.config.onErrorRetry
  assert.equal(typeof retry, 'function')

  try {
    retry?.(
      { response: { status: 429, headers: { 'retry-after': '2' } } },
      '/api/admin/instances',
      owner.config,
      () => {
        polls += 1
        return Promise.resolve(true)
      },
      { retryCount: 1, dedupe: true }
    )
    assert.equal(owner.pendingRetries(), 1)
    context.mock.timers.tick(1_999)
    assert.equal(polls, 0)
    context.mock.timers.tick(1)
    assert.equal(polls, 1)
    assert.equal(owner.pendingRetries(), 0)

    retry?.({ response: { status: 503 } }, '/api/admin/instances', owner.config, () => Promise.resolve(true), {
      retryCount: 2,
      dedupe: true,
    })
    assert.equal(owner.pendingRetries(), 1)
    owner.cancel()
    context.mock.timers.tick(ADMIN_INSTANCE_STATS_CADENCE_MS)
    assert.equal(owner.pendingRetries(), 0)
  } finally {
    owner.cancel()
    context.mock.timers.reset()
  }
})

test('hidden or offline recovery waits for SWR focus/reconnect instead of polling', (context) => {
  context.mock.timers.enable({ apis: ['setTimeout'], now: 0 })
  let polls = 0
  const owner = createAdminInstancePollingConfig(true, () => false)

  try {
    owner.config.onErrorRetry?.(
      { response: { status: 503 } },
      '/api/admin/instances',
      owner.config,
      () => {
        polls += 1
        return Promise.resolve(true)
      },
      { retryCount: 1, dedupe: true }
    )
    context.mock.timers.tick(ADMIN_INSTANCE_STATS_CADENCE_MS)
    assert.equal(polls, 0)
    assert.equal(owner.pendingRetries(), 0)
  } finally {
    owner.cancel()
    context.mock.timers.reset()
  }
})
