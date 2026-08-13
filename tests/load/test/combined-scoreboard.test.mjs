import assert from "node:assert/strict";
import test from "node:test";

import { validCombinedBoard } from "../combined-scoreboard.js";

function component(score, projectedScore = score) {
  return { active: true, score, projectedScore };
}

function board() {
  return {
    fullySettled: false,
    modes: {
      jeopardy: { active: true, challengeCount: 1, weight: 1 / 4 },
      attackDefense: { active: true, challengeCount: 1, weight: 1 / 4 },
      koth: { active: true, challengeCount: 2, weight: 2 / 4 },
    },
    items: [
      {
        score: 37.5,
        projectedScore: 45,
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

test("combined-board contract accepts challenge-count weights and exact weighted means", () => {
  assert.equal(validCombinedBoard(board()), true);
});

test("combined-board contract rejects field-relative weights and fabricated totals", () => {
  const badWeight = structuredClone(board());
  badWeight.modes.jeopardy.weight = 0.5;
  assert.equal(validCombinedBoard(badWeight), false);

  const badMean = structuredClone(board());
  badMean.items[0].score = 75;
  assert.equal(validCombinedBoard(badMean), false);

  const overflow = structuredClone(board());
  overflow.items[0].components.koth.score = 101;
  assert.equal(validCombinedBoard(overflow), false);

  const invalidCount = structuredClone(board());
  invalidCount.modes.koth.challengeCount = 1.5;
  assert.equal(validCombinedBoard(invalidCount), false);
});
