import { PLAYER_ACTION_COSTS } from "./player-model.js";

export const TEAM_EVIDENCE_SCHEMA_VERSION = 9;
export const TEAM_RUNNER_LOG_FILENAME = "runner.log";
export const MAX_TEAM_RUNNER_LOG_BYTES = 1024 * 1024;

// Schema-v9 evidence never infers an absent counter as zero. k6 must emit every
// counter below, including explicit zeroes, so a truncated summary cannot pass.
export const MANDATORY_TEAM_EVIDENCE_COUNTERS = Object.freeze([
  "http_reqs",
  "platform_first_attempt_failures",
  "platform_first_attempt_timeouts",
  "platform_first_attempt_rate_limits",
  "platform_first_attempt_server_errors",
  "platform_retry_attempts",
  "platform_retry_recoveries",
  "platform_retry_exhaustions",
  "vpn_retry_attempts",
  "accepted_captures",
  "duplicate_captures",
  "prior_round_captures",
  "capture_attempts",
  "capture_submission_replays",
  "terminal_capture_verdicts",
  "rounds_seen",
  "flag_sync_waits",
  "flag_delivery_failures",
  "iterations_completed",
  "active_iterations",
  "idle_iterations",
  "iteration_runtime_errors",
  "exploit_attempts",
  "exploit_patched",
  "exploit_captures",
  "defense_updates",
  "defense_incidents",
  "defense_repairs",
  "exploit_unavailable",
  "action_credits_spent",
  "action_credit_denials",
  "jeopardy_submissions",
  "jeopardy_details_viewed",
  "jeopardy_attachment_downloads",
  "jeopardy_wrong_guesses",
  "jeopardy_container_creates",
  "jeopardy_container_deletes",
  "jeopardy_container_failures",
  "koth_capture_attempts",
  "koth_capture_successes",
  "koth_opening_claims",
  "koth_takeover_claims",
  "koth_reset_races",
  "koth_capture_window_closed",
  "koth_capture_ineligible_transitions",
  "koth_capture_state_unavailable",
  "koth_capture_attempt_failures",
  "koth_capture_retry_recoveries",
  "koth_capture_pending_starts",
  "koth_capture_burst_exhaustions",
  "koth_capture_terminal_windows",
  "koth_capture_pending_invariant_failures",
  "koth_capture_network_errors",
  "koth_capture_http_4xx",
  "koth_capture_http_5xx",
  "koth_capture_other_status_failures",
  "koth_capture_target_unavailable",
  "koth_target_identity_mismatches",
  "koth_patch_attempts",
  "koth_patch_successes",
  "koth_patch_failures",
  "koth_patch_healthy",
  "koth_patch_mumble",
  "koth_patch_offline",
  "koth_patch_repair_attempts",
  "koth_patch_repairs",
  "koth_patch_repair_failures",
  "koth_patch_blocked_takeovers",
  "koth_patch_bypassed_takeovers",
  "koth_patch_healthy_holds",
  "koth_patch_hold_checks",
  "koth_patch_hold_check_failures",
  "koth_patch_hold_interruptions",
  "koth_patch_reset_checks",
  "koth_patch_reset_losses",
  "koth_patch_reset_retentions",
  "koth_patch_reset_check_failures",
]);

const RUN_ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_-]{14,126}[A-Za-z0-9]$/;

function object(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value;
}

function integer(value, label, minimum = 0) {
  if (
    typeof value !== "number" ||
    !Number.isSafeInteger(value) ||
    value < minimum
  ) {
    throw new Error(`${label} must be a safe integer >= ${minimum}`);
  }
  return value;
}

function runId(value, label = "competition run id") {
  if (typeof value !== "string" || !RUN_ID_PATTERN.test(value)) {
    throw new Error(`${label} is invalid`);
  }
  return value;
}

function nonEmptyString(value, label) {
  if (typeof value !== "string" || !value.trim()) {
    throw new Error(`${label} must be a non-empty string`);
  }
  return value.trim();
}

function exactPrimitiveObject(value, expected, label) {
  const actualObject = object(value, label);
  const expectedObject = object(expected, `expected ${label}`);
  const actualKeys = Object.keys(actualObject).sort();
  const expectedKeys = Object.keys(expectedObject).sort();
  if (
    actualKeys.length !== expectedKeys.length ||
    actualKeys.some((key, index) => key !== expectedKeys[index])
  ) {
    throw new Error(
      `${label} fields do not match the frozen competition configuration`,
    );
  }
  for (const key of expectedKeys) {
    const expectedValue = expectedObject[key];
    if (
      expectedValue === null ||
      !["string", "number", "boolean"].includes(typeof expectedValue) ||
      actualObject[key] !== expectedValue
    ) {
      throw new Error(
        `${label}.${key} does not match the frozen competition configuration`,
      );
    }
  }
  return actualObject;
}

function frozenProfile(value, index, teamCount, competitionSeed, label) {
  const profile = object(value, label);
  for (const [name, value] of Object.entries(profile)) {
    if (
      value === null ||
      !["string", "number", "boolean"].includes(typeof value)
    ) {
      throw new Error(`${label}.${name} must be a JSON primitive`);
    }
  }
  if (
    profile.version !== 2 ||
    profile.index !== index ||
    profile.seed !== `${competitionSeed}:${index}` ||
    typeof profile.engagementTier !== "string" ||
    !profile.engagementTier ||
    typeof profile.specialty !== "string" ||
    !profile.specialty
  ) {
    throw new Error(`${label} identity is invalid`);
  }
  for (const name of [
    "activity",
    "offense",
    "defense",
    "koth",
    "jeopardy",
    "skillBudget",
    "risk",
    "persistence",
    "exploration",
    "firstPatchProgress",
    "secondPatchProgress",
  ]) {
    if (typeof profile[name] !== "number" || !Number.isFinite(profile[name])) {
      throw new Error(`${label}.${name} must be finite`);
    }
  }
  for (const name of [
    "thinkSeconds",
    "actionCreditsPerRound",
    "maxAttacks",
    "discoveryRounds",
    "rivalIndex",
  ]) {
    integer(profile[name], `${label}.${name}`);
  }
  if (profile.rivalIndex >= teamCount) {
    throw new Error(`${label}.rivalIndex must reference the frozen roster`);
  }
  return profile;
}

function addCount(total, increment, label) {
  const next = total + increment;
  if (!Number.isSafeInteger(next))
    throw new Error(`${label} exceeds the safe integer range`);
  return next;
}

function sumCounts(counts, names, label) {
  return names.reduce((total, name) => addCount(total, counts[name], label), 0);
}

function multiplyCount(left, right, label) {
  const product = left * right;
  if (!Number.isSafeInteger(product))
    throw new Error(`${label} exceeds the safe integer range`);
  return product;
}

function exactIdArray(values, expectedLength, label) {
  if (!Array.isArray(values) || values.length !== expectedLength) {
    throw new Error(`${label} must contain exactly ${expectedLength} ids`);
  }
  const ids = values.map((value, index) =>
    integer(value, `${label}[${index}]`, 1),
  );
  if (new Set(ids).size !== ids.length)
    throw new Error(`${label} must contain distinct ids`);
  return ids;
}

export function expectedTeamEvidenceFilename(index) {
  const teamIndex = integer(index, "team index");
  if (teamIndex > 999)
    throw new Error(
      "team index cannot be represented by the evidence filename contract",
    );
  return `team-${String(teamIndex).padStart(3, "0")}.json`;
}

function metricCounts(metrics) {
  const source = object(metrics, "team evidence metrics");
  return Object.fromEntries(
    MANDATORY_TEAM_EVIDENCE_COUNTERS.map((name) => {
      if (!Object.prototype.hasOwnProperty.call(source, name)) {
        throw new Error(`team evidence is missing mandatory metric ${name}`);
      }
      const metric = object(source[name], `team evidence metric ${name}`);
      const values = object(
        metric.values,
        `team evidence metric ${name}.values`,
      );
      if (!Object.prototype.hasOwnProperty.call(values, "count")) {
        throw new Error(`team evidence metric ${name} is missing values.count`);
      }
      return [
        name,
        integer(values.count, `team evidence metric ${name}.values.count`),
      ];
    }),
  );
}

export function validateKothEvidenceConservation(
  counts,
  label = "KotH evidence",
) {
  object(counts, label);
  for (const name of MANDATORY_TEAM_EVIDENCE_COUNTERS.filter((name) =>
    name.startsWith("koth_"),
  )) {
    integer(counts[name], `${label} ${name}`);
  }

  const attempts = counts.koth_capture_attempts;
  const successes = counts.koth_capture_successes;
  const attemptFailures = counts.koth_capture_attempt_failures;
  const classifiedFailures = sumCounts(
    counts,
    [
      "koth_capture_network_errors",
      "koth_capture_http_4xx",
      "koth_capture_http_5xx",
      "koth_capture_other_status_failures",
    ],
    `${label} classified failures`,
  );
  const claims = sumCounts(
    counts,
    ["koth_opening_claims", "koth_takeover_claims"],
    `${label} claims`,
  );
  const pendingResolved = sumCounts(
    counts,
    [
      "koth_capture_retry_recoveries",
      "koth_reset_races",
      "koth_capture_window_closed",
      "koth_capture_ineligible_transitions",
      "koth_capture_pending_invariant_failures",
    ],
    `${label} pending resolutions`,
  );
  const pendingUnresolved =
    counts.koth_capture_pending_starts - pendingResolved;

  if (
    attempts !==
    addCount(successes, attemptFailures, `${label} attempt outcomes`)
  ) {
    throw new Error(
      `${label} violates attempts = successes + attempt failures`,
    );
  }
  if (attemptFailures !== classifiedFailures) {
    throw new Error(
      `${label} has unclassified or double-classified attempt failures`,
    );
  }
  if (successes !== claims) {
    throw new Error(
      `${label} violates successes = opening claims + takeover claims`,
    );
  }
  if (pendingUnresolved !== 0) {
    throw new Error(
      `${label} has ${pendingUnresolved} unresolved logical capture(s)`,
    );
  }
  if (counts.koth_capture_retry_recoveries > successes) {
    throw new Error(
      `${label} has more retry recoveries than successful captures`,
    );
  }
  if (counts.koth_capture_pending_starts > attemptFailures) {
    throw new Error(
      `${label} has more logical capture starts than failed attempts`,
    );
  }
  if (counts.koth_capture_pending_invariant_failures !== 0) {
    throw new Error(`${label} contains pending-capture invariant failures`);
  }
  if (counts.koth_capture_terminal_windows !== 0) {
    throw new Error(`${label} contains terminal capture windows`);
  }
  if (
    counts.koth_capture_terminal_windows > counts.koth_capture_window_closed
  ) {
    throw new Error(
      `${label} has terminal windows without matching closed windows`,
    );
  }
  const patches = validateKothPatchEvidenceConservation(
    counts,
    `${label} patch lifecycle`,
  );

  return Object.freeze({
    attempts,
    successes,
    attemptFailures,
    classifiedFailures,
    claims,
    pendingStarts: counts.koth_capture_pending_starts,
    pendingResolved,
    pendingUnresolved,
    patches,
  });
}

export function validateKothPatchEvidenceConservation(
  counts,
  label = "KotH patch evidence",
) {
  object(counts, label);
  const value = (name) => integer(counts[name], `${label} ${name}`);
  const patchAttempts = value("koth_patch_attempts");
  const patchSuccesses = value("koth_patch_successes");
  const patchFailures = value("koth_patch_failures");
  const healthy = value("koth_patch_healthy");
  const mumble = value("koth_patch_mumble");
  const offline = value("koth_patch_offline");
  const repairAttempts = value("koth_patch_repair_attempts");
  const repairs = value("koth_patch_repairs");
  const repairFailures = value("koth_patch_repair_failures");
  const blockedTakeovers = value("koth_patch_blocked_takeovers");
  const bypassedTakeovers = value("koth_patch_bypassed_takeovers");
  const healthyHolds = value("koth_patch_healthy_holds");
  const holdChecks = value("koth_patch_hold_checks");
  const holdCheckFailures = value("koth_patch_hold_check_failures");
  const holdInterruptions = value("koth_patch_hold_interruptions");
  const resetChecks = value("koth_patch_reset_checks");
  const resetLosses = value("koth_patch_reset_losses");
  const resetRetentions = value("koth_patch_reset_retentions");
  const resetCheckFailures = value("koth_patch_reset_check_failures");

  if (
    patchAttempts !==
    addCount(patchSuccesses, patchFailures, `${label} patch outcomes`)
  ) {
    throw new Error(`${label} violates patch attempts = successes + failures`);
  }
  const patchStates = sumCounts(
    counts,
    ["koth_patch_healthy", "koth_patch_mumble", "koth_patch_offline"],
    `${label} successful patch states`,
  );
  if (patchSuccesses !== patchStates) {
    throw new Error(`${label} does not classify every successful patch state`);
  }
  if (
    repairAttempts !==
    addCount(repairs, repairFailures, `${label} repair outcomes`)
  ) {
    throw new Error(`${label} violates repair attempts = successes + failures`);
  }
  if (repairs > addCount(mumble, offline, `${label} repairable incidents`)) {
    throw new Error(`${label} has more repairs than patch incidents`);
  }
  if (healthyHolds > patchSuccesses) {
    throw new Error(`${label} has more healthy holds than successful patches`);
  }
  if (
    holdChecks !==
    addCount(healthyHolds, holdCheckFailures, `${label} hold outcomes`)
  ) {
    throw new Error(
      `${label} does not classify every authoritative healthy-hold check`,
    );
  }
  if (holdInterruptions > patchSuccesses) {
    throw new Error(
      `${label} has more interrupted holds than successful patches`,
    );
  }
  if (blockedTakeovers > value("koth_capture_http_4xx")) {
    throw new Error(
      `${label} has blocked takeovers without matching capture 4xx evidence`,
    );
  }
  if (bypassedTakeovers > value("koth_takeover_claims")) {
    throw new Error(`${label} has bypasses without successful takeover claims`);
  }
  const resetOutcomes = sumCounts(
    counts,
    [
      "koth_patch_reset_losses",
      "koth_patch_reset_retentions",
      "koth_patch_reset_check_failures",
    ],
    `${label} reset outcomes`,
  );
  if (resetChecks !== resetOutcomes) {
    throw new Error(`${label} does not classify every reset check`);
  }
  if (resetChecks > patchSuccesses) {
    throw new Error(`${label} checks more resets than successful patches`);
  }

  return Object.freeze({
    patchAttempts,
    patchSuccesses,
    patchFailures,
    healthy,
    mumble,
    offline,
    repairAttempts,
    repairs,
    repairFailures,
    blockedTakeovers,
    bypassedTakeovers,
    healthyHolds,
    holdChecks,
    holdCheckFailures,
    holdInterruptions,
    resetChecks,
    resetLosses,
    resetRetentions,
    resetCheckFailures,
  });
}

export function validatePlatformRetryEvidence(
  counts,
  label = "platform retry evidence",
) {
  object(counts, label);
  const firstFailures = integer(
    counts.platform_first_attempt_failures,
    `${label} first-attempt failures`,
  );
  const firstTimeouts = integer(
    counts.platform_first_attempt_timeouts,
    `${label} first-attempt timeouts`,
  );
  const firstRateLimits = integer(
    counts.platform_first_attempt_rate_limits,
    `${label} first-attempt rate limits`,
  );
  const firstServerErrors = integer(
    counts.platform_first_attempt_server_errors,
    `${label} first-attempt server errors`,
  );
  const attempts = integer(counts.platform_retry_attempts, `${label} attempts`);
  const recoveries = integer(
    counts.platform_retry_recoveries,
    `${label} recoveries`,
  );
  const exhaustions = integer(
    counts.platform_retry_exhaustions,
    `${label} exhaustions`,
  );
  const classifiedFirstFailures = sumCounts(
    counts,
    [
      "platform_first_attempt_timeouts",
      "platform_first_attempt_rate_limits",
      "platform_first_attempt_server_errors",
    ],
    `${label} classified first-attempt failures`,
  );
  if (firstFailures !== classifiedFirstFailures) {
    throw new Error(
      `${label} has unclassified or double-classified first-attempt failures`,
    );
  }
  if (firstFailures !== attempts) {
    throw new Error(
      `${label} first-attempt failures must equal retry attempts`,
    );
  }
  if (attempts !== addCount(recoveries, exhaustions, `${label} outcomes`)) {
    throw new Error(`${label} attempts must equal recoveries plus exhaustions`);
  }
  return Object.freeze({
    firstFailures,
    firstTimeouts,
    firstRateLimits,
    firstServerErrors,
    attempts,
    recoveries,
    exhaustions,
  });
}

export function validateAdCaptureEvidenceConservation(
  counts,
  label = "A&D capture evidence",
) {
  object(counts, label);
  const attempts = integer(counts.capture_attempts, `${label} attempts`);
  const accepted = integer(
    counts.accepted_captures,
    `${label} accepted captures`,
  );
  const duplicates = integer(
    counts.duplicate_captures,
    `${label} duplicate captures`,
  );
  const terminalVerdicts = integer(
    counts.terminal_capture_verdicts,
    `${label} terminal verdicts`,
  );
  const submissionReplays = integer(
    counts.capture_submission_replays,
    `${label} submission replays`,
  );
  const settled = sumCounts(
    counts,
    ["accepted_captures", "duplicate_captures", "terminal_capture_verdicts"],
    `${label} settled outcomes`,
  );
  const unresolved = attempts - settled;
  if (unresolved !== 0) {
    throw new Error(`${label} has ${unresolved} unresolved logical capture(s)`);
  }
  return Object.freeze({
    attempts,
    accepted,
    duplicates,
    terminalVerdicts,
    submissionReplays,
    settled,
    unresolved,
  });
}

export function validateWorkloadEvidenceConservation(
  counts,
  profile = null,
  label = "team workload evidence",
  maximumUnclassifiedTail = 1,
) {
  object(counts, label);
  const value = (name) => integer(counts[name], `${label} ${name}`);
  const workCompletionSamples = value("iterations_completed");
  const active = value("active_iterations");
  const idle = value("idle_iterations");
  const runtimeErrors = value("iteration_runtime_errors");
  const classifiedIterations = addCount(
    active,
    idle,
    `${label} iteration classes`,
  );
  if (workCompletionSamples > classifiedIterations) {
    throw new Error(
      `${label} has more work-completion samples than classified iterations`,
    );
  }
  const workCompletionSkew = classifiedIterations - workCompletionSamples;
  if (runtimeErrors > workCompletionSkew) {
    throw new Error(
      `${label} runtime errors ${runtimeErrors} exceed ` +
        `${workCompletionSkew} incomplete classified iteration(s)`,
    );
  }
  const unclassifiedTail = workCompletionSkew - runtimeErrors;
  const unclassifiedTailLimit = integer(
    maximumUnclassifiedTail,
    `${label} maximum unclassified hard-stop tail`,
  );
  if (unclassifiedTail > unclassifiedTailLimit) {
    throw new Error(
      `${label} unclassified hard-stop tail ${unclassifiedTail} exceeds ` +
        `${unclassifiedTailLimit} iteration(s)`,
    );
  }
  if (runtimeErrors > 0) {
    throw new Error(
      `${label} records ${runtimeErrors} caught iteration runtime error(s)`,
    );
  }
  if (value("http_reqs") < active) {
    throw new Error(`${label} has fewer HTTP requests than active iterations`);
  }
  for (const name of ["rounds_seen", "flag_sync_waits", "capture_attempts"]) {
    if (value(name) > active) {
      throw new Error(`${label} ${name} exceeds active iterations`);
    }
  }
  if (value("capture_submission_replays") > classifiedIterations) {
    throw new Error(
      `${label} capture submission replays exceed classified iterations`,
    );
  }

  const exploitAttempts = value("exploit_attempts");
  const exploitOutcomes = sumCounts(
    counts,
    ["exploit_patched", "exploit_captures", "exploit_unavailable"],
    `${label} classified exploit outcomes`,
  );
  if (
    exploitAttempts >
    multiplyCount(active, 2, `${label} exploit attempt ceiling`)
  ) {
    throw new Error(
      `${label} has more than two exploit requests per active iteration`,
    );
  }
  if (exploitOutcomes > exploitAttempts) {
    throw new Error(
      `${label} has more classified exploit outcomes than exploit attempts`,
    );
  }
  if (value("capture_attempts") > value("exploit_captures")) {
    throw new Error(`${label} has A&D captures without a captured VPN flag`);
  }
  const settledCaptures = sumCounts(
    counts,
    ["accepted_captures", "duplicate_captures", "terminal_capture_verdicts"],
    `${label} settled A&D captures`,
  );
  if (value("prior_round_captures") > settledCaptures) {
    throw new Error(
      `${label} has prior-round captures without a settled verdict`,
    );
  }

  const defenseUpdates = value("defense_updates");
  const defenseIncidents = value("defense_incidents");
  const defenseRepairs = value("defense_repairs");
  if (defenseIncidents > defenseUpdates) {
    throw new Error(
      `${label} has more defense incidents than successful updates`,
    );
  }
  if (defenseRepairs > defenseIncidents) {
    throw new Error(`${label} has more defense repairs than incidents`);
  }
  if (
    addCount(defenseUpdates, defenseRepairs, `${label} defense operations`) >
    classifiedIterations
  ) {
    throw new Error(
      `${label} has more successful defense operations than classified iterations`,
    );
  }

  const jeopardyDetails = value("jeopardy_details_viewed");
  const jeopardyAttachments = value("jeopardy_attachment_downloads");
  const jeopardyCreates = value("jeopardy_container_creates");
  const jeopardyDeletes = value("jeopardy_container_deletes");
  const jeopardyFailures = value("jeopardy_container_failures");
  const jeopardySubmissions = value("jeopardy_submissions");
  const jeopardyWrong = value("jeopardy_wrong_guesses");
  if (
    jeopardyAttachments > jeopardyDetails ||
    jeopardyCreates > jeopardyDetails
  ) {
    throw new Error(
      `${label} has a Jeopardy journey without first viewing its challenge`,
    );
  }
  if (jeopardyDeletes > jeopardyCreates) {
    throw new Error(
      `${label} deletes more Jeopardy containers than it created`,
    );
  }
  if (jeopardyFailures > classifiedIterations) {
    throw new Error(
      `${label} has more Jeopardy container failures than classified iterations`,
    );
  }
  const jeopardyProgressActions = sumCounts(
    counts,
    [
      "jeopardy_details_viewed",
      "jeopardy_attachment_downloads",
      "jeopardy_container_creates",
      "jeopardy_submissions",
      "jeopardy_wrong_guesses",
    ],
    `${label} Jeopardy progress actions`,
  );
  if (jeopardyProgressActions > active) {
    throw new Error(
      `${label} has more Jeopardy progress actions than active iterations`,
    );
  }

  const kothAttempts = value("koth_capture_attempts");
  const kothFailures = value("koth_capture_attempt_failures");
  const kothStarts = value("koth_capture_pending_starts");
  const kothRecoveries = value("koth_capture_retry_recoveries");
  const kothSuccesses = value("koth_capture_successes");
  const kothBurstExhaustions = value("koth_capture_burst_exhaustions");
  const kothPatchAttempts = value("koth_patch_attempts");
  const kothPatchRepairAttempts = value("koth_patch_repair_attempts");
  if (kothAttempts > active) {
    throw new Error(`${label} has more KotH writes than active iterations`);
  }
  if (
    addCount(
      kothPatchAttempts,
      kothPatchRepairAttempts,
      `${label} KotH patch actions`,
    ) > active
  ) {
    throw new Error(
      `${label} has more KotH patch actions than active iterations`,
    );
  }
  if (
    kothBurstExhaustions > kothStarts ||
    multiplyCount(kothBurstExhaustions, 2, `${label} KotH exhausted attempts`) >
      kothFailures
  ) {
    throw new Error(`${label} has impossible KotH burst exhaustion evidence`);
  }
  if (kothRecoveries > kothSuccesses) {
    throw new Error(`${label} has more KotH retry recoveries than successes`);
  }
  if (value("koth_capture_terminal_windows") > kothBurstExhaustions) {
    throw new Error(
      `${label} has a terminal KotH window without an exhausted burst`,
    );
  }
  for (const name of [
    "koth_capture_state_unavailable",
    "koth_capture_target_unavailable",
    "koth_target_identity_mismatches",
  ]) {
    if (value(name) > classifiedIterations) {
      throw new Error(`${label} ${name} exceeds classified iterations`);
    }
  }

  const creditsSpent = value("action_credits_spent");
  if (
    value("action_credit_denials") >
    multiplyCount(active, 4, `${label} denial ceiling`)
  ) {
    throw new Error(
      `${label} has more action-credit denials than possible decisions`,
    );
  }
  if (profile !== null) {
    const frozen = object(profile, `${label} profile`);
    const creditsPerRound = integer(
      frozen.actionCreditsPerRound,
      `${label} action credits per round`,
      1,
    );
    // buildRoundActionBudget may grant one deterministic persistence bonus,
    // capped at the model's six-credit ceiling.
    const maximumCreditsPerRound = Math.min(6, creditsPerRound + 1);
    const creditBudget = multiplyCount(
      value("rounds_seen"),
      maximumCreditsPerRound,
      `${label} action-credit budget`,
    );
    if (creditsSpent > creditBudget) {
      throw new Error(
        `${label} spends more action credits than its observed-round budget`,
      );
    }
  }
  const newKothClaims = addCount(
    kothStarts,
    kothSuccesses - kothRecoveries,
    `${label} logical KotH claims`,
  );
  if (newKothClaims > creditsSpent) {
    throw new Error(
      `${label} has more logical KotH claims than spent action credits`,
    );
  }
  const minimumKothCredits = addCount(
    newKothClaims,
    multiplyCount(
      addCount(
        kothPatchAttempts,
        kothPatchRepairAttempts,
        `${label} KotH patch actions`,
      ),
      PLAYER_ACTION_COSTS.patch,
      `${label} KotH patch credits`,
    ),
    `${label} minimum KotH credits`,
  );
  if (minimumKothCredits > creditsSpent) {
    throw new Error(
      `${label} records more KotH actions than its spent credits permit`,
    );
  }
  const creditedOutcomes = sumCounts(
    counts,
    [
      "exploit_attempts",
      "defense_updates",
      "jeopardy_attachment_downloads",
      "jeopardy_container_creates",
      "jeopardy_submissions",
      "jeopardy_wrong_guesses",
      "koth_capture_attempts",
      "koth_patch_attempts",
      "koth_patch_repair_attempts",
    ],
    `${label} credited outcomes`,
  );
  if (creditedOutcomes > 0 && creditsSpent === 0) {
    throw new Error(
      `${label} records player actions without spending action credits`,
    );
  }

  return Object.freeze({
    classifiedIterations,
    workCompletionSamples,
    workCompletionSkew,
    runtimeErrors,
    unclassifiedTail,
    maximumUnclassifiedTail: unclassifiedTailLimit,
    active,
    idle,
    exploitAttempts,
    exploitOutcomes,
    defenseUpdates,
    defenseIncidents,
    defenseRepairs,
    jeopardyProgressActions,
    kothAttempts,
    kothBurstExhaustions,
    kothPatchAttempts,
    kothPatchRepairAttempts,
    creditsSpent,
  });
}

function expectedBinding(expected) {
  const source = object(expected, "team evidence expectation");
  const notBeforeMs = integer(
    source.notBeforeMs,
    "team evidence lower timestamp bound",
    1,
  );
  const notAfterMs = integer(
    source.notAfterMs,
    "team evidence upper timestamp bound",
    notBeforeMs,
  );
  const jeopardyGameId =
    source.jeopardyGameId === null
      ? null
      : integer(source.jeopardyGameId, "expected Jeopardy game id", 1);
  const teamCount = integer(source.teamCount, "expected team count", 2);
  const teamIndex = integer(source.teamIndex, "expected team index");
  if (teamIndex >= teamCount) {
    throw new Error("expected team index must be smaller than the team count");
  }
  const competitionSeed = nonEmptyString(
    source.competitionSeed,
    "expected competition seed",
  );
  if (competitionSeed.length > 64)
    throw new Error("expected competition seed is too long");
  const competitionModelVersion = integer(
    source.competitionModelVersion,
    "expected competition model version",
    1,
  );
  if (competitionModelVersion !== 2) {
    throw new Error("expected competition model version must be 2");
  }
  const duration = nonEmptyString(
    source.duration,
    "expected competition duration",
  );
  const durationMatch = duration.match(/^(\d+(?:\.\d+)?)(?:ms|s|m|h)$/);
  if (!durationMatch || Number(durationMatch[1]) <= 0) {
    throw new Error("expected competition duration is invalid");
  }
  if (!Array.isArray(source.profiles) || source.profiles.length !== teamCount) {
    throw new Error(
      `expected profiles must contain exactly ${teamCount} entries`,
    );
  }
  const profile = frozenProfile(
    source.profiles[teamIndex],
    teamIndex,
    teamCount,
    competitionSeed,
    `expected profile ${teamIndex}`,
  );
  return {
    runId: runId(source.runId),
    eventCreatedAtMs: integer(
      source.eventCreatedAtMs,
      "expected event creation time",
      1,
    ),
    gameId: integer(source.gameId, "expected mixed-event game id", 1),
    jeopardyGameId,
    kothChallengeId: integer(
      source.kothChallengeId,
      "expected KotH challenge id",
      1,
    ),
    epochStartRound: integer(
      source.epochStartRound,
      "expected epoch start round",
      1,
    ),
    teamCount,
    teamIndex,
    participationId: integer(
      source.participationId,
      "expected participation id",
      1,
    ),
    competitionSeed,
    competitionModelVersion,
    duration,
    profile,
    filename: source.filename,
    notBeforeMs,
    notAfterMs,
  };
}

export function validateTeamEvidence(value, expected) {
  const evidence = object(value, "team evidence");
  const binding = expectedBinding(expected);
  const expectedFilename = expectedTeamEvidenceFilename(binding.teamIndex);
  if (binding.filename !== expectedFilename) {
    throw new Error(`team evidence filename must be ${expectedFilename}`);
  }
  if (evidence.schemaVersion !== TEAM_EVIDENCE_SCHEMA_VERSION) {
    throw new Error(
      `team evidence must use schema v${TEAM_EVIDENCE_SCHEMA_VERSION}`,
    );
  }
  if (evidence.thresholdsPassed !== true) {
    throw new Error("team evidence thresholds did not all pass");
  }

  const generatedAtMs =
    typeof evidence.generatedAt === "string"
      ? Date.parse(evidence.generatedAt)
      : Number.NaN;
  if (
    !Number.isFinite(generatedAtMs) ||
    generatedAtMs < binding.notBeforeMs ||
    generatedAtMs > binding.notAfterMs
  ) {
    throw new Error(
      "team evidence timestamp is outside the competition run window",
    );
  }

  const team = object(evidence.team, "team evidence identity");
  if (
    team.index !== binding.teamIndex ||
    team.count !== binding.teamCount ||
    team.participationId !== binding.participationId
  ) {
    throw new Error("team evidence identity does not match the frozen roster");
  }

  const event = object(evidence.event, "team evidence event binding");
  if (
    event.runId !== binding.runId ||
    event.eventCreatedAtMs !== binding.eventCreatedAtMs ||
    event.gameId !== binding.gameId ||
    event.jeopardyGameId !== binding.jeopardyGameId ||
    event.kothChallengeId !== binding.kothChallengeId ||
    event.epochStartRound !== binding.epochStartRound
  ) {
    throw new Error(
      "team evidence belongs to a different competition run or event",
    );
  }

  exactPrimitiveObject(
    evidence.workload,
    {
      duration: binding.duration,
      thinkSeconds: binding.profile.thinkSeconds,
      mode: "competitive",
      seed: binding.competitionSeed,
      modelVersion: binding.competitionModelVersion,
    },
    "team evidence workload",
  );

  const counts = metricCounts(evidence.metrics);
  const koth = validateKothEvidenceConservation(
    counts,
    `team ${binding.teamIndex} KotH evidence`,
  );
  const adCaptures = validateAdCaptureEvidenceConservation(
    counts,
    `team ${binding.teamIndex} A&D capture evidence`,
  );
  const platformRetries = validatePlatformRetryEvidence(
    counts,
    `team ${binding.teamIndex} platform retry evidence`,
  );
  const profile = exactPrimitiveObject(
    evidence.profile,
    binding.profile,
    "team evidence profile",
  );
  const workload = validateWorkloadEvidenceConservation(
    counts,
    profile,
    `team ${binding.teamIndex} workload evidence`,
  );
  const profileTier = profile.engagementTier;
  const profileSpecialty = profile.specialty;

  return Object.freeze({
    filename: expectedFilename,
    teamIndex: binding.teamIndex,
    participationId: binding.participationId,
    generatedAtMs,
    profileTier,
    profileSpecialty,
    metricCounts: Object.freeze(counts),
    adCaptures,
    koth,
    platformRetries,
    workload,
  });
}

export function aggregateTeamEvidence(entries, expected) {
  const binding = object(expected, "fleet evidence expectation");
  const teamCount = integer(binding.teamCount, "expected fleet team count", 2);
  if (teamCount > 1_000)
    throw new Error("fleet evidence supports at most 1000 team files");
  const participationIds = exactIdArray(
    binding.participationIds,
    teamCount,
    "expected fleet participations",
  );
  if (!Array.isArray(entries) || entries.length !== teamCount) {
    throw new Error(
      `fleet evidence must contain exactly ${teamCount} team files`,
    );
  }

  const byFilename = new Map();
  for (const entry of entries) {
    object(entry, "fleet evidence entry");
    if (typeof entry.filename !== "string" || byFilename.has(entry.filename)) {
      throw new Error("fleet evidence filenames must be distinct strings");
    }
    byFilename.set(entry.filename, entry.evidence);
  }

  const metricCounts = Object.fromEntries(
    MANDATORY_TEAM_EVIDENCE_COUNTERS.map((name) => [name, 0]),
  );
  const tiers = new Map();
  const specialties = new Map();
  const teams = [];
  for (let teamIndex = 0; teamIndex < teamCount; teamIndex++) {
    const filename = expectedTeamEvidenceFilename(teamIndex);
    if (!byFilename.has(filename))
      throw new Error(`fleet evidence is missing ${filename}`);
    const validated = validateTeamEvidence(byFilename.get(filename), {
      ...binding,
      teamCount,
      teamIndex,
      participationId: participationIds[teamIndex],
      filename,
    });
    teams.push(validated);
    for (const name of MANDATORY_TEAM_EVIDENCE_COUNTERS) {
      metricCounts[name] = addCount(
        metricCounts[name],
        validated.metricCounts[name],
        `fleet metric ${name}`,
      );
    }
    tiers.set(
      validated.profileTier,
      (tiers.get(validated.profileTier) || 0) + 1,
    );
    specialties.set(
      validated.profileSpecialty,
      (specialties.get(validated.profileSpecialty) || 0) + 1,
    );
  }

  // Conservation is linear, but checking the sums independently makes the
  // fleet-wide acceptance contract explicit and protects future aggregation edits.
  const koth = validateKothEvidenceConservation(
    metricCounts,
    "fleet KotH evidence",
  );
  const adCaptures = validateAdCaptureEvidenceConservation(
    metricCounts,
    "fleet A&D capture evidence",
  );
  const platformRetries = validatePlatformRetryEvidence(
    metricCounts,
    "fleet platform retry evidence",
  );
  const workload = validateWorkloadEvidenceConservation(
    metricCounts,
    null,
    "fleet workload evidence",
    teams.length,
  );
  return Object.freeze({
    schemaVersion: TEAM_EVIDENCE_SCHEMA_VERSION,
    runId: runId(binding.runId),
    files: teams.length,
    teams: Object.freeze(teams),
    metricCounts: Object.freeze(metricCounts),
    tiers: Object.freeze(Object.fromEntries(tiers)),
    specialties: Object.freeze(Object.fromEntries(specialties)),
    adCaptures,
    koth,
    platformRetries,
    workload,
  });
}
