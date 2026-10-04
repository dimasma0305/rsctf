import assert from 'node:assert/strict'
import { test } from 'node:test'
import { runDownloadSingleFlight } from './DownloadSingleFlight'

const deferred = <T>() => {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => {
    resolve = done
  })
  return { promise, resolve }
}

test('rapid clicks and two mounted controls share one request factory', async () => {
  const response = deferred<string>()
  let firstControlRequests = 0
  let secondControlRequests = 0

  const first = runDownloadSingleFlight('scoreboard:17', () => {
    firstControlRequests += 1
    return response.promise
  })
  const rapidSecondClick = runDownloadSingleFlight('scoreboard:17', () => {
    firstControlRequests += 1
    return Promise.resolve('duplicate')
  })
  const secondMountedControl = runDownloadSingleFlight('scoreboard:17', () => {
    secondControlRequests += 1
    return Promise.resolve('duplicate')
  })

  assert.equal(first, rapidSecondClick)
  assert.equal(first, secondMountedControl)
  await Promise.resolve()
  assert.equal(firstControlRequests, 1)
  assert.equal(secondControlRequests, 0)

  response.resolve('xlsx')
  assert.deepEqual(await Promise.all([first, rapidSecondClick, secondMountedControl]), [
    'xlsx',
    'xlsx',
    'xlsx',
  ])
})

test('a settled key admits a fresh download and export kinds do not collide', async () => {
  let requests = 0
  await runDownloadSingleFlight('submissions:4', async () => {
    requests += 1
    return 'first'
  })

  const [submission, scoreboard] = await Promise.all([
    runDownloadSingleFlight('submissions:4', async () => {
      requests += 1
      return 'second'
    }),
    runDownloadSingleFlight('scoreboard:4', async () => {
      requests += 1
      return 'third'
    }),
  ])

  assert.equal(submission, 'second')
  assert.equal(scoreboard, 'third')
  assert.equal(requests, 3)
})
