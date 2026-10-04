import assert from 'node:assert/strict'
import test from 'node:test'
import {
  INITIAL_ADMIN_INSTANCE_VIEW,
  adminInstancePageQuery,
  mergeAdminInstanceFilterOptions,
  reduceAdminInstanceView,
} from './AdminInstanceFilters'

const team = (id: number, label: string) => ({ id, label })

test('authoritative instance filters atomically reset pagination', () => {
  const pageFour = reduceAdminInstanceView(INITIAL_ADMIN_INSTANCE_VIEW, { type: 'setPage', page: 4 })
  const filtered = reduceAdminInstanceView(pageFour, { type: 'setTeam', option: team(42, 'outside page one') })

  assert.deepEqual(adminInstancePageQuery(filtered, true), {
    count: 25,
    skip: 0,
    includeRuntimeStats: true,
    teamId: 42,
    challengeId: undefined,
  })
})

test('rapid filter changes produce isolated query keys and retain only the latest selection', () => {
  const first = reduceAdminInstanceView(INITIAL_ADMIN_INSTANCE_VIEW, { type: 'setTeam', option: team(7, 'first') })
  const second = reduceAdminInstanceView(first, { type: 'setTeam', option: team(8, 'second') })

  assert.notDeepEqual(adminInstancePageQuery(first, true), adminInstancePageQuery(second, true))
  assert.equal(second.team?.id, 8)
  assert.equal(second.page, 1)
})

test('filtered totals clamp a now-empty last page without losing active filters', () => {
  const selected = reduceAdminInstanceView(INITIAL_ADMIN_INSTANCE_VIEW, {
    type: 'setChallenge',
    option: team(40, 'practice-web'),
  })
  const latePage = reduceAdminInstanceView(selected, { type: 'setPage', page: 5 })
  const reconciled = reduceAdminInstanceView(latePage, { type: 'reconcileTotal', total: 26 })

  assert.equal(reconciled.page, 2)
  assert.equal(reconciled.challenge?.id, 40)
})

test('remote option refreshes preserve a selected option outside the latest result window', () => {
  assert.deepEqual(mergeAdminInstanceFilterOptions([team(2, 'visible')], team(99, 'selected')), [
    team(99, 'selected'),
    team(2, 'visible'),
  ])
  assert.deepEqual(mergeAdminInstanceFilterOptions([team(99, 'selected')], team(99, 'selected')), [
    team(99, 'selected'),
  ])
})
