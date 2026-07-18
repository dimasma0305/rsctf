import assert from "node:assert/strict";
import { test } from "node:test";

import {
  auditDefenseRecovery,
  auditKothPatchOperationFailures,
  auditOrdinalScoreboard,
  adScoreRangeFloor,
  populatedTimeBuckets,
  specialtyLiftFloor,
  specialtyLift,
} from "../competition-gates.js";

test("A&D score-range gates account for the fixed single-hill budget", () => {
  assert.equal(adScoreRangeFloor(true), 3);
  assert.equal(adScoreRangeFloor(false), 0.5);
  assert.throws(() => adScoreRangeFloor("true"), /boolean/);
});

test("defense recovery requires a strong rate without erasing late incidents", () => {
  assert.deepEqual(auditDefenseRecovery(40, 38), {
    valid: true,
    incidents: 40,
    repairs: 38,
    unresolved: 2,
    rate: 0.95,
    required: 36,
    missing: 0,
  });
  assert.equal(auditDefenseRecovery(10, 8).valid, false);
  assert.equal(auditDefenseRecovery(1, 0).valid, false);
  assert.equal(auditDefenseRecovery(0, 0).valid, true);
  assert.equal(auditDefenseRecovery(3, 4).valid, false);
  assert.throws(() => auditDefenseRecovery(3.5, 3), /invalid/);
});

test("KotH patch races are bounded without silently accepting broad failure", () => {
  assert.deepEqual(auditKothPatchOperationFailures(30, 5, 10, 2), {
    valid: true,
    attempts: 40,
    failures: 7,
    rate: 0.175,
    maximumRate: 0.25,
  });
  assert.equal(auditKothPatchOperationFailures(10, 3, 2, 1).valid, false);
  assert.equal(auditKothPatchOperationFailures(0, 0, 0, 0).valid, true);
  assert.throws(() => auditKothPatchOperationFailures(2, 3, 0, 0), /invalid/);
});

test("short KotH runs do not preselect a specialist winner", () => {
  assert.equal(specialtyLiftFloor("koth", false), 0);
  assert.equal(specialtyLiftFloor("offense", false), 0.8);
  assert.equal(specialtyLiftFloor("koth", true), 1);
  assert.equal(specialtyLiftFloor("jeopardy", true), 1);
  assert.throws(() => specialtyLiftFloor("unknown", true), /unknown specialty/);
});

test("A&D audit enforces the public comparator and exact ordinal ranks", () => {
  const rows = [
    {
      rank: 1,
      participationId: 4,
      settledTotal: 50,
      projectedTotal: 60,
      offenseRate: 0.8,
      defenseRate: 0.7,
      slaRate: 0.9,
    },
    {
      rank: 2,
      participationId: 3,
      settledTotal: 50,
      projectedTotal: 60,
      offenseRate: 0.8,
      defenseRate: 0.7,
      slaRate: 0.8,
    },
    {
      rank: 3,
      participationId: 8,
      settledTotal: 40,
      projectedTotal: 45,
      offenseRate: 0.9,
      defenseRate: 0.4,
      slaRate: 1,
    },
  ];
  assert.equal(
    auditOrdinalScoreboard("ad", { teams: rows }, [3, 4, 8]).valid,
    true,
  );
  const invalid = structuredClone(rows);
  [invalid[0], invalid[1]] = [invalid[1], invalid[0]];
  invalid[0].rank = 1;
  invalid[1].rank = 2;
  assert.equal(
    auditOrdinalScoreboard("ad", { teams: invalid }, [3, 4, 8]).valid,
    false,
  );
});

test("KotH audit uses control, reliability, acquisitions, then participation id", () => {
  const row = (
    rank,
    participationId,
    controlRate,
    reliabilityRate,
    acquisitions,
  ) => ({
    rank,
    participationId,
    settledTotal: 25,
    controlRate,
    reliabilityRate,
    hills: [{ acquisitionWindows: acquisitions }],
  });
  const board = {
    teams: [
      row(1, 7, 0.8, 0.7, 1),
      row(2, 4, 0.8, 0.6, 5),
      row(3, 2, 0.7, 1, 9),
    ],
  };
  assert.equal(auditOrdinalScoreboard("koth", board, [2, 4, 7]).valid, true);
});

test("Jeopardy audit rejects shared displayed ranks even when scores tie", () => {
  const board = {
    items: [
      { id: 5, rank: 1, score: 1000, lastSubmissionTime: 100 },
      { id: 8, rank: 1, score: 1000, lastSubmissionTime: 200 },
    ],
  };
  const audit = auditOrdinalScoreboard("jeopardy", board, [5, 8]);
  assert.equal(audit.valid, false);
  assert.match(audit.errors.join(" "), /ordinal/);
});

test("time buckets ignore evidence outside the exact event window", () => {
  assert.deepEqual(
    populatedTimeBuckets([-1, 0, 250, 400, 799, 1000], 0, 1000, 5),
    {
      populated: 4,
      buckets: [0, 1, 2, 3],
    },
  );
});

test("specialty lift compares a specialist cohort with the complete field", () => {
  const profiles = [
    { index: 0, specialty: "offense" },
    { index: 1, specialty: "offense" },
    { index: 2, specialty: "defense" },
    { index: 3, specialty: "koth" },
  ];
  const result = specialtyLift(
    profiles,
    new Map([
      [0, 8],
      [1, 6],
      [2, 2],
      [3, 4],
    ]),
    "offense",
  );
  assert.equal(result.specialtyMean, 7);
  assert.equal(result.fieldMean, 5);
  assert.equal(result.lift, 1.4);
});
