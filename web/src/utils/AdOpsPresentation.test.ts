import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import type { AdTeamRowModel } from '@Api'
import { filterAdOpsTeams, matchesAdOpsHealth } from './AdOpsPresentation'

const teams = [
  {
    participationId: 1,
    teamName: 'Good team',
    services: [
      { challengeId: 1, lastCheckStatus: 'Ok' },
      { challengeId: 2, lastCheckStatus: 'Offline' },
    ],
  },
  { participationId: 2, teamName: 'Unchecked team', services: [{ challengeId: 1, lastCheckStatus: null }] },
  { participationId: 3, teamName: 'New team', services: [] },
] as AdTeamRowModel[]

test('operator filters retain missing and unchecked services as attention states', () => {
  assert.equal(matchesAdOpsHealth(undefined, 'attention'), true)
  assert.equal(matchesAdOpsHealth(undefined, 'unchecked'), true)
  assert.equal(matchesAdOpsHealth(undefined, 'Offline'), false)
  assert.deepEqual(
    filterAdOpsTeams(teams, [1], '', 'attention').map((t) => t.participationId),
    [2, 3]
  )
  assert.deepEqual(
    filterAdOpsTeams(teams, [1, 2], '', 'attention').map((t) => t.participationId),
    [1, 2, 3]
  )
  assert.deepEqual(
    filterAdOpsTeams(teams, [2], '  GOOD ', 'Offline').map((t) => t.participationId),
    [1]
  )
  assert.equal(filterAdOpsTeams(teams, [1], 'not found', '').length, 0)
  assert.equal(filterAdOpsTeams(teams, [], '', 'Ok').length, 0)
})

test('operator presentation keeps command ownership and inactive-engine read boundaries', () => {
  const page = readFileSync('src/pages/admin/games/[id]/AdOps.tsx', 'utf8')
  assert.match(page, /useAdminAdState\(numId, fetchAd, polling\)/)
  assert.match(page, /useAdminKothState\(numId, fetchKoth, polling\)/)
  assert.match(page, /if \(ensureJobRef.current\) return ensureJobRef.current/)
  assert.match(page, /eventEnded[\s\S]*engineMetadata.end/)
  assert.match(page, /data-ad-loading/)
  assert.doesNotMatch(page, /<WithGameEditTab isLoading>/)
  assert.match(page, /data-ad-error/)
  assert.match(page, /showFlags && cell.currentFlag/)
  assert.match(page, /cell.containerGuid && !cell.selfHosted/)
  assert.match(page, /!cell.selfHosted && \(cell.snapshotAvailable/)
  assert.match(page, /!cell.selfHosted &&[\s\S]*admin.tooltip.ad_ops.restart/)
})

test('operator tables and dialogs expose keyboard and scroll controls', () => {
  const page = readFileSync('src/pages/admin/games/[id]/AdOps.tsx', 'utf8')
  const hills = readFileSync('src/components/admin/KothOpsPanel.tsx', 'utf8')
  assert.match(page, /<AccessibleModal/)
  assert.equal((hills.match(/<AccessibleModal/g) ?? []).length, 2)
  assert.ok((hills.match(/tabIndex: 0/g) ?? []).length >= 2)
  assert.match(page, /data-ad-team=/)
  assert.match(page, /filterAdOpsTeams\(/)
})

test('operator console copy is present in English and Indonesian', () => {
  const en = JSON.parse(readFileSync('src/locales/en-US/admin.json', 'utf8')).ad_console
  const id = JSON.parse(readFileSync('src/locales/id-ID/admin.json', 'utf8')).ad_console
  assert.deepEqual(Object.keys(en).sort(), Object.keys(id).sort())
  assert.ok(Object.values(en).every((x) => typeof x === 'string' && x.length))
  assert.ok(Object.values(id).every((x) => typeof x === 'string' && x.length))
})
