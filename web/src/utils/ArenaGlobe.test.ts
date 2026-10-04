import assert from 'node:assert/strict'
import test from 'node:test'
import {
  acceptedTerritorySolvers,
  faceLocation,
  projectGlobe,
  sphereLocation,
} from '../pages/games/[id]/arenaGlobeModel'

test('globe locations are 3D unit vectors; rotation hides the back and preserves depth', () => {
  for (const count of [1, 8, 49, 500]) {
    for (let i = 0; i < count; i++) {
      const p = sphereLocation(i, count)
      assert.ok(Math.abs(Math.hypot(p.x, p.y, p.z) - 1) < 1e-10)
      const view = faceLocation(p)
      const front = projectGlobe(p, view.yaw, view.pitch)
      assert.ok(front.visible)
      assert.ok(Math.abs(front.x - 500) < 1e-8)
      assert.ok(Math.abs(front.y - 500) < 1e-8)
      const back = projectGlobe(p, view.yaw + Math.PI, -view.pitch)
      assert.ok(!back.visible)
      const turn = projectGlobe(p, view.yaw + Math.PI * 2, view.pitch)
      assert.ok(Math.abs(turn.z - front.z) < 1e-10)
    }
  }
})

test('conquest includes all public accepted solvers, rejects unaccepted/unknown types, and deduplicates by name not color', () => {
  const solvers = acceptedTerritorySolvers(
    12,
    [
      { name: 'Later team', solvedChallenges: [{ id: 12, type: 'Normal', time: 40 }] },
      { name: 'First team', solvedChallenges: [{ id: 12, type: 'FirstBlood', time: 10 }] },
      { name: 'Rejected', solvedChallenges: [{ id: 12, type: 'Unaccepted', time: 1 }] },
      { name: 'Unknown', solvedChallenges: [{ id: 12, type: 'FutureType', time: 1 }] },
      { name: 'Other island', solvedChallenges: [{ id: 13, type: 'Normal', time: 1 }] },
      { name: 'No solves' },
    ],
    [{ name: 'First team' }],
    () => '#123456'
  )
  assert.deepEqual(
    solvers.map((s) => s.name),
    ['First team', 'Later team']
  )
  assert.equal(solvers[0].color, solvers[1].color)
})
