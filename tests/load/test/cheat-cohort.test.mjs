import assert from "node:assert/strict";
import test from "node:test";

import { freezeCheatCohort } from "../cheat-cohort.js";

test("freezes all 95 non-offenders even when ordinary play already flagged controls", () => {
  const roster = Array.from({ length: 100 }, (_, index) => index + 1);
  const cohort = freezeCheatCohort(roster, [1, 8, 55, 100], 5);
  const offenderIds = cohort.offenderIndices.map((index) => roster[index]);
  const cleanIds = cohort.cleanIndices.map((index) => roster[index]);

  assert.deepEqual(offenderIds, [2, 3, 4, 5, 6]);
  assert.equal(cleanIds.length, 95);
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
  const cohort = freezeCheatCohort(roster, [], 5);
  assert.equal(cohort.offenderIndices.length, 5);
  assert.equal(cohort.cleanIndices.length, 102);
});

test("rejects malformed partitions and too few fresh detector actors", () => {
  assert.throws(
    () => freezeCheatCohort([1, 2, 3, 4, 5, 6], [1, 2], 5),
    /only 4 are available/,
  );
  assert.throws(
    () => freezeCheatCohort([1, 2, 2, 3, 4, 5, 6], [], 5),
    /distinct participation ids/,
  );
  assert.throws(
    () => freezeCheatCohort([1, 2, 3, 4, 5, 6], [99], 5),
    /unknown participation/,
  );
});
