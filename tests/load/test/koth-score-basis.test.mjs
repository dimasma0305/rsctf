import assert from 'node:assert/strict';
import test from 'node:test';

import { validKothEventScoreBasis } from '../koth-score-basis.js';

const dimas = {
  settledTotal: 25 / 27,
  settledEpochPoints: 25,
  settledEpochWeight: 27,
  projectedTotal: 25 / 27.0625,
  projectedEpochPoints: 25,
  projectedEpochWeight: 27.0625,
};

test('KotH event-score basis accepts the real weighted 25 divided by 27 case', () => {
  assert.equal(validKothEventScoreBasis(dimas), true);
  assert.equal(
    validKothEventScoreBasis({
      settledTotal: 0,
      settledEpochPoints: 0,
      settledEpochWeight: 0,
      projectedTotal: 0,
      projectedEpochPoints: 0,
      projectedEpochWeight: 0,
    }),
    true
  );
});

test('KotH event-score basis rejects a hill-local value presented as the event average', () => {
  assert.equal(validKothEventScoreBasis({ ...dimas, settledTotal: 50 }), false);
  assert.equal(validKothEventScoreBasis({ ...dimas, settledEpochPoints: 50 }), false);
  assert.equal(validKothEventScoreBasis({ ...dimas, settledEpochWeight: 0 }), false);
  assert.equal(validKothEventScoreBasis({ ...dimas, projectedEpochWeight: 26 }), false);
  assert.equal(validKothEventScoreBasis({ ...dimas, settledEpochPoints: Number.NaN }), false);
});
