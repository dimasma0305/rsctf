import assert from "node:assert/strict";
import test from "node:test";

import {
  lifecycleStateBasename,
  lifecycleStateBasenameFromPath,
  lifecycleStateOpenPath,
} from "../lifecycle-state-file.js";

test("lifecycle manifest selection preserves the default and an isolated tag", () => {
  assert.equal(lifecycleStateBasename(), ".lifecycle-state.json");
  assert.equal(
    lifecycleStateBasenameFromPath(
      "/workspace/tests/load/.lifecycle-state-replica-smoke.json",
    ),
    ".lifecycle-state-replica-smoke.json",
  );
  assert.equal(
    lifecycleStateOpenPath(".lifecycle-state-replica-smoke.json"),
    "../.lifecycle-state-replica-smoke.json",
  );
});

test("lifecycle manifest selection rejects paths and malformed basenames", () => {
  for (const value of [
    "../.lifecycle-state.json",
    "/tmp/.lifecycle-state.json",
    ".lifecycle-state-Replica.json",
    ".lifecycle-state-.json",
    `.lifecycle-state-${"a".repeat(33)}.json`,
    "state.json",
    "",
  ]) {
    assert.throws(
      () => lifecycleStateBasename(value),
      /valid lifecycle manifest basename/,
    );
  }
});
