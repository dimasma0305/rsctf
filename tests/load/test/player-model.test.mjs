import assert from "node:assert/strict";
import { test } from "node:test";

import * as playerModel from "../player-model.js";
import {
  attackPlan,
  boundedPlatformRetryDelay,
  buildJeopardyCatalog,
  buildPlayerProfiles,
  buildRoundActionBudget,
  canSpendActionCredit,
  classifyPlatformFirstAttemptFailure,
  classifyKothCaptureOutcome,
  classifyKothPendingTransition,
  defenseLevelAt,
  isKothTerminalWindow,
  isJeopardyFocusRound,
  isProfileActive,
  isRetryablePlatformRequest,
  isRetryablePlatformStatus,
  isReplacementKothInstance,
  isPristineKothReplacement,
  jeopardyIntent,
  keyedUnit,
  kothCapabilityMatchesState,
  kothControllerParticipationId,
  kothHealthyHoldStatusMatches,
  kothTargetMatchesState,
  kothCapturePendingBalance,
  kothCaptureStatusBalance,
  kothIntent,
  kothPatchIntent,
  kothPatchRepairReady,
  kothTakeoverTechnique,
  parseKothServiceStatus,
  playerThinkDelay,
  publicAdNetworkTargets,
  recordAttackOutcome,
  spendActionCredit,
} from "../player-model.js";

const SEED = "rsctf-competitive-20260714-v2";

function firstJeopardyFocusRound(profile) {
  return Array.from({ length: 120 }, (_, index) => index + 1).find((round) =>
    isJeopardyFocusRound(profile, round),
  );
}

function correlation(left, right) {
  const leftMean = left.reduce((sum, value) => sum + value, 0) / left.length;
  const rightMean = right.reduce((sum, value) => sum + value, 0) / right.length;
  let covariance = 0;
  let leftVariance = 0;
  let rightVariance = 0;
  for (let index = 0; index < left.length; index++) {
    const leftDelta = left[index] - leftMean;
    const rightDelta = right[index] - rightMean;
    covariance += leftDelta * rightDelta;
    leftVariance += leftDelta * leftDelta;
    rightVariance += rightDelta * rightDelta;
  }
  return covariance / Math.sqrt(leftVariance * rightVariance);
}

function mean(values) {
  return values.reduce((sum, value) => sum + value, 0) / values.length;
}

function simulateKothLastWriteControl(profiles, competitionSeed, cycleCount) {
  const controlledTicks = Array(profiles.length).fill(0);
  const eligibleTicks = Array(profiles.length).fill(0);
  let cooldownIndex = null;

  const lastWriter = (cycleNumber, cycleTick, controllerIndex, blockedIndex) => {
    let winnerIndex = null;
    let latestWrite = -1;
    for (const profile of profiles) {
      const intent = kothIntent(
        profile,
        {
          cycleNumber,
          cycleTick,
          cycleTicks: 3,
          resetPhase: "Active",
          isScorable: true,
          eligibleNow: profile.index !== blockedIndex,
          holderParticipationId: null,
          provisionalClaimantParticipationId:
            controllerIndex === null ? null : controllerIndex + 1000,
        },
        {
          competitionSeed,
          active: true,
          attempted: false,
          availableCredits: 6,
          observationsInTick: 3,
          ownParticipationId: profile.index + 1000,
          scoreboardRank: profile.index + 1,
          teamCount: profiles.length,
        },
      );
      if (!intent.attempt) continue;
      const writeTime =
        intent.reactionObservation +
        keyedUnit(
          profile.seed,
          "koth-last-write-arrival",
          competitionSeed,
          cycleNumber,
          cycleTick,
          controllerIndex ?? "open",
        );
      if (writeTime > latestWrite) {
        latestWrite = writeTime;
        winnerIndex = profile.index;
      }
    }
    return winnerIndex;
  };

  for (let cycleNumber = 1; cycleNumber <= cycleCount; cycleNumber++) {
    for (const profile of profiles) {
      eligibleTicks[profile.index] += profile.index === cooldownIndex ? 2 : 3;
    }

    let controllerIndex = lastWriter(cycleNumber, 1, null, cooldownIndex);
    if (controllerIndex !== null) controlledTicks[controllerIndex]++;

    const takeoverIndex = lastWriter(cycleNumber, 2, controllerIndex, null);
    if (takeoverIndex !== null) controllerIndex = takeoverIndex;
    if (controllerIndex !== null) controlledTicks[controllerIndex] += 2;

    // The tick-two controller owns at least two ticks and is therefore the
    // unique cycle champion in this three-tick model. Its next opening tick is
    // excluded from the personal denominator, matching the real cooldown.
    cooldownIndex = controllerIndex;
  }

  return Object.freeze({
    controlledTicks,
    eligibleTicks,
    controlRates: controlledTicks.map(
      (controlled, index) => controlled / eligibleTicks[index],
    ),
  });
}

test("100 teams receive independent engagement and exactly balanced specialties", () => {
  const first = buildPlayerProfiles(100, SEED);
  assert.deepEqual(first, buildPlayerProfiles(100, SEED));
  assert.deepEqual(
    Object.fromEntries(
      ["always-on", "committed", "part-time", "casual"].map((tier) => [
        tier,
        first.filter((profile) => profile.engagementTier === tier).length,
      ]),
    ),
    { "always-on": 10, committed: 25, "part-time": 45, casual: 20 },
  );
  assert.deepEqual(
    Object.fromEntries(
      ["offense", "defense", "koth", "jeopardy", "balanced"].map(
        (specialty) => [
          specialty,
          first.filter((profile) => profile.specialty === specialty).length,
        ],
      ),
    ),
    { offense: 20, defense: 20, koth: 20, jeopardy: 20, balanced: 20 },
  );
  assert.ok(
    first.some(
      (profile) =>
        profile.engagementTier === "casual" && profile.specialty === "offense",
    ),
  );
  assert.ok(
    first.some(
      (profile) =>
        profile.engagementTier === "always-on" &&
        profile.specialty !== "offense",
    ),
  );
});

test("cross-domain skills have bounded budgets without one global strength axis", () => {
  const profiles = buildPlayerProfiles(100, SEED);
  const domains = ["offense", "defense", "koth", "jeopardy"];
  for (const profile of profiles) {
    const total = domains.reduce((sum, domain) => sum + profile[domain], 0);
    assert.ok(
      domains.every(
        (domain) => profile[domain] >= 0.16 && profile[domain] <= 0.94,
      ),
    );
    assert.ok(profile.skillBudget >= 2.05 && profile.skillBudget <= 2.8);
    assert.ok(Math.abs(total - profile.skillBudget) < 1e-9);
  }
  for (let left = 0; left < domains.length; left++) {
    for (let right = left + 1; right < domains.length; right++) {
      const value = correlation(
        profiles.map((profile) => profile[domains[left]]),
        profiles.map((profile) => profile[domains[right]]),
      );
      assert.ok(
        Math.abs(value) < 0.75,
        `${domains[left]}/${domains[right]} correlation ${value}`,
      );
    }
  }
  for (const domain of domains) {
    const specialists = profiles.filter(
      (profile) => profile.specialty === domain,
    );
    const others = profiles.filter((profile) => profile.specialty !== domain);
    assert.ok(
      mean(specialists.map((profile) => profile[domain])) >
        mean(others.map((profile) => profile[domain])) + 0.18,
      `${domain} specialty has no material advantage`,
    );
  }
  const topTen = Object.fromEntries(
    domains.map((domain) => [
      domain,
      new Set(
        [...profiles]
          .sort((left, right) => right[domain] - left[domain])
          .slice(0, 10)
          .map(({ index }) => index),
      ),
    ]),
  );
  assert.equal(
    profiles.filter((profile) =>
      domains.every((domain) => topTen[domain].has(profile.index)),
    ).length,
    0,
  );
});

test("engagement tiers affect sessions without determining domain skill", () => {
  const profiles = buildPlayerProfiles(100, "engagement-v2");
  const activityRanges = {
    "always-on": [0.9, 0.98],
    committed: [0.78, 0.86],
    "part-time": [0.6, 0.68],
    casual: [0.36, 0.44],
  };
  const activityMeans = Object.fromEntries(
    Object.entries(activityRanges).map(([tier, [minimum, maximum]]) => {
      const cohort = profiles.filter(
        (profile) => profile.engagementTier === tier,
      );
      assert.ok(
        cohort.every(
          (profile) =>
            profile.activity >= minimum && profile.activity <= maximum,
        ),
        `${tier} activity escaped its configured range`,
      );
      return [tier, mean(cohort.map((profile) => profile.activity))];
    }),
  );
  assert.ok(activityMeans["always-on"] > activityMeans.committed);
  assert.ok(activityMeans.committed > activityMeans["part-time"]);
  assert.ok(activityMeans["part-time"] > activityMeans.casual);
  const rates = Object.fromEntries(
    ["always-on", "committed", "part-time", "casual"].map((tier) => {
      const cohort = profiles.filter(
        (profile) => profile.engagementTier === tier,
      );
      const samples = cohort.flatMap((profile) =>
        Array.from({ length: 80 }, (_, block) =>
          isProfileActive(profile, block * 90),
        ),
      );
      return [tier, samples.filter(Boolean).length / samples.length];
    }),
  );
  assert.ok(rates["always-on"] > rates.committed);
  assert.ok(rates.committed > rates["part-time"]);
  assert.ok(rates["part-time"] > rates.casual);
  assert.ok(
    profiles.some(
      (profile) =>
        profile.engagementTier === "casual" &&
        profile.offense >
          profiles.find(
            (candidate) =>
              candidate.engagementTier === "always-on" &&
              candidate.specialty !== "offense",
          ).offense,
    ),
  );
});

test("player refresh bursts remain deterministic and inside the API cadence", () => {
  for (const profile of buildPlayerProfiles(100, "think-delay-v2")) {
    const delays = Array.from({ length: 200 }, (_, iteration) =>
      playerThinkDelay(profile, iteration),
    );
    assert.ok(delays.every((delay) => Number.isFinite(delay) && delay >= 4));
    assert.deepEqual(
      delays,
      Array.from({ length: 200 }, (_, iteration) =>
        playerThinkDelay(profile, iteration),
      ),
    );
  }
  assert.throws(
    () => playerThinkDelay(buildPlayerProfiles(2, "invalid-delay")[0], -1),
    /valid profile/,
  );
});

test("round action credits are deterministic, immutable, and conserved", () => {
  const profile = buildPlayerProfiles(10, "credits")[0];
  let budget = buildRoundActionBudget(profile, 7);
  assert.deepEqual(budget, buildRoundActionBudget(profile, 7));
  assert.equal(Object.isFrozen(budget), true);
  assert.equal(canSpendActionCredit(budget, "attack"), true);
  budget = spendActionCredit(budget, "attack");
  assert.equal(budget.spent + budget.remaining, budget.total);
  if (canSpendActionCredit(budget, "patch")) {
    budget = spendActionCredit(budget, "patch");
    assert.equal(budget.spent + budget.remaining, budget.total);
  }
  while (canSpendActionCredit(budget, "attack")) {
    budget = spendActionCredit(budget, "attack");
  }
  assert.throws(() => spendActionCredit(budget, "attack"), /insufficient/);
  assert.throws(() => canSpendActionCredit(budget, "futureAction"), /unknown/);
});

test("defense patches remain bounded under specialist schedules", () => {
  for (const profile of buildPlayerProfiles(100, "defense-v2")) {
    const levels = Array.from({ length: 101 }, (_, step) =>
      defenseLevelAt(profile, step / 100),
    );
    assert.ok(
      levels.every(
        (level) => Number.isInteger(level) && level >= 0 && level <= 2,
      ),
    );
    assert.equal(levels[0], 0);
  }
});

test("one-slot A&D plans keep the preferred public target instead of forcing a scan", () => {
  const profile = {
    ...buildPlayerProfiles(4, "one-slot")[0],
    offense: 1,
    activity: 1,
    maxAttacks: 1,
    exploration: 0,
    discoveryRounds: 0,
    rivalIndex: 2,
  };
  const publicOpponents = [
    {
      index: 1,
      rank: 1,
      settledTotal: 100,
      projectedTotal: 110,
      defenseRate: 0,
      slaRate: 1,
    },
    {
      index: 2,
      rank: 2,
      settledTotal: 50,
      projectedTotal: 50,
      defenseRate: 1,
      slaRate: 1,
    },
    {
      index: 3,
      rank: 3,
      settledTotal: 20,
      projectedTotal: 20,
      defenseRate: 0.8,
      slaRate: 1,
    },
  ];
  const plan = attackPlan(profile, publicOpponents, {}, 20, 1, 0.7, {
    maxTargets: 1,
  });
  assert.deepEqual(plan.targets, [1]);
  assert.equal(plan.strategy, "preferred");
});

test("A&D plans use learned patch outcomes and never use private opponent profiles", () => {
  const profile = {
    ...buildPlayerProfiles(5, "memory-plan")[0],
    offense: 1,
    activity: 1,
    maxAttacks: 1,
    exploration: 0,
    discoveryRounds: 0,
    rivalIndex: 4,
  };
  const opponents = [
    {
      index: 1,
      rank: 1,
      settledTotal: 100,
      projectedTotal: 100,
      defenseRate: 0,
      slaRate: 1,
    },
    {
      index: 2,
      rank: 2,
      settledTotal: 80,
      projectedTotal: 80,
      defenseRate: 0.5,
      slaRate: 1,
    },
    {
      index: 3,
      rank: 3,
      settledTotal: 40,
      projectedTotal: 40,
      defenseRate: 0.6,
      slaRate: 1,
    },
    {
      index: 4,
      rank: 4,
      settledTotal: 20,
      projectedTotal: 20,
      defenseRate: 0.8,
      slaRate: 1,
    },
  ];
  const first = attackPlan(profile, opponents, {}, 30, 1, 0.4, {
    maxTargets: 1,
  });
  assert.equal(first.targets.length, 1);
  const memory = recordAttackOutcome(
    {},
    {
      victimIndex: first.targets[0],
      technique: first.technique,
      outcome: "patched",
      round: 30,
    },
  );
  const next = attackPlan(profile, opponents, memory, 31, 1, 0.4, {
    maxTargets: 1,
  });
  assert.notEqual(next.targets[0], first.targets[0]);
  assert.deepEqual(memory[first.targets[0]].patchedTechniques, [
    first.technique,
  ]);
  assert.equal(memory[first.targets[0]].attempts, 1);
});

test("public A&D plans stay bounded, reproducible, broad, and non-uniform", () => {
  const profiles = buildPlayerProfiles(100, SEED);
  const publicOpponents = profiles.map((profile, index) => ({
    index,
    rank: index + 1,
    settledTotal: 100 - index,
    projectedTotal: 110 - index,
    defenseRate: ((index * 17) % 100) / 100,
    slaRate: 0.75 + ((index * 7) % 25) / 100,
  }));
  const victims = new Set();
  const counts = new Map();
  for (const profile of profiles) {
    for (let round = 1; round <= 120; round++) {
      const available = publicOpponents.filter(
        ({ index }) => index !== profile.index,
      );
      const first = attackPlan(profile, available, {}, round, 1, round / 120);
      const second = attackPlan(profile, available, {}, round, 1, round / 120);
      assert.deepEqual(first, second);
      assert.ok(first.targets.length <= 3);
      assert.equal(new Set(first.targets).size, first.targets.length);
      assert.ok(!first.targets.includes(profile.index));
      for (const victim of first.targets) victims.add(victim);
      counts.set(
        profile.index,
        (counts.get(profile.index) || 0) + first.targets.length,
      );
    }
  }
  assert.ok(victims.size >= 95);
  assert.ok(new Set(counts.values()).size >= 15);
});

test("Jeopardy decisions emerge from live history and credits without a solve plan", () => {
  const profiles = buildPlayerProfiles(100, SEED);
  const catalog = [
    { challengeId: 101, kind: "static", category: "Web", difficulty: 3 },
    { challengeId: 102, kind: "static", category: "Pwn", difficulty: 7 },
    { challengeId: 103, kind: "static", category: "Crypto", difficulty: 5 },
    { challengeId: 104, kind: "static", category: "Reverse", difficulty: 8 },
    { challengeId: 105, kind: "static", category: "Misc", difficulty: 2 },
    { challengeId: 106, kind: "static", category: "Web", difficulty: 9 },
    { challengeId: 107, kind: "attachment", category: "Crypto", difficulty: 6 },
    { challengeId: 108, kind: "container", category: "Pwn", difficulty: 8 },
  ];
  assert.deepEqual(
    buildJeopardyCatalog(catalog),
    catalog.map((challenge, catalogIndex) => ({
      ...challenge,
      catalogIndex,
    })),
  );
  assert.equal(playerModel.buildJeopardyPlans, undefined);

  const staticCatalog = [catalog[0]];
  const profile = profiles.find((candidate) => {
    const round = firstJeopardyFocusRound(candidate);
    return (
      round !== undefined &&
      jeopardyIntent(
        candidate,
        staticCatalog,
        {},
        {
          round,
          progress: 0.2,
          availableCredits: 5,
        },
      ).action === "view"
    );
  });
  assert.ok(profile);
  const focusRound = firstJeopardyFocusRound(profile);
  assert.ok(focusRound);
  const first = jeopardyIntent(
    profile,
    staticCatalog,
    {},
    {
      round: focusRound,
      progress: 0.2,
      availableCredits: 5,
    },
  );
  assert.equal(first.action, "view");
  const viewed = {
    [first.challengeId]: {
      viewed: true,
      attempts: 0,
      lastAttemptRound: 0,
    },
  };
  assert.equal(
    jeopardyIntent(profile, staticCatalog, viewed, {
      round: focusRound,
      progress: 0.2,
      availableCredits: 0,
    }).action,
    "wait",
  );
  const attempt = jeopardyIntent(profile, staticCatalog, viewed, {
    round: focusRound,
    progress: 0.2,
    availableCredits: 5,
  });
  assert.ok(
    ["research", "submitWrong", "submitCorrect"].includes(attempt.action),
  );
  assert.deepEqual(
    attempt,
    jeopardyIntent(profile, staticCatalog, viewed, {
      round: focusRound,
      progress: 0.2,
      availableCredits: 5,
    }),
  );
  assert.equal(
    jeopardyIntent(
      profile,
      staticCatalog,
      {
        [first.challengeId]: {
          viewed: true,
          attempts: 1,
          lastAttemptRound: focusRound,
        },
      },
      { round: focusRound, progress: 0.2, availableCredits: 5 },
    ).action,
    "wait",
  );
  assert.equal(
    jeopardyIntent(
      profile,
      staticCatalog,
      {},
      {
        round: focusRound,
        progress: 0.2,
        availableCredits: 5,
        actedThisRound: true,
      },
    ).reason,
    "already-acted-this-round",
  );
});

test("Jeopardy skill changes discovery while costly journeys remain player choices", () => {
  const staticChallenge = [
    { challengeId: 201, kind: "static", category: "Pwn", difficulty: 6 },
  ];
  let strongDiscoveries = 0;
  let weakDiscoveries = 0;
  for (let index = 0; index < 200; index++) {
    for (const [skill, field] of [
      [0.9, "strong"],
      [0.2, "weak"],
    ]) {
      const profile = {
        seed: `${field}:${index}`,
        jeopardy: skill,
        risk: 0.5,
        exploration: 0.5,
        persistence: 0.5,
      };
      const focusRound = firstJeopardyFocusRound(profile);
      assert.ok(focusRound);
      const intent = jeopardyIntent(
        profile,
        staticChallenge,
        { 201: { viewed: true, attempts: 0, lastAttemptRound: 0 } },
        { round: focusRound, progress: 0.5, availableCredits: 5 },
      );
      if (intent.action === "submitCorrect" && field === "strong")
        strongDiscoveries++;
      if (intent.action === "submitCorrect" && field === "weak")
        weakDiscoveries++;
    }
  }
  assert.ok(strongDiscoveries > weakDiscoveries * 2);

  const profiles = buildPlayerProfiles(100, SEED);
  const attachment = [
    { challengeId: 202, kind: "attachment", category: "Crypto", difficulty: 6 },
  ];
  const container = [
    { challengeId: 203, kind: "container", category: "Web", difficulty: 5 },
  ];
  const attachmentInterest = profiles.filter((profile) => {
    const focusRound = firstJeopardyFocusRound(profile);
    return (
      jeopardyIntent(
        profile,
        attachment,
        {},
        {
          round: focusRound,
          progress: 0,
          availableCredits: 5,
        },
      ).action === "view"
    );
  }).length;
  const containerInterest = profiles.filter((profile) => {
    const focusRound = firstJeopardyFocusRound(profile);
    return (
      jeopardyIntent(
        profile,
        container,
        {},
        {
          round: focusRound,
          progress: 0,
          availableCredits: 5,
        },
      ).action === "view"
    );
  }).length;
  assert.ok(attachmentInterest >= 20 && attachmentInterest <= 90);
  assert.ok(containerInterest >= 5 && containerInterest <= 50);
});

test("Jeopardy focus stays non-uniform and spans the complete hour", () => {
  const profiles = buildPlayerProfiles(100, SEED);
  const schedules = profiles.map((profile) =>
    Array.from({ length: 120 }, (_, index) => index + 1).filter((round) =>
      isJeopardyFocusRound(profile, round),
    ),
  );
  assert.ok(
    schedules.every((rounds) => rounds.length >= 6 && rounds.length <= 48),
  );
  assert.ok(new Set(schedules.map((rounds) => rounds.join(","))).size >= 95);
  for (let bucket = 0; bucket < 6; bucket++) {
    const from = bucket * 20 + 1;
    const through = from + 19;
    assert.ok(
      schedules.filter((rounds) =>
        rounds.some((round) => round >= from && round <= through),
      ).length >= 80,
    );
  }
});

test("platform reads retry only transient statuses", () => {
  for (const status of [0, 429, 500, 503, 599]) {
    assert.equal(isRetryablePlatformStatus(status), true);
  }
  for (const status of [200, 201, 400, 401, 403, 404, 600, NaN, -1]) {
    assert.equal(isRetryablePlatformStatus(status), false);
  }
});

test("platform request retries keep container mutations 429-only", () => {
  for (const method of ["GET", "get"]) {
    for (const status of [0, 429, 500, 503, 599]) {
      assert.equal(isRetryablePlatformRequest(method, status), true);
    }
  }
  for (const method of ["POST", "DELETE", "post", "delete"]) {
    assert.equal(isRetryablePlatformRequest(method, 429), true);
    for (const status of [0, 400, 401, 403, 404, 409, 500, 503, 599]) {
      assert.equal(isRetryablePlatformRequest(method, status), false);
    }
  }
  for (const method of ["PUT", "PATCH", "", null]) {
    assert.equal(isRetryablePlatformRequest(method, 429), false);
  }
});

test("platform first-attempt failures have one exact evidence class", () => {
  assert.equal(classifyPlatformFirstAttemptFailure(0), "timeout");
  assert.equal(classifyPlatformFirstAttemptFailure(429), "rateLimit");
  for (const status of [500, 503, 599]) {
    assert.equal(classifyPlatformFirstAttemptFailure(status), "serverError");
  }
  for (const status of [200, 400, 499, 600, -1, NaN]) {
    assert.equal(classifyPlatformFirstAttemptFailure(status), null);
  }
});

test("platform retries never sleep less than 200 ms", () => {
  assert.equal(boundedPlatformRetryDelay(0, 0.35), 0.2);
  assert.equal(boundedPlatformRetryDelay("0.1", 0.35), 0.2);
  assert.equal(boundedPlatformRetryDelay(1.25, 0.35), 1.25);
  assert.equal(boundedPlatformRetryDelay(61, 0.35, 60), 0.35);
  assert.equal(boundedPlatformRetryDelay(undefined, 0.35), 0.35);
  assert.throws(() => boundedPlatformRetryDelay(0, 0.1), /at least 200 ms/);
  assert.throws(
    () => boundedPlatformRetryDelay(0, 0.2, 0.1),
    /at least 200 ms/,
  );
});

test("KotH capabilities bind to the exact authoritative round", () => {
  const token = { status: "ready", token: "window-token", round: 42 };
  assert.equal(kothCapabilityMatchesState(token, { round: 42 }), true);
  assert.equal(kothCapabilityMatchesState(token, { round: 43 }), false);
  assert.equal(
    kothCapabilityMatchesState({ ...token, status: "pending" }, { round: 42 }),
    false,
  );
  assert.equal(
    kothCapabilityMatchesState({ ...token, token: "" }, { round: 42 }),
    false,
  );
  assert.equal(kothCapabilityMatchesState(null, { round: 42 }), false);
});

test("public A&D targets are the sole source of opponent network routes", () => {
  const roster = [101, 102, 103];
  const model = {
    currentRound: 42,
    challenges: [
      {
        challengeId: 6,
        hill: null,
        teams: [
          { participationId: 103, ip: "10.60.0.3", port: 3003 },
          { participationId: 102, ip: "team-two.internal", port: 3002 },
        ],
      },
    ],
  };
  assert.deepEqual(publicAdNetworkTargets(model, 6, roster, 101), [
    { index: 1, participationId: 102, host: "team-two.internal", port: 3002 },
    { index: 2, participationId: 103, host: "10.60.0.3", port: 3003 },
  ]);
  assert.equal(
    Object.isFrozen(publicAdNetworkTargets(model, 6, roster, 101)),
    true,
  );
  assert.deepEqual(publicAdNetworkTargets(model, 7, roster, 101), []);
  assert.deepEqual(
    publicAdNetworkTargets(
      {
        ...model,
        challenges: [
          {
            ...model.challenges[0],
            teams: [
              {
                participationId: 102,
                ip: "http://organizer.invalid",
                port: 3002,
              },
            ],
          },
        ],
      },
      6,
      roster,
      101,
    ),
    [],
  );
  assert.throws(
    () => publicAdNetworkTargets(model, 6, [101, 101, 103], 101),
    /participation roster/,
  );
});

test("public A&D target correlation fails closed on self, unknown, or duplicate identities", () => {
  const roster = [101, 102, 103];
  const response = (teams) => ({
    challenges: [{ challengeId: 6, hill: null, teams }],
  });
  for (const teams of [
    [{ participationId: 101, ip: "10.60.0.1", port: 3001 }],
    [{ participationId: 999, ip: "10.60.0.9", port: 3009 }],
    [
      { participationId: 102, ip: "10.60.0.2", port: 3002 },
      { participationId: 102, ip: "10.60.0.8", port: 3008 },
    ],
  ]) {
    assert.deepEqual(
      publicAdNetworkTargets(response(teams), 6, roster, 101),
      [],
    );
  }
});

test("KotH network targets bind to the exact public crown cycle", () => {
  const targets = {
    currentRound: 42,
    challenges: [
      {
        challengeId: 7,
        hill: {
          ip: "10.40.0.7",
          port: 8080,
          cycleNumber: 9,
        },
      },
    ],
  };
  const state = { round: 42, cycleNumber: 9 };
  assert.equal(kothTargetMatchesState(targets, state, 7), true);
  assert.equal(
    kothTargetMatchesState(targets, { ...state, cycleNumber: 10 }, 7),
    false,
  );
  assert.equal(
    kothTargetMatchesState({ ...targets, currentRound: 43 }, state, 7),
    false,
  );
  assert.equal(
    kothTargetMatchesState(targets, { ...state, cycleNumber: 0 }, 7),
    false,
  );
  assert.equal(
    kothTargetMatchesState(targets, { ...state, containerId: "raw-id" }, 7),
    false,
  );
  assert.equal(kothTargetMatchesState(targets, state, 8), false);
});

test("KotH intent creates independent multi-team contention without selected winners", () => {
  const profiles = buildPlayerProfiles(100, SEED);
  const writers = new Set();
  let multiTeamWindows = 0;
  for (let cycleNumber = 1; cycleNumber <= 30; cycleNumber++) {
    const attempts = profiles.filter(
      (profile) =>
        kothIntent(
          profile,
          {
            cycleNumber,
            cycleTick: 1,
            cycleTicks: 3,
            resetPhase: "Active",
            isScorable: true,
            eligibleNow: true,
            provisionalClaimantParticipationId: null,
            holderParticipationId: null,
          },
          {
            competitionSeed: SEED,
            active: true,
            attempted: false,
            availableCredits: 2,
            observationsInTick: 3,
            ownParticipationId: profile.index + 1000,
            scoreboardRank: profile.index + 1,
            teamCount: profiles.length,
          },
        ).attempt,
    );
    if (attempts.length > 1) multiTeamWindows++;
    for (const profile of attempts) writers.add(profile.index);
  }
  assert.ok(multiTeamWindows >= 25);
  assert.ok(writers.size >= 80);
  assert.equal("rankKothContenders" in playerModel, false);
  assert.equal("kothCyclePlan" in playerModel, false);
  assert.equal("freezeKothCyclePlan" in playerModel, false);
  assert.equal("isStableKothCycle" in playerModel, false);
  assert.equal("isConfirmationControlCycle" in playerModel, false);
});

test("observed KotH claims produce independent contests and organic hold windows", () => {
  const profiles = buildPlayerProfiles(100, SEED);
  const attemptsByCycle = (cycleNumber) =>
    profiles
      .filter(
        (profile) =>
          kothIntent(
            profile,
            {
              cycleNumber,
              cycleTick: 2,
              cycleTicks: 3,
              resetPhase: "Active",
              isScorable: true,
              eligibleNow: true,
              provisionalClaimantParticipationId: 99999,
              holderParticipationId: null,
            },
            {
              competitionSeed: SEED,
              active: true,
              attempted: false,
              availableCredits: 2,
              observationsInTick: 3,
              ownParticipationId: profile.index + 1000,
              scoreboardRank: profile.index + 1,
              teamCount: profiles.length,
            },
          ).attempt,
      )
      .map((profile) => profile.index);
  const firstPass = Array.from({ length: 80 }, (_, index) =>
    attemptsByCycle(index + 1),
  );
  const secondPass = Array.from({ length: 80 }, (_, index) =>
    attemptsByCycle(index + 1),
  );
  const uncontestedCycles = firstPass
    .map((attempts, index) => (attempts.length === 0 ? index + 1 : null))
    .filter((cycle) => cycle !== null);
  const contestedCycles = firstPass.filter((attempts) => attempts.length > 0);

  assert.deepEqual(
    secondPass,
    firstPass,
    "seeded decisions must be reproducible",
  );
  assert.ok(
    uncontestedCycles.length >= 15,
    "claims need organic confirmation opportunities",
  );
  assert.ok(
    contestedCycles.length >= 30,
    "an observed claim must not trigger a global truce",
  );
  assert.equal(
    new Set(uncontestedCycles.map((cycle) => cycle % 4)).size,
    4,
    "hold windows must not follow one coordinated four-cycle cadence",
  );
  assert.ok(
    new Set(contestedCycles.flat()).size >= 40,
    "late challenges should emerge across many independent teams",
  );
});

test("KotH expertise creates a durable timing and late-contest advantage", () => {
  for (const seed of [
    "rsctf-competitive-v2",
    SEED,
    "koth-specialty-a",
    "koth-specialty-b",
  ]) {
    const profiles = buildPlayerProfiles(100, seed);
    const specialists = { attempts: 0, observations: [] };
    const field = { attempts: 0, observations: [] };
    for (let cycleNumber = 1; cycleNumber <= 80; cycleNumber++) {
      for (const profile of profiles) {
        const intent = kothIntent(
          profile,
          {
            cycleNumber,
            cycleTick: 2,
            cycleTicks: 3,
            resetPhase: "Active",
            isScorable: true,
            eligibleNow: true,
            provisionalClaimantParticipationId: 99999,
            holderParticipationId: null,
          },
          {
            competitionSeed: seed,
            active: true,
            attempted: false,
            availableCredits: 6,
            observationsInTick: 3,
            ownParticipationId: profile.index + 1000,
            scoreboardRank: profile.index + 1,
            teamCount: profiles.length,
          },
        );
        const cohort = profile.specialty === "koth" ? specialists : field;
        cohort.attempts += Number(intent.attempt);
        cohort.observations.push(intent.reactionObservation);
      }
    }
    const specialistRate = specialists.attempts / (20 * 80);
    const fieldRate = field.attempts / (80 * 80);
    assert.ok(
      specialistRate >= fieldRate * 2,
      `${seed} KotH specialists lack a material late-contest advantage`,
    );
    assert.ok(
      mean(specialists.observations) >= mean(field.observations) + 0.4,
      `${seed} KotH specialists do not time writes later in the scoring tick`,
    );
  }
});

test("last-write-wins cycles give KotH specialists realized control lift", () => {
  for (const seed of [
    "rsctf-competitive-v2",
    SEED,
    "koth-specialty-a",
    "koth-specialty-b",
  ]) {
    const profiles = buildPlayerProfiles(100, seed);
    const first = simulateKothLastWriteControl(profiles, seed, 240);
    assert.deepEqual(
      simulateKothLastWriteControl(profiles, seed, 240),
      first,
      `${seed} last-write simulation is not reproducible`,
    );

    const specialistRates = profiles
      .filter((profile) => profile.specialty === "koth")
      .map((profile) => first.controlRates[profile.index]);
    const specialistMean = mean(specialistRates);
    const fieldMean = mean(first.controlRates);
    const totalControlled = first.controlledTicks.reduce(
      (total, value) => total + value,
      0,
    );
    const controllingTeams = first.controlledTicks.filter(
      (value) => value > 0,
    ).length;

    assert.equal(totalControlled, 240 * 3);
    assert.ok(
      specialistMean / fieldMean >= 1.25,
      `${seed} KotH specialists lack realized control-rate lift`,
    );
    assert.ok(
      controllingTeams >= 70,
      `${seed} control is concentrated in a preselected winner set`,
    );
    assert.ok(
      Math.max(...first.controlledTicks) / totalControlled < 0.08,
      `${seed} one team dominates the independent last-write process`,
    );
  }
});

test("confirmed patch windows organically include defended rival takeovers", () => {
  const profiles = buildPlayerProfiles(100, SEED);
  let patchableWindows = 0;
  let contestedPatchWindows = 0;
  let blockedCandidates = 0;
  let bypassCandidates = 0;

  for (let cycleNumber = 1; cycleNumber <= 30; cycleNumber++) {
    const controller = profiles[(cycleNumber * 37) % profiles.length];
    const controllerParticipationId = controller.index + 1000;
    const state = {
      cycleNumber,
      cycleTick: 2,
      cycleTicks: 3,
      resetPhase: "Active",
      isScorable: true,
      eligibleNow: true,
      holderParticipationId: controllerParticipationId,
      provisionalClaimantParticipationId: null,
    };
    const patch = kothPatchIntent(controller, state, {
      active: true,
      ownParticipationId: controllerParticipationId,
      availableCredits: 6,
      patchedCycleNumber: 0,
    });
    if (!patch.attempt) continue;
    patchableWindows++;

    const rivals = profiles
      .filter((profile) => profile.index !== controller.index)
      .map((profile) =>
        kothIntent(profile, state, {
          competitionSeed: SEED,
          active: true,
          attempted: false,
          availableCredits: 6,
          observationsInTick: 3,
          ownParticipationId: profile.index + 1000,
          scoreboardRank: profile.index + 1,
          teamCount: profiles.length,
        }),
      )
      .filter((intent) => intent.attempt);
    if (rivals.length > 0) contestedPatchWindows++;
    blockedCandidates += rivals.filter(
      (intent) => intent.technique <= patch.level,
    ).length;
    bypassCandidates += rivals.filter(
      (intent) => intent.technique > patch.level,
    ).length;
  }

  assert.ok(
    patchableWindows >= 20,
    "many controllers should independently choose to patch",
  );
  assert.ok(
    contestedPatchWindows >= 12,
    "patched controllers need genuine rival pressure",
  );
  assert.ok(
    contestedPatchWindows < patchableWindows,
    "some patched controllers still need an organic healthy-hold opportunity",
  );
  assert.ok(
    blockedCandidates > 0,
    "rivals should exercise a patch's blocking behavior",
  );
  assert.ok(
    bypassCandidates > 0,
    "strong rivals should exercise patch bypass behavior",
  );
});

test("KotH intent honors inactivity, cooldown, and confirmation boundaries", () => {
  const profile = {
    ...buildPlayerProfiles(10, "koth-rules")[0],
    koth: 1,
    risk: 1,
    persistence: 1,
  };
  const baseState = {
    cycleNumber: 5,
    cycleTick: 1,
    cycleTicks: 3,
    resetPhase: "Active",
    isScorable: true,
    eligibleNow: true,
    provisionalClaimantParticipationId: null,
    holderParticipationId: null,
  };
  const baseContext = {
    competitionSeed: "koth-rules",
    active: true,
    attempted: false,
    availableCredits: 2,
    observationsInTick: 3,
    ownParticipationId: 100,
    scoreboardRank: 10,
    teamCount: 10,
  };
  assert.equal(
    kothIntent(profile, baseState, { ...baseContext, active: false }).reason,
    "inactive",
  );
  assert.equal(
    kothIntent(profile, { ...baseState, eligibleNow: false }, baseContext)
      .reason,
    "ineligible",
  );
  assert.equal(
    kothIntent(profile, { ...baseState, cycleTick: 3 }, baseContext).reason,
    "confirmation-window",
  );
  assert.equal(
    kothIntent(
      profile,
      {
        ...baseState,
        provisionalClaimantParticipationId: baseContext.ownParticipationId,
      },
      baseContext,
    ).reason,
    "already-controlling",
  );
  const lateOpening = kothIntent(
    profile,
    {
      ...baseState,
      cycleTick: 2,
      provisionalClaimantParticipationId: null,
    },
    baseContext,
  );
  assert.notEqual(lateOpening.reason, "control-cycle-claim-observed");
  assert.equal("controlCycle" in lateOpening, false);
});

test("current KotH controllers spend finite credits on bounded container-local patches", () => {
  const profile = {
    ...buildPlayerProfiles(10, "koth-patch-rules")[0],
    koth: 0.8,
    persistence: 0.8,
    risk: 0.4,
  };
  const state = {
    round: 8,
    cycleNumber: 4,
    cycleTick: 2,
    cycleTicks: 3,
    resetPhase: "Active",
    isScorable: true,
    eligibleNow: true,
    holderParticipationId: 101,
    provisionalClaimantParticipationId: null,
  };
  const context = {
    active: true,
    ownParticipationId: 101,
    availableCredits: 2,
    patchedCycleNumber: 0,
  };
  const intent = kothPatchIntent(profile, state, context);
  assert.deepEqual(intent, kothPatchIntent(profile, state, context));
  assert.equal(intent.attempt, true);
  assert.ok([1, 2].includes(intent.level));
  assert.ok(["healthy", "mumble", "offline"].includes(intent.incident));
  assert.equal(
    kothPatchIntent(profile, state, { ...context, availableCredits: 1 }).reason,
    "no-action-credit",
  );
  assert.equal(
    kothPatchIntent(profile, { ...state, holderParticipationId: 202 }, context)
      .reason,
    "not-current-controller",
  );
  assert.equal(
    kothPatchIntent(profile, state, { ...context, patchedCycleNumber: 4 })
      .reason,
    "already-patched-cycle",
  );
});

test("checker-observed KotH controllers can patch before the final tick", () => {
  const profile = {
    ...buildPlayerProfiles(10, "koth-patch-rules")[0],
    koth: 0.8,
    persistence: 0.8,
    risk: 0.4,
  };
  const state = {
    round: 8,
    cycleNumber: 4,
    cycleTick: 2,
    cycleTicks: 3,
    resetPhase: "Active",
    isScorable: true,
    eligibleNow: true,
    holderParticipationId: 202,
    provisionalClaimantParticipationId: 101,
  };
  const context = {
    active: true,
    ownParticipationId: 101,
    availableCredits: 2,
    patchedCycleNumber: 0,
  };

  assert.equal(kothControllerParticipationId(state), 101);
  assert.equal(kothPatchIntent(profile, state, context).attempt, true);
  const confirmed = {
    ...state,
    holderParticipationId: 101,
    provisionalClaimantParticipationId: null,
  };
  assert.equal(kothPatchIntent(profile, confirmed, context).attempt, true);
  assert.equal(
    kothPatchIntent(profile, state, { ...context, ownParticipationId: 202 })
      .reason,
    "not-current-controller",
  );
  assert.equal(
    kothPatchIntent(
      profile,
      { ...confirmed, cycleTick: state.cycleTicks },
      context,
    ).reason,
    "no-hold-window",
  );
  assert.throws(
    () =>
      kothControllerParticipationId({
        holderParticipationId: null,
        provisionalClaimantParticipationId: 0,
      }),
    /positive integer/,
  );
});

test("KotH reset-boundary states are ineligible patch evidence, not runtime errors", () => {
  const profile = {
    ...buildPlayerProfiles(10, "koth-patch-boundary")[0],
    koth: 0.8,
    persistence: 0.8,
    risk: 0.4,
  };
  const context = {
    active: true,
    ownParticipationId: 101,
    availableCredits: 2,
    patchedCycleNumber: 0,
  };
  const boundary = {
    round: 9,
    cycleNumber: 0,
    cycleTick: 0,
    cycleTicks: 3,
    resetPhase: "Readiness",
    isScorable: false,
    eligibleNow: false,
    holderParticipationId: null,
    provisionalClaimantParticipationId: null,
  };

  assert.equal(kothPatchIntent(profile, boundary, context).reason, "ineligible");
  assert.equal(
    kothPatchIntent(
      profile,
      { ...boundary, cycleNumber: 5, cycleTick: 0, resetPhase: "Active" },
      context,
    ).reason,
    "ineligible",
  );
});

test("KotH takeover techniques are bounded and can advance past an observed patch", () => {
  const profile = buildPlayerProfiles(10, "koth-techniques")[0];
  const state = { cycleNumber: 7, cycleTick: 2 };
  const initial = kothTakeoverTechnique(profile, state);
  assert.ok(initial >= 1 && initial <= 3);
  assert.equal(kothTakeoverTechnique(profile, state, 3), 3);
  assert.throws(() => kothTakeoverTechnique(profile, state, 4), /technique/);
});

test("KotH patch incidents survive their scoring round before repair", () => {
  assert.equal(kothPatchRepairReady(12, 12), false);
  assert.equal(kothPatchRepairReady(12, 13), true);
  assert.throws(() => kothPatchRepairReady(-1, 13), /rounds/);
});

test("KotH status evidence distinguishes replacement identity from pristine state", () => {
  const pristine = parseKothServiceStatus(
    "instance=0011223344556677;patch=0;state=healthy\n",
    "0011223344556677",
  );
  assert.deepEqual(pristine, {
    instance: "0011223344556677",
    patchLevel: 0,
    state: "healthy",
  });
  assert.equal(Object.isFrozen(pristine), true);
  assert.equal(isReplacementKothInstance("8899aabbccddeeff", pristine), true);
  assert.equal(isReplacementKothInstance(pristine.instance, pristine), false);
  assert.equal(isPristineKothReplacement("8899aabbccddeeff", pristine), true);
  assert.equal(isPristineKothReplacement(pristine.instance, pristine), false);

  const retainedPatch = parseKothServiceStatus(
    "instance=0011223344556677;patch=2;state=healthy",
    "0011223344556677",
  );
  assert.equal(
    isPristineKothReplacement("8899aabbccddeeff", retainedPatch),
    false,
  );
  assert.equal(
    parseKothServiceStatus(
      "instance=0011223344556677;patch=0;state=healthy",
      "different-instance",
    ),
    null,
  );
  assert.equal(
    parseKothServiceStatus(
      "instance=0011223344556677;patch=3;state=healthy",
      "0011223344556677",
    ),
    null,
  );

  const patch = {
    instance: "0011223344556677",
    level: 2,
    controlInterrupted: false,
  };
  assert.equal(kothHealthyHoldStatusMatches(patch, retainedPatch), true);
  assert.equal(
    kothHealthyHoldStatusMatches(
      { ...patch, controlInterrupted: true },
      retainedPatch,
    ),
    false,
  );
  assert.equal(kothHealthyHoldStatusMatches(patch, pristine), false);
  assert.equal(
    kothHealthyHoldStatusMatches(patch, { ...retainedPatch, state: "mumble" }),
    false,
  );
});

test("KotH patch-loss evidence accepts an already-repatched replacement", () => {
  const repatchedReplacement = parseKothServiceStatus(
    "instance=0011223344556677;patch=2;state=healthy",
    "0011223344556677",
  );
  assert.notEqual(repatchedReplacement, null);
  assert.equal(
    isReplacementKothInstance("8899aabbccddeeff", repatchedReplacement),
    true,
  );
  assert.equal(
    isReplacementKothInstance(repatchedReplacement.instance, repatchedReplacement),
    false,
  );
  assert.equal(
    isPristineKothReplacement("8899aabbccddeeff", repatchedReplacement),
    false,
  );
});

test("KotH capture outcome precedence remains deterministic", () => {
  const base = {
    successful: false,
    resetRace: false,
    captureWindowClosed: false,
    stateAvailable: true,
    eligibleNow: true,
  };
  assert.equal(
    classifyKothCaptureOutcome({ ...base, successful: true }),
    "success",
  );
  assert.equal(
    classifyKothCaptureOutcome({ ...base, resetRace: true }),
    "resetRace",
  );
  assert.equal(
    classifyKothCaptureOutcome({ ...base, captureWindowClosed: true }),
    "windowClosed",
  );
  assert.equal(
    classifyKothCaptureOutcome({ ...base, stateAvailable: false }),
    "stateUnavailable",
  );
  assert.equal(
    classifyKothCaptureOutcome({ ...base, eligibleNow: false }),
    "ineligibleTransition",
  );
  assert.equal(classifyKothCaptureOutcome(base), "pending");
});

test("pending KotH captures resolve only on authoritative transitions", () => {
  const pending = { cycleNumber: 7, cycleTick: 2, burstExhausted: true };
  const active = {
    cycleNumber: 7,
    cycleTick: 2,
    resetPhase: "Active",
    isScorable: true,
    eligibleNow: true,
  };
  assert.equal(classifyKothPendingTransition(pending, active), "pending");
  assert.equal(
    classifyKothPendingTransition(pending, null),
    "stateUnavailable",
  );
  assert.equal(
    classifyKothPendingTransition(pending, { ...active, cycleTick: 3 }),
    "windowClosed",
  );
  assert.equal(
    classifyKothPendingTransition(pending, { ...active, eligibleNow: false }),
    "ineligibleTransition",
  );
  assert.equal(
    classifyKothPendingTransition(pending, {
      ...active,
      resetPhase: "Readiness",
    }),
    "resetRace",
  );
});

test("terminal and balance helpers reject malformed KotH accounting", () => {
  const pending = { cycleNumber: 7, cycleTick: 2, burstExhausted: true };
  assert.equal(isKothTerminalWindow(pending, "windowClosed"), true);
  assert.equal(
    isKothTerminalWindow({ ...pending, burstExhausted: false }, "windowClosed"),
    false,
  );
  assert.deepEqual(
    kothCapturePendingBalance({
      started: 5,
      recovered: 2,
      resetRaces: 1,
      windowClosed: 1,
      ineligibleTransitions: 0,
      invariantFailures: 0,
      terminalWindows: 1,
    }),
    { started: 5, resolved: 4, unresolved: 1, valid: false },
  );
  assert.equal(
    kothCapturePendingBalance({
      started: 4,
      recovered: 2,
      resetRaces: 0,
      windowClosed: 0,
      ineligibleTransitions: 0,
      invariantFailures: 0,
      terminalWindows: 0,
    }).valid,
    false,
  );
  assert.deepEqual(
    kothCaptureStatusBalance({
      attemptFailures: 8,
      networkErrors: 5,
      http4xx: 1,
      http5xx: 1,
      otherStatusFailures: 1,
    }),
    { attemptFailures: 8, classified: 8, valid: true },
  );
});
