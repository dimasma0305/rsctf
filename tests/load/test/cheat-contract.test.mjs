import assert from "node:assert/strict";
import test from "node:test";

import {
  DETECTOR_CAPABILITIES,
  SUSPICION_RULES,
  assertCanonicalRuleProfile,
  canonicalScenarioBreakdowns,
  computeExpectedBreakdown,
  effectiveRuleWeights,
  validateDetectorCapabilities,
} from "../cheat-contract.js";

const event = (kind, id, overrides = {}) => ({
  id,
  kind,
  evidenceKey: `incident:${id}`,
  scoreDelta: SUSPICION_RULES[kind].defaultWeight,
  createdAtMs: 1_700_000_000_000 + id,
  ...overrides,
});

test("pins every persisted suspicion kind to its public rule code", () => {
  assert.equal(SUSPICION_RULES.length, 38);
  assert.deepEqual(
    SUSPICION_RULES.map(({ kind, code }) => [kind, code]),
    [
      [0, "StolenFlag"],
      [1, "SharedIP"],
      [2, "SharedFingerprint"],
      [3, "FingerprintChurn"],
      [4, "IpChurn"],
      [5, "UnknownIP"],
      [6, "CrossTeamIP"],
      [7, "TokenAbuse"],
      [8, "Hoarding"],
      [9, "Burst"],
      [10, "NoDownload"],
      [11, "NoContainer"],
      [12, "FastSolve-Open"],
      [13, "FastSolve-Download"],
      [14, "FastSolve-Container"],
      [15, "SequenceSimilarity"],
      [16, "CollusionGroup"],
      [17, "ZeroWrongAttempts"],
      [18, "WrongFlagLeakage"],
      [19, "SolutionRelay"],
      [20, "AdaptiveFastSolve"],
      [21, "DirectedSolving"],
      [22, "ClusteredRegistration"],
      [23, "SubnetOverlap"],
      [24, "HighWrongRate"],
      [25, "AutomatedPattern"],
      [26, "SessionConcurrency"],
      [27, "FirstBloodAnomaly"],
      [28, "HoneypotHit"],
      [29, "HoneypotProtocolHit"],
      [30, "HoneypotCanaryFlag"],
      [31, "HoneypotChain"],
      [32, "FlagEgress"],
      [33, "CrossTeamContainerAccess"],
      [34, "DelayedSolveSubmission"],
      [35, "InstantSubmitAfterAccess"],
      [36, "SubmitterNeverAccessedContainer"],
      [37, "AccessIpMismatchAtSubmission"],
    ],
  );
});

test("canonical profile requires every default rule and weight", () => {
  const rows = SUSPICION_RULES.map((rule) => ({
    ruleCode: rule.code,
    weight: rule.defaultWeight,
  }));
  const weights = assertCanonicalRuleProfile(rows);
  assert.equal(weights.get("StolenFlag"), 100);
  assert.equal(weights.get("HoneypotChain"), 150);

  assert.throws(
    () => assertCanonicalRuleProfile(rows.slice(1)),
    /exactly 38 rules/,
  );
  assert.throws(
    () => assertCanonicalRuleProfile(
      rows.map((row) => row.ruleCode === "HighWrongRate" ? { ...row, weight: 41 } : row),
    ),
    /HighWrongRate=41/,
  );
});

test("global honeypot kinds are frozen as non-corroborating raw telemetry", () => {
  for (const kind of [28, 29, 31]) {
    const rule = SUSPICION_RULES[kind];
    assert.equal(rule.tier, "context");
    assert.equal(rule.corroborationUnit, 0);
    assert.deepEqual(
      DETECTOR_CAPABILITIES.find(({ code }) => code === rule.code),
      { code: rule.code, status: "telemetryOnly", scope: "platform" },
    );
    const breakdown = computeExpectedBreakdown([event(kind, 100 + kind)]);
    assert.equal(breakdown.total, 0);
    assert.equal(breakdown.corroboration, 0);
    assert.equal(breakdown.events[0].counted, false);
  }
});

test("configured weights override defaults without using report deltas", () => {
  const weights = effectiveRuleWeights([
    { ruleCode: "HighWrongRate", weight: 12 },
    { ruleCode: "AutomatedPattern", weight: 13 },
  ]);
  const breakdown = computeExpectedBreakdown(
    [
      event(24, 1, { scoreDelta: null }),
      event(25, 2, { scoreDelta: null }),
    ],
    weights,
  );
  assert.deepEqual(
    {
      strong: breakdown.strong,
      total: breakdown.total,
      band: breakdown.band,
      deltas: breakdown.events.map((row) => row.scoreDelta),
    },
    { strong: 25, total: 25, band: "investigate", deltas: [13, 12] },
  );
});

test("scenario breakdowns pin exact tiers, ceilings, totals, and bands", () => {
  const breakdowns = canonicalScenarioBreakdowns();
  assert.deepEqual(
    {
      hard: breakdowns.stolen.hard,
      strong: breakdowns.stolen.strong,
      behavioral: breakdowns.stolen.behavioral,
      corroboration: breakdowns.stolen.corroboration,
      total: breakdowns.stolen.total,
      band: breakdowns.stolen.band,
    },
    { hard: 100, strong: 0, behavioral: 0, corroboration: 0, total: 100, band: "evidenced" },
  );
  assert.deepEqual(
    {
      strong: breakdowns.brute.strong,
      total: breakdowns.brute.total,
      band: breakdowns.brute.band,
      tiers: breakdowns.brute.events.map((row) => row.tier),
      counted: breakdowns.brute.events.map((row) => row.counted),
      applied: breakdowns.brute.events.map((row) => row.appliedDelta),
    },
    {
      strong: 60,
      total: 60,
      band: "investigate",
      tiers: ["strong", "strong"],
      counted: [true, true],
      applied: [50, 10],
    },
  );
  assert.deepEqual(Object.keys(breakdowns).sort(), ["brute", "stolen"]);
});

test("sub-millisecond ledger order controls tier-ceiling allocation", () => {
  const createdAtMs = 1_700_000_000_000;
  const breakdown = computeExpectedBreakdown([
    event(24, 1, { createdAtMs, createdAtMicros: createdAtMs * 1000 + 900 }),
    event(25, 2, { createdAtMs, createdAtMicros: createdAtMs * 1000 + 100 }),
  ]);
  assert.deepEqual(
    breakdown.events.map((row) => [row.id, row.appliedDelta, row.time]),
    [
      [1, 40, createdAtMs],
      [2, 20, createdAtMs],
    ],
  );
});

test("context stays non-scoring alone and corroborates hard evidence within its cap", () => {
  const contextOnly = computeExpectedBreakdown([
    event(2, 1),
    event(6, 2),
    event(26, 3),
  ]);
  assert.deepEqual(
    {
      total: contextOnly.total,
      corroboration: contextOnly.corroboration,
      band: contextOnly.band,
      counted: contextOnly.events.map((row) => row.counted),
    },
    { total: 0, corroboration: 0, band: "context", counted: [false, false, false] },
  );

  const corroborated = computeExpectedBreakdown([
    event(0, 4),
    event(2, 5),
    event(6, 6),
    event(26, 7),
  ]);
  assert.equal(corroborated.hard, 100);
  assert.equal(corroborated.corroboration, 40);
  assert.equal(corroborated.total, 140);
  assert.equal(corroborated.band, "evidenced");
  assert.deepEqual(
    corroborated.events
      .filter((row) => row.tier === "context")
      .map((row) => [row.type, row.appliedDelta, row.counted]),
    [
      ["SessionConcurrency", 10, true],
      ["CrossTeamIP", 10, true],
      ["SharedFingerprint", 20, true],
    ],
  );

  const telemetryOnly = computeExpectedBreakdown([
    event(0, 8),
    event(10, 9),
    event(11, 10),
    event(32, 11),
    event(36, 12),
    event(21, 13),
    event(12, 14),
    event(13, 15),
    event(14, 16),
    event(22, 17),
    event(28, 18),
    event(29, 19),
    event(31, 20),
  ]);
  assert.equal(telemetryOnly.hard, 100);
  assert.equal(telemetryOnly.corroboration, 0);
  assert.equal(telemetryOnly.total, 100);
  assert.ok(telemetryOnly.events.slice(0, 12).every((row) => row.appliedDelta === 0));
});

test("incident caps count newest distinct evidence and ignore duplicate keys", () => {
  const events = [
    event(16, 1, { evidenceKey: "same" }),
    event(16, 2, { evidenceKey: "same" }),
    event(16, 3, { evidenceKey: "different" }),
  ];
  const breakdown = computeExpectedBreakdown(events);
  assert.equal(breakdown.behavioral, 10);
  assert.equal(breakdown.events.filter((row) => row.counted).length, 1);
  assert.equal(breakdown.events.find((row) => row.id === 3).counted, true);

  const legacy = computeExpectedBreakdown([
    event(0, 4, { evidenceKey: "legacy:4" }),
    event(0, 5, { evidenceKey: "legacy:5" }),
  ]);
  assert.equal(legacy.hard, 100);
  assert.equal(legacy.events.filter((row) => row.counted).length, 1);
  assert.equal(legacy.events.find((row) => row.id === 5).counted, true);
});

test("detector capability metadata covers every stable kind exactly once", () => {
  const rows = DETECTOR_CAPABILITIES.map((capability) => ({
    ...capability,
    detail: "Contract fixture.",
  }));
  assert.equal(validateDetectorCapabilities(rows).size, 38);
  assert.deepEqual(
    rows.filter(({ status }) => status === "background").map(({ code }) => code),
    [
      "SharedIP",
      "SharedFingerprint",
      "FingerprintChurn",
      "IpChurn",
      "UnknownIP",
      "CrossTeamIP",
      "Hoarding",
      "Burst",
      "SequenceSimilarity",
      "ZeroWrongAttempts",
      "SolutionRelay",
      "AdaptiveFastSolve",
      "SubnetOverlap",
      "HighWrongRate",
      "AutomatedPattern",
      "SessionConcurrency",
      "FirstBloodAnomaly",
      "CrossTeamContainerAccess",
      "DelayedSolveSubmission",
      "InstantSubmitAfterAccess",
      "AccessIpMismatchAtSubmission",
    ],
  );
  assert.deepEqual(
    rows.filter(({ status }) => status === "telemetryOnly").map(({ code }) => code),
    [
      "NoDownload",
      "NoContainer",
      "FastSolve-Open",
      "FastSolve-Download",
      "FastSolve-Container",
      "WrongFlagLeakage",
      "DirectedSolving",
      "ClusteredRegistration",
      "HoneypotHit",
      "HoneypotProtocolHit",
      "HoneypotChain",
      "FlagEgress",
      "SubmitterNeverAccessedContainer",
    ],
  );
  assert.throws(
    () => validateDetectorCapabilities(rows.slice(1)),
    /cover exactly 38 stable kinds.*StolenFlag/,
  );
  assert.throws(
    () => validateDetectorCapabilities([...rows, rows[0]]),
    /duplicate detector capability StolenFlag/,
  );
  assert.throws(
    () => validateDetectorCapabilities(rows.map((row, index) =>
      index === 0 ? { ...row, status: "maybe" } : row)),
    /invalid status/,
  );
  assert.throws(
    () => validateDetectorCapabilities(rows.map((row) =>
      row.code === "SubmitterNeverAccessedContainer"
        ? { ...row, status: "background" }
        : row)),
    /expected telemetryOnly\/jeopardyContainers/,
  );
  assert.throws(
    () => validateDetectorCapabilities(rows.map((row) =>
      row.code === "WrongFlagLeakage" ? { ...row, status: "active" } : row)),
    /expected telemetryOnly\/jeopardy/,
  );
});

test("unknown persisted kinds and unknown configured rules fail closed", () => {
  assert.throws(
    () => computeExpectedBreakdown([event(0, 1, { kind: 99 })]),
    /unsupported suspicion event kind 99/,
  );
  assert.throws(
    () => effectiveRuleWeights([{ ruleCode: "FutureRule", weight: 10 }]),
    /missing from the harness contract/,
  );
});
