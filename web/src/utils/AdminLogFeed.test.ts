import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import type { LogMessageModel } from '../Api'
import {
  adminLogIdentity,
  adminLogMatchesQuery,
  adminLogQueryReducer,
  adminLogQueryScope,
  compareAdminLogsNewestFirst,
  MAX_BUFFERED_ADMIN_LOGS,
} from './AdminLogFeed'
import { prependUniqueBoundedRow } from './FeedReconciliation'

const log = (id: number, overrides: Partial<LogMessageModel> = {}): LogMessageModel => ({
  id,
  time: 1_788_000_000_000 + id,
  level: 'Information',
  name: 'fixture-admin',
  msg: 'updated event settings',
  ip: '192.0.2.10',
  status: 'Success',
  fingerprint: 'browser-alpha',
  ...overrides,
})

test('level and debounced-search commits reset the page atomically', () => {
  const pageSeven = { level: 'Information', page: 7, search: '' }
  const errorQuery = adminLogQueryReducer(pageSeven, { type: 'level', level: 'Error' })
  assert.deepEqual(errorQuery, { level: 'Error', page: 1, search: '' })
  assert.equal(adminLogQueryScope(errorQuery), JSON.stringify(['Error', 1, '']))

  const searched = adminLogQueryReducer({ ...errorQuery, page: 4 }, { type: 'search', search: 'needle' })
  assert.deepEqual(searched, { level: 'Error', page: 1, search: 'needle' })
  assert.equal(
    adminLogQueryReducer(searched, { type: 'search', search: 'needle' }),
    searched,
    'an unchanged committed filter must not schedule another request'
  )
})

test('live admin logs honor level and every server-supported search field', () => {
  const base = { level: 'Information', page: 1, search: '' }
  assert.equal(adminLogMatchesQuery(log(1), base), true)
  assert.equal(adminLogMatchesQuery(log(1), { ...base, level: 'Error' }), false)

  for (const search of ['FIXTURE-ADMIN', 'event settings', '192.0.2', 'BROWSER-ALPHA']) {
    assert.equal(adminLogMatchesQuery(log(1), { ...base, search }), true, search)
  }
  assert.equal(adminLogMatchesQuery(log(1), { ...base, search: 'not-present' }), false)
})

test('admin logs use the stable id to break equal-timestamp ordering ties', () => {
  const rows = [log(2, { time: 100 }), log(4, { time: 99 }), log(3, { time: 100 })]
  rows.sort(compareAdminLogsNewestFirst)
  assert.deepEqual(
    rows.map(({ id }) => id),
    [3, 2, 4]
  )
})

test('five thousand admin pushes retain fifty unique stable ids', () => {
  let buffered: LogMessageModel[] = []
  for (let id = 1; id <= 5_000; id += 1) {
    buffered = prependUniqueBoundedRow(log(id), buffered, MAX_BUFFERED_ADMIN_LOGS, adminLogIdentity)
  }
  assert.equal(buffered.length, MAX_BUFFERED_ADMIN_LOGS)
  assert.equal(buffered[0].id, 5_000)
  assert.equal(buffered.at(-1)?.id, 4_951)

  buffered = prependUniqueBoundedRow(
    log(5_000, { msg: 'refreshed' }),
    buffered,
    MAX_BUFFERED_ADMIN_LOGS,
    adminLogIdentity
  )
  assert.equal(buffered.filter(({ id }) => id === 5_000).length, 1)
  assert.equal(buffered[0].msg, 'refreshed')
})

test('the Logs callsite owns one scoped latest request and no separate page-reset effect', () => {
  const source = readFileSync('src/pages/admin/Logs.tsx', 'utf8')
  assert.match(source, /useReducer\(adminLogQueryReducer/)
  assert.match(source, /new LatestListRequest<LogMessageModel>\(\)/)
  assert.match(source, /currentListSnapshotRows\(queryScope, logSnapshot\)/)
  assert.match(source, /if \(!queryReady\) return/)
  assert.match(source, /adminLogMatchesQuery\(item, query\)/)
  assert.doesNotMatch(source, /useEffect\(\(\) => \{\s*setPage\(1\)/)
})
