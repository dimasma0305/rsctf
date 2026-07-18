import {
  chmodSync,
  linkSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { isAbsolute } from "node:path";

import { recordCheatSimulation } from "./cheat-retention.js";

export const CHEAT_RESULT_SCHEMA_VERSION = 3;
export const CHEAT_OFFENDER_COUNT = 6;
export const CHEAT_INTEGRITY_KEYS = Object.freeze([
  "stolen submissions",
  "distinct stolen actors and answers",
  "brute-force submissions",
  "distinct brute-force answers",
  "honeypot row count",
  "honeypot bait coverage",
  "current stolen-flag evidence",
  "current high-wrong-rate evidence",
  "current automated-pattern evidence",
  "current honeypot-hit evidence",
  "current honeypot-chain evidence",
  "duplicate suspicion evidence",
  "clean-control actionable suspicion",
  "suspicion score matches evidence",
]);

const TOP_LEVEL_KEYS = new Set([
  "schemaVersion",
  "runId",
  "gameId",
  "eventCreatedAtMs",
  "challengeId",
  "completed",
  "completedAtMs",
  "offenderPids",
  "cleanControlCount",
  "suspicionRows",
  "cleanContextCount",
  "integrity",
]);
const RUN_ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_-]{14,126}[A-Za-z0-9]$/;

function integer(value, label, minimum = 0) {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < minimum) {
    throw new Error(`invalid anti-cheat ${label}: ${value}`);
  }
  return value;
}

function runId(value, label = "run id") {
  if (typeof value !== "string" || !RUN_ID_PATTERN.test(value)) {
    throw new Error(`invalid anti-cheat ${label}`);
  }
  return value;
}

function resultPath(value) {
  if (typeof value !== "string" || !isAbsolute(value) || value.length < 2) {
    throw new Error("the embedded anti-cheat result path must be an absolute path supplied by its parent");
  }
  return value;
}

function exactIntegerSet(values, label) {
  if (!Array.isArray(values) || values.length === 0) {
    throw new Error(`anti-cheat ${label} must be a non-empty array`);
  }
  const parsed = values.map((value) => integer(value, label, 1));
  if (new Set(parsed).size !== parsed.length) {
    throw new Error(`anti-cheat ${label} must contain distinct ids`);
  }
  return parsed;
}

export function sanitizeCheatResult(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("the anti-cheat result must be an object");
  }
  const unknown = Object.keys(value).filter((key) => !TOP_LEVEL_KEYS.has(key));
  if (unknown.length) {
    throw new Error(`the anti-cheat result contains unexpected fields: ${unknown.join(", ")}`);
  }
  if (value.schemaVersion !== CHEAT_RESULT_SCHEMA_VERSION || value.completed !== true) {
    throw new Error("the anti-cheat result is not a completed schema-v3 artifact");
  }

  const offenderPids = exactIntegerSet(value.offenderPids, "offender participations");
  if (!value.integrity || typeof value.integrity !== "object" || Array.isArray(value.integrity)) {
    throw new Error("the anti-cheat result needs an integrity map");
  }
  const integrity = Object.fromEntries(Object.entries(value.integrity));
  if (Object.keys(integrity).length === 0 || Object.values(integrity).some((passed) => typeof passed !== "boolean")) {
    throw new Error("the anti-cheat integrity map must contain boolean checks");
  }

  return {
    schemaVersion: CHEAT_RESULT_SCHEMA_VERSION,
    runId: runId(value.runId),
    gameId: integer(value.gameId, "game id", 1),
    eventCreatedAtMs: integer(value.eventCreatedAtMs, "event creation time", 1),
    challengeId: integer(value.challengeId, "challenge id", 1),
    completed: true,
    completedAtMs: integer(value.completedAtMs, "completion time", 1),
    offenderPids,
    cleanControlCount: integer(value.cleanControlCount, "clean control count"),
    suspicionRows: integer(value.suspicionRows, "suspicion row count"),
    cleanContextCount: integer(value.cleanContextCount, "clean context count"),
    integrity,
  };
}

export function validateCheatResultForRun(value, expected) {
  const artifact = sanitizeCheatResult(value);
  if (!expected || typeof expected !== "object" || Array.isArray(expected)) {
    throw new Error("anti-cheat run expectations must be an object");
  }

  const expectedRunId = runId(expected.runId, "expected run id");
  const expectedGameId = integer(expected.gameId, "expected game id", 1);
  const expectedEventCreatedAtMs = integer(expected.eventCreatedAtMs, "expected event creation time", 1);
  const childStartedAtMs = integer(expected.childStartedAtMs, "child start time", 1);
  const observedAtMs = integer(expected.observedAtMs, "result observation time", childStartedAtMs);
  const clockSkewMs =
    expected.clockSkewMs === undefined
      ? 5_000
      : integer(expected.clockSkewMs, "allowed clock skew");
  if (clockSkewMs > 60_000) {
    throw new Error("anti-cheat allowed clock skew must not exceed 60000 ms");
  }
  const fleetParticipationIds = exactIntegerSet(
    expected.fleetParticipationIds,
    "expected fleet participations",
  );
  const gameChallengeIds = new Set(
    exactIntegerSet(expected.gameChallengeIds, "expected game challenges"),
  );

  if (
    artifact.runId !== expectedRunId ||
    artifact.gameId !== expectedGameId ||
    artifact.eventCreatedAtMs !== expectedEventCreatedAtMs
  ) {
    throw new Error("the embedded anti-cheat result belongs to a different competition run");
  }
  if (!gameChallengeIds.has(artifact.challengeId)) {
    throw new Error("the embedded anti-cheat challenge does not belong to the expected game");
  }
  if (
    artifact.completedAtMs < childStartedAtMs - clockSkewMs ||
    artifact.completedAtMs > observedAtMs + clockSkewMs
  ) {
    throw new Error("the embedded anti-cheat completion time is outside the child execution window");
  }

  const fleet = new Set(fleetParticipationIds);
  if (
    artifact.offenderPids.length !== CHEAT_OFFENDER_COUNT ||
    artifact.offenderPids.some((participationId) => !fleet.has(participationId))
  ) {
    throw new Error(`the anti-cheat result must identify ${CHEAT_OFFENDER_COUNT} offenders from the frozen fleet`);
  }
  if (
    artifact.cleanControlCount < 1 ||
    artifact.cleanControlCount + artifact.offenderPids.length !== fleetParticipationIds.length
  ) {
    throw new Error("the anti-cheat result must cover every non-offender in the frozen fleet");
  }
  if (artifact.cleanContextCount > artifact.cleanControlCount) {
    throw new Error("the anti-cheat clean-context count exceeds the clean-control cohort");
  }
  if (artifact.suspicionRows < artifact.offenderPids.length) {
    throw new Error("the anti-cheat report has fewer suspicion rows than simulated offenders");
  }

  const integrityKeys = Object.keys(artifact.integrity).sort();
  const expectedIntegrityKeys = [...CHEAT_INTEGRITY_KEYS].sort();
  if (
    integrityKeys.length !== expectedIntegrityKeys.length ||
    integrityKeys.some((key, index) => key !== expectedIntegrityKeys[index]) ||
    Object.values(artifact.integrity).some((passed) => passed !== true)
  ) {
    throw new Error("the anti-cheat result does not contain the complete passing integrity contract");
  }

  return artifact;
}

export function writeCheatResult(path, value) {
  const destination = resultPath(path);
  const sanitized = sanitizeCheatResult(value);
  const temporary = `${destination}.${process.pid}.tmp`;
  try {
    writeFileSync(temporary, `${JSON.stringify(sanitized, null, 2)}\n`, {
      flag: "wx",
      mode: 0o600,
    });
    chmodSync(temporary, 0o600);
    // A hard link publishes the complete file atomically and, unlike rename,
    // refuses to replace evidence left by another process or run.
    linkSync(temporary, destination);
  } finally {
    rmSync(temporary, { force: true });
  }
  return sanitized;
}

export function readCheatResult(path) {
  const destination = resultPath(path);
  let decoded;
  try {
    decoded = JSON.parse(readFileSync(destination, "utf8"));
  } catch (error) {
    throw new Error(`cannot read the embedded anti-cheat result: ${error.message}`);
  }
  return sanitizeCheatResult(decoded);
}

export function mergeCheatResult(state, artifact, policy, expected) {
  const validated = validateCheatResultForRun(artifact, expected);
  if (
    Number(state?.mixGame) !== validated.gameId ||
    Number(state?.createdAtMs) !== validated.eventCreatedAtMs
  ) {
    throw new Error("the embedded anti-cheat result belongs to a different lifecycle event");
  }
  const {
    schemaVersion: _schemaVersion,
    gameId: _gameId,
    eventCreatedAtMs: _eventCreatedAtMs,
    ...simulation
  } = validated;
  return recordCheatSimulation(state, simulation, policy);
}
