import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

import { DASHBOARD_OPERATIONS, validDashboardResponse } from '../admin-dashboard.js'

const runnerSource = readFileSync(new URL('../admin-dashboard.mjs', import.meta.url), 'utf8')
const scenarioSource = readFileSync(new URL('../k6/admin-dashboard.js', import.meta.url), 'utf8')
const adminRouterSource = readFileSync(new URL('../../../src/controllers/admin/mod.rs', import.meta.url), 'utf8')

test('dashboard load catalog covers every range with fixed bucket bounds', () => {
  assert.deepEqual(
    DASHBOARD_OPERATIONS.filter((operation) => operation.kind === 'trend').map(({ id, rows }) => [id, rows]),
    [
      ['trend_day', 24],
      ['trend_week', 7],
      ['trend_month', 30],
      ['trend_year', 12],
    ]
  )
})

test('dashboard response validator rejects oversized aggregate and activity results', () => {
  const dashboard = DASHBOARD_OPERATIONS.find((operation) => operation.id === 'dashboard')
  const reviews = DASHBOARD_OPERATIONS.find((operation) => operation.id === 'reviews')
  assert.equal(
    validDashboardResponse(dashboard, {
      systemStats: { userCount: 1, teamCount: 1, activeContainerCount: 0 },
      topGames: Array.from({ length: 5 }, (_, id) => ({ id })),
    }),
    true
  )
  assert.equal(
    validDashboardResponse(dashboard, {
      systemStats: { userCount: 1, teamCount: 1, activeContainerCount: 0 },
      topGames: Array.from({ length: 6 }, (_, id) => ({ id })),
    }),
    false
  )
  assert.equal(validDashboardResponse(reviews, Array(10).fill({})), true)
  assert.equal(validDashboardResponse(reviews, Array(11).fill({})), false)
})

test('dashboard fixture bypasses evidence triggers only inside bounded transactions', () => {
  assert.match(runnerSource, /SET LOCAL session_replication_role = replica/)
  assert.match(runnerSource, /fixtureSql\(\s*`INSERT INTO "Submissions"/)
  assert.match(runnerSource, /`SELECT '\$\{escapedTag\}' \|\| g, 0, `/)
  assert.match(runnerSource, /fixtureSql\(`DELETE FROM "Submissions" WHERE answer LIKE/)
})

test('dashboard traffic and every expensive activity route retain query-work admission', () => {
  assert.match(runnerSource, /RATE: process\.env\.RATE \|\| 1/)
  assert.match(scenarioSource, /RATE !== 1/)

  const compactRouter = adminRouterSource.split(/\s+/).join(' ')
  for (const handler of [
    'dashboard',
    'submission_trend',
    'reviews',
    'cheat_reports',
    'all_writeups',
    'game_writeups',
    'download_all_writeups',
  ]) {
    assert.ok(
      compactRouter.includes(`limited(Policy::Query, get(${handler}))`),
      `${handler} is missing named query-work admission`
    )
  }
})
