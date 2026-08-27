import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { createLatestAutocompleteRequests, normalizeManagerAutocompleteQuery } from './ManagerAutocomplete'

const deferred = <Value>() => {
  let resolve!: (value: Value) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<Value>((onResolve, onReject) => {
    resolve = onResolve
    reject = onReject
  })
  return { promise, resolve, reject }
}

test('manager query normalization rejects empty, short, long, and control input', () => {
  assert.equal(normalizeManagerAutocompleteQuery('  Alice  '), 'Alice')
  assert.equal(normalizeManagerAutocompleteQuery('a'), null)
  assert.equal(normalizeManagerAutocompleteQuery('x'.repeat(65)), null)
  assert.equal(normalizeManagerAutocompleteQuery('ab\ncd'), null)
  assert.equal(normalizeManagerAutocompleteQuery('%_'), '%_')
})

test('only the newest autocomplete request can own results and loading', async () => {
  const requests = createLatestAutocompleteRequests()
  const first = deferred<string[]>()
  const second = deferred<string[]>()
  const states: string[] = []
  let results: string[] | undefined
  let loading = false
  const handlers = {
    setLoading: (value: boolean) => {
      loading = value
      states.push(`loading:${value}`)
    },
    setResults: (value: string[]) => {
      results = value
      states.push(`results:${value.join(',')}`)
    },
    onError: (error: unknown) => states.push(`error:${String(error)}`),
  }

  const firstRun = requests.run(() => first.promise, handlers)
  const secondRun = requests.run(() => second.promise, handlers)
  assert.equal(requests.pending(), 1)

  second.resolve(['ab'])
  await secondRun
  first.resolve(['a'])
  await firstRun

  assert.deepEqual(results, ['ab'])
  assert.equal(loading, false)
  assert.deepEqual(states, ['loading:true', 'loading:true', 'results:ab', 'loading:false'])
})

test('clear, route change, and unmount invalidation defeat late transports', async () => {
  for (const boundary of ['clear', 'route', 'unmount']) {
    const requests = createLatestAutocompleteRequests()
    const slow = deferred<string[]>()
    let results: string[] | undefined = ['old']
    let loading = false
    let errors = 0
    const run = requests.run(() => slow.promise, {
      setLoading: (value) => {
        loading = value
      },
      setResults: (value) => {
        results = value
      },
      onError: () => {
        errors += 1
      },
    })

    requests.invalidate()
    results = undefined
    loading = false
    slow.resolve([boundary])
    await run

    assert.equal(results, undefined, boundary)
    assert.equal(loading, false, boundary)
    assert.equal(errors, 0, boundary)
    assert.equal(requests.pending(), 0, boundary)
  }
})

test('a current 429 is surfaced once without a retry or stale loading update', async () => {
  const requests = createLatestAutocompleteRequests()
  const limited = deferred<string[]>()
  const seen: unknown[] = []
  const loading: boolean[] = []
  const run = requests.run(() => limited.promise, {
    setLoading: (value) => loading.push(value),
    setResults: () => assert.fail('429 must not publish results'),
    onError: (error) => seen.push(error),
  })
  const error = { response: { status: 429, headers: { 'retry-after': '10' } } }
  limited.reject(error)
  await run

  assert.deepEqual(seen, [error])
  assert.deepEqual(loading, [true, false])
  assert.equal(requests.pending(), 0)
})

test('the manager selector forwards abort and invalidates input and route boundaries', () => {
  const source = readFileSync('src/pages/admin/games/[id]/Managers.tsx', 'utf8')

  assert.match(source, /adminManagerAutocomplete\(\{ query \}, \{ signal \}\)/)
  assert.doesNotMatch(source, /adminGetUsers\(\{ search:/)
  assert.match(source, /autocompleteRequests\.current\.invalidate\(\)[\s\S]*?\}, \[searchValue\]\)/)
  assert.match(
    source,
    /setSearchValue\(''\)[\s\S]*?return \(\) => autocompleteRequests\.current\.invalidate\(\)[\s\S]*?\}, \[gameId\]\)/
  )
  assert.match(source, /className="app-sr-only" aria-live="polite"/)
})
