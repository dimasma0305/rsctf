import assert from "node:assert/strict";
import test from "node:test";

import {
  MANDATORY_TEAM_EVIDENCE_COUNTERS,
  TEAM_EVIDENCE_SCHEMA_VERSION,
  aggregateTeamEvidence,
  expectedTeamEvidenceFilename,
  validateAdCaptureEvidenceConservation,
  validateKothEvidenceConservation,
  validateKothPatchEvidenceConservation,
  validatePlatformRetryEvidence,
  validateTeamEvidence,
  validateWorkloadEvidenceConservation,
} from "../team-evidence.js";

const runId = "4ecbded0-c199-40c2-8f34-da9606a98967";
const generatedAtMs = 1_700_000_060_000;
const competitionSeed = "team-evidence-v2";

function profile(index, count = 2) {
  return {
    version: 2,
    index,
    seed: `${competitionSeed}:${index}`,
    engagementTier: index % 2 === 0 ? "expert" : "regular",
    specialty: index % 2 === 0 ? "offense" : "defense",
    activity: 0.8,
    thinkSeconds: 5 + (index % 2),
    offense: 0.7,
    defense: 0.6,
    koth: 0.5,
    jeopardy: 0.4,
    skillBudget: 2.2,
    risk: 0.45,
    persistence: 0.7,
    exploration: 0.35,
    actionCreditsPerRound: 4,
    maxAttacks: 2,
    firstPatchProgress: 0.25,
    secondPatchProgress: 0.65,
    discoveryRounds: 3,
    rivalIndex: (index + 1) % count,
  };
}

const baseExpected = {
  runId,
  eventCreatedAtMs: 1_700_000_000_000,
  gameId: 44,
  jeopardyGameId: 45,
  kothChallengeId: 149,
  epochStartRound: 12,
  teamCount: 2,
  participationIds: [501, 502],
  competitionSeed,
  competitionModelVersion: 2,
  duration: "1h",
  profiles: [profile(0), profile(1)],
  notBeforeMs: generatedAtMs - 60_000,
  notAfterMs: generatedAtMs + 60_000,
};

function counts(overrides = {}) {
  const values = Object.fromEntries(
    MANDATORY_TEAM_EVIDENCE_COUNTERS.map((name) => [name, 0]),
  );
  return {
    ...values,
    http_reqs: 50,
    iterations_completed: 4,
    active_iterations: 3,
    idle_iterations: 1,
    rounds_seen: 1,
    action_credits_spent: 2,
    koth_capture_attempts: 3,
    koth_capture_successes: 2,
    koth_opening_claims: 1,
    koth_takeover_claims: 1,
    koth_capture_attempt_failures: 1,
    koth_capture_network_errors: 1,
    koth_capture_pending_starts: 1,
    koth_capture_retry_recoveries: 1,
    ...overrides,
  };
}

function evidence(index, overrides = {}) {
  const metricCounts = overrides.counts || counts();
  return {
    schemaVersion: TEAM_EVIDENCE_SCHEMA_VERSION,
    generatedAt: new Date(generatedAtMs + index).toISOString(),
    team: {
      index,
      count: 2,
      participationId: baseExpected.participationIds[index],
    },
    event: {
      runId,
      eventCreatedAtMs: baseExpected.eventCreatedAtMs,
      gameId: baseExpected.gameId,
      jeopardyGameId: baseExpected.jeopardyGameId,
      kothChallengeId: baseExpected.kothChallengeId,
      epochStartRound: baseExpected.epochStartRound,
    },
    workload: {
      duration: baseExpected.duration,
      thinkSeconds: baseExpected.profiles[index].thinkSeconds,
      mode: "competitive",
      seed: competitionSeed,
      modelVersion: 2,
    },
    profile: baseExpected.profiles[index],
    thresholdsPassed: true,
    metrics: Object.fromEntries(
      Object.entries(metricCounts).map(([name, count]) => [
        name,
        { values: { count } },
      ]),
    ),
    ...overrides.evidence,
  };
}

function entry(index, overrides) {
  return {
    filename: expectedTeamEvidenceFilename(index),
    evidence: evidence(index, overrides),
  };
}

test("validates one run-bound team artifact and aggregates exact fleet totals", () => {
  const single = validateTeamEvidence(evidence(0), {
    ...baseExpected,
    teamIndex: 0,
    participationId: 501,
    filename: "team-000.json",
  });
  assert.equal(single.participationId, 501);
  assert.equal(single.profileTier, "expert");
  assert.equal(single.profileSpecialty, "offense");
  assert.equal(single.koth.pendingUnresolved, 0);

  const aggregate = aggregateTeamEvidence([entry(0), entry(1)], baseExpected);
  assert.equal(aggregate.files, 2);
  assert.equal(aggregate.metricCounts.http_reqs, 100);
  assert.equal(aggregate.koth.attempts, 6);
  assert.deepEqual(aggregate.tiers, { expert: 1, regular: 1 });
  assert.deepEqual(aggregate.specialties, { offense: 1, defense: 1 });
});

test("rejects mutated competitive engagement and specialty identity", () => {
  const expected = {
    ...baseExpected,
    teamIndex: 0,
    participationId: 501,
    filename: "team-000.json",
  };
  assert.throws(
    () =>
      validateTeamEvidence(
        evidence(0, {
          evidence: { profile: { ...profile(0), engagementTier: "" } },
        }),
        expected,
      ),
    /profile.*engagementTier does not match|profile.*configuration/,
  );
  assert.throws(
    () =>
      validateTeamEvidence(
        evidence(0, {
          evidence: { profile: { ...profile(0), specialty: "" } },
        }),
        expected,
      ),
    /profile.*specialty does not match|profile.*configuration/,
  );
  assert.throws(
    () =>
      validateTeamEvidence(
        evidence(0, {
          evidence: { profile: { ...profile(0), specialty: "   " } },
        }),
        expected,
      ),
    /profile.*specialty does not match|profile.*configuration/,
  );
  assert.throws(
    () =>
      validateTeamEvidence(
        evidence(0, {
          evidence: { profile: { ...profile(0), engagementTier: 7 } },
        }),
        expected,
      ),
    /profile.*engagementTier does not match|profile.*configuration/,
  );
});

test("aggregates repeated specialties exactly across the fleet", () => {
  const entries = [
    entry(0),
    entry(1, {
      evidence: {
        profile: { ...profile(1), specialty: "offense" },
      },
    }),
  ];
  const aggregate = aggregateTeamEvidence(entries, {
    ...baseExpected,
    profiles: [profile(0), { ...profile(1), specialty: "offense" }],
  });
  assert.deepEqual(aggregate.specialties, { offense: 2 });
  assert.equal(
    Object.values(aggregate.specialties).reduce(
      (total, value) => total + value,
      0,
    ),
    baseExpected.teamCount,
  );
});

test("rejects stale, cross-run, swapped, missing, and extra team artifacts", () => {
  assert.throws(
    () =>
      validateTeamEvidence(
        evidence(0, {
          evidence: {
            event: { ...evidence(0).event, runId: "different-run-id-0000001" },
          },
        }),
        {
          ...baseExpected,
          teamIndex: 0,
          participationId: 501,
          filename: "team-000.json",
        },
      ),
    /different competition run/,
  );
  assert.throws(
    () =>
      validateTeamEvidence(
        evidence(0, {
          evidence: { generatedAt: new Date(1_600_000_000_000).toISOString() },
        }),
        {
          ...baseExpected,
          teamIndex: 0,
          participationId: 501,
          filename: "team-000.json",
        },
      ),
    /outside the competition run window/,
  );
  assert.throws(
    () =>
      aggregateTeamEvidence(
        [{ ...entry(0), filename: "team-001.json" }, entry(1)],
        baseExpected,
      ),
    /filenames must be distinct|missing team-000/,
  );
  assert.throws(
    () => aggregateTeamEvidence([entry(0)], baseExpected),
    /exactly 2 team files/,
  );
  assert.throws(
    () =>
      aggregateTeamEvidence(
        [
          entry(0),
          entry(1),
          { filename: "team-002.json", evidence: evidence(1) },
        ],
        baseExpected,
      ),
    /exactly 2 team files/,
  );
});

test("requires every schema-v9 counter as a non-negative safe integer", () => {
  const missing = counts();
  delete missing.koth_capture_attempts;
  assert.throws(
    () =>
      validateTeamEvidence(evidence(0, { counts: missing }), {
        ...baseExpected,
        teamIndex: 0,
        participationId: 501,
        filename: "team-000.json",
      }),
    /missing mandatory metric koth_capture_attempts/,
  );
  const withoutRuntimeErrors = evidence(0);
  delete withoutRuntimeErrors.metrics.iteration_runtime_errors;
  assert.throws(
    () =>
      validateTeamEvidence(withoutRuntimeErrors, {
        ...baseExpected,
        teamIndex: 0,
        participationId: 501,
        filename: "team-000.json",
      }),
    /missing mandatory metric iteration_runtime_errors/,
  );
  const renamed = evidence(0);
  renamed.metrics.koth_capture_attempt = renamed.metrics.koth_capture_attempts;
  delete renamed.metrics.koth_capture_attempts;
  assert.throws(
    () =>
      validateTeamEvidence(renamed, {
        ...baseExpected,
        teamIndex: 0,
        participationId: 501,
        filename: "team-000.json",
      }),
    /missing mandatory metric koth_capture_attempts/,
  );
  const truncated = evidence(0);
  truncated.metrics.defense_repairs = { values: {} };
  assert.throws(
    () =>
      validateTeamEvidence(truncated, {
        ...baseExpected,
        teamIndex: 0,
        participationId: 501,
        filename: "team-000.json",
      }),
    /defense_repairs is missing values.count/,
  );
  assert.throws(
    () =>
      validateTeamEvidence(
        evidence(0, { counts: counts({ koth_capture_attempts: 3.5 }) }),
        {
          ...baseExpected,
          teamIndex: 0,
          participationId: 501,
          filename: "team-000.json",
        },
      ),
    /safe integer/,
  );
  assert.throws(
    () =>
      validateTeamEvidence(
        evidence(0, { counts: counts({ koth_capture_attempts: -1 }) }),
        {
          ...baseExpected,
          teamIndex: 0,
          participationId: 501,
          filename: "team-000.json",
        },
      ),
    /safe integer/,
  );
});

test("KotH patch evidence conserves patch, repair, takeover, hold, and reset outcomes", () => {
  const valid = counts({
    koth_capture_network_errors: 0,
    koth_capture_http_4xx: 1,
    koth_patch_attempts: 3,
    koth_patch_successes: 2,
    koth_patch_failures: 1,
    koth_patch_healthy: 1,
    koth_patch_mumble: 1,
    koth_patch_repair_attempts: 1,
    koth_patch_repairs: 1,
    koth_patch_blocked_takeovers: 1,
    koth_patch_bypassed_takeovers: 1,
    koth_patch_healthy_holds: 2,
    koth_patch_hold_checks: 2,
    koth_patch_hold_interruptions: 1,
    koth_patch_reset_checks: 1,
    koth_patch_reset_losses: 1,
  });
  const result = validateKothPatchEvidenceConservation(valid);
  assert.equal(result.patchAttempts, 3);
  assert.equal(result.resetLosses, 1);
  assert.throws(
    () =>
      validateKothPatchEvidenceConservation({
        ...valid,
        koth_patch_successes: 1,
      }),
    /patch attempts/,
  );
  assert.throws(
    () =>
      validateKothPatchEvidenceConservation({
        ...valid,
        koth_patch_reset_losses: 0,
      }),
    /reset check/,
  );
  assert.throws(
    () =>
      validateKothPatchEvidenceConservation({
        ...valid,
        koth_patch_hold_checks: 1,
      }),
    /healthy-hold check/,
  );
  assert.throws(
    () =>
      validateKothPatchEvidenceConservation({
        ...valid,
        koth_patch_hold_interruptions: 3,
      }),
    /interrupted holds/,
  );
});

test("platform retry evidence retains every first failure and terminal outcome", () => {
  assert.deepEqual(
    validatePlatformRetryEvidence(
      counts({
        platform_first_attempt_failures: 4,
        platform_first_attempt_timeouts: 1,
        platform_first_attempt_rate_limits: 2,
        platform_first_attempt_server_errors: 1,
        platform_retry_attempts: 4,
        platform_retry_recoveries: 3,
        platform_retry_exhaustions: 1,
      }),
    ),
    {
      firstFailures: 4,
      firstTimeouts: 1,
      firstRateLimits: 2,
      firstServerErrors: 1,
      attempts: 4,
      recoveries: 3,
      exhaustions: 1,
    },
  );
  assert.throws(
    () =>
      validatePlatformRetryEvidence(
        counts({
          platform_first_attempt_failures: 3,
          platform_first_attempt_rate_limits: 3,
          platform_retry_attempts: 2,
          platform_retry_recoveries: 2,
        }),
      ),
    /first-attempt failures must equal retry attempts/,
  );
  assert.throws(
    () =>
      validatePlatformRetryEvidence(
        counts({
          platform_first_attempt_failures: 3,
          platform_first_attempt_timeouts: 1,
          platform_first_attempt_rate_limits: 1,
          platform_first_attempt_server_errors: 1,
          platform_retry_attempts: 3,
          platform_retry_recoveries: 2,
        }),
      ),
    /attempts must equal recoveries plus exhaustions/,
  );
  assert.throws(
    () =>
      validatePlatformRetryEvidence(
        counts({
          platform_first_attempt_failures: 3,
          platform_first_attempt_timeouts: 1,
          platform_first_attempt_rate_limits: 1,
          platform_retry_attempts: 3,
          platform_retry_recoveries: 3,
        }),
      ),
    /unclassified or double-classified first-attempt failures/,
  );
});

test("A&D capture evidence balances logical captures independently of replays", () => {
  assert.deepEqual(
    validateAdCaptureEvidenceConservation(
      counts({
        capture_attempts: 4,
        accepted_captures: 2,
        duplicate_captures: 1,
        terminal_capture_verdicts: 1,
        capture_submission_replays: 3,
      }),
    ),
    {
      attempts: 4,
      accepted: 2,
      duplicates: 1,
      terminalVerdicts: 1,
      submissionReplays: 3,
      settled: 4,
      unresolved: 0,
    },
  );
  assert.throws(
    () =>
      validateAdCaptureEvidenceConservation(
        counts({
          capture_attempts: 2,
          accepted_captures: 1,
          capture_submission_replays: 5,
        }),
      ),
    /1 unresolved logical capture/,
  );
  assert.throws(
    () =>
      validateAdCaptureEvidenceConservation(
        counts({
          capture_attempts: 1,
          accepted_captures: 1,
          duplicate_captures: 1,
        }),
      ),
    /-1 unresolved logical capture/,
  );
});

test("workload evidence conserves iterations and emitted domain relationships", () => {
  assert.deepEqual(validateWorkloadEvidenceConservation(counts(), profile(0)), {
    classifiedIterations: 4,
    workCompletionSamples: 4,
    workCompletionSkew: 0,
    runtimeErrors: 0,
    unclassifiedTail: 0,
    maximumUnclassifiedTail: 1,
    active: 3,
    idle: 1,
    exploitAttempts: 0,
    exploitOutcomes: 0,
    defenseUpdates: 0,
    defenseIncidents: 0,
    defenseRepairs: 0,
    jeopardyProgressActions: 0,
    kothAttempts: 3,
    kothBurstExhaustions: 0,
    kothPatchAttempts: 0,
    kothPatchRepairAttempts: 0,
    creditsSpent: 2,
  });
  assert.throws(
    () =>
      validateWorkloadEvidenceConservation(
        counts({ iterations_completed: 5 }),
        profile(0),
      ),
    /more work-completion samples than classified iterations/,
  );
  assert.deepEqual(
    validateWorkloadEvidenceConservation(
      counts({
        http_reqs: 500,
        iterations_completed: 329,
        active_iterations: 189,
        idle_iterations: 141,
      }),
      profile(0),
    ),
    {
      classifiedIterations: 330,
      workCompletionSamples: 329,
      workCompletionSkew: 1,
      runtimeErrors: 0,
      unclassifiedTail: 1,
      maximumUnclassifiedTail: 1,
      active: 189,
      idle: 141,
      exploitAttempts: 0,
      exploitOutcomes: 0,
      defenseUpdates: 0,
      defenseIncidents: 0,
      defenseRepairs: 0,
      jeopardyProgressActions: 0,
      kothAttempts: 3,
      kothBurstExhaustions: 0,
      kothPatchAttempts: 0,
      kothPatchRepairAttempts: 0,
      creditsSpent: 2,
    },
  );
  assert.throws(
    () =>
      validateWorkloadEvidenceConservation(
        counts({
          http_reqs: 500,
          iterations_completed: 328,
          active_iterations: 189,
          idle_iterations: 141,
        }),
        profile(0),
      ),
    /unclassified hard-stop tail 2 exceeds 1 iteration/,
  );
  assert.throws(
    () =>
      validateWorkloadEvidenceConservation(
        counts({
          http_reqs: 500,
          iterations_completed: 329,
          active_iterations: 189,
          idle_iterations: 141,
          iteration_runtime_errors: 1,
        }),
        profile(0),
      ),
    /records 1 caught iteration runtime error/,
  );
  assert.throws(
    () =>
      validateWorkloadEvidenceConservation(
        counts({ iteration_runtime_errors: 1 }),
        profile(0),
      ),
    /runtime errors 1 exceed 0 incomplete classified iteration/,
  );
  assert.throws(
    () =>
      validateTeamEvidence(
        evidence(0, {
          counts: counts({
            iterations_completed: 3,
            iteration_runtime_errors: 1,
          }),
        }),
        {
          ...baseExpected,
          teamIndex: 0,
          participationId: 501,
          filename: "team-000.json",
        },
      ),
    /records 1 caught iteration runtime error/,
  );
  assert.throws(
    () =>
      validateWorkloadEvidenceConservation(
        counts({
          exploit_attempts: 1,
          exploit_captures: 2,
          action_credits_spent: 1,
          rounds_seen: 1,
        }),
        profile(0),
      ),
    /more classified exploit outcomes/,
  );
  assert.throws(
    () =>
      validateWorkloadEvidenceConservation(
        counts({
          defense_updates: 1,
          defense_incidents: 1,
          defense_repairs: 2,
        }),
        profile(0),
      ),
    /more defense repairs than incidents/,
  );
  assert.throws(
    () =>
      validateWorkloadEvidenceConservation(
        counts({
          jeopardy_details_viewed: 1,
          jeopardy_container_creates: 1,
          jeopardy_container_deletes: 2,
        }),
        profile(0),
      ),
    /deletes more Jeopardy containers/,
  );
});

test("fleet workload evidence permits at most one hard-stop tail per client", () => {
  const aggregate = aggregateTeamEvidence(
    [0, 1].map((index) => ({
      filename: expectedTeamEvidenceFilename(index),
      evidence: evidence(index, {
        counts: counts({ iterations_completed: 3 }),
      }),
    })),
    baseExpected,
  );
  assert.equal(aggregate.workload.unclassifiedTail, 2);
  assert.equal(aggregate.workload.maximumUnclassifiedTail, 2);
});

test("workload evidence enforces action budgets and KotH burst accounting", () => {
  assert.throws(
    () =>
      validateWorkloadEvidenceConservation(
        counts({ rounds_seen: 1, action_credits_spent: 6 }),
        profile(0),
      ),
    /more action credits than its observed-round budget/,
  );
  assert.throws(
    () =>
      validateWorkloadEvidenceConservation(
        counts({ koth_capture_burst_exhaustions: 1 }),
        profile(0),
      ),
    /impossible KotH burst exhaustion/,
  );
  assert.throws(
    () =>
      validateWorkloadEvidenceConservation(
        counts({
          koth_capture_attempts: 2,
          koth_capture_successes: 0,
          koth_opening_claims: 0,
          koth_takeover_claims: 0,
          koth_capture_attempt_failures: 2,
          koth_capture_network_errors: 2,
          koth_capture_pending_starts: 1,
          koth_capture_retry_recoveries: 0,
          koth_capture_window_closed: 1,
          koth_capture_burst_exhaustions: 1,
          action_credits_spent: 0,
        }),
        profile(0),
      ),
    /logical KotH claims than spent action credits/,
  );
});

test("binds workload and the complete assigned profile to the frozen run", () => {
  const expected = {
    ...baseExpected,
    teamIndex: 0,
    participationId: 501,
    filename: "team-000.json",
  };
  for (const workload of [
    { ...evidence(0).workload, seed: "other-seed" },
    { ...evidence(0).workload, modelVersion: 1 },
    { ...evidence(0).workload, duration: "12m" },
    { ...evidence(0).workload, thinkSeconds: 99 },
  ]) {
    assert.throws(
      () =>
        validateTeamEvidence(evidence(0, { evidence: { workload } }), expected),
      /workload.*frozen competition configuration/,
    );
  }

  const missingField = { ...profile(0) };
  delete missingField.exploration;
  assert.throws(
    () =>
      validateTeamEvidence(
        evidence(0, { evidence: { profile: missingField } }),
        expected,
      ),
    /profile fields do not match the frozen competition configuration/,
  );
  assert.throws(
    () =>
      validateTeamEvidence(
        evidence(0, {
          evidence: { profile: { ...profile(0), offense: 0.7000001 } },
        }),
        expected,
      ),
    /profile.offense does not match the frozen competition configuration/,
  );
});

test("enforces attempt, status, claim, and pending conservation", () => {
  assert.throws(
    () =>
      validateKothEvidenceConservation(counts({ koth_capture_attempts: 4 })),
    /attempts = successes/,
  );
  assert.throws(
    () =>
      validateKothEvidenceConservation(
        counts({ koth_capture_network_errors: 0 }),
      ),
    /unclassified or double-classified/,
  );
  assert.throws(
    () => validateKothEvidenceConservation(counts({ koth_takeover_claims: 0 })),
    /successes = opening claims/,
  );
  assert.throws(
    () =>
      validateKothEvidenceConservation(
        counts({ koth_capture_retry_recoveries: 0 }),
      ),
    /unresolved logical capture/,
  );
});

test("does not waive one unresolved or terminal capture per team", () => {
  const fleetProfiles = Array.from({ length: 100 }, (_, index) =>
    profile(index, 100),
  );
  const unresolvedEntries = Array.from({ length: 100 }, (_, index) => {
    const participantId = 1_000 + index;
    const teamEvidence = evidence(0, {
      counts: counts({ koth_capture_retry_recoveries: 0 }),
      evidence: {
        team: { index, count: 100, participationId: participantId },
        workload: {
          ...evidence(0).workload,
          thinkSeconds: fleetProfiles[index].thinkSeconds,
        },
        profile: fleetProfiles[index],
      },
    });
    return {
      filename: expectedTeamEvidenceFilename(index),
      evidence: teamEvidence,
    };
  });
  const expected = {
    ...baseExpected,
    teamCount: 100,
    participationIds: Array.from({ length: 100 }, (_, index) => 1_000 + index),
    profiles: fleetProfiles,
  };
  assert.throws(
    () => aggregateTeamEvidence(unresolvedEntries, expected),
    /team 0 KotH evidence has 1 unresolved logical capture/,
  );

  assert.throws(
    () =>
      validateKothEvidenceConservation(
        counts({
          koth_capture_retry_recoveries: 0,
          koth_capture_window_closed: 1,
          koth_capture_terminal_windows: 1,
        }),
      ),
    /terminal capture windows/,
  );
  assert.throws(
    () =>
      validateKothEvidenceConservation(
        counts({
          koth_capture_retry_recoveries: 0,
          koth_capture_pending_invariant_failures: 1,
        }),
      ),
    /pending-capture invariant failures/,
  );
});
