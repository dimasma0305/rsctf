import assert from "node:assert/strict";
import test from "node:test";

import { adoptPausedCompetitionReadiness } from "../competition-readiness.js";

const exact = {
  liveRound: 4,
  requestedServices: 100,
  plantedFlags: 100,
  deliveredFlags: 100,
  verifiedFlags: 100,
};

test("adopts the exact provisioned round while realistic scoring remains paused", () => {
  assert.deepEqual(
    adoptPausedCompetitionReadiness({
      state: { scoringPausedAfterReadiness: true, readinessRound: 4 },
      realisticCompetition: true,
      scoringPaused: true,
      fleetAdoptable: true,
      epoch: { liveRound: 4 },
      evidence: exact,
      expectedServices: 100,
    }),
    exact,
  );
});

test("leaves capacity manifests on the existing fresh-round path", () => {
  for (const input of [
    {
      realisticCompetition: false,
      scoringPaused: true,
      state: { scoringPausedAfterReadiness: true },
    },
    {
      realisticCompetition: true,
      scoringPaused: false,
      state: { scoringPausedAfterReadiness: true },
    },
    { realisticCompetition: true, scoringPaused: true, state: {} },
  ]) {
    assert.equal(
      adoptPausedCompetitionReadiness({
        ...input,
        fleetAdoptable: true,
        epoch: { liveRound: 4 },
        evidence: exact,
        expectedServices: 100,
      }),
      null,
    );
  }
});

test("rejects a stale fleet or a changed paused round", () => {
  const base = {
    state: { scoringPausedAfterReadiness: true, readinessRound: 4 },
    realisticCompetition: true,
    scoringPaused: true,
    fleetAdoptable: true,
    epoch: { liveRound: 4 },
    evidence: exact,
    expectedServices: 100,
  };
  assert.throws(
    () => adoptPausedCompetitionReadiness({ ...base, fleetAdoptable: false }),
    /no longer adoptable/,
  );
  assert.throws(
    () =>
      adoptPausedCompetitionReadiness({
        ...base,
        epoch: { liveRound: 5 },
        evidence: { ...exact, liveRound: 5 },
      }),
    /no longer matches/,
  );
});
