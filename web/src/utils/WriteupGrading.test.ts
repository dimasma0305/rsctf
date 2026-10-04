import assert from 'node:assert/strict'
import test from 'node:test'
import type { WriteupGradeTeam } from '../Api'
import { gradeScore, rankWriteupTeams } from './WriteupGrading'

const team = (id: number, percentage: number | null = null): WriteupGradeTeam => ({
  participationId: id,
  teamId: id,
  name: `Team ${id}`,
  divisionId: 1,
  division: 'Open',
  writeupUrl: null,
  originalScore: 60,
  overallEligible: true,
  divisionEligible: true,
  challenges: [
    { challengeId: 10, title: 'Web', mode: 'Jeopardy', earnedPoints: 550, overallPoints: 40, percentage, revision: 0 },
    {
      challengeId: 11,
      title: 'Hill',
      mode: 'KingOfTheHill',
      earnedPoints: 40,
      overallPoints: 20,
      percentage: null,
      revision: 0,
    },
  ],
})
test('writeup grades retain ungraded scores and zero does not fall back to 100', () => {
  assert.equal(gradeScore(team(1), 'Overall', true), 60)
  assert.equal(gradeScore(team(1, 0), 'Overall', true), 20)
  assert.equal(gradeScore(team(1, 50), 'Jeopardy', true), 275)
  assert.equal(gradeScore(team(1, 0), 'Jeopardy', false), 550)
  assert.equal(gradeScore(team(1, 0), 'KingOfTheHill', true), 40)
})
test('writeup grades rank all teams, preserve tied places and division eligibility without mutation', () => {
  const source = [team(1, 0), team(2), team(3), { ...team(4), overallEligible: false }]
  const before = JSON.stringify(source)
  assert.deepEqual(
    rankWriteupTeams(source, 'Overall', '').map((r) => [r.team.teamId, r.rank]),
    [
      [2, 1],
      [3, 1],
      [4, 0],
      [1, 3],
    ]
  )
  assert.equal(rankWriteupTeams(source, 'Overall', '2').length, 0)
  assert.equal(rankWriteupTeams(source, 'Overall', '1').find((r) => r.team.teamId === 4)?.rank, 1)
  assert.equal(JSON.stringify(source), before)
})
