import assert from "node:assert/strict";
import test from "node:test";
import { validVisibleChallengeProjection } from "../details-read-model.js";

const details = (overrides = {}) => ({
  challenges: {
    Misc: [{ id: 1 }, { id: 2 }],
    Web: [{ id: 3 }],
  },
  challengeCount: 3,
  rank: { solvedCount: 2, solvedChallenges: [{ id: 1 }, { id: 3 }] },
  ...overrides,
});

test("challenge-details load validation counts visible challenges rather than categories", () => {
  assert.equal(validVisibleChallengeProjection(details()), true);
  assert.equal(
    validVisibleChallengeProjection(details({ challengeCount: 2 })),
    false,
  );
});

test("challenge-details load validation rejects solves outside the visible projection", () => {
  assert.equal(
    validVisibleChallengeProjection(
      details({
        rank: { solvedCount: 2, solvedChallenges: [{ id: 1 }, { id: 99 }] },
      }),
    ),
    false,
  );
  assert.equal(
    validVisibleChallengeProjection(
      details({
        challenges: {},
        challengeCount: 0,
        rank: { solvedCount: 0, solvedChallenges: [] },
      }),
    ),
    true,
  );
});
