import assert from "node:assert/strict";
import test from "node:test";

import {
  abortedLifecycleState,
  assertLifecycleRunClaimable,
  cleanupFailureState,
  shouldResumeOfficialScoring,
} from "../lifecycle-recovery.js";

test("the original run failure is recorded before external cleanup begins", () => {
  const recovered = abortedLifecycleState(
    { simulationStatus: "running", mixGame: 94 },
    new Error("boom"),
    10,
  );
  assert.deepEqual(recovered, {
    simulationStatus: "aborted",
    mixGame: 94,
    abortedAtMs: 10,
    abortReason: "boom",
  });
});

test("combined run and cleanup failures stay aborted with recovery details", () => {
  const recovered = cleanupFailureState(
    { simulationStatus: "running", abortReason: "original" },
    ["remove clients: unavailable"],
    true,
    20,
  );
  assert.equal(recovered.simulationStatus, "aborted");
  assert.equal(recovered.cleanupIncomplete, true);
  assert.deepEqual(recovered.cleanupErrors, ["remove clients: unavailable"]);
  assert.equal(recovered.abortReason, "original");
});

test("cleanup-only failures use a distinct status and reject empty evidence", () => {
  assert.equal(
    cleanupFailureState(
      { simulationStatus: "running" },
      ["remove fleet: failed"],
      false,
      30,
    ).simulationStatus,
    "cleanup-failed",
  );
  assert.throws(() => cleanupFailureState({}, [], false, 30), /non-empty/);
});

test("official scoring resumes only after a successful run", () => {
  assert.equal(shouldResumeOfficialScoring(false), true);
  assert.equal(shouldResumeOfficialScoring(true), false);
  assert.throws(() => shouldResumeOfficialScoring(null), /must be boolean/);
});

test("only a fresh provision or an interrupted running manifest can be claimed", () => {
  assert.doesNotThrow(() => assertLifecycleRunClaimable({ mixGame: 10 }));
  assert.doesNotThrow(() =>
    assertLifecycleRunClaimable({ mixGame: 10, simulationStatus: "running" }),
  );
  for (const status of ["completed", "aborted", "cleanup-failed"]) {
    assert.throws(
      () =>
        assertLifecycleRunClaimable({ mixGame: 10, simulationStatus: status }),
      /terminal/,
    );
  }
});
