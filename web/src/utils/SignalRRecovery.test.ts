import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import {
  cappedJitterDelay,
  CappedJitterRetryPolicy,
  GenerationBoundOpener,
  HUB_KEEPALIVE_MS,
  HUB_REVALIDATE_RETRY_LIMIT,
  HUB_RETRY_CAP_MS,
  HUB_SERVER_TIMEOUT_MS,
  HubRecoveryController,
  hubRevalidationRetryDelay,
  isRetryableHubFailure,
  type RecoverableHubConnection,
  type RecoveryTimers,
} from './SignalRRecovery'

const settle = async () => {
  for (let index = 0; index < 8; index += 1) await Promise.resolve()
}

class ManualTimers implements RecoveryTimers {
  now = 0
  private nextId = 1
  private tasks = new Map<number, { due: number; callback: () => void }>()

  setTimeout = (callback: () => void, milliseconds: number) => {
    const id = this.nextId++
    this.tasks.set(id, { due: this.now + milliseconds, callback })
    return id as unknown as ReturnType<typeof setTimeout>
  }

  clearTimeout = (handle: ReturnType<typeof setTimeout>) => {
    this.tasks.delete(handle as unknown as number)
  }

  get count() {
    return this.tasks.size
  }

  async advance(milliseconds: number) {
    const target = this.now + milliseconds
    while (true) {
      const next = [...this.tasks.entries()]
        .filter(([, task]) => task.due <= target)
        .sort((left, right) => left[1].due - right[1].due || left[0] - right[0])[0]
      if (!next) break
      this.tasks.delete(next[0])
      this.now = next[1].due
      next[1].callback()
      await settle()
    }
    this.now = target
    await settle()
  }
}

class FakeHub implements RecoverableHubConnection {
  startCalls = 0
  stopCalls = 0
  concurrentStarts = 0
  maxConcurrentStarts = 0
  private outcomes: Array<() => Promise<void>> = []
  private closeHandler: (error?: Error) => void = () => undefined
  private reconnectingHandler: (error?: Error) => void = () => undefined
  private reconnectedHandler: (connectionId?: string) => void = () => undefined

  enqueue(outcome: () => Promise<void>) {
    this.outcomes.push(outcome)
  }

  start = async () => {
    this.startCalls += 1
    this.concurrentStarts += 1
    this.maxConcurrentStarts = Math.max(this.maxConcurrentStarts, this.concurrentStarts)
    try {
      await (this.outcomes.shift()?.() ?? Promise.resolve())
    } finally {
      this.concurrentStarts -= 1
    }
  }

  stop = async () => {
    this.stopCalls += 1
  }

  onclose(callback: (error?: Error) => void) {
    this.closeHandler = callback
  }

  onreconnecting(callback: (error?: Error) => void) {
    this.reconnectingHandler = callback
  }

  onreconnected(callback: (connectionId?: string) => void) {
    this.reconnectedHandler = callback
  }

  reconnecting(error?: Error) {
    this.reconnectingHandler(error)
  }

  reconnected(connectionId?: string) {
    this.reconnectedHandler(connectionId)
  }

  close(error?: Error) {
    this.closeHandler(error)
  }
}

const statusError = (status: number) =>
  Object.assign(new Error(`Response status code '${status}'`), { statusCode: status })

test('recovery timing is capped, jittered, finite, and compatible with the server keepalive', () => {
  assert.equal(HUB_KEEPALIVE_MS, 15_000)
  assert.ok(HUB_SERVER_TIMEOUT_MS >= HUB_KEEPALIVE_MS * 2)
  assert.ok(HUB_SERVER_TIMEOUT_MS < HUB_KEEPALIVE_MS * 3)

  for (let attempt = 0; attempt < 20; attempt += 1) {
    const low = cappedJitterDelay(attempt, () => 0)
    const high = cappedJitterDelay(attempt, () => 1)
    assert.ok(low > 0 && low <= high)
    assert.ok(high <= HUB_RETRY_CAP_MS)
  }
  const policy = new CappedJitterRetryPolicy(2, () => 0.5)
  assert.ok(
    policy.nextRetryDelayInMilliseconds({ previousRetryCount: 0, elapsedMilliseconds: 0, retryReason: new Error() })! >
      0
  )
  assert.ok(
    policy.nextRetryDelayInMilliseconds({ previousRetryCount: 1, elapsedMilliseconds: 1, retryReason: new Error() })! >
      0
  )
  assert.equal(
    policy.nextRetryDelayInMilliseconds({ previousRetryCount: 2, elapsedMilliseconds: 2, retryReason: new Error() }),
    null
  )
  assert.equal(
    new CappedJitterRetryPolicy().nextRetryDelayInMilliseconds({
      previousRetryCount: 0,
      elapsedMilliseconds: 0,
      retryReason: statusError(403),
    }),
    null
  )
  assert.ok(
    new CappedJitterRetryPolicy().nextRetryDelayInMilliseconds({
      previousRetryCount: 0,
      elapsedMilliseconds: 0,
      retryReason: statusError(429),
    })! > 0
  )
})

test('server admission and availability statuses retain the correct retry boundary', () => {
  for (const status of [429, 500, 502, 503, 504])
    assert.equal(isRetryableHubFailure(statusError(status)), true, `${status}`)
  for (const status of [400, 401, 403, 404])
    assert.equal(isRetryableHubFailure(statusError(status)), false, `${status}`)
  assert.equal(isRetryableHubFailure(new Error('WebSocket transport failed')), true)
  assert.equal(isRetryableHubFailure({ response: { status: 403 } }), false)
  assert.equal(isRetryableHubFailure({ response: { status: 503 } }), true)
})

test('HTTP reconciliation retries are finite, status-aware, and honor bounded Retry-After', () => {
  const limited = { response: { status: 429, headers: { 'retry-after': '12' } } }
  assert.equal(
    hubRevalidationRetryDelay(limited, 0, () => 0, 0),
    12_000
  )
  assert.equal(
    hubRevalidationRetryDelay({ response: { status: 503 } }, 0, () => 0, 0),
    375
  )
  assert.equal(
    hubRevalidationRetryDelay({ response: { status: 403 } }, 0, () => 0, 0),
    null
  )
  assert.equal(
    hubRevalidationRetryDelay(limited, HUB_REVALIDATE_RETRY_LIMIT, () => 0, 0),
    null
  )
  assert.equal(
    hubRevalidationRetryDelay({ response: { status: 429, headers: { 'retry-after': '999999999' } } }, 0, () => 0, 0),
    null
  )
})

test('a failed reconnect backfill owns one Retry-After timer and cancels it on stop', async () => {
  const hub = new FakeHub()
  const timers = new ManualTimers()
  let refreshes = 0
  const controller = new HubRecoveryController(hub, {
    revalidate: () => {
      refreshes += 1
      if (refreshes === 1) throw { response: { status: 429, headers: { 'retry-after': '12' } } }
    },
    random: () => 0,
    timers,
  })

  controller.start()
  await settle()
  assert.equal(refreshes, 1)
  assert.equal(timers.count, 1)
  await timers.advance(11_999)
  assert.equal(refreshes, 1)
  await timers.advance(1)
  assert.equal(refreshes, 2)
  assert.equal(timers.count, 0)

  const pending = new HubRecoveryController(new FakeHub(), {
    revalidate: () => {
      throw { response: { status: 503 } }
    },
    random: () => 0,
    timers,
  })
  pending.start()
  await settle()
  assert.equal(timers.count, 1)
  await pending.stop()
  assert.equal(timers.count, 0)
  await controller.stop()
})

test('permanent reconciliation denials suppress fallback polls and reconnect backfills', async () => {
  for (const status of [401, 403, 404]) {
    const hub = new FakeHub()
    const timers = new ManualTimers()
    let refreshes = 0
    const controller = new HubRecoveryController(hub, {
      revalidate: () => {
        refreshes += 1
        throw statusError(status)
      },
      exhaustedRetryMs: null,
      pollingIntervalMs: 1_000,
      random: () => 0,
      timers,
    })

    controller.start()
    await settle()
    assert.equal(refreshes, 1, `${status} initial reconciliation`)

    await timers.advance(9_000)
    assert.equal(refreshes, 1, `${status} fallback polling remains suppressed`)

    hub.reconnecting(new Error('transport interrupted'))
    hub.reconnected('replacement-transport')
    await settle()
    assert.equal(refreshes, 1, `${status} reconnect backfill remains suppressed`)

    await controller.stop()
    assert.equal(timers.count, 0, `${status} cleanup`)
  }
})

test('explicit transport retry clears permanent reconciliation suppression', async () => {
  const hub = new FakeHub()
  const timers = new ManualTimers()
  let refreshes = 0
  const controller = new HubRecoveryController(hub, {
    revalidate: () => {
      refreshes += 1
      if (refreshes === 1) throw statusError(403)
    },
    exhaustedRetryMs: null,
    pollingIntervalMs: 1_000,
    random: () => 0,
    timers,
  })

  controller.start()
  await settle()
  await timers.advance(1_800)
  assert.equal(refreshes, 1)

  hub.close(statusError(503))
  assert.equal(controller.currentState, 'exhausted')
  controller.retryNow()
  await settle()

  assert.equal(hub.startCalls, 2)
  assert.equal(controller.currentState, 'connected')
  assert.equal(refreshes, 2, 'explicit retry performs a fresh authoritative backfill')
  await timers.advance(900)
  assert.equal(refreshes, 3, 'successful explicit retry restores fallback polling')
  await controller.stop()
})

test('a permanent initial denial remains terminal for automatic recovery', async () => {
  const hub = new FakeHub()
  const timers = new ManualTimers()
  hub.enqueue(() => Promise.reject(statusError(403)))
  const controller = new HubRecoveryController(hub, {
    revalidate: () => undefined,
    exhaustedRetryMs: 1_000,
    timers,
  })

  controller.start()
  await settle()
  assert.equal(controller.currentState, 'exhausted')
  assert.equal(controller.canRetryAutomatically, false)
  await timers.advance(10_000)
  assert.equal(hub.startCalls, 1)
  await controller.stop()
})

test('failed initial connections retry serially and reconcile after the first successful handshake', async () => {
  const hub = new FakeHub()
  const timers = new ManualTimers()
  hub.enqueue(() => Promise.reject(statusError(503)))
  hub.enqueue(() => Promise.reject(new Error('network unavailable')))
  hub.enqueue(() => Promise.resolve())
  let refreshes = 0
  const generations: number[] = []
  const controller = new HubRecoveryController(hub, {
    revalidate: () => {
      refreshes += 1
    },
    onConnected: (generation) => generations.push(generation),
    exhaustedRetryMs: null,
    random: () => 0,
    timers,
  })

  controller.start()
  await settle()
  assert.equal(hub.startCalls, 1)
  await timers.advance(375)
  assert.equal(hub.startCalls, 2)
  await timers.advance(750)
  assert.equal(hub.startCalls, 3)
  assert.equal(hub.maxConcurrentStarts, 1)
  assert.equal(controller.currentState, 'connected')
  assert.deepEqual(generations, [1])
  assert.equal(refreshes, 1)
  await controller.stop()
})

test('reconnect backfills an outage gap and duplicate reconnect callbacks create one generation', async () => {
  const hub = new FakeHub()
  const authoritative = ['initial']
  let visible: string[] = []
  let reconnectingCalls = 0
  const generations: number[] = []
  const controller = new HubRecoveryController(hub, {
    revalidate: () => {
      visible = [...authoritative]
    },
    onConnected: (generation) => generations.push(generation),
    onReconnecting: () => {
      reconnectingCalls += 1
    },
    exhaustedRetryMs: null,
  })

  controller.start()
  await settle()
  assert.deepEqual(visible, ['initial'])
  authoritative.unshift('committed-during-outage')
  hub.reconnecting(new Error('replica restart'))
  hub.reconnecting(new Error('duplicate callback'))
  hub.reconnected('transport-b')
  hub.reconnected('transport-b')
  await settle()

  assert.equal(reconnectingCalls, 1)
  assert.deepEqual(generations, [1, 2])
  assert.deepEqual(visible, ['committed-during-outage', 'initial'])
  await controller.stop()
})

test('automatic reconnect exhaustion starts one slow jittered recovery cycle', async () => {
  const hub = new FakeHub()
  const timers = new ManualTimers()
  let refreshes = 0
  const controller = new HubRecoveryController(hub, {
    revalidate: () => {
      refreshes += 1
    },
    exhaustedRetryMs: 1_000,
    random: () => 0,
    timers,
  })
  controller.start()
  await settle()
  assert.equal(refreshes, 1)

  hub.reconnecting(new Error('replica unavailable'))
  hub.close(statusError(503))
  assert.equal(controller.currentState, 'exhausted')
  assert.equal(hub.startCalls, 1)
  await timers.advance(799)
  assert.equal(hub.startCalls, 1)
  await timers.advance(1)
  assert.equal(hub.startCalls, 2)
  assert.equal(controller.currentState, 'connected')
  assert.equal(refreshes, 2)
  await controller.stop()
})

test('exhaustion keeps a bounded polling fallback and explicit Retry resumes recovery', async () => {
  const hub = new FakeHub()
  const timers = new ManualTimers()
  for (let index = 0; index < 3; index += 1) hub.enqueue(() => Promise.reject(statusError(index ? 503 : 429)))
  hub.enqueue(() => Promise.resolve())
  let refreshes = 0
  let exhausted = 0
  const controller = new HubRecoveryController(hub, {
    revalidate: () => {
      refreshes += 1
    },
    onExhausted: () => {
      exhausted += 1
    },
    initialRetryLimit: 3,
    exhaustedRetryMs: null,
    pollingIntervalMs: 1_000,
    random: () => 0,
    timers,
  })

  controller.start()
  await settle()
  await timers.advance(375)
  await timers.advance(750)
  assert.equal(hub.startCalls, 3)
  assert.equal(controller.currentState, 'exhausted')
  assert.equal(exhausted, 1)
  assert.equal(refreshes, 1, 'the 900ms polling fallback remains alive during handshake exhaustion')

  controller.retryNow()
  await settle()
  assert.equal(hub.startCalls, 4)
  assert.equal(controller.currentState, 'connected')
  assert.equal(refreshes, 2, 'successful explicit recovery performs an authoritative backfill')
  await controller.stop()
})

test('unmount cancels retry and polling work and ignores a late initial connection', async () => {
  const timers = new ManualTimers()
  const delayedHub = new FakeHub()
  let resolveStart = () => undefined
  delayedHub.enqueue(
    () =>
      new Promise<void>((resolve) => {
        resolveStart = resolve
      })
  )
  let connected = 0
  let refreshes = 0
  const delayed = new HubRecoveryController(delayedHub, {
    revalidate: () => {
      refreshes += 1
    },
    onConnected: () => {
      connected += 1
    },
    pollingIntervalMs: 1_000,
    timers,
  })
  delayed.start()
  await delayed.stop()
  resolveStart()
  await settle()
  await timers.advance(10_000)
  assert.equal(connected, 0)
  assert.equal(refreshes, 0)
  assert.equal(timers.count, 0)
  assert.ok(delayedHub.stopCalls >= 1)

  const retryHub = new FakeHub()
  retryHub.enqueue(() => Promise.reject(statusError(503)))
  const retry = new HubRecoveryController(retryHub, {
    revalidate: () => undefined,
    timers,
    random: () => 0,
  })
  retry.start()
  await settle()
  await retry.stop()
  await timers.advance(10_000)
  assert.equal(retryHub.startCalls, 1)
  assert.equal(timers.count, 0)
})

test('generation-bound Open creates exactly one replacement PTY and disposes a stale result', async () => {
  const opener = new GenerationBoundOpener<string>()
  const creates: number[] = []
  const accepted: string[] = []
  const disposed: string[] = []
  const resolvers = new Map<number, (value: string) => void>()
  const create = (generation: number) => () => {
    creates.push(generation)
    return new Promise<string>((resolve) => resolvers.set(generation, resolve))
  }
  const accept = (value: string) => accepted.push(value)
  const dispose = (value: string) => disposed.push(value)

  opener.beginGeneration(1)
  const first = opener.open(1, create(1), accept, dispose)
  const concurrent = opener.open(1, create(1), accept, dispose)
  assert.equal(first, concurrent)
  await settle()
  assert.deepEqual(creates, [1])

  opener.beginGeneration(2)
  const replacement = opener.open(2, create(2), accept, dispose)
  const duplicateReplacement = opener.open(2, create(2), accept, dispose)
  assert.equal(replacement, duplicateReplacement)
  await settle()
  assert.deepEqual(creates, [1, 2])

  resolvers.get(1)!('stale-pty')
  resolvers.get(2)!('replacement-pty')
  await Promise.all([first, concurrent, replacement, duplicateReplacement])
  assert.deepEqual(disposed, ['stale-pty'])
  assert.deepEqual(accepted, ['replacement-pty'])
})

test('feed and terminal callsites use the shared recovery owner and preserve server-side Open admission', () => {
  for (const path of [
    'src/components/GameNoticePanel.tsx',
    'src/pages/games/[id]/monitor/Submissions.tsx',
    'src/pages/games/[id]/monitor/Events.tsx',
    'src/pages/admin/Logs.tsx',
  ]) {
    const source = readFileSync(path, 'utf8')
    assert.match(source, /useRecoveringHub\(\{/, path)
    assert.doesNotMatch(source, /serverTimeoutInMilliseconds\s*=\s*60 \* 1000 \* 60/, path)
  }
  assert.match(readFileSync('src/components/GameNoticePanel.tsx', 'utf8'), /NOTICE_FALLBACK_POLL_MS/)
  for (const path of [
    'src/pages/games/[id]/monitor/Submissions.tsx',
    'src/pages/games/[id]/monitor/Events.tsx',
    'src/pages/admin/Logs.tsx',
  ]) {
    assert.match(readFileSync(path, 'utf8'), /OPERATOR_FALLBACK_POLL_MS/, path)
  }

  const terminal = readFileSync('src/components/admin/ContainerExecModal.tsx', 'utf8')
  assert.match(terminal, /new GenerationBoundOpener<string>\(\)/)
  assert.match(terminal, /hub\.invoke<string>\('Open', containerGuid, shellRef\.current\)/)
  assert.match(terminal, /aria-label=\{t\('admin\.content\.exec\.retry_label'/)
  assert.match(terminal, /exhaustedRetryMs: null/)

  const admission = readFileSync('../src/hubs/container/admission.rs', 'utf8')
  assert.match(admission, /MAX_EXEC_CONNECTIONS_PER_USER: usize = 4/)
  assert.match(admission, /OPEN_BUDGET_CAPACITY: f64 = 16\.0/)
  assert.match(admission, /try_session_permit/)
})
