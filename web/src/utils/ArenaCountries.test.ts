import assert from 'node:assert/strict'
import test from 'node:test'
import type { AdScoreboardChallenge, ChallengeInfo } from '@Api'
import { buildArenaCountries, countryStatus, isJeopardyCountry } from '../pages/games/[id]/arenaCountries'

const color = () => '#123456'
const service = { challengeId: 820, title: 'Defend web', category: 'Web' } as AdScoreboardChallenge
const challenge = (id: number, type: string, title: string) =>
  ({ id, type, title, score: 1000, solved: 7, bloods: [{ name: 'Old solver' }] }) as ChallengeInfo

test('A&D and KotH-only event has all three countries without invented Jeopardy solves', () => {
  const countries = buildArenaCountries(
    { challenges: [service] },
    {
      hills: [
        { challengeId: 838, title: 'Web hill', currentHolderTeamName: 'Current holder' },
        { challengeId: 839, title: 'PPC hill', currentHolderTeamName: null },
      ],
    },
    {
      items: [],
      challenges: {
        Web: [challenge(820, 'AttackDefense', 'Defend web'), challenge(838, 'KingOfTheHill', 'Web hill')],
        PPC: [challenge(839, 'KingOfTheHill', 'PPC hill')],
      },
    },
    color
  )
  assert.deepEqual(
    countries.map((c) => [c.id, c.challenges.length]),
    [
      ['Web', 2],
      ['PPC', 1],
    ]
  )
  const all = countries.flatMap((c) => c.challenges)
  assert.equal(all.length, 3)
  assert.ok(all.every((c) => !isJeopardyCountry(c) && !c.solveCount && !c.solvers.length))
  assert.match(countryStatus(all[0]), /Per-team services/)
  assert.match(countryStatus(all[1]), /Held by Current holder/)
  assert.match(countryStatus(all[2]), /No current holder/)
})

test('country snapshots preserve Jeopardy solves, distinguish unknown holders, and deduplicate mode fallbacks', () => {
  const catalog = {
    items: [],
    challenges: { Web: [challenge(838, 'KingOfTheHill', 'Hill'), challenge(1000, 'StaticAttachment', 'Puzzle')] },
  }
  const missing = buildArenaCountries({ challenges: [service] }, null, catalog, color).flatMap((c) => c.challenges)
  assert.match(countryStatus(missing.find((c) => c.id === 838)!), /Holder unavailable/)
  const puzzle = missing.find((c) => c.id === 1000)!
  assert.equal(puzzle.solveCount, 7)
  assert.equal(puzzle.solvers[0].name, 'Old solver')
  assert.equal(isJeopardyCountry(puzzle), true)
  assert.equal(missing.filter((c) => c.id === 820).length, 1)
  const fallback = buildArenaCountries(
    { challenges: [service] },
    { hills: [{ challengeId: 838, title: 'Hill' }] },
    null,
    color
  )
  assert.equal(fallback.flatMap((c) => c.challenges).length, 2)
})
