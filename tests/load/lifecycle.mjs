// Whole-platform stress run: preflight → (provision) → official epoch readiness →
// k6 lifecycle load + liveness/readiness samplers → integrity checks → teardown.
//   npm run provision && VUS=400 DURATION=300s npm run lifecycle
//   KEEP=1 ... npm run lifecycle    # leave state + games up
import { spawn, execFileSync } from "node:child_process";
import { lstatSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import * as A from "./applib.mjs";
import { lifecycleStateBasenameFromPath } from "./lifecycle-state-file.js";
import { selectKothCapacityClaimant } from "./lifecycle-load-model.js";
import { readCheatResult, mergeCheatResult } from "./cheat-result.js";
import { cheatRetentionPolicy } from "./cheat-retention.js";
import {
  adScoreRangeFloor,
  auditDefenseRecovery,
  auditKothPatchOperationFailures,
  auditOrdinalScoreboard,
  specialtyLiftFloor,
  specialtyLift,
} from "./competition-gates.js";
import { adoptPausedCompetitionReadiness } from "./competition-readiness.js";
import {
  assessKothDeadlineCleanup,
  kothDeadlineCleanupQuery,
} from "./koth-deadline-cleanup.js";
import { kothResetReceiptIntegrityQuery } from "./koth-reset-receipts.js";
import { sql, JWT_SECRET, RSCTF, mintJwt } from "./lib.mjs";
import { countContainerFatalLogs } from "./log-audit.mjs";
import {
  abortedLifecycleState,
  assertLifecycleRunClaimable,
  cleanupFailureState,
  shouldResumeOfficialScoring,
} from "./lifecycle-recovery.js";
import {
  kothCapturePendingBalance,
  kothCaptureStatusBalance,
} from "./player-model.js";
import {
  acquireExclusiveProcessLock,
  InFlightMutationDrain,
  loadOrchestrationLockPath,
  stopChildTree,
  waitForCompletion,
} from "./process-control.mjs";
import {
  aggregateTeamEvidence,
  expectedTeamEvidenceFilename,
  MAX_TEAM_RUNNER_LOG_BYTES,
  TEAM_RUNNER_LOG_FILENAME,
} from "./team-evidence.js";
import * as TeamClients from "./team-clients.mjs";

const VUS = process.env.VUS || 400;
const DURATION = process.env.DURATION || "90s";
const HOSTPORT = process.env.HOSTPORT || "127.0.0.1:8080";
const HEALTH_URL = process.env.HEALTH_URL || `http://${HOSTPORT}/livez`;
const READINESS_URL = process.env.READINESS_URL || `http://${HOSTPORT}/healthz`;
const REALISTIC_COMPETITION = process.env.REALISTIC_COMPETITION === "1";
const COMPETITION_SEED = process.env.SIMULATION_SEED || "rsctf-competitive-v2";
const K6_STATE_BASENAME = lifecycleStateBasenameFromPath(A.stateFile);

const COMPETITIVE_KOTH_TAG = "rsctf-load-koth:competitive-v1";

function competitiveKothImageMatches(stateImage) {
  if (stateImage === COMPETITIVE_KOTH_TAG) return true;
  try {
    const expectedDigest = execFileSync(
      "docker",
      ["image", "inspect", COMPETITIVE_KOTH_TAG, "--format", "{{.Id}}"],
      { encoding: "utf8" }
    ).trim();
    return expectedDigest && stateImage === expectedDigest;
  } catch {
    return false;
  }
}

let shutdownSignal = null;
let orchestrationLock = null;
const inFlightMutations = new InFlightMutationDrain();
let resolveShutdown;
const shutdownRequested = new Promise((resolve) => {
  resolveShutdown = resolve;
});
const shutdownHandlers = Object.fromEntries(
  ["SIGINT", "SIGTERM"].map((signal) => [
    signal,
    () => {
      if (shutdownSignal !== null) {
        process.removeListener(signal, shutdownHandlers[signal]);
        process.kill(process.pid, signal);
        return;
      }
      shutdownSignal = signal;
      resolveShutdown(signal);
      console.error(
        `\n  ${signal} received; stopping the event and cleaning up...`,
      );
    },
  ]),
);
for (const [signal, handler] of Object.entries(shutdownHandlers)) {
  process.on(signal, handler);
}

function throwIfShuttingDown() {
  if (shutdownSignal !== null) {
    throw shutdownError(shutdownSignal);
  }
}

function shutdownError(signal) {
  return new Error(`event interrupted by ${signal}`);
}

function interruptible(operation) {
  throwIfShuttingDown();
  return Promise.race([
    operation,
    shutdownRequested.then((signal) => {
      throw shutdownError(signal);
    }),
  ]);
}

async function ensureCapacityVpnPeer(state, participationId, readyPeers) {
  if (readyPeers.has(participationId)) return;
  const index = state.adPartIds.indexOf(participationId);
  const userId = state.adUsers[index];
  const securityStamp = userId ? state.userStamps[userId] : null;
  if (index < 0 || !userId || !securityStamp) {
    throw new Error(
      `capacity KotH claimant ${participationId} has no exact user identity`,
    );
  }
  const response = await A.api(
    "GET",
    `/api/Game/${Number(state.mixGame)}/Ad/Vpn/Config`,
    {
      jwt: mintJwt(userId, securityStamp, 1),
      ip: `10.8.${Math.floor(index / 254)}.${(index % 254) + 1}`,
      timeoutMs: 30_000,
    },
  );
  if (response.status !== 200) {
    throw new Error(
      `create capacity KotH VPN peer for participation ${participationId} ` +
        `returned ${response.status}: ${response.text.slice(0, 160)}`,
    );
  }
  readyPeers.add(participationId);
}

function interruptibleMutation(operation) {
  return interruptible(inFlightMutations.track(operation));
}

function removeShutdownHandlers() {
  for (const [signal, handler] of Object.entries(shutdownHandlers)) {
    process.removeListener(signal, handler);
  }
}

async function provisionLifecycleState(lockToken) {
  const child = spawn(process.execPath, ["provision.mjs"], {
    stdio: "inherit",
    cwd: new URL(".", import.meta.url).pathname,
    detached: true,
    env: {
      ...process.env,
      RSCTF_LOAD_ORCHESTRATION_LOCK_TOKEN: lockToken,
    },
  });
  const completion = new Promise((resolve) => {
    child.once("error", (error) =>
      resolve({ error, code: null, signal: null }),
    );
    child.once("close", (code, signal) =>
      resolve({ error: null, code, signal }),
    );
  });
  const interrupted = shutdownRequested.then(async (signal) => {
    await stopChildTree(child, { processGroup: true, graceMs: 30_000 });
    throw shutdownError(signal);
  });
  const result = await Promise.race([completion, interrupted]);
  if (result.error) throw result.error;
  if (result.code !== 0) {
    throw new Error(
      `lifecycle provision failed: ${result.signal || `status ${result.code}`}`,
    );
  }
}

function safePositiveInteger(value, name, minimum = 1) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum) {
    throw new Error(`${name} must be an integer >= ${minimum} (got ${value})`);
  }
  return parsed;
}

function durationSeconds(value) {
  const match = String(value)
    .trim()
    .match(/^(\d+(?:\.\d+)?)(ms|s|m|h)$/);
  if (!match) return 0;
  const scale = { ms: 0.001, s: 1, m: 60, h: 3600 }[match[2]];
  return Number(match[1]) * scale;
}

const healthProbe = (url) => {
  try {
    const o = execFileSync(
      "curl",
      [
        "-s",
        "-o",
        "/dev/null",
        "-m",
        "2",
        "-w",
        "%{http_code}:%{time_total}",
        url,
      ],
      {
        encoding: "utf8",
      },
    );
    const [c, t] = o.split(":");
    return { ok: c === "200", ms: parseFloat(t) * 1000 };
  } catch {
    return { ok: false, ms: 0 };
  }
};

const recordProbe = (probe, result) => {
  if (result.ok) {
    probe.ok++;
    probe.lat.push(result.ms);
  } else probe.fail++;
};

function attackEvidence(
  st,
  baselineId,
  participationIds,
  expectedPerRound,
  windowStartMs,
  windowEndMs,
) {
  const gameId = Number(st.mixGame);
  const floor = Number(baselineId);
  const pids = participationIds.map(Number);
  if (
    !Number.isSafeInteger(gameId) ||
    gameId <= 0 ||
    !Number.isSafeInteger(floor) ||
    floor < 0 ||
    pids.length < 2 ||
    pids.some((pid) => !Number.isSafeInteger(pid) || pid <= 0) ||
    !Number.isSafeInteger(windowStartMs) ||
    !Number.isSafeInteger(windowEndMs) ||
    windowEndMs <= windowStartMs
  ) {
    throw new Error("invalid distributed A&D evidence scope");
  }
  const raw = sql(
    `WITH captured AS (` +
      `SELECT attack.round_id,attack.attacker_participation_id,service.participation_id AS victim_participation_id,` +
      `attack.submitted_at ` +
      `FROM "AdAttacks" attack ` +
      `JOIN "AdRounds" round ON round.id=attack.round_id ` +
      `JOIN "AdTeamServices" service ON service.id=attack.victim_team_service_id ` +
      `JOIN "AdFlags" flag ON flag.id=attack.flag_id AND flag.round_id=attack.round_id ` +
      `WHERE attack.id>${floor} AND round.game_id=${gameId} ` +
      `AND attack.attacker_participation_id IN (${pids.join(",")}) ` +
      `AND service.participation_id IN (${pids.join(",")}) ` +
      `AND attack.submitted_at>=to_timestamp(${windowStartMs}/1000.0) ` +
      `AND attack.submitted_at<to_timestamp(${windowEndMs}/1000.0)` +
      `), per_round AS (` +
      `SELECT round_id,count(DISTINCT attacker_participation_id) AS attackers ` +
      `FROM captured GROUP BY round_id` +
      `), per_attacker AS (` +
      `SELECT attacker_participation_id,count(*) AS captures FROM captured GROUP BY attacker_participation_id` +
      `), per_victim AS (` +
      `SELECT victim_participation_id,count(*) AS losses FROM captured GROUP BY victim_participation_id` +
      `), per_pair AS (` +
      `SELECT attacker_participation_id,victim_participation_id,count(*) AS captures FROM captured ` +
      `GROUP BY attacker_participation_id,victim_participation_id` +
      `) SELECT json_build_object(` +
      `'accepted',count(*),` +
      `'attackers',count(DISTINCT attacker_participation_id),` +
      `'victims',count(DISTINCT victim_participation_id),` +
      `'pairs',count(DISTINCT (attacker_participation_id,victim_participation_id)),` +
      `'rounds',count(DISTINCT round_id),` +
      `'activeBuckets',count(DISTINCT width_bucket(extract(epoch FROM submitted_at)*1000,` +
      `${windowStartMs},${windowEndMs},6)) FILTER (WHERE submitted_at>=to_timestamp(${windowStartMs}/1000.0) ` +
      `AND submitted_at<to_timestamp(${windowEndMs}/1000.0)),` +
      `'completeRounds',(SELECT count(*) FROM per_round WHERE attackers=${Number(expectedPerRound)}),` +
      `'distinctAttackerCounts',(SELECT count(DISTINCT captures) FROM per_attacker),` +
      `'attackerMean',(SELECT COALESCE(avg(captures),0) FROM per_attacker),` +
      `'attackerStddev',(SELECT COALESCE(stddev_pop(captures),0) FROM per_attacker),` +
      `'attackerMin',(SELECT COALESCE(min(captures),0) FROM per_attacker),` +
      `'attackerMax',(SELECT COALESCE(max(captures),0) FROM per_attacker),` +
      `'distinctVictimCounts',(SELECT count(DISTINCT losses) FROM per_victim),` +
      `'victimMin',(SELECT COALESCE(min(losses),0) FROM per_victim),` +
      `'victimMax',(SELECT COALESCE(max(losses),0) FROM per_victim),` +
      `'repeatedPairs',(SELECT count(*) FROM per_pair WHERE captures>1),` +
      `'contestedVictimRounds',(SELECT count(*) FROM (` +
      `SELECT 1 FROM captured GROUP BY round_id,victim_participation_id ` +
      `HAVING count(DISTINCT attacker_participation_id)>1) contested)` +
      `)::text FROM captured`,
  );
  return JSON.parse(raw);
}

function jeopardyEvidence(st, baselineId, windowStartMs, windowEndMs) {
  const gameId = Number(st.jeoGame);
  const floor = Number(baselineId);
  const catalog = Array.isArray(st.jeopardyCatalog) ? st.jeopardyCatalog : [];
  const challengeIds =
    catalog.length > 0
      ? catalog.map((challenge) => Number(challenge.challengeId))
      : Object.keys(st.staticFlags || {}).map(Number);
  const attachmentIds = catalog
    .filter((challenge) => challenge?.kind === "attachment")
    .map((challenge) => Number(challenge.challengeId));
  const containerIds = catalog
    .filter((challenge) => challenge?.kind === "container")
    .map((challenge) => Number(challenge.challengeId));
  if (
    !Number.isSafeInteger(gameId) ||
    gameId <= 0 ||
    !Number.isSafeInteger(floor) ||
    floor < 0 ||
    challengeIds.length === 0 ||
    challengeIds.some((id) => !Number.isSafeInteger(id) || id <= 0) ||
    !Number.isSafeInteger(windowStartMs) ||
    !Number.isSafeInteger(windowEndMs) ||
    windowEndMs <= windowStartMs
  ) {
    throw new Error("invalid Jeopardy evidence scope");
  }
  const attachmentEvent =
    attachmentIds.length > 0
      ? `challenge_id IN (${attachmentIds.join(",")})`
      : "FALSE";
  const containerEvent =
    containerIds.length > 0
      ? `challenge_id IN (${containerIds.join(",")})`
      : "FALSE";
  return JSON.parse(
    sql(
      `WITH accepted_raw AS (` +
        `SELECT participation_id,challenge_id,submit_time_utc FROM "Submissions" ` +
        `WHERE game_id=${gameId} AND id>${floor} AND status=1 ` +
        `AND challenge_id IN (${challengeIds.join(",")}) ` +
        `AND submit_time_utc>=to_timestamp(${windowStartMs}/1000.0) ` +
        `AND submit_time_utc<to_timestamp(${windowEndMs}/1000.0)` +
        `), accepted AS (` +
        `SELECT participation_id,challenge_id,min(submit_time_utc) AS submit_time_utc ` +
        `FROM accepted_raw GROUP BY participation_id,challenge_id` +
        `), per_solver AS (` +
        `SELECT participation_id,count(*) AS solves FROM accepted GROUP BY participation_id` +
        `), journey_events AS (` +
        `SELECT "Type" AS event_type,team_id,` +
        `CASE WHEN (values->>0)~'^[0-9]+$' THEN (values->>0)::int END AS challenge_id ` +
        `FROM "GameEvents" WHERE game_id=${gameId} AND "Type" IN (1,2,5) ` +
        `AND json_typeof(values)='array' AND json_array_length(values)>0 ` +
        `AND (values->>0)~'^[0-9]+$' ` +
        `AND publish_time_utc>=to_timestamp(${windowStartMs}/1000.0) ` +
        `AND publish_time_utc<to_timestamp(${windowEndMs}/1000.0)` +
        `) SELECT json_build_object(` +
        `'accepted',(SELECT count(*) FROM accepted),` +
        `'solvers',(SELECT count(DISTINCT participation_id) FROM accepted),` +
        `'teamChallengePairs',(SELECT count(DISTINCT (participation_id,challenge_id)) FROM accepted),` +
        `'challenges',(SELECT count(DISTINCT challenge_id) FROM accepted),` +
        `'distinctSolveCounts',(SELECT count(DISTINCT solves) FROM per_solver),` +
        `'solverMin',(SELECT COALESCE(min(solves),0) FROM per_solver),` +
        `'solverMax',(SELECT COALESCE(max(solves),0) FROM per_solver),` +
        `'activeBuckets',(SELECT count(DISTINCT width_bucket(extract(epoch FROM submit_time_utc)*1000,` +
        `${windowStartMs},${windowEndMs},6)) FROM accepted ` +
        `WHERE submit_time_utc>=to_timestamp(${windowStartMs}/1000.0) ` +
        `AND submit_time_utc<to_timestamp(${windowEndMs}/1000.0)),` +
        `'attachmentDownloads',(SELECT count(DISTINCT (team_id,challenge_id)) ` +
        `FROM journey_events WHERE event_type=5 AND ${attachmentEvent}),` +
        `'containerStarts',(SELECT count(DISTINCT (team_id,challenge_id)) ` +
        `FROM journey_events WHERE event_type=1 AND ${containerEvent}),` +
        `'containerDestroys',(SELECT count(DISTINCT (team_id,challenge_id)) ` +
        `FROM journey_events WHERE event_type=2 AND ${containerEvent}),` +
        `'containerJourneys',(SELECT count(*) FROM (` +
        `SELECT team_id,challenge_id FROM journey_events WHERE ${containerEvent} ` +
        `GROUP BY team_id,challenge_id HAVING bool_or(event_type=1) AND bool_or(event_type=2)` +
        `) complete),` +
        `'wrong',(SELECT count(*) FROM "Submissions" WHERE game_id=${gameId} AND id>${floor} ` +
        `AND status=2 AND challenge_id IN (${challengeIds.join(",")}) ` +
        `AND submit_time_utc>=to_timestamp(${windowStartMs}/1000.0) ` +
        `AND submit_time_utc<to_timestamp(${windowEndMs}/1000.0))` +
        `)::text`,
    ),
  );
}

function validateTeamRunnerLog(path, label) {
  const runnerLog = lstatSync(path);
  if (
    runnerLog.isSymbolicLink() ||
    !runnerLog.isFile() ||
    runnerLog.size > MAX_TEAM_RUNNER_LOG_BYTES
  ) {
    throw new Error(
      `${label} runner log must be a regular file no larger than ` +
        `${MAX_TEAM_RUNNER_LOG_BYTES} bytes`,
    );
  }
}

function capacityTeamEvidenceSnapshot(directory, expectedCount, notBeforeMs) {
  const snapshot = {
    files: 0,
    malformed: 0,
    thresholdFailures: 0,
    requests: 0,
    flagSyncWaits: 0,
    activeIterations: 0,
    idleIterations: 0,
    exploitAttempts: 0,
    exploitPatched: 0,
    exploitCaptures: 0,
    defenseUpdates: 0,
    jeopardySubmissions: 0,
    defenseAdvancedTeams: 0,
    kothCaptureAttempts: 0,
    kothCaptureSuccesses: 0,
    kothWriters: 0,
    kothOpeningClaims: 0,
    kothTakeoverClaims: 0,
    kothResetRaces: 0,
    kothCaptureWindowClosed: 0,
    kothCaptureIneligibleTransitions: 0,
    kothCaptureStateUnavailable: 0,
    kothCaptureAttemptFailures: 0,
    kothCaptureRetryRecoveries: 0,
    kothCapturePendingStarts: 0,
    kothCaptureBurstExhaustions: 0,
    kothCaptureTerminalWindows: 0,
    kothCapturePendingInvariantFailures: 0,
    kothCaptureNetworkErrors: 0,
    kothCaptureHttp4xx: 0,
    kothCaptureHttp5xx: 0,
    kothCaptureOtherStatusFailures: 0,
    kothCaptureTargetUnavailable: 0,
    kothCapturePendingUnresolved: 0,
    kothCapturePendingBalanceErrors: 0,
    kothCaptureStatusBalanceErrors: 0,
    tiers: {},
  };
  let files = [];
  try {
    files = readdirSync(directory).filter((name) =>
      /^team-\d{3}\.json$/.test(name),
    );
  } catch {
    return snapshot;
  }
  const indices = new Set();
  for (const file of files) {
    try {
      validateTeamRunnerLog(
        join(directory, file.replace(/\.json$/, ".runner.log")),
        file,
      );
      const evidence = JSON.parse(readFileSync(`${directory}/${file}`, "utf8"));
      const index = Number(evidence?.team?.index);
      const generatedAt = Date.parse(evidence?.generatedAt);
      if (
        evidence?.schemaVersion !== (REALISTIC_COMPETITION ? 2 : 1) ||
        evidence?.team?.count !== expectedCount ||
        !Number.isSafeInteger(index) ||
        index < 0 ||
        index >= expectedCount ||
        indices.has(index) ||
        !Number.isFinite(generatedAt) ||
        generatedAt < notBeforeMs
      ) {
        snapshot.malformed++;
        continue;
      }
      const metricCount = (name) =>
        Number(evidence?.metrics?.[name]?.values?.count || 0);
      const hasPendingAccounting = Object.prototype.hasOwnProperty.call(
        evidence?.metrics || {},
        "koth_capture_pending_starts",
      );
      const pendingBalance = hasPendingAccounting
        ? kothCapturePendingBalance({
            started: metricCount("koth_capture_pending_starts"),
            recovered: metricCount("koth_capture_retry_recoveries"),
            resetRaces: metricCount("koth_reset_races"),
            windowClosed: metricCount("koth_capture_window_closed"),
            ineligibleTransitions: metricCount(
              "koth_capture_ineligible_transitions",
            ),
            invariantFailures: metricCount(
              "koth_capture_pending_invariant_failures",
            ),
            terminalWindows: metricCount("koth_capture_terminal_windows"),
          })
        : { unresolved: 0, valid: true };
      const statusBalance = hasPendingAccounting
        ? kothCaptureStatusBalance({
            attemptFailures: metricCount("koth_capture_attempt_failures"),
            networkErrors: metricCount("koth_capture_network_errors"),
            http4xx: metricCount("koth_capture_http_4xx"),
            http5xx: metricCount("koth_capture_http_5xx"),
            otherStatusFailures: metricCount(
              "koth_capture_other_status_failures",
            ),
          })
        : { valid: true };
      indices.add(index);
      if (evidence.thresholdsPassed !== true) snapshot.thresholdFailures++;
      snapshot.requests += Number(
        evidence?.metrics?.http_reqs?.values?.count || 0,
      );
      snapshot.flagSyncWaits += Number(
        evidence?.metrics?.flag_sync_waits?.values?.count || 0,
      );
      for (const [field, metric] of [
        ["activeIterations", "active_iterations"],
        ["idleIterations", "idle_iterations"],
        ["exploitAttempts", "exploit_attempts"],
        ["exploitPatched", "exploit_patched"],
        ["exploitCaptures", "exploit_captures"],
        ["defenseUpdates", "defense_updates"],
        ["jeopardySubmissions", "jeopardy_submissions"],
        ["kothCaptureAttempts", "koth_capture_attempts"],
        ["kothCaptureSuccesses", "koth_capture_successes"],
        ["kothOpeningClaims", "koth_opening_claims"],
        ["kothTakeoverClaims", "koth_takeover_claims"],
        ["kothResetRaces", "koth_reset_races"],
        ["kothCaptureWindowClosed", "koth_capture_window_closed"],
        [
          "kothCaptureIneligibleTransitions",
          "koth_capture_ineligible_transitions",
        ],
        ["kothCaptureStateUnavailable", "koth_capture_state_unavailable"],
        ["kothCaptureAttemptFailures", "koth_capture_attempt_failures"],
        ["kothCaptureRetryRecoveries", "koth_capture_retry_recoveries"],
        ["kothCapturePendingStarts", "koth_capture_pending_starts"],
        ["kothCaptureBurstExhaustions", "koth_capture_burst_exhaustions"],
        ["kothCaptureTerminalWindows", "koth_capture_terminal_windows"],
        [
          "kothCapturePendingInvariantFailures",
          "koth_capture_pending_invariant_failures",
        ],
        ["kothCaptureNetworkErrors", "koth_capture_network_errors"],
        ["kothCaptureHttp4xx", "koth_capture_http_4xx"],
        ["kothCaptureHttp5xx", "koth_capture_http_5xx"],
        [
          "kothCaptureOtherStatusFailures",
          "koth_capture_other_status_failures",
        ],
        ["kothCaptureTargetUnavailable", "koth_capture_target_unavailable"],
      ]) {
        snapshot[field] += metricCount(metric);
      }
      snapshot.kothCapturePendingUnresolved += Math.max(
        0,
        pendingBalance.unresolved,
      );
      if (!pendingBalance.valid) snapshot.kothCapturePendingBalanceErrors++;
      if (!statusBalance.valid) snapshot.kothCaptureStatusBalanceErrors++;
      if (Number(evidence?.metrics?.defense_updates?.values?.count || 0) >= 2) {
        snapshot.defenseAdvancedTeams++;
      }
      if (
        Number(evidence?.metrics?.koth_capture_successes?.values?.count || 0) >
        0
      ) {
        snapshot.kothWriters++;
      }
      if (typeof evidence?.profile?.tier === "string") {
        snapshot.tiers[evidence.profile.tier] =
          (snapshot.tiers[evidence.profile.tier] || 0) + 1;
      }
    } catch {
      snapshot.malformed++;
    }
  }
  snapshot.files = indices.size;
  return snapshot;
}

function competitiveTeamEvidenceSnapshot(directory, expected) {
  const entries = Array.from({ length: expected.teamCount }, (_, index) => {
    const filename = expectedTeamEvidenceFilename(index);
    const teamDirectory = join(directory, filename.slice(0, -5));
    const runnerLogPath = join(teamDirectory, TEAM_RUNNER_LOG_FILENAME);
    validateTeamRunnerLog(runnerLogPath, filename);
    return {
      filename,
      evidence: JSON.parse(
        readFileSync(join(teamDirectory, "summary.json"), "utf8"),
      ),
    };
  });
  const aggregate = aggregateTeamEvidence(entries, expected);
  const count = (name) => aggregate.metricCounts[name];
  const mapped = {
    files: aggregate.files,
    malformed: 0,
    thresholdFailures: 0,
    requests: count("http_reqs"),
    platformFirstAttemptFailures: count("platform_first_attempt_failures"),
    platformFirstAttemptTimeouts: count("platform_first_attempt_timeouts"),
    platformFirstAttemptRateLimits: count("platform_first_attempt_rate_limits"),
    platformFirstAttemptServerErrors: count(
      "platform_first_attempt_server_errors",
    ),
    platformRetryRecoveries: count("platform_retry_recoveries"),
    platformRetryExhaustions: count("platform_retry_exhaustions"),
    flagSyncWaits: count("flag_sync_waits"),
    captureAttempts: count("capture_attempts"),
    acceptedCaptures: count("accepted_captures"),
    duplicateCaptures: count("duplicate_captures"),
    captureSubmissionReplays: count("capture_submission_replays"),
    terminalCaptureVerdicts: count("terminal_capture_verdicts"),
    captureUnresolved: aggregate.adCaptures.unresolved,
    workCompletionSamples: aggregate.workload.workCompletionSamples,
    iterationsClassified: aggregate.workload.classifiedIterations,
    workCompletionSkew: aggregate.workload.workCompletionSkew,
    iterationRuntimeErrors: aggregate.workload.runtimeErrors,
    unclassifiedHardStopTails: aggregate.workload.unclassifiedTail,
    activeIterations: count("active_iterations"),
    idleIterations: count("idle_iterations"),
    exploitAttempts: count("exploit_attempts"),
    exploitPatched: count("exploit_patched"),
    exploitCaptures: count("exploit_captures"),
    defenseUpdates: count("defense_updates"),
    defenseIncidents: count("defense_incidents"),
    defenseRepairs: count("defense_repairs"),
    exploitUnavailable: count("exploit_unavailable"),
    actionCreditsSpent: count("action_credits_spent"),
    actionCreditDenials: count("action_credit_denials"),
    jeopardySubmissions: count("jeopardy_submissions"),
    jeopardyDetailsViewed: count("jeopardy_details_viewed"),
    jeopardyAttachmentDownloads: count("jeopardy_attachment_downloads"),
    jeopardyWrongGuesses: count("jeopardy_wrong_guesses"),
    jeopardyContainerCreates: count("jeopardy_container_creates"),
    jeopardyContainerDeletes: count("jeopardy_container_deletes"),
    jeopardyContainerFailures: count("jeopardy_container_failures"),
    kothCaptureAttempts: count("koth_capture_attempts"),
    kothCaptureSuccesses: count("koth_capture_successes"),
    kothOpeningClaims: count("koth_opening_claims"),
    kothTakeoverClaims: count("koth_takeover_claims"),
    kothResetRaces: count("koth_reset_races"),
    kothCaptureWindowClosed: count("koth_capture_window_closed"),
    kothCaptureIneligibleTransitions: count(
      "koth_capture_ineligible_transitions",
    ),
    kothCaptureStateUnavailable: count("koth_capture_state_unavailable"),
    kothCaptureAttemptFailures: count("koth_capture_attempt_failures"),
    kothCaptureRetryRecoveries: count("koth_capture_retry_recoveries"),
    kothCapturePendingStarts: count("koth_capture_pending_starts"),
    kothCaptureBurstExhaustions: count("koth_capture_burst_exhaustions"),
    kothCaptureTerminalWindows: count("koth_capture_terminal_windows"),
    kothCapturePendingInvariantFailures: count(
      "koth_capture_pending_invariant_failures",
    ),
    kothCaptureNetworkErrors: count("koth_capture_network_errors"),
    kothCaptureHttp4xx: count("koth_capture_http_4xx"),
    kothCaptureHttp5xx: count("koth_capture_http_5xx"),
    kothCaptureOtherStatusFailures: count("koth_capture_other_status_failures"),
    kothCaptureTargetUnavailable: count("koth_capture_target_unavailable"),
    kothPatchAttempts: count("koth_patch_attempts"),
    kothPatchSuccesses: count("koth_patch_successes"),
    kothPatchFailures: count("koth_patch_failures"),
    kothPatchHealthy: count("koth_patch_healthy"),
    kothPatchMumble: count("koth_patch_mumble"),
    kothPatchOffline: count("koth_patch_offline"),
    kothPatchRepairAttempts: count("koth_patch_repair_attempts"),
    kothPatchRepairs: count("koth_patch_repairs"),
    kothPatchRepairFailures: count("koth_patch_repair_failures"),
    kothPatchBlockedTakeovers: count("koth_patch_blocked_takeovers"),
    kothPatchBypassedTakeovers: count("koth_patch_bypassed_takeovers"),
    kothPatchHealthyHolds: count("koth_patch_healthy_holds"),
    kothPatchHoldChecks: count("koth_patch_hold_checks"),
    kothPatchHoldCheckFailures: count("koth_patch_hold_check_failures"),
    kothPatchHoldInterruptions: count("koth_patch_hold_interruptions"),
    kothPatchResetChecks: count("koth_patch_reset_checks"),
    kothPatchResetLosses: count("koth_patch_reset_losses"),
    kothPatchResetRetentions: count("koth_patch_reset_retentions"),
    kothPatchResetCheckFailures: count("koth_patch_reset_check_failures"),
    kothCapturePendingUnresolved: aggregate.koth.pendingUnresolved,
    kothCapturePendingBalanceErrors: 0,
    kothCaptureStatusBalanceErrors: 0,
    defenseAdvancedTeams: aggregate.teams.filter(
      (team) => team.metricCounts.defense_updates >= 2,
    ).length,
    kothWriters: aggregate.teams.filter(
      (team) => team.metricCounts.koth_capture_successes > 0,
    ).length,
    kothPatchLifecycleTeams: aggregate.teams.filter(
      (team) =>
        team.metricCounts.koth_capture_successes > 0 &&
        team.metricCounts.koth_patch_successes > 0 &&
        team.metricCounts.koth_patch_healthy_holds > 0,
    ).length,
    kothPatchResetTeams: aggregate.teams.filter(
      (team) => team.metricCounts.koth_patch_reset_losses > 0,
    ).length,
    tiers: aggregate.tiers,
    specialties: aggregate.specialties,
    aggregate,
  };
  return mapped;
}

function eventSettlementSnapshot(st) {
  const kothCleanup = assessKothDeadlineCleanup(
    JSON.parse(sql(kothDeadlineCleanupQuery(st.mixGame))),
  );
  return {
    secondsToEnd: Number(
      sql(
        `SELECT GREATEST(0,CEIL(EXTRACT(EPOCH FROM (end_time_utc-clock_timestamp()))))::bigint ` +
          `FROM "Games" WHERE id=${Number(st.mixGame)}`,
      ) || 0,
    ),
    unfinalizedRounds: Number(
      sql(
        `SELECT count(*) FROM "AdRounds" WHERE game_id=${Number(st.mixGame)} AND finalized=false`,
      ) || 0,
    ),
    nonterminalCycles: Number(
      sql(
        `SELECT count(*) FROM "KothCrownCycles" WHERE game_id=${Number(st.mixGame)} ` +
          `AND phase NOT IN ('Completed','Ended')`,
      ) || 0,
    ),
    kothCleanup,
  };
}

function scoreSpread(board) {
  const rows = Array.isArray(board?.teams) ? board.teams : [];
  const values = rows
    .map((team) => Number(team.settledTotal))
    .filter((value) => Number.isFinite(value) && value >= 0);
  const mean = values.length
    ? values.reduce((total, value) => total + value, 0) / values.length
    : 0;
  const variance = values.length
    ? values.reduce((total, value) => total + (value - mean) ** 2, 0) /
      values.length
    : 0;
  return {
    teams: values.length,
    distinct: new Set(values.map((value) => value.toFixed(6))).size,
    nonzero: values.filter((value) => value > 0).length,
    nonzeroDistinct: new Set(
      values.filter((value) => value > 0).map((value) => value.toFixed(6)),
    ).size,
    minimum: values.length ? Math.min(...values) : 0,
    maximum: values.length ? Math.max(...values) : 0,
    cv: mean > 0 ? Math.sqrt(variance) / mean : 0,
  };
}

function indexedScoreValues(rows, rosterIds, rowId, rowValue) {
  const rosterIndex = new Map(
    rosterIds.map((id, index) => [Number(id), index]),
  );
  const values = new Map();
  for (const row of rows || []) {
    const index = rosterIndex.get(Number(rowId(row)));
    const value = Number(rowValue(row));
    if (Number.isSafeInteger(index) && Number.isFinite(value))
      values.set(index, value);
  }
  return values;
}

function competitionSpecialtyEvidence(
  profiles,
  settlement,
  adParticipationIds,
  jeopardyTeamIds,
) {
  if (!Array.isArray(profiles) || !settlement) return null;
  const adRows = Array.isArray(settlement.adBoard?.teams)
    ? settlement.adBoard.teams
    : [];
  const kothRows = Array.isArray(settlement.kothBoard?.teams)
    ? settlement.kothBoard.teams
    : [];
  const jeopardyRows = Array.isArray(settlement.jeopardyBoard?.items)
    ? settlement.jeopardyBoard.items
    : [];
  const offense = indexedScoreValues(
    adRows,
    adParticipationIds,
    (row) => row.participationId,
    (row) => row.offenseRate,
  );
  const defense = indexedScoreValues(
    adRows,
    adParticipationIds,
    (row) => row.participationId,
    (row) => row.defenseRate,
  );
  const koth = indexedScoreValues(
    kothRows,
    adParticipationIds,
    (row) => row.participationId,
    (row) => row.controlRate,
  );
  const jeopardy = indexedScoreValues(
    jeopardyRows,
    jeopardyTeamIds,
    (row) => row.id,
    (row) => row.score,
  );
  return {
    offense: specialtyLift(profiles, offense, "offense"),
    defense: specialtyLift(profiles, defense, "defense"),
    koth: specialtyLift(profiles, koth, "koth"),
    jeopardy: specialtyLift(profiles, jeopardy, "jeopardy"),
    complete:
      offense.size === profiles.length &&
      defense.size === profiles.length &&
      koth.size === profiles.length &&
      jeopardy.size === profiles.length,
  };
}

async function waitForEventSettlement(st, probe, readinessProbe) {
  const timeout = Number(process.env.EVENT_SETTLEMENT_TIMEOUT_SECONDS || 240);
  if (!Number.isSafeInteger(timeout) || timeout < 1) {
    throw new Error(
      `EVENT_SETTLEMENT_TIMEOUT_SECONDS must be a positive integer (got ${timeout})`,
    );
  }
  let snapshot = eventSettlementSnapshot(st);
  while (snapshot.secondsToEnd > 0) {
    throwIfShuttingDown();
    recordProbe(probe, healthProbe(HEALTH_URL));
    recordProbe(readinessProbe, healthProbe(READINESS_URL));
    await A.sleep(Math.min(1000, snapshot.secondsToEnd * 1000));
    snapshot = eventSettlementSnapshot(st);
  }

  const jwt = A.adminJwt();
  let fullySettled = false;
  let kothFullySettled = false;
  let finalAdBoard = null;
  let finalKothBoard = null;
  let finalJeopardyBoard = null;
  for (let waited = 0; waited <= timeout; waited++) {
    throwIfShuttingDown();
    const [response, kothResponse, jeopardyResponse] = await interruptible(
      Promise.all([
        A.api("GET", `/api/Game/${Number(st.mixGame)}/Ad/Scoreboard`, {
          jwt,
          ip: "10.9.9.11",
          timeoutMs: 10_000,
        }).catch(() => null),
        A.api("GET", `/api/game/${Number(st.mixGame)}/ad/koth/scoreboard`, {
          jwt,
          ip: "10.9.9.12",
          timeoutMs: 10_000,
        }).catch(() => null),
        A.api("GET", `/api/game/${Number(st.jeoGame)}/scoreboard`, {
          jwt,
          ip: "10.9.9.13",
          timeoutMs: 10_000,
        }).catch(() => null),
      ]),
    );
    const board = response?.json?.data ?? response?.json;
    const kothBoard = kothResponse?.json?.data ?? kothResponse?.json;
    const jeopardyBoard =
      jeopardyResponse?.json?.data ?? jeopardyResponse?.json;
    if (response?.status === 200) finalAdBoard = board;
    if (kothResponse?.status === 200) finalKothBoard = kothBoard;
    if (jeopardyResponse?.status === 200) finalJeopardyBoard = jeopardyBoard;
    snapshot = eventSettlementSnapshot(st);
    fullySettled = response?.status === 200 && board?.fullySettled === true;
    kothFullySettled =
      kothResponse?.status === 200 && kothBoard?.fullySettled === true;
    if (
      fullySettled &&
      kothFullySettled &&
      snapshot.unfinalizedRounds === 0 &&
      snapshot.nonterminalCycles === 0 &&
      snapshot.kothCleanup.converged
    ) {
      break;
    }
    if (waited < timeout) await A.sleep(1000);
  }
  return {
    ...snapshot,
    fullySettled,
    kothFullySettled,
    adBoard: finalAdBoard,
    kothBoard: finalKothBoard,
    jeopardyBoard: finalJeopardyBoard,
  };
}

async function main() {
  orchestrationLock = await acquireExclusiveProcessLock(
    loadOrchestrationLockPath,
    {
      label: "RSCTF lifecycle simulation",
      metadata: { stateTag: process.env.LIFECYCLE_STATE_TAG || null },
    },
  );

  throwIfShuttingDown();
  const runDuration = durationSeconds(DURATION);
  if (!Number.isFinite(runDuration) || runDuration <= 0) {
    throw new Error(
      `DURATION must use a positive ms/s/m/h value (got ${DURATION})`,
    );
  }
  const distributedTeamClients = process.env.DISTRIBUTED_TEAM_CLIENTS === "1";
  if (REALISTIC_COMPETITION && !distributedTeamClients) {
    throw new Error(
      "REALISTIC_COMPETITION=1 requires DISTRIBUTED_TEAM_CLIENTS=1",
    );
  }
  if (
    REALISTIC_COMPETITION &&
    process.env.INTEGRATED_CHEAT_SIMULATION !== "1"
  ) {
    throw new Error(
      "REALISTIC_COMPETITION=1 requires INTEGRATED_CHEAT_SIMULATION=1",
    );
  }
  if (process.env.RETAIN_EVENT === "1" && process.env.KEEP !== "1") {
    throw new Error("RETAIN_EVENT=1 requires KEEP=1");
  }
  let st = null,
    k6Process = null,
    cheatProcess = null;
  let scoringPausedByHarness = false;
  let intentionallyPausedRound = null;
  let teamClientOwnership = null;
  const abortPreparationErrors = [];
  let runFailure = null;
  let retentionCompletionPending = false;
  let lifecycleRunClaimed = false;
  try {
    await interruptible(A.preflight());
    st = A.readState();
    if (!st || process.env.PROVISION === "1") {
      console.log("provisioning…");
      await provisionLifecycleState(orchestrationLock.token);
      st = A.readState();
    }
    if (
      !st ||
      !Array.isArray(st.adTeamIds) ||
      st.adTeamIds.length !== st.adPartIds?.length
    ) {
      throw new Error(
        "lifecycle state lacks the A&D team/participation identity mapping; reprovision it",
      );
    }
    if (
      REALISTIC_COMPETITION &&
      (Number(st.competitionModelVersion) !== 2 ||
        typeof st.competitionRunId !== "string" ||
        st.competitionSeed !== COMPETITION_SEED)
    ) {
      throw new Error(
        "competitive lifecycle state is not bound to model v2 and the requested seed; reprovision it",
      );
    }
    if (
      REALISTIC_COMPETITION &&
      (Number(st.kothContainerPort) !== 8080 || !competitiveKothImageMatches(st.kothContainerImage))
    ) {
      throw new Error(
        "competitive lifecycle state lacks the network-capturable KotH image; reprovision it",
      );
    }
    const FLEET = Number(process.env.FLEET || 80);
    if (!Number.isSafeInteger(FLEET) || FLEET < 2) {
      throw new Error(
        `FLEET must be an integer >= 2 (got ${process.env.FLEET})`,
      );
    }
    if (REALISTIC_COMPETITION && FLEET !== 100) {
      throw new Error(
        `model-v2 competitive acceptance requires exactly 100 teams (got ${FLEET})`,
      );
    }
    const isolatedServices = process.env.LIFECYCLE_ISOLATED_SERVICES === "1";
    if (
      (process.env.REQUIRE_ISOLATED_SERVICES === "1" || FLEET >= 100) &&
      !isolatedServices
    ) {
      throw new Error(
        "this lifecycle run requires LIFECYCLE_ISOLATED_SERVICES=1",
      );
    }
    const fleetPids = (st.adPartIds || []).slice(0, FLEET);
    if (
      fleetPids.length < FLEET ||
      (distributedTeamClients && st.adPartIds.length !== FLEET)
    ) {
      throw new Error(
        distributedTeamClients
          ? `distributed lifecycle requires the complete ${FLEET}-team roster, but provisioned state has ` +
              `${st.adPartIds.length}; reprovision with TEAMS_AD=${FLEET}`
          : `lifecycle BYOC fleet requires at least ${FLEET} real Accepted participations, but provisioned state has ` +
              `${st.adPartIds.length}`,
      );
    }
    if (
      REALISTIC_COMPETITION &&
      process.env.INTEGRATED_CHEAT_SIMULATION === "1" &&
      st.adPartIds.length < 100
    ) {
      throw new Error(
        "integrated anti-cheat competition requires at least 100 mixed-event teams",
      );
    }
    if (REALISTIC_COMPETITION && runDuration >= 1_800) {
      const jeopardyPlayers = Array.isArray(st.jeoUsers)
        ? st.jeoUsers.length
        : 0;
      const jeopardyChallenges = Array.isArray(st.jeopardyCatalog)
        ? st.jeopardyCatalog.length
        : 0;
      const minimumSolvers = Math.ceil(FLEET * 0.9);
      const minimumSolves = FLEET * 2;
      if (
        jeopardyPlayers < minimumSolvers ||
        jeopardyPlayers * jeopardyChallenges < minimumSolves
      ) {
        throw new Error(
          `long competitive lifecycle cannot satisfy its Jeopardy gates: ` +
            `${jeopardyPlayers} players × ${jeopardyChallenges} challenges; ` +
            `need at least ${minimumSolvers} players and ${minimumSolves} possible solves`,
        );
      }
    }

    // Claim the provision only after every read-only configuration check. A
    // typo against a terminal retained manifest must not truncate its event or
    // reap resources that belong to another invocation.
    assertLifecycleRunClaimable(st);
    lifecycleRunClaimed = true;
    if ("secret" in st) {
      delete st.secret;
      A.writeState(st);
    }
    if (process.env.RETAIN_EVENT === "1") {
      const simulationStartedAtMs = st.simulationStartedAtMs || A.nowMs();
      A.writeState({
        ...st,
        retained: true,
        retentionRequestedAtMs:
          st.retentionRequestedAtMs || simulationStartedAtMs,
        simulationStartedAtMs,
        simulationStatus: "running",
        simulationMode: REALISTIC_COMPETITION ? "competitive" : "capacity",
      });
      st = A.readState();
      console.log(`  retained event protection enabled at ${A.stateFile}`);
    }
    console.log(
      `lifecycle: G_JEO=${st.jeoGame}, G_MIX=${st.mixGame} | ` +
        `${distributedTeamClients ? `distributed teams=${FLEET}` : `VUS=${VUS}`} DURATION=${DURATION}`,
    );

    const provisionedReadinessPaused =
      REALISTIC_COMPETITION &&
      st.scoringPausedAfterReadiness === true &&
      A.adScoringPaused(st.mixGame);
    if (provisionedReadinessPaused) {
      scoringPausedByHarness = true;
      console.log(
        `  adopting provisioned paused readiness round ${st.readinessRound}`,
      );
    }

    // The official boundary freezes the complete event roster, but exact service
    // evidence belongs only to the configured live relay cohort. Capture the
    // current round before replacing any stale fleet so readiness cannot reuse an
    // exact result produced by an earlier relay/service process.
    const epoch = await interruptible(
      A.waitForEpochBoundary(st.mixGame, st.adPartIds.length),
    );
    const beforeFleetRound = Number(epoch.liveRound || 0);
    if (
      !Number.isSafeInteger(beforeFleetRound) ||
      beforeFleetRound < epoch.startRound
    ) {
      throw new Error(
        `invalid pre-fleet epoch boundary: ${JSON.stringify(epoch)}`,
      );
    }

    const canAdoptProvisionedFleet =
      REALISTIC_COMPETITION &&
      A.tunnelsUpFor(st.mixGame, st.adChal, fleetPids) === FLEET &&
      A.fleetResourcesReady(st.mixGame, st.adChal, fleetPids, isolatedServices);
    let fleetEvidence = provisionedReadinessPaused
      ? adoptPausedCompetitionReadiness({
          state: st,
          realisticCompetition: REALISTIC_COMPETITION,
          scoringPaused: true,
          fleetAdoptable: canAdoptProvisionedFleet,
          epoch,
          evidence: A.fleetExactReadiness(st.mixGame, st.adChal, fleetPids),
          expectedServices: FLEET,
        })
      : null;
    let fleetCapabilities;
    let up;
    if (canAdoptProvisionedFleet) {
      fleetCapabilities = A.adoptFleetForPids(st.mixGame, st.adChal, fleetPids);
      up = await interruptible(
        A.waitForFleetReady(st.mixGame, st.adChal, fleetPids),
      );
      console.log(
        `  BYOC fleet: adopted ${up}/${fleetPids.length} provisioned tunnels`,
      );
    } else {
      // teardownFleet is idempotent, so this supports both a clean provision and
      // an interrupted prior lifecycle attempt without retaining stale endpoints.
      A.teardownFleet({ gameId: st.mixGame, cid: st.adChal, pids: fleetPids });
      const fleetService = A.startFleetService(st.mixGame, st.adChal);
      A.restoreSeededAdServices(st.mixGame, st.adChal, fleetService.checker);
      fleetCapabilities = A.startFleetForPids(
        st.mixGame,
        st.adChal,
        fleetPids,
        fleetService.tunnel,
      );
      up = await interruptible(
        A.waitForFleetReady(st.mixGame, st.adChal, fleetPids),
      );
      console.log(`  BYOC fleet: ${up}/${fleetPids.length} tunnels up`);
    }

    // Require one complete post-connect round with durable delivery and exact
    // functional checker evidence for every selected relay, not inactive teams.
    if (!fleetEvidence) {
      fleetEvidence = await interruptible(
        A.waitForFleetExactEvidence(st.mixGame, st.adChal, fleetPids, {
          afterRound: beforeFleetRound,
          allowCurrentRoundEvidence: provisionedReadinessPaused,
        }),
      );
    }
    let loadEpoch = A.epochReadiness(st.mixGame);
    while (loadEpoch.liveRound > fleetEvidence.liveRound) {
      fleetEvidence = await interruptible(
        A.waitForFleetExactEvidence(st.mixGame, st.adChal, fleetPids, {
          afterRound: fleetEvidence.liveRound,
        }),
      );
      loadEpoch = A.epochReadiness(st.mixGame);
    }
    const loadFlags = A.plantedFlags(st.mixGame);
    let crown = await interruptible(
      A.waitForCrownReady(st.mixGame, st.kothChal, (st.adPartIds || []).length),
    );
    if (
      !epoch.startRound ||
      epoch.rosterTeams < 2 ||
      loadEpoch.startRound !== epoch.startRound ||
      loadFlags.length !== epoch.rosterServices ||
      loadEpoch.liveRound !== fleetEvidence.liveRound
    ) {
      throw new Error(
        `invalid pre-k6 epoch snapshot: ` +
          `${JSON.stringify({ epoch, loadEpoch, fleetEvidence, plantedFlags: loadFlags.length })}`,
      );
    }
    console.log(
      `  official epoch boundary: start round ${epoch.startRound}, frozen ${epoch.rosterTeams} teams / ` +
        `${epoch.rosterServices} services, ${loadFlags.length} current flags`,
    );
    console.log(
      `  selected fleet exact: round ${fleetEvidence.liveRound}, ` +
        `${fleetEvidence.deliveredFlags}/${FLEET} delivered and ${fleetEvidence.verifiedFlags}/${FLEET} verified`,
    );
    console.log(
      `  crown cycle ready: #${crown.cycleNumber} ${crown.phase}, ` +
        `${crown.tokenCount}/${crown.rosterCount} scoped tokens, container ${crown.containerId.slice(0, 12)}`,
    );
    console.log(
      `  official board rollups warmed in ${await interruptible(A.warmEpochBoard(st.mixGame, st.adPartIds.length))} ms`,
    );
    if (distributedTeamClients) {
      intentionallyPausedRound = Number(
        sql(
          `SELECT COALESCE(max(number),0) FROM "AdRounds" WHERE game_id=${Number(st.mixGame)}`,
        ) || 0,
      );
      if (
        !Number.isSafeInteger(intentionallyPausedRound) ||
        intentionallyPausedRound < 1
      ) {
        throw new Error(
          `could not identify the round frozen for distributed client startup`,
        );
      }
      if (!scoringPausedByHarness) {
        await interruptibleMutation(A.setAdScoringPaused(st.mixGame, true));
        scoringPausedByHarness = true;
        console.log(
          `  official scoring paused at round ${intentionallyPausedRound} while distributed clients start`,
        );
      } else {
        console.log(
          `  official scoring remains paused at verified round ${intentionallyPausedRound}`,
        );
      }
    }

    // The checker probes these live services each tick and k6 captures their
    // exact flags through the tunnel listeners established above.
    if (
      isolatedServices &&
      !A.fleetResourcesReady(st.mixGame, st.adChal, fleetPids, true)
    ) {
      throw new Error(
        "isolated BYOC fleet lost an exact labeled relay, service, or flag volume before k6",
      );
    }
    const byocListeners = A.tunnelListenersFor(
      st.mixGame,
      st.adChal,
      fleetPids,
    );
    if (byocListeners.length !== fleetPids.length) {
      throw new Error(
        `BYOC listener snapshot is incomplete (${byocListeners.length}/${fleetPids.length})`,
      );
    }
    if (
      loadEpoch.startRound !== epoch.startRound ||
      loadFlags.length !== epoch.rosterServices
    ) {
      throw new Error(
        `official epoch roster/flags changed before k6: ` +
          `${JSON.stringify({ expectedStartRound: epoch.startRound, snapshot: loadEpoch, plantedFlags: loadFlags.length })}`,
      );
    }
    let evidenceStartRound = Number(fleetEvidence.liveRound);
    if (
      !Number.isSafeInteger(evidenceStartRound) ||
      evidenceStartRound < epoch.startRound
    ) {
      throw new Error(
        `invalid authoritative load-evidence round: ${JSON.stringify(loadEpoch)}`,
      );
    }
    crown = await interruptible(
      A.waitForCrownReady(st.mixGame, st.kothChal, st.adPartIds.length),
    );
    A.writeState({
      ...st,
      epochStartRound: epoch.startRound,
      plantedFlags: loadFlags,
      byocListeners,
      dedupFlag: loadFlags[0]?.flag || st.dedupFlag,
      kothContainer: crown.containerId,
      crownCycleId: crown.cycleId,
    });
    st = A.readState();
    const before = snapshot(st);

    // k6 allows an in-flight iteration to drain for up to 30 seconds after a
    // scenario's nominal duration. Keep the event open beyond that window so
    // the harness does not manufacture end-of-event 4xx responses while its
    // own container hold/delete sequence is still draining.
    const graceSeconds = safePositiveInteger(
      process.env.EVENT_END_GRACE_SECONDS || 45,
      "EVENT_END_GRACE_SECONDS",
      5,
    );
    const attackBaselineId = Number(
      sql('SELECT COALESCE(max(id),0) FROM "AdAttacks"') || 0,
    );
    const jeopardyBaselineId = Number(
      sql(
        `SELECT COALESCE(max(id),0) FROM "Submissions" WHERE game_id=${Number(st.jeoGame)}`,
      ) || 0,
    );
    const kothControlBaselineId = Number(
      sql(
        `SELECT COALESCE(max(id),0) FROM "KothControlResults" WHERE game_id=${Number(st.mixGame)}`,
      ) || 0,
    );
    const kothAcquisitionBaselineId = Number(
      sql(
        `SELECT COALESCE(max(acquisition.id),0) FROM "KothAcquisitions" acquisition ` +
          `JOIN "KothCrownCycles" cycle ON cycle.id=acquisition.cycle_id ` +
          `WHERE cycle.game_id=${Number(st.mixGame)}`,
      ) || 0,
    );
    const evidenceNotBeforeMs = Date.now();
    let teamRun = null;
    let teamStatus = null;
    let teamEvidence = null;
    const evidenceTag = process.env.LIFECYCLE_STATE_TAG?.trim();
    let teamEvidenceDir =
      process.env.TEAM_EVIDENCE_DIR ||
      `/tmp/rsctf-team-event-evidence${evidenceTag ? `-${evidenceTag}` : ""}`;
    let out = "",
      k6Err = "";
    let done = distributedTeamClients,
      k6Exit = null,
      k6Signal = null,
      k6SpawnError = null;
    const probe = { ok: 0, fail: 0, lat: [] };
    const readinessProbe = { ok: 0, fail: 0, lat: [] };
    const capacityVpnPeers = new Set();

    if (distributedTeamClients) {
      teamClientOwnership = TeamClients.vpnTeamClientOwnership(st, FLEET);
      A.writeState({ ...A.readState(), teamClientOwnership });
      st = A.readState();
      teamRun = await TeamClients.startVpnTeamClients({
        state: st,
        ownership: teamClientOwnership,
        listeners: byocListeners,
        count: FLEET,
        duration: DURATION,
        thinkSeconds: Number(process.env.TEAM_THINK_SECONDS || 5),
        evidenceDir: teamEvidenceDir,
        startDelaySeconds: Number(process.env.TEAM_START_DELAY_SECONDS || 90),
        realisticCompetition: REALISTIC_COMPETITION,
        competitionSeed: COMPETITION_SEED,
        defenseKeys: fleetCapabilities.map(({ defenseKey }) => defenseKey),
        throwIfInterrupted: throwIfShuttingDown,
      });
      teamClientOwnership = teamRun.ownership;
      teamEvidenceDir = teamRun.evidenceDir;
      A.writeState({ ...A.readState(), teamEvidenceDir, teamClientOwnership });
      st = A.readState();
      const eventEndSeconds =
        teamRun.startAtSeconds + Math.ceil(runDuration) + graceSeconds;
      sql(
        `UPDATE "Games" SET end_time_utc=to_timestamp(${eventEndSeconds}) ` +
          `WHERE id IN (${Number(st.jeoGame)},${Number(st.mixGame)})`,
      );
      console.log(
        `  event deadline aligned to the distributed start barrier (+${graceSeconds}s grace)`,
      );

      let handshakeCount = 0;
      const routingDeadlineMs = (teamRun.startAtSeconds - 5) * 1000;
      while (Date.now() < routingDeadlineMs) {
        throwIfShuttingDown();
        teamStatus = TeamClients.vpnTeamClientStatus(
          teamClientOwnership,
          FLEET,
        );
        if (teamStatus.failed > 0 || teamStatus.missing > 0) {
          throw new Error(
            `distributed team clients failed during routing readiness: ${JSON.stringify(teamStatus)}`,
          );
        }
        handshakeCount = TeamClients.vpnHandshakeCount(
          teamRun.createdAtSeconds,
          teamRun.peerPublicKeys,
        );
        if (teamStatus.running === FLEET) break;
        await A.sleep(1000);
      }
      teamStatus = TeamClients.vpnTeamClientStatus(teamClientOwnership, FLEET);
      handshakeCount = TeamClients.vpnHandshakeCount(
        teamRun.createdAtSeconds,
        teamRun.peerPublicKeys,
      );
      if (
        teamStatus.running !== FLEET ||
        teamStatus.failed > 0 ||
        teamStatus.missing > 0
      ) {
        throw new Error(
          `distributed routing was not ready before the start barrier: ` +
            `${JSON.stringify({ ...teamStatus, handshakes: handshakeCount, expected: FLEET })}`,
        );
      }
      if (handshakeCount < FLEET) {
        console.log(
          `  distributed WireGuard handshakes still in-progress (${handshakeCount}/${FLEET}); continuing`,
        );
      }
      console.log(
        `  distributed clients: ${teamStatus.running}/${FLEET} running · ${handshakeCount} authenticated WireGuard peers`,
      );
      const resumeAtMs =
        teamRun.startAtSeconds * 1000 - (REALISTIC_COMPETITION ? 0 : 1000);
      while (Date.now() < resumeAtMs) {
        throwIfShuttingDown();
        recordProbe(probe, healthProbe(HEALTH_URL));
        recordProbe(readinessProbe, healthProbe(READINESS_URL));
        await A.sleep(Math.min(1000, Math.max(1, resumeAtMs - Date.now())));
      }
      await interruptibleMutation(A.setAdScoringPaused(st.mixGame, false));
      scoringPausedByHarness = false;
      console.log(
        "  official scoring resumed at the distributed player start barrier",
      );
    } else {
      // Capacity-mode crown captures run out of band rather than through the
      // distributed team clients. Provision every possible claimant's VPN
      // identity before timed player traffic begins. Doing this lazily in the
      // capture loop briefly takes the team credential fence and can manufacture
      // otherwise-impossible 503s on that team's concurrent token/submit reads.
      for (const participationId of fleetPids) {
        await interruptible(
          ensureCapacityVpnPeer(st, participationId, capacityVpnPeers),
        );
      }
      console.log(
        `  capacity VPN identities ready: ${capacityVpnPeers.size}/${FLEET}`,
      );
      if (process.env.ALIGN_EVENT_END === "1") {
        const endAfterSeconds = Math.ceil(runDuration) + graceSeconds;
        sql(
          `UPDATE "Games" SET end_time_utc=now()+make_interval(secs=>${endAfterSeconds}) ` +
            `WHERE id IN (${Number(st.jeoGame)},${Number(st.mixGame)})`,
        );
        console.log(
          `  event deadline aligned to host load duration (+${graceSeconds}s grace)`,
        );
      }
      // --summary-export dumps every metric's full stat spread (avg/med/p90/p95/p99/max)
      // as JSON for the performance report (see tests/load/README.md).
      const k6Args = [
        "run",
        new URL("./k6/lifecycle.js", import.meta.url).pathname,
      ];
      if (process.env.SUMMARY_JSON)
        k6Args.push("--summary-export", process.env.SUMMARY_JSON);
      const k6 = spawn("k6", k6Args, {
        stdio: ["ignore", "pipe", "pipe"],
        env: {
          ...process.env,
          VUS: String(VUS),
          FLEET: String(FLEET),
          DURATION,
          SECRET: JWT_SECRET,
          LIFECYCLE_STATE_FILE: K6_STATE_BASENAME,
        },
      });
      k6Process = k6;
      k6.stdout.on("data", (b) => (out += b));
      k6.stderr.on("data", (b) => {
        k6Err += b;
        process.stderr.write(b);
      });
      done = false;
      k6.on("error", (error) => {
        k6SpawnError = error;
        done = true;
      });
      k6.on("close", (code, signal) => {
        k6Exit = code;
        k6Signal = signal;
        done = true;
      });
    }

    let tick = 0,
      kothCaps = 0,
      kothCaptureRaces = 0;
    let staleWrites = 0,
      staleRejections = 0;
    let capture = null,
      lastCycleCapture = null,
      staleProbe = null;
    const crownCyclesSeen = new Set();
    const crownPhasesSeen = new Set();
    const staleCyclesProbed = new Set();
    const adLeadersSeen = new Set();
    const jeopardyLeadersSeen = new Set();
    const kothLeadersSeen = new Set();
    let lastAdLeader = null;
    let adLeaderChanges = 0;
    let lastJeopardyLeader = null;
    let jeopardyLeaderChanges = 0;
    let nextCompetitionSampleAtMs = 0;
    const integratedCheat =
      REALISTIC_COMPETITION && process.env.INTEGRATED_CHEAT_SIMULATION === "1";
    const cheatAtFraction = Number(process.env.CHEAT_AT_FRACTION || 0.45);
    if (
      integratedCheat &&
      (!Number.isFinite(cheatAtFraction) ||
        cheatAtFraction <= 0.1 ||
        cheatAtFraction >= 0.9)
    ) {
      throw new Error("CHEAT_AT_FRACTION must be between 0.1 and 0.9");
    }
    let cheatStarted = false;
    let cheatChildStartedAtMs = null;
    let cheatExit = null;
    let cheatOutput = "";
    let cheatError = "";
    const cheatResultPath = integratedCheat
      ? join(teamEvidenceDir, `anti-cheat-result-${process.pid}.json`)
      : null;
    let cheatResultMerged = false;
    const mergeEmbeddedCheatResult = () => {
      if (!integratedCheat || cheatExit !== 0 || cheatResultMerged) return;
      const current = A.readState();
      const artifact = readCheatResult(cheatResultPath);
      const gameChallengeIds = String(
        sql(
          `SELECT string_agg(id::text,',' ORDER BY id) FROM "GameChallenges" WHERE game_id=${Number(current.mixGame)}`,
        ) || "",
      )
        .split(",")
        .filter(Boolean)
        .map(Number);
      A.writeState(
        mergeCheatResult(
          current,
          artifact,
          cheatRetentionPolicy({
            ...process.env,
            RSCTF_INTEGRATED_CHEAT_CHILD: "1",
          }),
          {
            runId: current.competitionRunId,
            gameId: Number(current.mixGame),
            eventCreatedAtMs: Number(current.createdAtMs),
            childStartedAtMs: cheatChildStartedAtMs,
            observedAtMs: A.nowMs(),
            fleetParticipationIds: fleetPids,
            gameChallengeIds,
          },
        ),
      );
      st = A.readState();
      cheatResultMerged = true;
    };
    let resolveCheatDone;
    const cheatDone = new Promise((resolve) => {
      resolveCheatDone = resolve;
    });
    while (distributedTeamClients || !done) {
      throwIfShuttingDown();
      if (distributedTeamClients) {
        const completionDeadlineSeconds =
          teamRun.startAtSeconds + Math.ceil(runDuration) + 60;
        if (Date.now() / 1000 > completionDeadlineSeconds) {
          throw new Error(
            `distributed team clients did not finish within 60 seconds of ${DURATION}`,
          );
        }
        teamStatus = TeamClients.vpnTeamClientStatus(
          teamClientOwnership,
          FLEET,
        );
        if (
          teamStatus.missing > 0 ||
          teamStatus.running + teamStatus.succeeded + teamStatus.failed !==
            FLEET
        ) {
          throw new Error(
            `distributed team-client failure: ${JSON.stringify(teamStatus)}`,
          );
        }
        if (teamStatus.succeeded + teamStatus.failed === FLEET) break;
      }
      if (!distributedTeamClients && ((done && k6Exit !== 0) || k6SpawnError)) {
        throw new Error(
          `application k6 failed early: ${k6SpawnError?.message || k6Err.slice(-500)}`,
        );
      }
      recordProbe(probe, healthProbe(HEALTH_URL));
      recordProbe(readinessProbe, healthProbe(READINESS_URL));
      tick++;
      // Hold one exact active-cycle token across checker rounds. This exercises the
      // provisional → confirmed path; rotating every write would intentionally keep
      // breaking the confirmation streak and would never test acquisition credit.
      const loadStarted =
        !distributedTeamClients || Date.now() / 1000 >= teamRun.startAtSeconds;
      const loadElapsedSeconds = distributedTeamClients
        ? Math.max(0, Date.now() / 1000 - teamRun.startAtSeconds)
        : tick / 2;
      if (
        integratedCheat &&
        loadStarted &&
        !cheatStarted &&
        loadElapsedSeconds >= runDuration * cheatAtFraction
      ) {
        cheatStarted = true;
        cheatChildStartedAtMs = A.nowMs();
        cheatProcess = spawn(
          process.execPath,
          [new URL("./cheat-event.mjs", import.meta.url).pathname],
          {
            cwd: new URL(".", import.meta.url).pathname,
            stdio: ["ignore", "pipe", "pipe"],
            detached: true,
            env: {
              ...process.env,
              CHEAT_SIMULATION: "1",
              RSCTF_INTEGRATED_CHEAT_CHILD: "1",
              RSCTF_CHEAT_RESULT_PATH: cheatResultPath,
              RSCTF_LOAD_ORCHESTRATION_LOCK_TOKEN: orchestrationLock.token,
              COMPETITION_RUN_ID: st.competitionRunId,
            },
          },
        );
        cheatProcess.stdout.on("data", (chunk) => (cheatOutput += chunk));
        cheatProcess.stderr.on("data", (chunk) => (cheatError += chunk));
        cheatProcess.on("error", (error) => {
          cheatError += error.message;
          cheatExit = 1;
          resolveCheatDone();
        });
        cheatProcess.on("close", (code, signal) => {
          if (signal) cheatError += `anti-cheat child terminated by ${signal}`;
          cheatExit = signal ? 1 : (code ?? 1);
          // The child waits for its own k6 process before closing. Forget the
          // completed process group immediately so final cleanup cannot signal
          // a numeric PID/PGID that the kernel may reuse later in a long event.
          cheatProcess = null;
          resolveCheatDone();
        });
        console.log(
          `  integrated anti-cheat drill started at ${(cheatAtFraction * 100).toFixed(0)}% event progress`,
        );
      }
      if (integratedCheat && cheatExit !== null && cheatExit !== 0) {
        throw new Error(
          `integrated anti-cheat drill failed: ${(cheatError || cheatOutput).trim().slice(-800)}`,
        );
      }
      if (integratedCheat && cheatExit === 0 && !cheatResultMerged) {
        mergeEmbeddedCheatResult();
      }
      if (
        REALISTIC_COMPETITION &&
        loadStarted &&
        Date.now() >= nextCompetitionSampleAtMs
      ) {
        const jwt = A.adminJwt();
        const [adResponse, jeopardyResponse] = await Promise.all([
          A.api("GET", `/api/Game/${Number(st.mixGame)}/Ad/Scoreboard`, {
            jwt,
            ip: "10.9.9.21",
            timeoutMs: 10_000,
          }).catch(() => null),
          A.api("GET", `/api/game/${Number(st.jeoGame)}/scoreboard`, {
            jwt,
            ip: "10.9.9.22",
            timeoutMs: 10_000,
          }).catch(() => null),
        ]);
        const adBoard = adResponse?.json?.data ?? adResponse?.json;
        const jeopardyBoard =
          jeopardyResponse?.json?.data ?? jeopardyResponse?.json;
        const leader = Number(
          adBoard?.teams?.find((team) => team?.rank === 1)?.participationId ||
            0,
        );
        if (leader > 0) {
          adLeadersSeen.add(leader);
          if (lastAdLeader && lastAdLeader !== leader) adLeaderChanges++;
          lastAdLeader = leader;
        }
        const jeopardyLeader = Number(
          jeopardyBoard?.items?.find((team) => team?.rank === 1)?.id || 0,
        );
        if (jeopardyLeader > 0) {
          jeopardyLeadersSeen.add(jeopardyLeader);
          if (lastJeopardyLeader && lastJeopardyLeader !== jeopardyLeader)
            jeopardyLeaderChanges++;
          lastJeopardyLeader = jeopardyLeader;
        }
        nextCompetitionSampleAtMs = Date.now() + 30_000;
      }
      if (loadStarted && tick % 4 === 0 && (st.adPartIds || []).length) {
        const view = A.crownReadiness(st.mixGame, st.kothChal);
        if (view.cycleId) crownCyclesSeen.add(view.cycleId);
        if (view.phase) crownPhasesSeen.add(view.phase);
        if (view.confirmedParticipationId)
          kothLeadersSeen.add(Number(view.confirmedParticipationId));
        if (view.phase === "Active" && view.containerId) {
          if (REALISTIC_COMPETITION && view.confirmedParticipationId) {
            const confirmedToken = A.latestKothToken(
              st.mixGame,
              Number(view.confirmedParticipationId),
              st.kothChal,
            );
            if (confirmedToken) {
              lastCycleCapture = {
                cycleId: view.cycleId,
                container: view.containerId,
                pid: Number(view.confirmedParticipationId),
                token: confirmedToken,
              };
            }
          }
          if (staleProbe && staleProbe.cycleId !== view.cycleId)
            staleProbe = null;
          if (staleProbe) {
            const rejected = Number(
              sql(
                `SELECT count(*) FROM "KothControlResults" ` +
                  `WHERE id>${staleProbe.afterResultId} AND game_id=${st.mixGame} ` +
                  `AND challenge_id=${st.kothChal} AND cycle_id=${staleProbe.cycleId} ` +
                  `AND container_id='${staleProbe.container}' AND is_scorable ` +
                  `AND marker_observed AND token_id IS NULL AND controlling_participation_id IS NULL`,
              ) || 0,
            );
            if (rejected > 0) {
              staleRejections++;
              staleProbe = null;
            } else {
              try {
                // Keep the revoked capability present until one authoritative
                // checker round observes it. Real team writes may race this
                // probe, so a single host write is not durable evidence.
                A.kothCaptureWrite(staleProbe.container, staleProbe.token);
              } catch {
                kothCaptureRaces++;
              }
            }
          }
          if (
            !staleProbe &&
            lastCycleCapture &&
            lastCycleCapture.cycleId !== view.cycleId &&
            // A three-tick cycle has only two observable ticks after the
            // boundary reset activates its replacement. Spending one of those
            // ticks on an old capability leaves only one healthy observation,
            // so a two-tick qualified claim can never confirm. One sacrificed
            // cycle is enough to prove reset revocation; later cycles must keep
            // the fresh capability stable across both remaining checker ticks.
            staleCyclesProbed.size === 0 &&
            !staleCyclesProbed.has(view.cycleId)
          ) {
            const afterResultId = Number(
              sql(
                `SELECT COALESCE(max(id),0) FROM "KothControlResults" ` +
                  `WHERE game_id=${st.mixGame} AND challenge_id=${st.kothChal}`,
              ) || 0,
            );
            try {
              A.kothCaptureWrite(view.containerId, lastCycleCapture.token);
              staleWrites++;
              staleCyclesProbed.add(view.cycleId);
              staleProbe = {
                cycleId: view.cycleId,
                container: view.containerId,
                afterResultId,
                token: lastCycleCapture.token,
              };
              capture = null;
            } catch {
              kothCaptureRaces++;
            }
          }
          if (
            !REALISTIC_COMPETITION &&
            (!capture ||
              capture.cycleId !== view.cycleId ||
              capture.container !== view.containerId)
          ) {
            if (!staleProbe) {
              const cooled = new Set(
                (
                  sql(
                    `SELECT participation_id FROM "KothCycleCooldowns" ` +
                      `WHERE cycle_id=${Number(view.cycleId)} AND network_released_at IS NULL ORDER BY participation_id`,
                  ) || ""
                )
                  .split("\n")
                  .filter(Boolean)
                  .map(Number),
              );
              // Capacity mode writes KotH tokens out-of-band, without the
              // distributed WireGuard clients. Restrict claimants to the exact
              // relay fleet and provision their real VPN identity before a
              // capture, otherwise the fail-closed champion cooldown correctly
              // refuses to activate the next crown cycle for a peerless winner.
              const pid = selectKothCapacityClaimant(
                fleetPids,
                cooled,
                Math.max(view.cycleNumber, 1),
              );
              if (pid) {
                await interruptible(
                  ensureCapacityVpnPeer(st, pid, capacityVpnPeers),
                );
              }
              const token = pid
                ? A.latestKothToken(st.mixGame, pid, st.kothChal)
                : null;
              capture = token
                ? {
                    cycleId: view.cycleId,
                    container: view.containerId,
                    pid,
                    token,
                  }
                : null;
              if (capture) lastCycleCapture = capture;
            }
          }
          if (
            !REALISTIC_COMPETITION &&
            !staleProbe &&
            capture &&
            tick % 8 === 0
          ) {
            try {
              A.kothCaptureWrite(capture.container, capture.token);
              kothCaps++;
            } catch {
              // A snapshot can race the exact destroy boundary. The next poll must
              // discover a new identity; stale evidence is checked in SQL below.
              capture = null;
              kothCaptureRaces++;
            }
          }
        } else {
          capture = null;
        }
      }
      await A.sleep(500);
    }
    if (!distributedTeamClients && (k6SpawnError || k6Exit !== 0)) {
      throw new Error(
        `application k6 failed: ${k6SpawnError?.message || k6Err.slice(-500)}`,
      );
    }
    if (integratedCheat) {
      if (!cheatStarted)
        throw new Error(
          "integrated anti-cheat drill never reached its scheduled event window",
        );
      if (
        cheatExit === null &&
        !(await waitForCompletion(interruptible(cheatDone), 180_000))
      ) {
        throw new Error(
          "integrated anti-cheat drill did not finish within 180 seconds",
        );
      }
      if (cheatExit === null)
        throw new Error(
          "integrated anti-cheat drill did not finish within 180 seconds",
        );
      if (cheatExit !== 0) {
        throw new Error(
          `integrated anti-cheat drill failed: ${(cheatError || cheatOutput).trim().slice(-800)}`,
        );
      }
      mergeEmbeddedCheatResult();
      if (!cheatResultMerged)
        throw new Error(
          "integrated anti-cheat result was not merged into the lifecycle manifest",
        );
      console.log(
        `  integrated anti-cheat drill passed during normal team play`,
      );
    }
    if (distributedTeamClients) {
      teamStatus = TeamClients.vpnTeamClientStatus(teamClientOwnership, FLEET);
      teamEvidence = REALISTIC_COMPETITION
        ? competitiveTeamEvidenceSnapshot(teamEvidenceDir, {
            runId: st.competitionRunId,
            eventCreatedAtMs: Number(st.createdAtMs),
            gameId: Number(st.mixGame),
            jeopardyGameId: Number(st.jeoGame),
            kothChallengeId: Number(st.kothChal),
            epochStartRound: Number(st.epochStartRound),
            teamCount: FLEET,
            participationIds: fleetPids,
            competitionModelVersion: teamRun.competitionModelVersion,
            competitionSeed: teamRun.competitionSeed,
            duration: teamRun.competitionDuration,
            profiles: teamRun.profiles,
            notBeforeMs: Number(teamRun.startAtSeconds) * 1000,
            notAfterMs:
              (Number(teamRun.startAtSeconds) + Math.ceil(runDuration) + 60) *
              1000,
          })
        : capacityTeamEvidenceSnapshot(
            teamEvidenceDir,
            FLEET,
            evidenceNotBeforeMs,
          );
      if (REALISTIC_COMPETITION) kothCaps = teamEvidence.kothCaptureSuccesses;
      // Fleet replacement is deliberately completed before the real team start
      // barrier. At 100 services that setup can span several scheduler rounds;
      // those immutable rows remain in PostgreSQL, but they are provisioning
      // evidence rather than observations from the requested attack window.
      // The round frozen during client startup is intentionally longer than a
      // normal tick. Start cadence/publication integrity at the first complete
      // post-resume round; player attack evidence remains baseline-scoped and
      // still includes valid attacks made immediately after the barrier.
      const attackStartRound = Number(
        sql(
          `SELECT number FROM "AdRounds" WHERE game_id=${Number(st.mixGame)} ` +
            `AND number>${Number(intentionallyPausedRound)} ` +
            `AND start_time_utc>=to_timestamp(${Number(teamRun.startAtSeconds)}) ` +
            `ORDER BY number LIMIT 1`,
        ) || 0,
      );
      if (
        !Number.isSafeInteger(attackStartRound) ||
        attackStartRound < epoch.startRound
      ) {
        throw new Error(
          `invalid distributed attack-window round ${attackStartRound}`,
        );
      }
      evidenceStartRound = attackStartRound;
      console.log(
        `  cadence/publication evidence starts at post-resume round ${evidenceStartRound}`,
      );
    }
    const competitionWindowStartMs = distributedTeamClients
      ? Number(teamRun.startAtSeconds) * 1000
      : evidenceNotBeforeMs;
    const competitionWindowEndMs =
      competitionWindowStartMs + Math.round(runDuration * 1000);
    const adEvidence = attackEvidence(
      st,
      attackBaselineId,
      fleetPids,
      FLEET,
      competitionWindowStartMs,
      competitionWindowEndMs,
    );
    const jeoEvidence = jeopardyEvidence(
      st,
      jeopardyBaselineId,
      competitionWindowStartMs,
      competitionWindowEndMs,
    );
    const settlement =
      distributedTeamClients || process.env.ALIGN_EVENT_END === "1"
        ? await waitForEventSettlement(st, probe, readinessProbe)
        : null;
    const adScoreSpread = scoreSpread(settlement?.adBoard);
    const kothScoreSpread = scoreSpread(settlement?.kothBoard);
    const adRankAudit = settlement
      ? auditOrdinalScoreboard("ad", settlement.adBoard, fleetPids)
      : null;
    const kothRankAudit = settlement
      ? auditOrdinalScoreboard("koth", settlement.kothBoard, fleetPids)
      : null;
    const jeopardyExpectedTeamIds = (st.jeoTeamIds || [])
      .slice(0, FLEET)
      .map(Number);
    const jeopardyRankAudit =
      settlement && jeopardyExpectedTeamIds.length === FLEET
        ? auditOrdinalScoreboard(
            "jeopardy",
            settlement.jeopardyBoard,
            jeopardyExpectedTeamIds,
          )
        : null;
    const specialtyEvidence = REALISTIC_COMPETITION
      ? competitionSpecialtyEvidence(
          teamRun?.profiles,
          settlement,
          fleetPids,
          jeopardyExpectedTeamIds,
        )
      : null;
    console.log(
      `  KotH captures written: ${kothCaps} · reset races ${kothCaptureRaces} · ` +
        `stale-token rejections ${staleRejections}/${staleWrites} · ` +
        `cycles ${crownCyclesSeen.size} · phases ${[...crownPhasesSeen].sort().join(",")}`,
    );

    // ── report ─────────────────────────────────────────────────────────────────
    const pick = (re) => (out.match(re) || [, "?"])[1];
    probe.lat.sort((a, b) => a - b);
    const pc = (x) =>
      probe.lat.length ? probe.lat[(probe.lat.length * x) | 0] : 0;
    const maxProbeLatency = probe.lat.length
      ? probe.lat[probe.lat.length - 1]
      : 0;
    console.log("\n  === LOAD RESULT ===");
    if (distributedTeamClients) {
      console.log(
        `    team clients: ${teamStatus.succeeded}/${FLEET} exited successfully · ${teamEvidence.requests} HTTPS/VPN requests over ${DURATION}`,
      );
      console.log(
        `    summaries   : ${teamEvidence.files}/${FLEET} sanitized files · ` +
          `${teamEvidence.thresholdFailures} threshold failures · ${teamEvidence.malformed} malformed · ` +
          `${teamEvidence.flagSyncWaits} flag-sync waits`,
      );
      if (REALISTIC_COMPETITION) {
        console.log(
          `    profiles    : seed ${COMPETITION_SEED} · ` +
            Object.entries(teamEvidence.tiers)
              .sort(([left], [right]) => left.localeCompare(right))
              .map(([tier, count]) => `${tier}=${count}`)
              .join(" · "),
        );
        console.log(
          `    specialties : ` +
            Object.entries(teamEvidence.specialties)
              .sort(([left], [right]) => left.localeCompare(right))
              .map(([specialty, count]) => `${specialty}=${count}`)
              .join(" · "),
        );
        console.log(
          `    competition : active/idle ${teamEvidence.activeIterations}/${teamEvidence.idleIterations} · ` +
            `work-complete ${teamEvidence.workCompletionSamples}/${teamEvidence.iterationsClassified} ` +
            `(runtime errors ${teamEvidence.iterationRuntimeErrors}, hard-stop tails ` +
            `${teamEvidence.unclassifiedHardStopTails}) · ` +
            `exploits ${teamEvidence.exploitAttempts} (${teamEvidence.exploitCaptures} flags, ` +
            `${teamEvidence.exploitPatched} patched, ${teamEvidence.exploitUnavailable} unavailable) · defense ` +
            `${teamEvidence.defenseAdvancedTeams}/${FLEET} teams advanced · incidents/repairs ` +
            `${teamEvidence.defenseIncidents}/${teamEvidence.defenseRepairs} · credits ` +
            `${teamEvidence.actionCreditsSpent} spent/${teamEvidence.actionCreditDenials} denied`,
        );
        console.log(
          `    A&D submits : ${teamEvidence.captureAttempts} logical · ` +
            `${teamEvidence.acceptedCaptures} accepted · ${teamEvidence.duplicateCaptures} duplicate · ` +
            `${teamEvidence.terminalCaptureVerdicts} terminal · ${teamEvidence.captureSubmissionReplays} replays · ` +
            `${teamEvidence.captureUnresolved} unresolved`,
        );
        console.log(
          `    API retries : ${teamEvidence.platformFirstAttemptFailures} first failures ` +
            `(timeout ${teamEvidence.platformFirstAttemptTimeouts}, 429 ${teamEvidence.platformFirstAttemptRateLimits}, ` +
            `5xx ${teamEvidence.platformFirstAttemptServerErrors}) · ` +
            `${teamEvidence.platformRetryRecoveries} recovered · ` +
            `${teamEvidence.platformRetryExhaustions} exhausted`,
        );
        console.log(
          `    objectives  : Jeopardy ${jeoEvidence.accepted} accepted solves by ${jeoEvidence.solvers} teams · ` +
            `KotH writes ${teamEvidence.kothCaptureSuccesses}/${teamEvidence.kothCaptureAttempts} · ` +
            `writers ${teamEvidence.kothWriters} · opening/takeover writes ` +
            `${teamEvidence.kothOpeningClaims}/${teamEvidence.kothTakeoverClaims} · reset races ` +
            `${teamEvidence.kothResetRaces} · closed windows ${teamEvidence.kothCaptureWindowClosed} · ` +
            `ineligible transitions ${teamEvidence.kothCaptureIneligibleTransitions} · retry recoveries ` +
            `${teamEvidence.kothCaptureRetryRecoveries}/${teamEvidence.kothCapturePendingStarts} pending starts · ` +
            `pending at stop ` +
            `${teamEvidence.kothCapturePendingUnresolved}`,
        );
        console.log(
          `    Jeopardy UX : details ${teamEvidence.jeopardyDetailsViewed} · attachments ` +
            `${teamEvidence.jeopardyAttachmentDownloads} · wrong guesses ${teamEvidence.jeopardyWrongGuesses} · ` +
            `containers ${teamEvidence.jeopardyContainerCreates}/${teamEvidence.jeopardyContainerDeletes} ` +
            `(failures ${teamEvidence.jeopardyContainerFailures}) · durable attachments ` +
            `${jeoEvidence.attachmentDownloads} · durable containers ` +
            `${jeoEvidence.containerStarts}/${jeoEvidence.containerDestroys}`,
        );
        if (specialtyEvidence) {
          console.log(
            `    skill lift  : offense ${specialtyEvidence.offense.lift.toFixed(3)} · ` +
              `defense ${specialtyEvidence.defense.lift.toFixed(3)} · ` +
              `KotH ${specialtyEvidence.koth.lift.toFixed(3)} · ` +
              `Jeopardy ${specialtyEvidence.jeopardy.lift.toFixed(3)}`,
          );
        }
        console.log(
          `    KotH network: failed attempts ${teamEvidence.kothCaptureAttemptFailures} ` +
            `(network ${teamEvidence.kothCaptureNetworkErrors}, 4xx ${teamEvidence.kothCaptureHttp4xx}, ` +
            `5xx ${teamEvidence.kothCaptureHttp5xx}, other ${teamEvidence.kothCaptureOtherStatusFailures}) · ` +
            `burst exhaustions ${teamEvidence.kothCaptureBurstExhaustions} · ` +
            `terminal windows ${teamEvidence.kothCaptureTerminalWindows} · ` +
            `state unavailable ${teamEvidence.kothCaptureStateUnavailable} · ` +
            `target unavailable ${teamEvidence.kothCaptureTargetUnavailable} · ` +
            `invariant failures ${teamEvidence.kothCapturePendingInvariantFailures}`,
        );
        console.log(
          `    KotH patches: ${teamEvidence.kothPatchSuccesses}/${teamEvidence.kothPatchAttempts} applied ` +
            `(healthy ${teamEvidence.kothPatchHealthy}, mumble ${teamEvidence.kothPatchMumble}, ` +
            `offline ${teamEvidence.kothPatchOffline}, failures ${teamEvidence.kothPatchFailures}) · repairs ` +
            `${teamEvidence.kothPatchRepairs}/${teamEvidence.kothPatchRepairAttempts} ` +
            `(failures ${teamEvidence.kothPatchRepairFailures}) · ` +
            `blocked/bypassed takeovers ${teamEvidence.kothPatchBlockedTakeovers}/` +
            `${teamEvidence.kothPatchBypassedTakeovers} · healthy holds ` +
            `${teamEvidence.kothPatchHealthyHolds}/${teamEvidence.kothPatchHoldChecks} ` +
            `(interruptions ${teamEvidence.kothPatchHoldInterruptions}, failures ` +
            `${teamEvidence.kothPatchHoldCheckFailures}) · replacement-observed patch losses ` +
            `${teamEvidence.kothPatchResetLosses}/${teamEvidence.kothPatchResetChecks}`,
        );
      }
    } else {
      console.log(
        `    throughput : ${pick(/http_reqs[^\n]*?\s([\d.]+)\/s/)} req/s over ${DURATION}`,
      );
      console.log(
        `    5xx / non2 : ${pick(/server_5xx[^\n]*?:\s([\d.]+%)/)} 5xx · ${pick(/non_2xx[^\n]*?:\s([\d.]+%)/)} non-2xx (429s excluded)`,
      );
      console.log(
        `    board p95  : ${pick(/board_poll_ms[^\n]*?p\(95\)=([^\s]+)/)}   epoch A&D p95 ${pick(/ad_epoch_board_ms[^\n]*?p\(95\)=([^\s]+)/)}   details p95 ${pick(/details_ms[^\n]*?p\(95\)=([^\s]+)/)}   A&D submit p95 ${pick(/ad_submit_ms[^\n]*?p\(95\)=([^\s]+)/)}`,
      );
      console.log(
        `    onboard p95: ${pick(/onboard_ms[^\n]*?p\(95\)=([^\s]+)/)}   captures accepted ${pick(/captures_accepted[^\n]*?\s(\d+)/)}   dedup dups seen ${pick(/dedup_duplicates[^\n]*?\s(\d+)/)}`,
      );
      console.log(
        `    k6 process  : exit ${k6Exit ?? "none"}${k6Signal ? ` signal ${k6Signal}` : ""}`,
      );
      if (k6SpawnError || (k6Exit !== 0 && k6Err)) {
        console.log(
          `    k6 stderr   : ${(k6SpawnError?.message || k6Err).trim().slice(-500)}`,
        );
      }
    }
    console.log(
      `    livez      : ${probe.ok} ok / ${probe.fail} fail · p95 ${pc(0.95) | 0}ms max ${maxProbeLatency | 0}ms`,
    );
    console.log(
      `    healthz    : ${readinessProbe.ok} ready / ${readinessProbe.fail} unavailable`,
    );
    if (distributedTeamClients) {
      console.log(
        `    heavy paths: BYOC tunnels ${up} · VPN team clients ${FLEET} · KotH captures ${kothCaps}`,
      );
      console.log(
        `    real A&D   : ${adEvidence.completeRounds} complete / ${adEvidence.rounds} observed rounds · ` +
          `${adEvidence.accepted} accepted · ${adEvidence.attackers}/${FLEET} attackers · ` +
          `${adEvidence.victims}/${FLEET} victims · ${adEvidence.pairs} unique pairs`,
      );
      if (REALISTIC_COMPETITION) {
        const attackCv =
          adEvidence.attackerMean > 0
            ? adEvidence.attackerStddev / adEvidence.attackerMean
            : 0;
        console.log(
          `    A&D spread : ${adEvidence.distinctAttackerCounts} attacker counts · CV ${attackCv.toFixed(3)} · ` +
            `range ${adEvidence.attackerMin}-${adEvidence.attackerMax} · ` +
            `${adEvidence.contestedVictimRounds} contested victim-rounds · ${adEvidence.repeatedPairs} repeated pairs`,
        );
        console.log(
          `    lead churn : A&D leaders ${adLeadersSeen.size} / changes ${adLeaderChanges} · ` +
            `Jeopardy leaders ${jeopardyLeadersSeen.size} / changes ${jeopardyLeaderChanges} · ` +
            `KotH confirmed leaders ${kothLeadersSeen.size}`,
        );
      }
    } else {
      console.log(
        `    heavy paths: BYOC tunnels ${up} · containers spawned ${pick(/containers_spawned[^\n]*?\s(\d+)/)} (p95 ${pick(/container_ms[^\n]*?p\(95\)=([^\s]+)/)}) · attachment dl p95 ${pick(/asset_ms[^\n]*?p\(95\)=([^\s]+)/)} · KotH captures ${kothCaps}`,
      );
      console.log(
        `    A&D rows    : ${adEvidence.accepted} accepted after the load baseline`,
      );
    }
    if (settlement) {
      console.log(
        `    settlement  : A&D=${settlement.fullySettled} · KotH=${settlement.kothFullySettled} · ` +
          `cleanup=${settlement.kothCleanup.converged} · ` +
          `unfinalized rounds ${settlement.unfinalizedRounds} · nonterminal cycles ${settlement.nonterminalCycles}`,
      );
      console.log(
        `    hill cleanup: receipts ${settlement.kothCleanup.validTerminalReceipts}/` +
          `${settlement.kothCleanup.hillCount} · live tokens ${settlement.kothCleanup.liveTokens} · ` +
          `claims ${settlement.kothCleanup.claimStates} · containers ` +
          `${settlement.kothCleanup.liveContainerRows}/${settlement.kothCleanup.sharedContainerReferences}`,
      );
      if (REALISTIC_COMPETITION) {
        console.log(
          `    final spread: A&D ${adScoreSpread.distinct} scores, range ` +
            `${adScoreSpread.minimum.toFixed(2)}-${adScoreSpread.maximum.toFixed(2)}, CV ${adScoreSpread.cv.toFixed(3)} · ` +
            `KotH ${kothScoreSpread.nonzeroDistinct} nonzero scores, range ` +
            `${kothScoreSpread.minimum.toFixed(2)}-${kothScoreSpread.maximum.toFixed(2)}, CV ${kothScoreSpread.cv.toFixed(3)} · ` +
            `Jeopardy ${jeopardyRankAudit?.distinctScores ?? 0} scores`,
        );
      }
    }

    console.log("\n  === INTEGRITY CHECKS (all must be 0) ===");
    const after = snapshot(st);
    const publishLagP95 = Number(
      sql(
        `SELECT COALESCE(percentile_cont(0.95) WITHIN GROUP (ORDER BY extract(epoch FROM flags_published_at-start_time_utc)),0) ` +
          `FROM "AdRounds" WHERE game_id=${st.mixGame} AND number>=${evidenceStartRound} ` +
          `AND flags_published_at IS NOT NULL`,
      ) || 0,
    );
    const publishLagMax = Number(
      sql(
        `SELECT COALESCE(max(extract(epoch FROM flags_published_at-start_time_utc)),0) ` +
          `FROM "AdRounds" WHERE game_id=${st.mixGame} AND number>=${evidenceStartRound} ` +
          `AND flags_published_at IS NOT NULL`,
      ) || 0,
    );
    const publishLagP95Limit = Number(
      process.env.AD_PUBLISH_LAG_P95_MAX_SECONDS ?? 8,
    );
    const publishLagMaxLimit = Number(
      process.env.AD_PUBLISH_LAG_MAX_SECONDS ?? 12,
    );
    if (
      !Number.isFinite(publishLagP95Limit) ||
      !Number.isFinite(publishLagMaxLimit) ||
      publishLagP95Limit <= 0 ||
      publishLagMaxLimit < publishLagP95Limit
    ) {
      throw new Error(
        "A&D publication-lag gates must be positive and max must be >= p95",
      );
    }
    const checks = {
      "duplicate rounds": sql(
        `SELECT count(*) FROM (SELECT number FROM "AdRounds" WHERE game_id=${st.mixGame} GROUP BY number HAVING count(*)>1) t`,
      ),
      "duplicate A&D attacks": sql(
        `SELECT count(*) FROM (SELECT 1 FROM "AdAttacks" a JOIN "AdRounds" r ON r.id=a.round_id WHERE r.game_id=${st.mixGame} GROUP BY a.attacker_participation_id,a.flag_id HAVING count(*)>1) t`,
      ),
      "duplicate KotH tokens": sql(
        `SELECT count(*) FROM (` +
          `SELECT 1 FROM "KothTokens" token JOIN "Participations" participation ON participation.id=token.participation_id ` +
          `WHERE participation.game_id=${st.mixGame} ` +
          `GROUP BY token.cycle_id,token.challenge_id,token.reset_attempt,token.participation_id ` +
          `HAVING token.cycle_id IS NOT NULL AND count(*)>1) duplicate`,
      ),
      "duplicate crown cycles": sql(
        `SELECT count(*) FROM (SELECT 1 FROM "KothCrownCycles" WHERE game_id=${st.mixGame} GROUP BY challenge_id,cycle_number HAVING count(*)>1) duplicate`,
      ),
      "overlapping active crown cycles": sql(
        `SELECT count(*) FROM (SELECT 1 FROM "KothCrownCycles" WHERE game_id=${st.mixGame} AND phase='Active' GROUP BY challenge_id HAVING count(*)>1) duplicate`,
      ),
      "duplicate KotH control ticks": sql(
        `SELECT count(*) FROM (SELECT 1 FROM "KothControlResults" WHERE game_id=${st.mixGame} GROUP BY challenge_id,ad_round_id HAVING count(*)>1) duplicate`,
      ),
      "duplicate KotH acquisitions": sql(
        `SELECT count(*) FROM (SELECT 1 FROM "KothAcquisitions" acquisition JOIN "KothCrownCycles" cycle ON cycle.id=acquisition.cycle_id WHERE cycle.game_id=${st.mixGame} GROUP BY acquisition.cycle_id,acquisition.token_id HAVING count(*)>1) duplicate`,
      ),
      "invalid KotH reset receipt chain": sql(
        kothResetReceiptIntegrityQuery(st.mixGame, crown.cycleId),
      ),
      "stale container evidence": sql(
        `SELECT count(*) FROM "KothControlResults" result JOIN "KothCrownCycles" cycle ON cycle.id=result.cycle_id WHERE result.game_id=${st.mixGame} AND result.is_scorable AND result.container_id IS DISTINCT FROM cycle.replacement_container_id`,
      ),
      "cross-cycle token evidence": sql(
        `SELECT count(*) FROM "KothControlResults" result JOIN "KothTokens" token ON token.id=result.token_id WHERE result.game_id=${st.mixGame} AND (token.cycle_id IS DISTINCT FROM result.cycle_id OR token.challenge_id IS DISTINCT FROM result.challenge_id OR token.reset_attempt IS DISTINCT FROM result.token_window_attempt)`,
      ),
      "unbound scoring control ticks": sql(
        `SELECT count(*) FROM "KothControlResults" result JOIN "AdRounds" round ON round.id=result.ad_round_id JOIN "KothOfficialConfigs" config ON config.game_id=result.game_id WHERE result.game_id=${st.mixGame} AND round.number>=config.scoring_start_round AND result.cycle_id IS NULL`,
      ),
      "scorable platform voids": sql(
        `SELECT count(*) FROM "KothControlResults" WHERE game_id=${st.mixGame} AND is_scorable AND status=3`,
      ),
      "invalid cooldown windows": sql(
        `SELECT count(*) FROM "KothCycleCooldowns" cooldown JOIN "KothCrownCycles" cycle ON cycle.id=cooldown.cycle_id JOIN "KothOfficialConfigs" config ON config.game_id=cycle.game_id WHERE cycle.game_id=${st.mixGame} AND cooldown.expires_after_round-cooldown.starts_round+1<>config.champion_cooldown_ticks`,
      ),
      "holder outside current cycle": sql(
        `SELECT count(*) FROM "KothTargets" target LEFT JOIN LATERAL (SELECT cycle.phase,cycle.confirmed_participation_id FROM "KothCrownCycles" cycle WHERE cycle.game_id=target.game_id AND cycle.challenge_id=target.challenge_id ORDER BY cycle.cycle_number DESC LIMIT 1) current ON TRUE WHERE target.game_id=${st.mixGame} AND current.phase='Active' AND target.holder_participation_id IS DISTINCT FROM current.confirmed_participation_id`,
      ),
      "duplicate runtime operations": execFileSync(
        "bash",
        [
          "-c",
          `docker ps -a --filter label=rsctf.operation --format '{{.Label "rsctf.operation"}}' | sed '/^$/d' | sort | uniq -d | wc -l`,
        ],
        { encoding: "utf8" },
      ).trim(),
      "duplicate participations": sql(
        `SELECT count(*) FROM (SELECT 1 FROM "Participations" WHERE game_id IN (${st.jeoGame},${st.mixGame}) GROUP BY game_id,team_id HAVING count(*)>1) t`,
      ),
      "non-contiguous A&D rounds": sql(
        `SELECT count(*) FROM (SELECT number,lag(number) OVER (ORDER BY number) previous FROM "AdRounds" WHERE game_id=${st.mixGame}) rounds WHERE previous IS NOT NULL AND number<>previous+1`,
      ),
      "A&D round cadence drift": sql(
        `SELECT count(*) FROM (` +
          `SELECT start_time_utc-lag(start_time_utc) OVER (ORDER BY number) AS gap, ` +
          `number, ` +
          `(SELECT ad_tick_seconds FROM "Games" WHERE id=${st.mixGame}) AS tick ` +
          `FROM "AdRounds" WHERE game_id=${st.mixGame} AND number>=${evidenceStartRound}` +
          `) rounds WHERE gap IS NOT NULL ` +
          `AND abs(extract(epoch FROM gap)-tick)>0.001`,
      ),
      "invalid A&D round duration": sql(
        `SELECT count(*) FROM "AdRounds" round JOIN "Games" game ON game.id=round.game_id ` +
          `WHERE round.game_id=${st.mixGame} AND round.number>=${evidenceStartRound} ` +
          `AND round.end_time_utc<game.end_time_utc ` +
          `AND extract(epoch FROM round.end_time_utc-round.start_time_utc)<>COALESCE(game.ad_tick_seconds,60)`,
      ),
      "overdue unfinished pipelines": sql(
        `SELECT count(*) FROM "AdRounds" WHERE game_id=${st.mixGame} AND end_time_utc<=clock_timestamp() AND pipeline_completed_at IS NULL`,
      ),
      "late scorable A&D evidence": sql(
        `SELECT count(*) FROM "AdCheckResults" result JOIN "AdRounds" round ON round.id=result.round_id WHERE round.game_id=${st.mixGame} AND result.sla_credit>0 AND result.checked_at>=round.end_time_utc`,
      ),
      "late scorable KotH evidence": sql(
        `SELECT count(*) FROM "KothControlResults" result JOIN "AdRounds" round ON round.id=result.ad_round_id WHERE result.game_id=${st.mixGame} AND result.is_scorable AND result.checked_at>=round.end_time_utc`,
      ),
      "selected A&D flag delivery failures": sql(
        `SELECT count(*) FROM "AdFlagDeliveryResults" delivery ` +
          `JOIN "AdRounds" round ON round.id=delivery.round_id ` +
          `JOIN "AdTeamServices" service ON service.id=delivery.team_service_id ` +
          `WHERE round.game_id=${st.mixGame} AND round.number>=${evidenceStartRound} ` +
          `AND service.participation_id IN (${fleetPids.join(",")}) AND delivery.delivered=false`,
      ),
      "unpublished or late A&D flags": sql(
        `SELECT count(*) FROM "AdRounds" WHERE game_id=${st.mixGame} AND number>=${evidenceStartRound} ` +
          `AND (flags_published_at IS NULL OR flags_published_at>=end_time_utc)`,
      ),
      "A&D publication p95 gate": publishLagP95 <= publishLagP95Limit ? 0 : 1,
      "A&D publication max gate": publishLagMax <= publishLagMaxLimit ? 0 : 1,
      "self-capture A&D attacks": sql(
        `SELECT count(*) FROM "AdAttacks" attack JOIN "AdFlags" flag ON flag.id=attack.flag_id JOIN "AdTeamServices" service ON service.id=flag.team_service_id JOIN "AdRounds" round ON round.id=attack.round_id WHERE round.game_id=${st.mixGame} AND attack.attacker_participation_id=service.participation_id`,
      ),
      "post-deadline A&D attacks": sql(
        `SELECT count(*) FROM "AdAttacks" attack JOIN "AdRounds" round ON round.id=attack.round_id JOIN "Games" game ON game.id=round.game_id WHERE round.game_id=${st.mixGame} AND attack.submitted_at>=game.end_time_utc`,
      ),
      "liveness probe failures": probe.fail,
      "readiness probe failures": readinessProbe.fail,
      "k6 process failure":
        !distributedTeamClients && (k6Exit !== 0 || k6SpawnError) ? 1 : 0,
      "integrated anti-cheat failure":
        integratedCheat && cheatExit !== 0 ? 1 : 0,
      panics: countContainerFatalLogs(RSCTF, evidenceNotBeforeMs),
    };
    if (distributedTeamClients) {
      checks["team-client process failures"] =
        teamStatus.succeeded === FLEET &&
        teamStatus.failed === 0 &&
        teamStatus.missing === 0
          ? 0
          : 1;
      checks["missing team evidence files"] = Math.max(
        0,
        FLEET - teamEvidence.files,
      );
      checks["malformed team evidence"] = teamEvidence.malformed;
      checks["team threshold failures"] = teamEvidence.thresholdFailures;
      if (REALISTIC_COMPETITION) {
        const longCompetition = runDuration >= 1800;
        const minimumAttackers = longCompetition
          ? Math.ceil(FLEET * 0.8)
          : Math.ceil(FLEET * 0.55);
        const minimumVictims = longCompetition
          ? Math.ceil(FLEET * 0.9)
          : Math.ceil(FLEET * 0.65);
        const minimumPairs = longCompetition ? FLEET * 5 : FLEET;
        const minimumDistinctCounts = Math.min(FLEET, longCompetition ? 15 : 4);
        const minimumCaptures = longCompetition ? FLEET * 8 : FLEET;
        const minimumActiveBuckets = longCompetition ? 6 : 2;
        const attackCv =
          adEvidence.attackerMean > 0
            ? adEvidence.attackerStddev / adEvidence.attackerMean
            : 0;
        checks["missing competitive attackers"] = Math.max(
          0,
          minimumAttackers - adEvidence.attackers,
        );
        checks["missing competitive victims"] = Math.max(
          0,
          minimumVictims - adEvidence.victims,
        );
        checks["missing competitive attack pairs"] = Math.max(
          0,
          minimumPairs - adEvidence.pairs,
        );
        checks["inactive A&D event window"] = Math.max(
          0,
          minimumActiveBuckets - adEvidence.activeBuckets,
        );
        checks["uniform attacker counts"] =
          adEvidence.distinctAttackerCounts >= minimumDistinctCounts ? 0 : 1;
        checks["insufficient capture dispersion"] =
          attackCv >= (longCompetition ? 0.15 : 0.05) && attackCv <= 1.5
            ? 0
            : 1;
        checks["missing competitive captures"] = Math.max(
          0,
          minimumCaptures - adEvidence.accepted,
        );
        checks["missing patched exploit outcomes"] =
          teamEvidence.exploitPatched > 0 ? 0 : 1;
        checks["missing patch-induced outages"] =
          teamEvidence.exploitUnavailable > 0 ? 0 : 1;
        checks["missing defense changes"] = Math.max(
          0,
          Math.ceil(FLEET * 0.7) - teamEvidence.defenseAdvancedTeams,
        );
        checks["missing defense incidents"] =
          teamEvidence.defenseIncidents > 0 ? 0 : 1;
        const defenseRecovery = auditDefenseRecovery(
          teamEvidence.defenseIncidents,
          teamEvidence.defenseRepairs,
        );
        checks["insufficient defense repair rate"] = defenseRecovery.missing;
        checks["missing action tradeoffs"] =
          teamEvidence.actionCreditsSpent > 0 &&
          teamEvidence.actionCreditDenials > 0
            ? 0
            : 1;
        checks["unresolved A&D logical captures"] =
          teamEvidence.captureUnresolved;
        checks["platform first-attempt server errors"] =
          teamEvidence.platformFirstAttemptServerErrors;
        checks["missing distributed KotH writes"] = Math.max(
          0,
          (longCompetition ? 20 : 1) - teamEvidence.kothCaptureSuccesses,
        );
        const patchOperations = auditKothPatchOperationFailures(
          teamEvidence.kothPatchAttempts,
          teamEvidence.kothPatchFailures,
          teamEvidence.kothPatchRepairAttempts,
          teamEvidence.kothPatchRepairFailures,
        );
        checks["excessive KotH patch operation failures"] =
          patchOperations.valid ? 0 : 1;
        if (longCompetition) {
          checks["missing capture-patch-healthy-hold lifecycle"] = Math.max(
            0,
            1 - teamEvidence.kothPatchLifecycleTeams,
          );
          checks["missing defended KotH takeover interaction"] =
            teamEvidence.kothPatchBlockedTakeovers +
              teamEvidence.kothPatchBypassedTakeovers >
            0
              ? 0
              : 1;
          checks["missing KotH patch-loss replacement evidence"] = Math.max(
            0,
            1 - teamEvidence.kothPatchResetTeams,
          );
        }
        checks["KotH patch reset retained old instance"] =
          teamEvidence.kothPatchResetRetentions;
        checks["KotH patch reset check failures"] =
          teamEvidence.kothPatchResetCheckFailures;
        checks["KotH healthy-hold check failures"] =
          teamEvidence.kothPatchHoldCheckFailures;
        checks["missing competitive Jeopardy solves"] = Math.max(
          0,
          (longCompetition ? FLEET * 2 : 1) - jeoEvidence.accepted,
        );
        checks["missing competitive Jeopardy solvers"] = Math.max(
          0,
          (longCompetition ? Math.ceil(FLEET * 0.9) : 1) - jeoEvidence.solvers,
        );
        checks["uniform Jeopardy solve counts"] =
          jeoEvidence.distinctSolveCounts >= (longCompetition ? 4 : 2) ? 0 : 1;
        checks["missing Jeopardy challenge coverage"] = Math.max(
          0,
          (st.jeopardyCatalog || []).length - jeoEvidence.challenges,
        );
        checks["inactive Jeopardy event window"] = Math.max(
          0,
          minimumActiveBuckets - jeoEvidence.activeBuckets,
        );
        checks["missing wrong Jeopardy guesses"] =
          jeoEvidence.wrong > 0 && teamEvidence.jeopardyWrongGuesses > 0
            ? 0
            : 1;
        checks["Jeopardy solve evidence mismatch"] = Math.abs(
          teamEvidence.jeopardySubmissions - jeoEvidence.accepted,
        );
        checks["Jeopardy wrong-guess evidence mismatch"] = Math.abs(
          teamEvidence.jeopardyWrongGuesses - jeoEvidence.wrong,
        );
        checks["missing Jeopardy detail journeys"] =
          teamEvidence.jeopardyDetailsViewed >= jeoEvidence.accepted ? 0 : 1;
        checks["missing attachment journeys"] = Math.max(
          0,
          (longCompetition ? 20 : 1) - jeoEvidence.attachmentDownloads,
        );
        checks["attachment journey evidence mismatch"] = Math.abs(
          teamEvidence.jeopardyAttachmentDownloads -
            jeoEvidence.attachmentDownloads,
        );
        checks["missing container journeys"] = Math.max(
          0,
          (longCompetition ? 8 : 1) - jeoEvidence.containerJourneys,
        );
        checks["container start evidence mismatch"] = Math.abs(
          teamEvidence.jeopardyContainerCreates - jeoEvidence.containerStarts,
        );
        checks["container destroy evidence mismatch"] = Math.abs(
          teamEvidence.jeopardyContainerDeletes - jeoEvidence.containerDestroys,
        );
        checks["incomplete authoritative container journeys"] =
          Math.abs(
            jeoEvidence.containerStarts - jeoEvidence.containerDestroys,
          ) +
          Math.abs(jeoEvidence.containerStarts - jeoEvidence.containerJourneys);
        checks["incomplete client container journeys"] = Math.abs(
          teamEvidence.jeopardyContainerCreates -
            teamEvidence.jeopardyContainerDeletes,
        );
        checks["Jeopardy container workflow failures"] =
          teamEvidence.jeopardyContainerFailures;
        const expectedTiers = {
          "always-on": 10,
          committed: 25,
          "part-time": 45,
          casual: 20,
        };
        const expectedSpecialties = {
          offense: 20,
          defense: 20,
          koth: 20,
          jeopardy: 20,
          balanced: 20,
        };
        checks["profile allocation mismatch"] =
          FLEET === 100 &&
          Object.entries(expectedTiers).every(
            ([tier, count]) => teamEvidence.tiers[tier] === count,
          )
            ? 0
            : FLEET === 100
              ? 1
              : 0;
        checks["specialty allocation mismatch"] =
          FLEET === 100 &&
          Object.entries(expectedSpecialties).every(
            ([specialty, count]) =>
              teamEvidence.specialties[specialty] === count,
          )
            ? 0
            : FLEET === 100
              ? 1
              : 0;
        checks["invalid final A&D ranking"] = adRankAudit?.valid ? 0 : 1;
        checks["invalid final KotH ranking"] = kothRankAudit?.valid ? 0 : 1;
        checks["invalid final Jeopardy ranking"] = jeopardyRankAudit?.valid
          ? 0
          : 1;
        checks["missing A&D leader changes"] = Math.max(
          0,
          (longCompetition ? 1 : 0) - adLeaderChanges,
        );
        checks["missing Jeopardy leader changes"] = Math.max(
          0,
          (longCompetition ? 1 : 0) - jeopardyLeaderChanges,
        );
        checks["incomplete specialty outcome evidence"] =
          specialtyEvidence?.complete ? 0 : 1;
        for (const specialty of ["offense", "defense", "koth", "jeopardy"]) {
          checks[`missing ${specialty} specialty lift`] =
            specialtyEvidence?.[specialty]?.lift >=
            specialtyLiftFloor(specialty, longCompetition)
              ? 0
              : 1;
        }
        checks["unmerged anti-cheat evidence"] = cheatResultMerged ? 0 : 1;
      } else {
        const minAdAttackRounds = Number(
          process.env.AD_MIN_ATTACK_ROUNDS ??
            (runDuration >= 300
              ? Math.max(1, Math.floor(runDuration / 90))
              : 0),
        );
        if (!Number.isSafeInteger(minAdAttackRounds) || minAdAttackRounds < 0) {
          throw new Error(
            `AD_MIN_ATTACK_ROUNDS must be a non-negative integer (got ${minAdAttackRounds})`,
          );
        }
        const minimumPairs = FLEET * Math.min(minAdAttackRounds, FLEET - 1);
        checks["missing DB attacker coverage"] = Math.max(
          0,
          FLEET - adEvidence.attackers,
        );
        checks["missing DB victim coverage"] = Math.max(
          0,
          FLEET - adEvidence.victims,
        );
        checks["missing rotated attack pairs"] = Math.max(
          0,
          minimumPairs - adEvidence.pairs,
        );
        checks["missing complete A&D rounds"] = Math.max(
          0,
          minAdAttackRounds - adEvidence.completeRounds,
        );
        checks["missing accepted captures"] = Math.max(
          0,
          FLEET * minAdAttackRounds - adEvidence.accepted,
        );
      }
      const loadUsers = st.adUsers
        .slice(0, FLEET)
        .map((id) => `'${id}'::uuid`)
        .join(",");
      const attributedIps = Number(
        sql(
          `SELECT count(DISTINCT ip) FROM "AspNetUsers" WHERE id IN (${loadUsers}) AND ip IS NOT NULL`,
        ) || 0,
      );
      const recentActivity = Number(
        sql(
          `SELECT count(*) FROM "AspNetUsers" WHERE id IN (${loadUsers}) AND last_visited_utc>=to_timestamp(${evidenceNotBeforeMs}/1000.0)`,
        ) || 0,
      );
      const minimumActiveUsers = REALISTIC_COMPETITION
        ? Math.ceil(FLEET * 0.8)
        : FLEET;
      checks["missing distinct client IPs"] = Math.max(
        0,
        minimumActiveUsers - attributedIps,
      );
      checks["missing recent user activity"] = Math.max(
        0,
        minimumActiveUsers - recentActivity,
      );
    }
    const completedCycles = Number(
      sql(
        `SELECT count(*) FROM "KothCrownCycles" cycle WHERE cycle.game_id=${st.mixGame} ` +
          `AND cycle.phase='Completed' AND EXISTS (` +
          `SELECT 1 FROM "KothControlResults" result WHERE result.cycle_id=cycle.id ` +
          `AND result.id>${kothControlBaselineId})`,
      ) || 0,
    );
    const acquisitions = Number(
      sql(
        `SELECT count(*) FROM "KothAcquisitions" acquisition ` +
          `JOIN "KothCrownCycles" cycle ON cycle.id=acquisition.cycle_id ` +
          `WHERE cycle.game_id=${st.mixGame} AND acquisition.id>${kothAcquisitionBaselineId}`,
      ) || 0,
    );
    const provisionalClaimants = Number(
      sql(
        `SELECT count(DISTINCT provisional_participation_id) FROM "KothControlResults" ` +
          `WHERE game_id=${st.mixGame} AND id>${kothControlBaselineId} ` +
          `AND provisional_participation_id IS NOT NULL`,
      ) || 0,
    );
    const confirmedControllers = Number(
      sql(
        `SELECT count(DISTINCT confirmed_participation_id) FROM "KothControlResults" ` +
          `WHERE game_id=${st.mixGame} AND id>${kothControlBaselineId} ` +
          `AND confirmed_participation_id IS NOT NULL`,
      ) || 0,
    );
    const interruptedClaims = Number(
      sql(
        `WITH ordered AS (` +
          `SELECT cycle_id,ad_round_id,provisional_participation_id,` +
          `lag(provisional_participation_id) OVER (PARTITION BY cycle_id ORDER BY ad_round_id) AS previous ` +
          `FROM "KothControlResults" WHERE game_id=${st.mixGame} ` +
          `AND id>${kothControlBaselineId} AND is_scorable` +
          `) SELECT count(*) FROM ordered WHERE previous IS NOT NULL ` +
          `AND provisional_participation_id IS NOT NULL AND provisional_participation_id<>previous`,
      ) || 0,
    );
    const stableConfirmations = Number(
      sql(
        `WITH ordered AS (` +
          `SELECT result.cycle_id,result.container_id,result.token_id,` +
          `result.controlling_participation_id,result.provisional_participation_id,` +
          `result.confirmed_participation_id,result.confirmation_streak,result.status,` +
          `result.marker_observed,result.ad_round_id,` +
          `lag(result.container_id) OVER evidence AS previous_container_id,` +
          `lag(result.token_id) OVER evidence AS previous_token_id,` +
          `lag(result.controlling_participation_id) OVER evidence AS previous_controller,` +
          `lag(result.provisional_participation_id) OVER evidence AS previous_provisional,` +
          `lag(result.confirmed_participation_id) OVER evidence AS previous_confirmed,` +
          `lag(result.confirmation_streak) OVER evidence AS previous_streak,` +
          `lag(result.status) OVER evidence AS previous_status,` +
          `lag(result.marker_observed) OVER evidence AS previous_marker_observed ` +
          `FROM "KothControlResults" result WHERE result.game_id=${st.mixGame} ` +
          `AND result.id>${kothControlBaselineId} AND result.is_scorable ` +
          `WINDOW evidence AS (PARTITION BY result.cycle_id ORDER BY result.ad_round_id,result.id)` +
          `), qualified AS (` +
          `SELECT cycle_id,container_id,token_id,controlling_participation_id,ad_round_id ` +
          `FROM ordered WHERE status=0 AND previous_status=0 ` +
          `AND marker_observed AND previous_marker_observed ` +
          `AND confirmation_streak=2 AND previous_streak=1 ` +
          `AND token_id IS NOT NULL AND token_id=previous_token_id ` +
          `AND container_id IS NOT NULL AND container_id=previous_container_id ` +
          `AND controlling_participation_id IS NOT NULL ` +
          `AND controlling_participation_id=previous_controller ` +
          `AND confirmed_participation_id=controlling_participation_id ` +
          `AND provisional_participation_id IS NULL ` +
          `AND previous_provisional=controlling_participation_id ` +
          `AND previous_confirmed IS NULL` +
          `), exact AS (` +
          `SELECT qualified.cycle_id,qualified.container_id,qualified.token_id,` +
          `qualified.controlling_participation_id,qualified.ad_round_id ` +
          `FROM qualified JOIN "KothAcquisitions" acquisition ` +
          `ON acquisition.cycle_id=qualified.cycle_id ` +
          `AND acquisition.container_id=qualified.container_id ` +
          `AND acquisition.token_id=qualified.token_id ` +
          `AND acquisition.participation_id=qualified.controlling_participation_id ` +
          `WHERE acquisition.id>${kothAcquisitionBaselineId} ` +
          `GROUP BY qualified.cycle_id,qualified.container_id,qualified.token_id,` +
          `qualified.controlling_participation_id,qualified.ad_round_id ` +
          `HAVING count(*)=1 AND min(acquisition.ad_round_id)=qualified.ad_round_id` +
          `) SELECT count(DISTINCT cycle_id) FROM exact`,
      ) || 0,
    );
    const duration = runDuration;
    // Crown-cycle acceptance is based on authoritative scoring rounds. Reset/readiness
    // remains excluded from evidence, but round boundaries themselves must not drift.
    const minAcquisitions = Number(
      process.env.CROWN_MIN_ACQUISITIONS ?? (duration >= 120 ? 1 : 0),
    );
    const minCompleted = Number(
      process.env.CROWN_MIN_COMPLETED ?? (duration >= 240 ? 1 : 0),
    );
    const minStaleRejections = Number(
      process.env.CROWN_MIN_STALE_REJECTIONS ??
        (duration >= 300 && (!REALISTIC_COMPETITION || duration >= 1800)
          ? 1
          : 0),
    );
    const defaultStableConfirmations =
      REALISTIC_COMPETITION && duration >= 1800
        ? Math.max(3, Math.floor(duration / 600))
        : duration >= 120
          ? 1
          : 0;
    const minStableConfirmations = Number(
      process.env.CROWN_MIN_STABLE_CONFIRMATIONS ?? defaultStableConfirmations,
    );
    if (
      !Number.isSafeInteger(minStableConfirmations) ||
      minStableConfirmations < 0
    ) {
      throw new Error(
        `CROWN_MIN_STABLE_CONFIRMATIONS must be a non-negative integer (got ${minStableConfirmations})`,
      );
    }
    checks["missing confirmed acquisitions"] = Math.max(
      0,
      minAcquisitions - acquisitions,
    );
    checks["missing completed crown cycles"] = Math.max(
      0,
      minCompleted - completedCycles,
    );
    checks["missing stale-token rejections"] = Math.max(
      0,
      minStaleRejections - staleRejections,
    );
    checks["missing stable confirmations"] = Math.max(
      0,
      minStableConfirmations - stableConfirmations,
    );
    if (REALISTIC_COMPETITION) {
      const longCompetition = runDuration >= 1800;
      checks["uniform final A&D scores"] =
        adScoreSpread.distinct >= Math.min(FLEET, longCompetition ? 20 : 4)
          ? 0
          : 1;
      checks["flat final A&D range"] =
        adScoreSpread.maximum - adScoreSpread.minimum >=
        adScoreRangeFloor(longCompetition)
          ? 0
          : 1;
      checks["uniform final KotH scores"] =
        kothScoreSpread.nonzero >= Math.min(FLEET, longCompetition ? 10 : 1) &&
        kothScoreSpread.nonzeroDistinct >=
          Math.min(FLEET, longCompetition ? 4 : 1)
          ? 0
          : 1;
      checks["missing KotH leader competition"] = Math.max(
        0,
        Math.min(FLEET, longCompetition ? 8 : 1) - kothLeadersSeen.size,
      );
      checks["missing provisional claimants"] = Math.max(
        0,
        Math.min(FLEET, longCompetition ? 15 : 1) - provisionalClaimants,
      );
      checks["missing confirmed controllers"] = Math.max(
        0,
        Math.min(FLEET, longCompetition ? 8 : 1) - confirmedControllers,
      );
      checks["missing interrupted claims"] = Math.max(
        0,
        (longCompetition ? 1 : 0) - interruptedClaims,
      );
      checks["KotH pending balance errors"] =
        teamEvidence.kothCapturePendingBalanceErrors;
      checks["KotH pending invariant failures"] =
        teamEvidence.kothCapturePendingInvariantFailures;
      checks["KotH attempt status imbalance"] =
        teamEvidence.kothCaptureStatusBalanceErrors;
      checks["KotH terminal capture windows"] =
        teamEvidence.kothCaptureTerminalWindows;
    }
    if (settlement) {
      checks["A&D scoreboard unsettled"] = settlement.fullySettled ? 0 : 1;
      checks["KotH scoreboard unsettled"] = settlement.kothFullySettled ? 0 : 1;
      checks["unfinalized rounds after end"] = settlement.unfinalizedRounds;
      checks["nonterminal cycles after end"] = settlement.nonterminalCycles;
      Object.assign(checks, settlement.kothCleanup.failures);
    }
    // Capacity runs may still have an in-flight generic journey here. Competitive
    // clients enter a drain window and must delete every created challenge container
    // before their summaries are accepted.
    const inflight = after.containers - before.containers;
    if (REALISTIC_COMPETITION) {
      checks["live Jeopardy journey containers"] = Math.max(0, inflight);
    }
    let bad = 0;
    for (const [k, v] of Object.entries(checks)) {
      const ok = String(v) === "0";
      if (!ok) bad++;
      console.log(`    ${ok ? "✓" : "✗"} ${k.padEnd(30)} ${v}`);
    }
    console.log(
      `    · containers in-flight at check (auto-expire + reaped at teardown): ${inflight}`,
    );
    console.log(
      `    · KotH kings elected (control results with a controller): ${sql(`SELECT count(*) FROM "KothControlResults" WHERE game_id=${st.mixGame} AND controlling_participation_id IS NOT NULL`)}`,
    );
    console.log(
      `    · crown cycles completed: ${completedCycles} · confirmed acquisitions: ${acquisitions}`,
    );
    console.log(
      `    · qualified stable confirmations: ${stableConfirmations} ` +
        `(same cycle/container/token/team, exact single acquisition; gate ${minStableConfirmations})`,
    );
    if (REALISTIC_COMPETITION) {
      console.log(
        `    · competitive KotH: ${provisionalClaimants} provisional claimants · ` +
          `${confirmedControllers} confirmed controllers · ${interruptedClaims} interrupted claims`,
      );
    }
    console.log(
      `    · A&D flag publication lag: p95 ${publishLagP95.toFixed(3)}s / max ${publishLagMax.toFixed(3)}s ` +
        `(round ${evidenceStartRound}+; gates ${publishLagP95Limit}s / ${publishLagMaxLimit}s)`,
    );
    console.log(
      bad === 0
        ? "\n  ✓ ALL INTEGRITY CHECKS PASSED"
        : `\n  ✗ ${bad} CHECK(S) FAILED`,
    );

    if (bad > 0) {
      throw new Error(`${bad} lifecycle integrity check(s) failed`);
    }
    retentionCompletionPending = process.env.RETAIN_EVENT === "1";
  } catch (error) {
    runFailure = error;
    if (st && lifecycleRunClaimed) {
      // The workload fleet is always reaped below. End a failed event at
      // its abort boundary so it cannot continue producing empty scoring rounds
      // after its simulated players and challenge services are gone. A normal
      // disposable run removes this manifest only after every cleanup succeeds.
      try {
        const failedState = A.readState() || st;
        A.writeState(abortedLifecycleState(failedState, error, Date.now()));
        st = A.readState() || st;
      } catch (manifestError) {
        abortPreparationErrors.push(
          `persist abort state: ${manifestError.message}`,
        );
      }
      try {
        sql(
          `UPDATE "Games" SET end_time_utc=LEAST(end_time_utc,clock_timestamp()) ` +
            `WHERE id IN (${Number(st.jeoGame)},${Number(st.mixGame)})`,
        );
      } catch (deadlineError) {
        abortPreparationErrors.push(
          `set event abort deadline: ${deadlineError.message}`,
        );
      }
    }
    throw error;
  } finally {
    const cleanupErrors = [...abortPreparationErrors];
    const attempt = async (label, action) => {
      try {
        await action();
      } catch (error) {
        cleanupErrors.push(`${label}: ${error.message}`);
      }
    };
    // A shutdown can win an interruptible mutation's Promise.race. Drain that
    // mutation before cleanup discovers resources or decides the run is done.
    await inFlightMutations.drain();
    await attempt("stop host k6", () => stopChildTree(k6Process));
    await attempt("stop integrated anti-cheat drill", () =>
      stopChildTree(cheatProcess, { processGroup: true }),
    );
    await attempt("resume official scoring", async () => {
      if (
        lifecycleRunClaimed &&
        shouldResumeOfficialScoring(runFailure !== null) &&
        scoringPausedByHarness &&
        st
      ) {
        await A.setAdScoringPaused(st.mixGame, false);
        scoringPausedByHarness = false;
      }
    });
    if (lifecycleRunClaimed) {
      await attempt("remove VPN team clients", () => {
        const retainedOwnership =
          teamClientOwnership ?? A.readState()?.teamClientOwnership ?? null;
        const result = TeamClients.teardownVpnTeamClients(retainedOwnership);
        if (retainedOwnership && st) {
          const current = A.readState() || st;
          A.writeState({
            ...current,
            teamClientOwnership: null,
            teamClientContainersReapedAtMs: A.nowMs(),
            teamClientContainersReaped: result.removed,
          });
          st = A.readState();
        }
      });
      // Reap only the relay/service fleet claimed by this invocation,
      // including partial startup failures.
      await attempt("remove BYOC fleet", () => A.teardownFleet(st));
      await attempt("remove bootstrap hill", () => A.teardownHill());
    }
    if (retentionCompletionPending && st && cleanupErrors.length === 0) {
      await attempt("finalize retained manifest", () => {
        const retainedState = A.readState() || st;
        A.writeState({
          ...retainedState,
          retained: true,
          retainedAtMs: A.nowMs(),
          simulationStatus: "completed",
          simulationMode: REALISTIC_COMPETITION ? "competitive" : "capacity",
        });
        st = A.readState();
        console.log(`  retained event manifest completed at ${A.stateFile}`);
      });
    }
    if (lifecycleRunClaimed && st && process.env.KEEP !== "1") {
      await attempt("remove event namespace", () =>
        A.teardownNamespace([st.jeoGame, st.mixGame]),
      );
      if (cleanupErrors.length === 0) {
        rmSync(A.stateFile, { force: true });
        console.log("  torn down (KEEP=1 to keep)");
      }
    } else if (lifecycleRunClaimed && st && cleanupErrors.length === 0) {
      console.log(
        "  BYOC fleet reaped; namespace + managed crown hill kept (KEEP=1)",
      );
    }
    if (cleanupErrors.length) {
      if (lifecycleRunClaimed && st && A.readState()) {
        try {
          const recoveryState = A.readState() || st;
          A.writeState(
            cleanupFailureState(
              recoveryState,
              cleanupErrors,
              runFailure !== null,
              Date.now(),
            ),
          );
          st = A.readState();
        } catch (error) {
          cleanupErrors.push(
            `persist cleanup recovery state: ${error.message}`,
          );
        }
      }
      const message = `lifecycle cleanup incomplete: ${cleanupErrors.join("; ")}`;
      if (runFailure) console.error(message);
      else throw new Error(message);
    }
  }
}

function snapshot(st) {
  return {
    containers: Number(
      sql(
        `SELECT count(*) FROM "Containers" c JOIN "GameInstances" gi ON gi.id=c.game_instance_id JOIN "Participations" p ON p.id=gi.participation_id WHERE p.game_id IN (${st.jeoGame},${st.mixGame})`,
      ) || 0,
    ),
  };
}

main()
  .catch((e) => {
    console.error("lifecycle failed:", e.message);
    process.exitCode =
      shutdownSignal === "SIGINT"
        ? 130
        : shutdownSignal === "SIGTERM"
          ? 143
          : 1;
  })
  .finally(async () => {
    removeShutdownHandlers();
    await orchestrationLock?.release();
  });
