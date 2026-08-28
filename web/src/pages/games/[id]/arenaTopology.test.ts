import assert from 'node:assert/strict'
import test from 'node:test'
import { arenaTopologySignature, buildArenaRosterRows } from './arenaTopology'

test('arena topology detects late teams, services, and hills without reacting to score-only changes', () => {
  const ad = {
    challenges: [{ challengeId: 11, title: 'Vault' }],
    teams: [{ participationId: 1, teamId: 101, teamName: 'Alpha', settledTotal: 10 }],
  }
  const koth = { hills: [{ challengeId: 22, title: 'Crown' }], teams: [] }
  const jeopardy = { items: [] }
  const initial = arenaTopologySignature(ad, koth, jeopardy)

  assert.equal(
    arenaTopologySignature({ ...ad, teams: [{ ...ad.teams[0], settledTotal: 999 }] }, koth, jeopardy),
    initial,
    'score-only polls must retain the current DOM topology'
  )
  assert.notEqual(
    arenaTopologySignature(
      ad,
      { ...koth, teams: [{ participationId: 2, teamId: 202, teamName: 'Late Team' }] },
      jeopardy
    ),
    initial
  )
  assert.notEqual(
    arenaTopologySignature(
      { ...ad, challenges: [...ad.challenges, { challengeId: 12, title: 'Proxy' }] },
      koth,
      jeopardy
    ),
    initial
  )
  assert.notEqual(
    arenaTopologySignature(ad, { ...koth, hills: [...koth.hills, { challengeId: 23, title: 'Root' }] }, jeopardy),
    initial
  )
})

test('arena roster is one stable union across A&D, KotH, and Jeopardy boards', () => {
  const rows = buildArenaRosterRows(
    { teams: [{ participationId: 1, teamId: 10, teamName: 'Alpha' }] },
    {
      teams: [
        { participationId: 1, teamId: 10, teamName: 'Alpha' },
        { participationId: 2, teamId: 20, teamName: 'Bravo' },
      ],
    },
    {
      items: [
        { id: 30, name: 'Charlie' },
        { id: 20, name: 'Bravo' },
      ],
    }
  )
  assert.deepEqual(
    rows.map((row) => [row.teamId, row.teamName]),
    [
      [10, 'Alpha'],
      [20, 'Bravo'],
      [30, 'Charlie'],
    ]
  )
})
