// Independent anti-cheat wire/persistence contract used by the load harness.
//
// Keep this table explicit. Deriving it from a report response or the Rust source
// would let a discriminator, tier, cap, or default-weight regression change both
// the implementation and its oracle in the same run.

const definitions = [
  ["StolenFlag", 100, "hard", 10],
  ["SharedIP", 10, "context", 3],
  ["SharedFingerprint", 60, "context", 3, 20],
  ["FingerprintChurn", 30, "context", 3],
  ["IpChurn", 20, "context", 3],
  ["UnknownIP", 10, "context", 3],
  ["CrossTeamIP", 20, "context", 3, 10],
  ["TokenAbuse", 80, "hard", 5],
  ["Hoarding", 30, "behavioral", 3],
  ["Burst", 30, "behavioral", 3],
  ["NoDownload", 80, "context", 3, 0],
  ["NoContainer", 80, "context", 3, 0],
  ["FastSolve-Open", 50, "context", 3, 0],
  ["FastSolve-Download", 50, "context", 3, 0],
  ["FastSolve-Container", 50, "context", 3, 0],
  ["SequenceSimilarity", 40, "behavioral", 3],
  ["CollusionGroup", 10, "behavioral", 1],
  ["ZeroWrongAttempts", 50, "behavioral", 3],
  ["WrongFlagLeakage", 80, "hard", 10],
  ["SolutionRelay", 60, "strong", 2],
  ["AdaptiveFastSolve", 60, "behavioral", 3],
  ["DirectedSolving", 30, "context", 1, 0],
  ["ClusteredRegistration", 40, "context", 3, 0],
  ["SubnetOverlap", 5, "context", 3],
  ["HighWrongRate", 40, "strong", 3],
  ["AutomatedPattern", 50, "strong", 3],
  ["SessionConcurrency", 30, "context", 3, 10],
  ["FirstBloodAnomaly", 20, "behavioral", 4],
  ["HoneypotHit", 70, "context", 5, 0],
  ["HoneypotProtocolHit", 90, "context", 3, 0],
  ["HoneypotCanaryFlag", 100, "hard", 3],
  ["HoneypotChain", 150, "context", 1, 0],
  ["FlagEgress", 80, "context", 3, 0],
  ["CrossTeamContainerAccess", 120, "hard", 10],
  ["DelayedSolveSubmission", 40, "behavioral", 5],
  ["InstantSubmitAfterAccess", 50, "behavioral", 3],
  ["SubmitterNeverAccessedContainer", 30, "context", 3, 0],
  ["AccessIpMismatchAtSubmission", 30, "context", 3],
];

export const SUSPICION_RULES = Object.freeze(
  definitions.map(([code, defaultWeight, tier, maxIncidents, corroborationUnit = 5], kind) =>
    Object.freeze({ kind, code, defaultWeight, tier, maxIncidents, corroborationUnit }),
  ),
);

export const SUSPICION_RULE_BY_KIND = new Map(
  SUSPICION_RULES.map((rule) => [rule.kind, rule]),
);
export const SUSPICION_RULE_BY_CODE = new Map(
  SUSPICION_RULES.map((rule) => [rule.code, rule]),
);

export const TIER_CEILINGS = Object.freeze({
  context: 0,
  behavioral: 25,
  strong: 60,
  hard: Number.MAX_SAFE_INTEGER,
});

const DETECTOR_STATUSES = new Set([
  "active",
  "background",
  "telemetryOnly",
  "unimplemented",
]);
const DETECTOR_SCOPES = new Set([
  "allGames",
  "jeopardy",
  "jeopardyContainers",
  "platform",
]);

// This is deliberately a frozen acceptance contract rather than a projection
// of the API response. A rule that silently stops running must change this
// table and its reviewable test expectation before the harness will pass.
export const DETECTOR_CAPABILITIES = Object.freeze([
  ["StolenFlag", "active", "jeopardy"],
  ["SharedIP", "background", "allGames"],
  ["SharedFingerprint", "background", "allGames"],
  ["FingerprintChurn", "background", "allGames"],
  ["IpChurn", "background", "allGames"],
  ["UnknownIP", "background", "allGames"],
  ["CrossTeamIP", "background", "allGames"],
  ["TokenAbuse", "unimplemented", "jeopardy"],
  ["Hoarding", "background", "jeopardy"],
  ["Burst", "background", "jeopardy"],
  ["NoDownload", "telemetryOnly", "jeopardy"],
  ["NoContainer", "telemetryOnly", "jeopardyContainers"],
  ["FastSolve-Open", "telemetryOnly", "jeopardy"],
  ["FastSolve-Download", "telemetryOnly", "jeopardy"],
  ["FastSolve-Container", "telemetryOnly", "jeopardyContainers"],
  ["SequenceSimilarity", "background", "jeopardy"],
  ["CollusionGroup", "unimplemented", "jeopardy"],
  ["ZeroWrongAttempts", "background", "jeopardy"],
  ["WrongFlagLeakage", "telemetryOnly", "jeopardy"],
  ["SolutionRelay", "background", "jeopardy"],
  ["AdaptiveFastSolve", "background", "jeopardy"],
  ["DirectedSolving", "telemetryOnly", "jeopardy"],
  ["ClusteredRegistration", "telemetryOnly", "allGames"],
  ["SubnetOverlap", "background", "allGames"],
  ["HighWrongRate", "background", "jeopardy"],
  ["AutomatedPattern", "background", "jeopardy"],
  ["SessionConcurrency", "background", "allGames"],
  ["FirstBloodAnomaly", "background", "jeopardy"],
  ["HoneypotHit", "telemetryOnly", "platform"],
  ["HoneypotProtocolHit", "telemetryOnly", "platform"],
  ["HoneypotCanaryFlag", "unimplemented", "jeopardy"],
  ["HoneypotChain", "telemetryOnly", "platform"],
  ["FlagEgress", "telemetryOnly", "jeopardyContainers"],
  ["CrossTeamContainerAccess", "background", "jeopardyContainers"],
  ["DelayedSolveSubmission", "background", "jeopardyContainers"],
  ["InstantSubmitAfterAccess", "background", "jeopardyContainers"],
  ["SubmitterNeverAccessedContainer", "telemetryOnly", "jeopardyContainers"],
  ["AccessIpMismatchAtSubmission", "background", "jeopardyContainers"],
].map(([code, status, scope]) => Object.freeze({ code, status, scope })));

const DETECTOR_CAPABILITY_BY_CODE = new Map(
  DETECTOR_CAPABILITIES.map((capability) => [capability.code, capability]),
);

export const CHEAT_SCENARIO_RULES = Object.freeze({
  stolen: Object.freeze({ live: ["StolenFlag"], reconciled: ["StolenFlag"] }),
  brute: Object.freeze({ live: ["HighWrongRate"], reconciled: ["HighWrongRate", "AutomatedPattern"] }),
});

function integer(value, label, minimum = 0) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum) {
    throw new Error(`invalid ${label}: ${value}`);
  }
  return parsed;
}

export function effectiveRuleWeights(rows) {
  if (!Array.isArray(rows)) throw new Error("configured suspicion rules must be an array");
  const weights = new Map(SUSPICION_RULES.map((rule) => [rule.code, rule.defaultWeight]));
  const seen = new Set();
  for (const row of rows) {
    const code = String(row?.ruleCode ?? row?.rule_code ?? "");
    if (!SUSPICION_RULE_BY_CODE.has(code)) {
      throw new Error(`configured suspicion rule ${JSON.stringify(code)} is missing from the harness contract`);
    }
    if (seen.has(code)) throw new Error(`duplicate configured suspicion rule ${code}`);
    seen.add(code);
    weights.set(code, integer(row?.weight, `${code} weight`));
  }
  return weights;
}

export function assertCanonicalRuleProfile(rows) {
  const weights = effectiveRuleWeights(rows);
  const present = new Set(rows.map((row) => String(row?.ruleCode ?? row?.rule_code ?? "")));
  const missing = SUSPICION_RULES.filter((rule) => !present.has(rule.code)).map((rule) => rule.code);
  if (missing.length || rows.length !== SUSPICION_RULES.length) {
    throw new Error(
      `canonical suspicion-rule profile must contain exactly ${SUSPICION_RULES.length} rules; ` +
        `missing: ${missing.join(", ") || "none"}`,
    );
  }
  const changed = SUSPICION_RULES.filter(
    (rule) => weights.get(rule.code) !== rule.defaultWeight,
  ).map((rule) => `${rule.code}=${weights.get(rule.code)} (expected ${rule.defaultWeight})`);
  if (changed.length) {
    throw new Error(`canonical suspicion-rule defaults changed: ${changed.join(", ")}`);
  }
  return weights;
}

export function validateDetectorCapabilities(rows) {
  if (!Array.isArray(rows)) throw new Error("detector capabilities must be an array");
  const byCode = new Map();
  for (const row of rows) {
    const code = String(row?.code ?? "");
    if (!SUSPICION_RULE_BY_CODE.has(code)) {
      throw new Error(`unknown detector capability ${JSON.stringify(code)}`);
    }
    if (byCode.has(code)) throw new Error(`duplicate detector capability ${code}`);
    if (!DETECTOR_STATUSES.has(row?.status)) {
      throw new Error(`detector capability ${code} has invalid status ${row?.status}`);
    }
    if (!DETECTOR_SCOPES.has(row?.scope)) {
      throw new Error(`detector capability ${code} has invalid scope ${row?.scope}`);
    }
    const expected = DETECTOR_CAPABILITY_BY_CODE.get(code);
    if (row.status !== expected.status || row.scope !== expected.scope) {
      throw new Error(
        `detector capability ${code} is ${row.status}/${row.scope}; ` +
          `expected ${expected.status}/${expected.scope}`,
      );
    }
    if (typeof row?.detail !== "string" || row.detail.trim().length === 0) {
      throw new Error(`detector capability ${code} needs a non-empty detail`);
    }
    byCode.set(code, Object.freeze({ ...row }));
  }
  const missing = SUSPICION_RULES.filter((rule) => !byCode.has(rule.code));
  if (missing.length || byCode.size !== SUSPICION_RULES.length) {
    throw new Error(
      `detector capabilities must cover exactly ${SUSPICION_RULES.length} stable kinds; ` +
        `missing: ${missing.map((rule) => rule.code).join(", ") || "none"}`,
    );
  }
  return byCode;
}

function eventIdentity(event) {
  const kind = integer(event?.kind, "suspicion event kind");
  const rule = SUSPICION_RULE_BY_KIND.get(kind);
  if (!rule) throw new Error(`unsupported suspicion event kind ${kind}`);
  const evidenceKey = String(event?.evidenceKey ?? event?.evidence_key ?? "");
  const scoreDeltaValue = event?.scoreDelta ?? event?.score_delta;
  const scoreDelta = scoreDeltaValue === null || scoreDeltaValue === undefined
    ? null
    : integer(scoreDeltaValue, `${rule.code} score delta`);
  const createdAtMs = integer(
    event?.createdAtMs ?? event?.created_at_ms ?? event?.time,
    `${rule.code} event time`,
    1,
  );
  const createdAtMicros = integer(
    event?.createdAtMicros ?? event?.created_at_micros ?? createdAtMs * 1000,
    `${rule.code} event ordering time`,
    1,
  );
  return {
    id: integer(event?.id ?? 0, `${rule.code} event id`),
    rule,
    evidenceKey,
    scoreDelta,
    createdAtMs,
    createdAtMicros,
  };
}

export function computeExpectedBreakdown(events, configuredWeights = new Map()) {
  if (!Array.isArray(events)) throw new Error("suspicion events must be an array");
  // Both the cached-score rebuild and report load the canonical ledger
  // newest-first. That order matters when different rules share one tier
  // ceiling because the newest rule receives the remaining allowance first.
  const normalized = events.map(eventIdentity).sort(
    (left, right) => right.createdAtMicros - left.createdAtMicros || right.id - left.id,
  );
  const groupOrder = [];
  const groups = new Map();
  for (const event of normalized) {
    if (!groups.has(event.rule.code)) {
      groups.set(event.rule.code, []);
      groupOrder.push(event.rule.code);
    }
    groups.get(event.rule.code).push(event);
  }

  const tierSubtotal = new Map();
  const tierScored = new Map();
  const contextSeen = new Set();
  const contextCandidates = [];
  const annotated = [];

  for (const code of groupOrder) {
    const rule = SUSPICION_RULE_BY_CODE.get(code);
    const ordered = [...groups.get(code)].sort(
      (left, right) => right.createdAtMicros - left.createdAtMicros || right.id - left.id,
    );
    const seenIncidents = new Set();
    let legacyIncidentSeen = false;
    let countedIncidents = 0;
    const newestAnnotatedIndex = annotated.length;

    for (const event of ordered) {
      const isLegacy = event.scoreDelta === null || event.evidenceKey.startsWith("legacy:");
      const isNewIncident = isLegacy
        ? !legacyIncidentSeen
        : event.evidenceKey.length > 0 && !seenIncidents.has(event.evidenceKey);
      if (isLegacy) legacyIncidentSeen = true;
      else if (event.evidenceKey.length > 0) seenIncidents.add(event.evidenceKey);
      const scoreDelta = event.scoreDelta ?? configuredWeights.get(code) ?? rule.defaultWeight;
      let counted = false;
      let appliedDelta = 0;
      if (rule.tier !== "context" && isNewIncident && countedIncidents < rule.maxIncidents) {
        countedIncidents += 1;
        const scored = tierScored.get(rule.tier) ?? 0;
        const remaining = Math.max(0, TIER_CEILINGS[rule.tier] - scored);
        const contribution = Math.min(Math.max(0, scoreDelta), remaining);
        if (contribution > 0) {
          counted = true;
          appliedDelta = contribution;
          tierScored.set(rule.tier, scored + contribution);
          tierSubtotal.set(rule.tier, (tierSubtotal.get(rule.tier) ?? 0) + contribution);
        }
      }
      annotated.push({
        id: event.id,
        type: code,
        scoreDelta,
        appliedDelta,
        tier: rule.tier,
        counted,
        time: event.createdAtMs,
      });
    }

    if (rule.tier === "context" && !contextSeen.has(code)) {
      contextSeen.add(code);
      if (rule.corroborationUnit > 0) {
        contextCandidates.push({
          createdAtMicros: ordered[0].createdAtMicros,
          code,
          eventIndex: newestAnnotatedIndex,
          unit: rule.corroborationUnit,
        });
      }
    }
  }

  const hard = tierSubtotal.get("hard") ?? 0;
  const strong = Math.min(TIER_CEILINGS.strong, tierSubtotal.get("strong") ?? 0);
  const behavioral = Math.min(
    TIER_CEILINGS.behavioral,
    tierSubtotal.get("behavioral") ?? 0,
  );
  contextCandidates.sort(
    (left, right) =>
      right.createdAtMicros - left.createdAtMicros ||
      (left.code < right.code ? -1 : left.code > right.code ? 1 : 0),
  );
  let remainingCorroboration = Math.floor(hard / 2);
  let corroboration = 0;
  for (const candidate of contextCandidates) {
    const applied = Math.min(candidate.unit, remainingCorroboration);
    if (applied === 0) continue;
    annotated[candidate.eventIndex].appliedDelta = applied;
    annotated[candidate.eventIndex].counted = true;
    corroboration += applied;
    remainingCorroboration -= applied;
  }
  const total = hard + strong + behavioral + corroboration;
  const band = hard > 0
    ? "evidenced"
    : strong > 0
      ? "investigate"
      : behavioral > 0
        ? "watch"
        : contextSeen.size > 0
          ? "context"
          : "clean";

  return {
    hard,
    strong,
    behavioral,
    corroboration,
    total,
    band,
    events: annotated,
  };
}

export function canonicalScenarioBreakdowns() {
  const at = (kind, id) => ({
    id,
    kind,
    evidenceKey: `contract:${id}`,
    scoreDelta: SUSPICION_RULE_BY_KIND.get(kind).defaultWeight,
    createdAtMs: 1_700_000_000_000 + id,
  });
  return Object.freeze({
    stolen: computeExpectedBreakdown([at(0, 1)]),
    brute: computeExpectedBreakdown([at(24, 2), at(25, 3)]),
  });
}
