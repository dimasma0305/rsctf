import assert from 'node:assert/strict';
import test from 'node:test';

import { validCombinedBoard } from '../combined-scoreboard.js';

function component(score, projectedScore = score) {
  return { active: true, score, projectedScore };
}

function board() {
  return {
    fullySettled: false,
    modes: {
      jeopardy: { active: true, weight: 1 / 3 },
      attackDefense: { active: true, weight: 1 / 3 },
      koth: { active: true, weight: 1 / 3 },
    },
    items: [
      {
        score: 50,
        projectedScore: 60,
        components: {
          jeopardy: component(100),
          attackDefense: component(50, 80),
          koth: component(0),
        },
      },
      {
        score: 25,
        projectedScore: 25,
        components: {
          jeopardy: component(25),
          attackDefense: component(25),
          koth: component(25),
        },
      },
    ],
  };
}

test('combined-board contract accepts equal weights and exact component means', () => {
  assert.equal(validCombinedBoard(board()), true);
});

test('combined-board contract rejects field-relative weights and fabricated totals', () => {
  const badWeight = structuredClone(board());
  badWeight.modes.jeopardy.weight = 0.5;
  assert.equal(validCombinedBoard(badWeight), false);

  const badMean = structuredClone(board());
  badMean.items[0].score = 75;
  assert.equal(validCombinedBoard(badMean), false);

  const overflow = structuredClone(board());
  overflow.items[0].components.koth.score = 101;
  assert.equal(validCombinedBoard(overflow), false);
});
