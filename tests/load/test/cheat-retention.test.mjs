import assert from "node:assert/strict";
import test from "node:test";
import {
  cheatRetentionPolicy,
  inheritedCheatOrchestrationToken,
  recordCheatSimulation,
} from "../cheat-retention.js";

test("standalone cheat drills retain their evidence", () => {
  assert.deepEqual(cheatRetentionPolicy({}), {
    integrated: false,
    retainNamespace: true,
  });
});

test("embedded cheat drills inherit the parent retention decision", () => {
  assert.equal(
    cheatRetentionPolicy({ RSCTF_INTEGRATED_CHEAT_CHILD: "1", KEEP: "0" })
      .retainNamespace,
    false,
  );
  assert.equal(
    cheatRetentionPolicy({
      RSCTF_INTEGRATED_CHEAT_CHILD: "1",
      KEEP: "1",
      RETAIN_EVENT: "1",
    }).retainNamespace,
    true,
  );
});

test("standalone drills own the lease while embedded drills require the parent token", () => {
  const token = "parent-orchestration-token";
  assert.equal(
    inheritedCheatOrchestrationToken({
      RSCTF_LOAD_ORCHESTRATION_LOCK_TOKEN: token,
    }),
    null,
  );
  assert.equal(
    inheritedCheatOrchestrationToken({
      RSCTF_INTEGRATED_CHEAT_CHILD: "1",
      RSCTF_LOAD_ORCHESTRATION_LOCK_TOKEN: token,
    }),
    token,
  );
  assert.throws(
    () =>
      inheritedCheatOrchestrationToken({
        RSCTF_INTEGRATED_CHEAT_CHILD: "1",
      }),
    /parent's process-lock token/,
  );
});

test("embedded evidence does not independently protect a disposable namespace", () => {
  const state = recordCheatSimulation(
    { mixGame: 44 },
    { challengeId: 149, completed: true },
    cheatRetentionPolicy({ RSCTF_INTEGRATED_CHEAT_CHILD: "1" }),
  );
  assert.equal(state.retained, undefined);
  assert.equal(state.cheatSimulation.retained, false);
});

test("an existing retention guard is never removed", () => {
  const state = recordCheatSimulation(
    { mixGame: 44, retained: true },
    { completed: true },
    cheatRetentionPolicy({ RSCTF_INTEGRATED_CHEAT_CHILD: "1" }),
  );
  assert.equal(state.retained, true);
  assert.equal(state.cheatSimulation.retained, true);
});
