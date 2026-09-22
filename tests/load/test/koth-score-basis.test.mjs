import assert from 'node:assert/strict';
import test from 'node:test';

import { validKothEventScoreBasis } from '../koth-score-basis.js';

// Hill 5: field best 80 -> x1.25. Hill 6: field best 10 -> capped at x4.
const hills = [
  {
    challengeId: 5,
    settledFieldBest: 80,
    projectedFieldBest: 80,
    settledMultiplier: 1.25,
    projectedMultiplier: 1.25,
    settledShare: 0.5,
    projectedShare: 0.5,
  },
  {
    challengeId: 6,
    settledFieldBest: 10,
    projectedFieldBest: 10,
    settledMultiplier: 4,
    projectedMultiplier: 4,
    settledShare: 0.5,
    projectedShare: 0.5,
  },
];

const leader = {
  settledTotal: 70,
  projectedTotal: 70,
  hills: [
    { challengeId: 5, settledPoints: 80, projectedPoints: 80, settledNormalizedPoints: 100, projectedNormalizedPoints: 100 },
    { challengeId: 6, settledPoints: 10, projectedPoints: 10, settledNormalizedPoints: 40, projectedNormalizedPoints: 40 },
  ],
};

test('KotH event-score basis accepts the capped field-best aggregate', () => {
  assert.equal(validKothEventScoreBasis(leader, hills), true);
  assert.equal(
    validKothEventScoreBasis(
      { settledTotal: 0, projectedTotal: 0, hills: [] },
      [{ ...hills[0], settledShare: 0, projectedShare: 0, settledMultiplier: 1, projectedMultiplier: 1, settledFieldBest: 0, projectedFieldBest: 0 }]
    ),
    true
  );
});

test('KotH event-score basis rejects totals, multipliers, and shares that break the contract', () => {
  assert.equal(validKothEventScoreBasis({ ...leader, settledTotal: 45 }, hills), false);
  assert.equal(validKothEventScoreBasis(leader, [{ ...hills[0], settledMultiplier: 5 }, hills[1]]), false);
  assert.equal(validKothEventScoreBasis(leader, [{ ...hills[0], settledShare: 0.8 }, hills[1]]), false);
  const inflated = { ...leader, hills: [{ ...leader.hills[0], settledNormalizedPoints: 90 }, leader.hills[1]] };
  assert.equal(validKothEventScoreBasis(inflated, hills), false);
  const aboveFieldBest = { ...leader, hills: [{ ...leader.hills[0], settledPoints: 90 }, leader.hills[1]] };
  assert.equal(validKothEventScoreBasis(aboveFieldBest, hills), false);
  assert.equal(validKothEventScoreBasis({ ...leader, settledTotal: Number.NaN }, hills), false);
  assert.equal(validKothEventScoreBasis(leader, undefined), false);
});
