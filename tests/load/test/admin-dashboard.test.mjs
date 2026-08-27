import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

import { DASHBOARD_OPERATIONS, validDashboardResponse } from '../admin-dashboard.js'

const runnerSource = readFileSync(new URL('../admin-dashboard.mjs', import.meta.url), 'utf8')

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
