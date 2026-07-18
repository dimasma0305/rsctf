import assert from "node:assert/strict";
import test from "node:test";

import { freezeCheatCohort } from "../cheat-cohort.js";

test("freezes all 94 non-offenders even when ordinary play already flagged controls", () => {
  const roster = Array.from({ length: 100 }, (_, index) => index + 1);
  const cohort = freezeCheatCohort(roster, [1, 8, 55, 100], 6);
  const offenderIds = cohort.offenderIndices.map((index) => roster[index]);
  const cleanIds = cohort.cleanIndices.map((index) => roster[index]);

  assert.deepEqual(offenderIds, [2, 3, 4, 5, 6, 7]);
  assert.equal(cleanIds.length, 94);
  assert.deepEqual(
    [1, 8, 55, 100].filter((participationId) =>
      cleanIds.includes(participationId),
    ),
    [1, 8, 55, 100],
  );
  assert.equal(new Set([...offenderIds, ...cleanIds]).size, roster.length);
  assert.equal(Object.isFrozen(cohort.cleanIndices), true);
});

test("standalone rosters freeze their exact non-offender complement", () => {
  const roster = Array.from({ length: 107 }, (_, index) => index + 10);
  const cohort = freezeCheatCohort(roster, [], 6);
  assert.equal(cohort.offenderIndices.length, 6);
  assert.equal(cohort.cleanIndices.length, 101);
});

test("rejects malformed partitions and too few fresh detector actors", () => {
  assert.throws(
    () => freezeCheatCohort([1, 2, 3, 4, 5, 6, 7], [1, 2], 6),
    /only 5 are available/,
  );
  assert.throws(
    () => freezeCheatCohort([1, 2, 2, 3, 4, 5, 6, 7], [], 6),
    /distinct participation ids/,
  );
  assert.throws(
    () => freezeCheatCohort([1, 2, 3, 4, 5, 6, 7], [99], 6),
    /unknown participation/,
  );
});
