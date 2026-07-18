import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { cheatRetentionPolicy } from "../cheat-retention.js";
import {
  CHEAT_INTEGRITY_KEYS,
  CHEAT_RESULT_SCHEMA_VERSION,
  mergeCheatResult,
  readCheatResult,
  sanitizeCheatResult,
  validateCheatResultForRun,
  writeCheatResult,
} from "../cheat-result.js";

const runId = "4ecbded0-c199-40c2-8f34-da9606a98967";
const integrity = Object.fromEntries(
  CHEAT_INTEGRITY_KEYS.map((key) => [key, true]),
);
const result = {
  schemaVersion: CHEAT_RESULT_SCHEMA_VERSION,
  runId,
  gameId: 44,
  eventCreatedAtMs: 1_700_000_000_000,
  challengeId: 149,
  completed: true,
  completedAtMs: 1_700_000_123_000,
  offenderPids: [1, 2, 3, 4, 5, 6],
  cleanControlCount: 94,
  suspicionRows: 16,
  cleanContextCount: 10,
  integrity,
};
const expected = {
  runId,
  gameId: result.gameId,
  eventCreatedAtMs: result.eventCreatedAtMs,
  childStartedAtMs: result.completedAtMs - 60_000,
  observedAtMs: result.completedAtMs + 1_000,
  fleetParticipationIds: Array.from({ length: 100 }, (_, index) => index + 1),
  gameChallengeIds: [100, result.challengeId],
};

test("writes and reads only a schema-v3 run-bound result", () => {
  const directory = mkdtempSync(join(tmpdir(), "rsctf-cheat-result-test-"));
  try {
    const path = join(directory, "result.json");
    writeCheatResult(path, result);
    assert.deepEqual(readCheatResult(path), result);
    assert.equal(readFileSync(path, "utf8").includes("secret"), false);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("publishes the result exclusively instead of replacing prior evidence", () => {
  const directory = mkdtempSync(join(tmpdir(), "rsctf-cheat-result-test-"));
  try {
    const path = join(directory, "result.json");
    writeCheatResult(path, result);
    assert.throws(
      () => writeCheatResult(path, { ...result, suspicionRows: 17 }),
      /EEXIST/,
    );
    assert.deepEqual(readCheatResult(path), result);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("rejects extra fields, legacy schemas, and non-numeric identifiers", () => {
  assert.throws(
    () => sanitizeCheatResult({ ...result, secret: "must-not-leak" }),
    /unexpected fields/,
  );
  assert.throws(
    () => sanitizeCheatResult({ ...result, schemaVersion: 1 }),
    /schema-v3/,
  );
  assert.throws(
    () => sanitizeCheatResult({ ...result, gameId: "44" }),
    /game id/,
  );
});

test("validates exact run, event, challenge, cohort, and execution window", () => {
  assert.deepEqual(validateCheatResultForRun(result, expected), result);
  assert.throws(
    () =>
      validateCheatResultForRun(result, {
        ...expected,
        runId: "wrong-run-identifier-0001",
      }),
    /different competition run/,
  );
  assert.throws(
    () =>
      validateCheatResultForRun(result, {
        ...expected,
        gameChallengeIds: [100],
      }),
    /challenge does not belong/,
  );
  assert.throws(
    () =>
      validateCheatResultForRun(result, {
        ...expected,
        observedAtMs: result.completedAtMs - 10_000,
      }),
    /execution window/,
  );
  assert.throws(
    () =>
      validateCheatResultForRun(
        { ...result, offenderPids: [1, 2, 3, 4, 5, 101] },
        expected,
      ),
    /offenders from the frozen fleet/,
  );
  for (const cleanControlCount of [1, 93, 95]) {
    assert.throws(
      () =>
        validateCheatResultForRun({ ...result, cleanControlCount }, expected),
      /every non-offender in the frozen fleet/,
    );
  }
});

test("requires the complete passing semantic integrity contract", () => {
  const failedIntegrity = { ...integrity, "stolen submissions": false };
  assert.throws(
    () =>
      validateCheatResultForRun(
        { ...result, integrity: failedIntegrity },
        expected,
      ),
    /complete passing integrity contract/,
  );
  const missingIntegrity = { ...integrity };
  delete missingIntegrity[CHEAT_INTEGRITY_KEYS[0]];
  assert.throws(
    () =>
      validateCheatResultForRun(
        { ...result, integrity: missingIntegrity },
        expected,
      ),
    /complete passing integrity contract/,
  );
  assert.throws(
    () =>
      validateCheatResultForRun({ ...result, cleanContextCount: 95 }, expected),
    /clean-context count/,
  );
  assert.throws(
    () => validateCheatResultForRun({ ...result, suspicionRows: 5 }, expected),
    /fewer suspicion rows/,
  );
});

test("parent merge validates first and preserves fresh manifest fields", () => {
  const merged = mergeCheatResult(
    {
      mixGame: result.gameId,
      createdAtMs: result.eventCreatedAtMs,
      teamEvidenceDir: "/tmp/current-parent-value",
      retained: true,
    },
    result,
    cheatRetentionPolicy({
      RSCTF_INTEGRATED_CHEAT_CHILD: "1",
      RETAIN_EVENT: "1",
    }),
    expected,
  );
  assert.equal(merged.teamEvidenceDir, "/tmp/current-parent-value");
  assert.equal(merged.cheatSimulation.challengeId, result.challengeId);
  assert.equal(merged.cheatSimulation.runId, runId);
  assert.equal(merged.cheatSimulation.completed, true);
  assert.equal(merged.cheatSimulation.retained, true);
  assert.equal("gameId" in merged.cheatSimulation, false);

  assert.throws(
    () =>
      mergeCheatResult(
        { mixGame: 45, createdAtMs: result.eventCreatedAtMs },
        result,
        cheatRetentionPolicy({ RSCTF_INTEGRATED_CHEAT_CHILD: "1" }),
        expected,
      ),
    /different lifecycle event/,
  );
});
