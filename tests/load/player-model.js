// Deterministic, runtime-neutral player behavior for Node orchestration and k6.
// Decisions are reproducible from public observations plus per-team memory; no
// client needs another team's private skill profile.

const ENGAGEMENT_SPECS = Object.freeze([
  {
    engagementTier: "always-on",
    share: 0.1,
    activity: 0.94,
    thinkSeconds: 4,
    actionCredits: 5,
  },
  {
    engagementTier: "committed",
    share: 0.25,
    activity: 0.82,
    thinkSeconds: 5,
    actionCredits: 4,
  },
  {
    engagementTier: "part-time",
    share: 0.45,
    activity: 0.64,
    thinkSeconds: 7,
    actionCredits: 3,
  },
  {
    engagementTier: "casual",
    share: 0.2,
    activity: 0.4,
    thinkSeconds: 10,
    actionCredits: 2,
  },
]);

const SKILL_NAMES = Object.freeze([
  "offense",
  "defense",
  "koth",
  "jeopardy",
]);

const SPECIALTY_SPECS = Object.freeze([
  {
    specialty: "offense",
    share: 0.2,
    offsets: Object.freeze({
      offense: 0.38,
      defense: -0.14,
      koth: -0.12,
      jeopardy: -0.12,
    }),
  },
  {
    specialty: "defense",
    share: 0.2,
    offsets: Object.freeze({
      offense: -0.14,
      defense: 0.38,
      koth: -0.12,
      jeopardy: -0.12,
    }),
  },
  {
    specialty: "koth",
    share: 0.2,
    offsets: Object.freeze({
      offense: -0.12,
      defense: -0.12,
      koth: 0.38,
      jeopardy: -0.14,
    }),
  },
  {
    specialty: "jeopardy",
    share: 0.2,
    offsets: Object.freeze({
      offense: -0.12,
      defense: -0.12,
      koth: -0.14,
      jeopardy: 0.38,
    }),
  },
  {
    specialty: "balanced",
    share: 0.2,
    offsets: Object.freeze({
      offense: 0,
      defense: 0,
      koth: 0,
      jeopardy: 0,
    }),
  },
]);

export const PLAYER_ACTION_COSTS = Object.freeze({
  attack: 1,
  kothClaim: 1,
  staticSolve: 1,
  patch: 2,
  attachmentSolve: 2,
  containerSolve: 2,
});

const clamp = (value, minimum, maximum) =>
  Math.min(maximum, Math.max(minimum, value));

function finite(value, fallback = 0) {
  const number = Number(value);
  return Number.isFinite(number) ? number : fallback;
}

export function keyedUnit(seed, ...parts) {
  const input = [seed, ...parts].join("|");
  let hash = 0x811c9dc5;
  for (let index = 0; index < input.length; index++) {
    hash ^= input.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  // A final avalanche prevents adjacent team/round identifiers from clustering.
  hash ^= hash >>> 16;
  hash = Math.imul(hash, 0x7feb352d);
  hash ^= hash >>> 15;
  hash = Math.imul(hash, 0x846ca68b);
  hash ^= hash >>> 16;
  return (hash >>> 0) / 0x1_0000_0000;
}

function proportionalCounts(total, specs) {
  const raw = specs.map((spec) => total * spec.share);
  const counts = raw.map(Math.floor);
  let remaining = total - counts.reduce((sum, count) => sum + count, 0);
  const remainderOrder = raw
    .map((value, index) => ({ index, remainder: value - Math.floor(value) }))
    .sort(
      (left, right) =>
        right.remainder - left.remainder || left.index - right.index,
    );
  for (let index = 0; index < remaining; index++) {
    counts[remainderOrder[index % remainderOrder.length].index]++;
  }
  return counts;
}

function shuffledAllocation(total, specs, seed, label) {
  const counts = proportionalCounts(total, specs);
  return counts
    .flatMap((count, specIndex) =>
      Array.from({ length: count }, () => specs[specIndex]),
    )
    .map((spec, slot) => ({
      spec,
      slot,
      priority: keyedUnit(seed, label, slot),
    }))
    .sort((left, right) => left.priority - right.priority || left.slot - right.slot)
    .map(({ spec }) => spec);
}

function normalizeSkillBudget(rawSkills, budget) {
  const minimum = 0.16;
  const maximum = 0.94;
  const values = {};
  const remainingSkills = new Set(SKILL_NAMES);
  let remainingBudget = budget;

  while (remainingSkills.size > 0) {
    const rawTotal = [...remainingSkills].reduce(
      (sum, name) => sum + rawSkills[name],
      0,
    );
    const scale = remainingBudget / rawTotal;
    let bounded = null;
    for (const name of remainingSkills) {
      const projected = rawSkills[name] * scale;
      if (projected < minimum) {
        bounded = { name, value: minimum, distance: minimum - projected };
      } else if (
        projected > maximum &&
        (!bounded || projected - maximum > bounded.distance)
      ) {
        bounded = { name, value: maximum, distance: projected - maximum };
      }
    }
    if (!bounded) {
      for (const name of remainingSkills) values[name] = rawSkills[name] * scale;
      break;
    }
    values[bounded.name] = bounded.value;
    remainingBudget -= bounded.value;
    remainingSkills.delete(bounded.name);
  }

  return Object.freeze(values);
}

function buildSkills(seed, index, specialtySpec) {
  const raw = Object.fromEntries(
    SKILL_NAMES.map((name) => [
      name,
      0.52 +
        specialtySpec.offsets[name] +
        (keyedUnit(seed, index, "skill", name) - 0.5) * 0.28,
    ]),
  );
  const budget = 2.05 + keyedUnit(seed, index, "skill-budget") * 0.75;
  return {
    budget,
    values: normalizeSkillBudget(raw, budget),
  };
}

function profileJitter(seed, index, label, scale) {
  return (keyedUnit(seed, index, label) - 0.5) * scale;
}

export function buildPlayerProfiles(teamCount, seed = "rsctf-competitive-v2") {
  const count = Number(teamCount);
  if (!Number.isSafeInteger(count) || count < 2) {
    throw new Error("teamCount must be an integer >= 2");
  }
  const normalizedSeed = String(seed).trim();
  if (!normalizedSeed || normalizedSeed.length > 64) {
    throw new Error("seed must contain 1-64 characters");
  }

  const engagement = shuffledAllocation(
    count,
    ENGAGEMENT_SPECS,
    normalizedSeed,
    "engagement-allocation",
  );
  const specialties = shuffledAllocation(
    count,
    SPECIALTY_SPECS,
    normalizedSeed,
    "specialty-allocation",
  );

  return Array.from({ length: count }, (_, index) => {
    const engagementSpec = engagement[index];
    const specialtySpec = specialties[index];
    const profileSeed = `${normalizedSeed}:${index}`;
    const skills = buildSkills(normalizedSeed, index, specialtySpec);
    const riskBias = {
      offense: 0.12,
      defense: -0.1,
      koth: 0.1,
      jeopardy: 0.02,
      balanced: 0,
    }[specialtySpec.specialty];
    const explorationBias = {
      offense: 0.06,
      defense: -0.05,
      koth: 0.02,
      jeopardy: 0.1,
      balanced: 0,
    }[specialtySpec.specialty];
    const activity = clamp(
      engagementSpec.activity +
        profileJitter(normalizedSeed, index, "activity", 0.08),
      0.25,
      0.99,
    );
    const risk = clamp(
      0.48 + riskBias + profileJitter(normalizedSeed, index, "risk", 0.28),
      0.12,
      0.9,
    );
    const exploration = clamp(
      0.42 +
        explorationBias +
        profileJitter(normalizedSeed, index, "exploration", 0.32),
      0.12,
      0.88,
    );
    const persistence = clamp(
      0.35 + activity * 0.38 + profileJitter(normalizedSeed, index, "persistence", 0.24),
      0.2,
      0.92,
    );
    const firstPatchProgress = clamp(
      0.62 - skills.values.defense * 0.5 + profileJitter(normalizedSeed, index, "patch-1", 0.08),
      0.05,
      0.82,
    );

    return Object.freeze({
      version: 2,
      index,
      seed: profileSeed,
      engagementTier: engagementSpec.engagementTier,
      specialty: specialtySpec.specialty,
      activity,
      thinkSeconds: Math.max(
        4,
        engagementSpec.thinkSeconds +
          Math.round(profileJitter(normalizedSeed, index, "think", 2)),
      ),
      offense: skills.values.offense,
      defense: skills.values.defense,
      koth: skills.values.koth,
      jeopardy: skills.values.jeopardy,
      skillBudget: skills.budget,
      risk,
      persistence,
      exploration,
      actionCreditsPerRound: clamp(
        engagementSpec.actionCredits +
          (keyedUnit(profileSeed, "credit-capacity") < persistence * 0.3 ? 1 : 0),
        2,
        6,
      ),
      maxAttacks:
        specialtySpec.specialty === "offense"
          ? 3
          : specialtySpec.specialty === "defense"
            ? 1
            : 2,
      firstPatchProgress,
      secondPatchProgress: clamp(
        firstPatchProgress +
          0.24 +
          (1 - skills.values.defense) * 0.16 +
          profileJitter(normalizedSeed, index, "patch-2", 0.08),
        0.18,
        1.18,
      ),
      discoveryRounds: Math.max(
        0,
        Math.round(
          (1 - skills.values.offense) * 24 +
            profileJitter(normalizedSeed, index, "discovery", 8),
        ),
      ),
      rivalIndex:
        (index +
          1 +
          Math.floor(keyedUnit(profileSeed, "rival") * (count - 1))) %
        count,
    });
  });
}

export function isProfileActive(profile, elapsedSeconds) {
  const elapsed = Math.max(0, Number(elapsedSeconds) || 0);
  const sessionBlock = Math.floor(elapsed / 90);
  return keyedUnit(profile.seed, "session", sessionBlock) < profile.activity;
}

export function playerThinkDelay(profile, iteration, multiplier = 1) {
  const currentIteration = Number(iteration);
  const scale = Number(multiplier);
  if (
    !profile ||
    typeof profile !== "object" ||
    typeof profile.seed !== "string" ||
    profile.seed.length === 0 ||
    !Number.isFinite(Number(profile.thinkSeconds)) ||
    Number(profile.thinkSeconds) <= 0 ||
    !Number.isSafeInteger(currentIteration) ||
    currentIteration < 0 ||
    !Number.isFinite(scale) ||
    scale <= 0
  ) {
    throw new TypeError("player think delay requires a valid profile, iteration, and multiplier");
  }
  const jitter = 0.65 + keyedUnit(profile.seed, "think-jitter", currentIteration) * 0.7;
  const burst = keyedUnit(profile.seed, "short-burst", currentIteration) < 0.08 ? 0.5 : 1;
  // One iteration polls several views as a browser would. A four-second floor
  // keeps a fast interaction burst inside the normal per-account API budget.
  return Math.max(4, Number(profile.thinkSeconds) * scale * jitter * burst);
}

export function isRetryablePlatformStatus(status) {
  const code = Number(status);
  return (
    Number.isSafeInteger(code) &&
    (code === 0 || code === 429 || (code >= 500 && code <= 599))
  );
}

export function classifyPlatformFirstAttemptFailure(status) {
  const code = Number(status);
  if (code === 0) return "timeout";
  if (code === 429) return "rateLimit";
  if (Number.isSafeInteger(code) && code >= 500 && code <= 599) {
    return "serverError";
  }
  return null;
}

export function boundedPlatformRetryDelay(
  retryAfter,
  fallbackDelay,
  maximumRetryAfter = 2,
) {
  const fallback = Number(fallbackDelay);
  const maximum = Number(maximumRetryAfter);
  if (!Number.isFinite(fallback) || fallback < 0.2) {
    throw new TypeError("platform retry fallback must be at least 200 ms");
  }
  if (!Number.isFinite(maximum) || maximum < 0.2) {
    throw new TypeError("platform retry maximum must be at least 200 ms");
  }
  const requested = Number(retryAfter);
  if (Number.isFinite(requested) && requested >= 0 && requested <= maximum) {
    return Math.max(0.2, requested);
  }
  return fallback;
}

export function isRetryablePlatformRequest(method, status) {
  const normalizedMethod =
    typeof method === "string" ? method.toUpperCase() : "";
  if (normalizedMethod === "GET") return isRetryablePlatformStatus(status);
  // Container mutations are safe to replay only when the rate limiter rejected
  // the request before its handler ran. A timeout or 5xx may follow a successful
  // create/delete, so retrying either could duplicate or undo real state.
  return (
    (normalizedMethod === "POST" || normalizedMethod === "DELETE") &&
    Number(status) === 429
  );
}

export function kothCapabilityMatchesState(tokenModel, stateModel) {
  if (
    !tokenModel ||
    typeof tokenModel !== "object" ||
    !stateModel ||
    typeof stateModel !== "object" ||
    tokenModel.status !== "ready" ||
    typeof tokenModel.token !== "string" ||
    tokenModel.token.length === 0
  ) {
    return false;
  }
  const tokenRound = Number(tokenModel.round);
  const stateRound = Number(stateModel.round);
  return (
    Number.isSafeInteger(tokenRound) &&
    tokenRound >= 0 &&
    Number.isSafeInteger(stateRound) &&
    stateRound >= 0 &&
    tokenRound === stateRound
  );
}

function publicTargetHost(value) {
  if (typeof value !== "string") return null;
  const host = value.trim();
  if (host.length === 0 || host.length > 253) return null;
  const octets = host.split(".");
  if (
    octets.length === 4 &&
    octets.every(
      (octet) => /^\d{1,3}$/.test(octet) && Number(octet) >= 0 && Number(octet) <= 255,
    )
  ) {
    return host;
  }
  const labels = host.split(".");
  return labels.every(
    (label) =>
      label.length >= 1 &&
      label.length <= 63 &&
      /^[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?$/.test(label),
  )
    ? host
    : null;
}

/// Resolve attack endpoints exclusively from one team's authenticated public
/// A&D target response. The roster supplies identity-to-index correlation but
/// never supplies an opponent's network address.
export function publicAdNetworkTargets(
  targetsModel,
  challengeId,
  participationIds,
  ownParticipationId,
) {
  if (!Number.isSafeInteger(challengeId) || challengeId <= 0) {
    throw new TypeError("A&D challengeId must be a positive integer");
  }
  if (
    !Array.isArray(participationIds) ||
    participationIds.length < 2 ||
    participationIds.some((id) => !Number.isSafeInteger(id) || id <= 0) ||
    new Set(participationIds).size !== participationIds.length ||
    !participationIds.includes(ownParticipationId)
  ) {
    throw new TypeError("A&D participation roster must be distinct and include the caller");
  }
  if (
    !targetsModel ||
    typeof targetsModel !== "object" ||
    !Array.isArray(targetsModel.challenges)
  ) {
    return [];
  }
  const matches = targetsModel.challenges.filter(
    (challenge) => challenge?.challengeId === challengeId,
  );
  const challenge = matches.length === 1 ? matches[0] : null;
  if (!challenge || challenge.hill !== null || !Array.isArray(challenge.teams)) {
    return [];
  }

  const rosterIndex = new Map(participationIds.map((id, index) => [id, index]));
  const seen = new Set();
  const targets = [];
  for (const team of challenge.teams) {
    const participationId = Number(team?.participationId);
    const host = publicTargetHost(team?.ip);
    const port = Number(team?.port);
    if (
      !Number.isSafeInteger(participationId) ||
      participationId === ownParticipationId ||
      !rosterIndex.has(participationId) ||
      seen.has(participationId) ||
      host === null ||
      !Number.isSafeInteger(port) ||
      port < 1 ||
      port > 65535
    ) {
      return [];
    }
    seen.add(participationId);
    targets.push(
      Object.freeze({
        index: rosterIndex.get(participationId),
        participationId,
        host,
        port,
      }),
    );
  }
  return Object.freeze(targets.sort((left, right) => left.index - right.index));
}

export function kothTargetMatchesState(targetsModel, stateModel, challengeId) {
  if (
    !targetsModel ||
    typeof targetsModel !== "object" ||
    !stateModel ||
    typeof stateModel !== "object" ||
    !Number.isSafeInteger(challengeId) ||
    challengeId <= 0 ||
    !Number.isSafeInteger(targetsModel.currentRound) ||
    !Number.isSafeInteger(stateModel.round) ||
    targetsModel.currentRound !== stateModel.round ||
    !Number.isSafeInteger(stateModel.cycleNumber) ||
    stateModel.cycleNumber < 1 ||
    Object.prototype.hasOwnProperty.call(stateModel, "containerId") ||
    !Array.isArray(targetsModel.challenges)
  ) {
    return false;
  }
  const hill = targetsModel.challenges.find(
    (challenge) => challenge?.challengeId === challengeId,
  )?.hill;
  return (
    hill !== null &&
    typeof hill === "object" &&
    Number.isSafeInteger(hill.cycleNumber) &&
    hill.cycleNumber === stateModel.cycleNumber &&
    !Object.prototype.hasOwnProperty.call(hill, "containerId")
  );
}

export function defenseLevelAt(profile, progress) {
  const bounded = clamp(Number(progress) || 0, 0, 1);
  let level =
    bounded >= profile.secondPatchProgress
      ? 2
      : bounded >= profile.firstPatchProgress
        ? 1
        : 0;
  if (level > 0) {
    const block = Math.floor(bounded * 30);
    const rollbackChance = 0.04 + (1 - profile.defense) * 0.14;
    if (keyedUnit(profile.seed, "rollback", block) < rollbackChance) level--;
  }
  return level;
}

function actionCost(action) {
  const cost = PLAYER_ACTION_COSTS[action];
  if (!Number.isSafeInteger(cost)) throw new Error(`unknown player action: ${action}`);
  return cost;
}

function validateActionBudget(budget) {
  if (
    !budget ||
    typeof budget !== "object" ||
    !Number.isSafeInteger(budget.round) ||
    budget.round < 1 ||
    !Number.isSafeInteger(budget.total) ||
    budget.total < 1 ||
    !Number.isSafeInteger(budget.spent) ||
    budget.spent < 0 ||
    !Number.isSafeInteger(budget.remaining) ||
    budget.remaining < 0 ||
    budget.spent + budget.remaining !== budget.total
  ) {
    throw new TypeError("invalid player action budget");
  }
}

export function buildRoundActionBudget(profile, round) {
  const currentRound = Number(round);
  if (!Number.isSafeInteger(currentRound) || currentRound < 1) {
    throw new Error("round must be an integer >= 1");
  }
  const base = Number(profile?.actionCreditsPerRound);
  if (!Number.isSafeInteger(base) || base < 1 || base > 6) {
    throw new Error("profile actionCreditsPerRound must be an integer from 1 to 6");
  }
  const bonus =
    keyedUnit(profile.seed, "round-credit", currentRound) <
    profile.persistence * 0.22
      ? 1
      : 0;
  const total = clamp(base + bonus, 1, 6);
  return Object.freeze({ round: currentRound, total, spent: 0, remaining: total });
}

export function canSpendActionCredit(budget, action) {
  validateActionBudget(budget);
  return budget.remaining >= actionCost(action);
}

export function spendActionCredit(budget, action) {
  validateActionBudget(budget);
  const cost = actionCost(action);
  if (budget.remaining < cost) {
    throw new RangeError(`insufficient action credits for ${action}`);
  }
  return Object.freeze({
    ...budget,
    spent: budget.spent + cost,
    remaining: budget.remaining - cost,
  });
}

function attackTechnique(profile, progress, round) {
  let technique = 1;
  if (progress >= 0.28 + (1 - profile.offense) * 0.22) technique = 2;
  if (
    progress >= 0.72 + (1 - profile.offense) * 0.18 &&
    profile.offense >= 0.68
  ) {
    technique = 3;
  }
  if (technique > 1 && keyedUnit(profile.seed, "technique", round) < 0.08) {
    technique--;
  }
  return technique;
}

function memoryEntry(memory, index) {
  if (memory instanceof Map) return memory.get(index) || {};
  return memory?.[index] || {};
}

function normalizedPublicOpponents(profile, opponents) {
  if (!Array.isArray(opponents)) {
    throw new TypeError("public opponents must be an array");
  }
  const indices = new Set();
  return opponents
    .filter((candidate) => candidate?.index !== profile.index)
    .map((candidate) => {
      const index = Number(candidate?.index);
      if (!Number.isSafeInteger(index) || index < 0 || indices.has(index)) {
        throw new TypeError("public opponent indices must be distinct non-negative integers");
      }
      indices.add(index);
      return {
        index,
        rank:
          Number.isSafeInteger(Number(candidate.rank)) && Number(candidate.rank) > 0
            ? Number(candidate.rank)
            : opponents.length,
        settledTotal: finite(candidate.settledTotal),
        projectedTotal: finite(candidate.projectedTotal),
        defenseRate: clamp(finite(candidate.defenseRate), 0, 1),
        slaRate: clamp(finite(candidate.slaRate, 1), 0, 1),
      };
    });
}

function publicScoreRange(opponents) {
  const values = opponents.map((candidate) =>
    Math.max(candidate.settledTotal, candidate.projectedTotal),
  );
  return {
    minimum: values.length ? Math.min(...values) : 0,
    maximum: values.length ? Math.max(...values) : 0,
  };
}

export function attackPlan(
  profile,
  publicOpponents,
  opponentMemory,
  round,
  startRound,
  progress = 0,
  options = {},
) {
  const currentRound = Number(round);
  const firstRound = Number(startRound);
  const boundedProgress = clamp(Number(progress) || 0, 0, 1);
  if (
    !Number.isSafeInteger(currentRound) ||
    !Number.isSafeInteger(firstRound) ||
    currentRound < firstRound
  ) {
    return { targets: [], technique: 1, strategy: "waiting" };
  }
  const opponents = normalizedPublicOpponents(profile, publicOpponents);
  if (opponents.length === 0) {
    return { targets: [], technique: 1, strategy: "no-targets" };
  }
  const elapsedRounds = currentRound - firstRound;
  const discoveryProgress = clamp(
    0.04 + (1 - profile.offense) * 0.16,
    0.04,
    0.2,
  );
  if (
    elapsedRounds < profile.discoveryRounds &&
    boundedProgress < discoveryProgress
  ) {
    return { targets: [], technique: 1, strategy: "discovering" };
  }

  const configuredMaximum = options.maxTargets ?? profile.maxAttacks;
  if (!Number.isSafeInteger(configuredMaximum) || configuredMaximum < 0) {
    throw new TypeError("maxTargets must be a non-negative integer");
  }
  const maximum = clamp(configuredMaximum, 0, Math.min(3, opponents.length));
  const expected = profile.offense * maximum;
  let budget = Math.floor(expected);
  if (keyedUnit(profile.seed, "budget", currentRound) < expected - budget) budget++;
  if (
    keyedUnit(profile.seed, "round-off", currentRound) >
    profile.activity + 0.12
  ) {
    budget = 0;
  }
  if (budget === 0) {
    return {
      targets: [],
      technique: attackTechnique(profile, boundedProgress, currentRound),
      strategy: "no-budget",
    };
  }

  const technique = attackTechnique(profile, boundedProgress, currentRound);
  const scoreRange = publicScoreRange(opponents);
  const scored = opponents
    .map((candidate) => {
      const history = memoryEntry(opponentMemory, candidate.index);
      const attempts = Math.max(0, Number(history.attempts) || 0);
      const captures = clamp(Number(history.captures) || 0, 0, attempts);
      const successRate = (captures + 1) / (attempts + 2);
      const patchedTechniques = Array.isArray(history.patchedTechniques)
        ? history.patchedTechniques
        : [];
      const patchedPenalty = patchedTechniques.includes(technique) ? 0.72 : 0;
      const publicTotal = Math.max(
        candidate.settledTotal,
        candidate.projectedTotal,
      );
      const scorePressure =
        scoreRange.maximum > scoreRange.minimum
          ? (publicTotal - scoreRange.minimum) /
            (scoreRange.maximum - scoreRange.minimum)
          : 0;
      const leaderPressure =
        boundedProgress > 0.35
          ? 1 -
            (candidate.rank - 1) /
              Math.max(1, publicOpponents.length - 1)
          : 0;
      const rivalry = candidate.index === profile.rivalIndex ? 0.34 : 0;
      const lastTargetRound = Number(history.lastTargetRound) || 0;
      const recencyPenalty = lastTargetRound >= currentRound - 1 ? 0.24 : 0;
      const exploration = keyedUnit(
        profile.seed,
        "target",
        currentRound,
        candidate.index,
      );
      return {
        index: candidate.index,
        score:
          (1 - candidate.defenseRate) * 0.3 +
          candidate.slaRate * 0.08 +
          successRate * 0.2 +
          scorePressure * 0.13 +
          leaderPressure * 0.19 +
          rivalry +
          exploration * 0.18 -
          patchedPenalty -
          recencyPenalty,
      };
    })
    .sort(
      (left, right) => right.score - left.score || left.index - right.index,
    );

  const targets = scored.slice(0, budget).map(({ index }) => index);
  const scanStart = Math.floor(
    keyedUnit(profile.seed, "scan-start") * opponents.length,
  );
  const scanTarget =
    opponents[(scanStart + elapsedRounds) % opponents.length].index;
  const explore =
    keyedUnit(profile.seed, "explore", currentRound) <
    profile.exploration * (budget === 1 ? 0.28 : 0.42);
  let strategy = "preferred";
  if (explore && budget === 1) {
    targets[0] = scanTarget;
    strategy = "explore";
  } else if (explore && budget > 1 && !targets.includes(scanTarget)) {
    targets[targets.length - 1] = scanTarget;
    strategy = "mixed";
  }

  return { targets: [...new Set(targets)], technique, strategy };
}

const ATTACK_OUTCOMES = Object.freeze([
  "captured",
  "patched",
  "unavailable",
  "transportFailure",
]);

export function recordAttackOutcome(
  memory,
  { victimIndex, technique, outcome, round },
) {
  const index = Number(victimIndex);
  const currentRound = Number(round);
  const currentTechnique = Number(technique);
  if (!Number.isSafeInteger(index) || index < 0) {
    throw new TypeError("victimIndex must be a non-negative integer");
  }
  if (!Number.isSafeInteger(currentRound) || currentRound < 1) {
    throw new TypeError("round must be a positive integer");
  }
  if (!Number.isSafeInteger(currentTechnique) || currentTechnique < 1 || currentTechnique > 3) {
    throw new TypeError("technique must be an integer from 1 to 3");
  }
  if (!ATTACK_OUTCOMES.includes(outcome)) {
    throw new TypeError(`unknown attack outcome: ${outcome}`);
  }
  const existing = memory?.[index] || {};
  const patchedTechniques = new Set(
    Array.isArray(existing.patchedTechniques)
      ? existing.patchedTechniques
      : [],
  );
  if (outcome === "patched") patchedTechniques.add(currentTechnique);
  const updated = Object.freeze({
    attempts: Math.max(0, Number(existing.attempts) || 0) + 1,
    captures:
      Math.max(0, Number(existing.captures) || 0) +
      (outcome === "captured" ? 1 : 0),
    unavailable:
      Math.max(0, Number(existing.unavailable) || 0) +
      (outcome === "unavailable" ? 1 : 0),
    transportFailures:
      Math.max(0, Number(existing.transportFailures) || 0) +
      (outcome === "transportFailure" ? 1 : 0),
    patchedTechniques: Object.freeze([...patchedTechniques].sort()),
    lastTargetRound: currentRound,
  });
  return Object.freeze({ ...(memory || {}), [index]: updated });
}

const JEOPARDY_KINDS = new Set(["static", "attachment", "container"]);

function normalizedChallengeCatalog(catalog) {
  if (!Array.isArray(catalog) || catalog.length === 0) {
    throw new Error("challenge catalog must contain at least one challenge");
  }
  const ids = new Set();
  return catalog.map((challenge, index) => {
    const challengeId = Number(challenge?.challengeId);
    const kind = String(challenge?.kind || "static");
    if (!Number.isSafeInteger(challengeId) || challengeId < 1 || ids.has(challengeId)) {
      throw new Error("challenge catalog ids must be distinct positive integers");
    }
    if (!JEOPARDY_KINDS.has(kind)) {
      throw new Error(`unsupported Jeopardy challenge kind: ${kind}`);
    }
    ids.add(challengeId);
    return Object.freeze({
      challengeId,
      kind,
      category: String(challenge?.category || "Misc"),
      difficulty: clamp(finite(challenge?.difficulty, 5), 1, 10),
      catalogIndex: index,
    });
  });
}

function challengePreference(profile, challenge) {
  const difficultyTarget = 2 + profile.jeopardy * 8;
  const difficultyFit =
    1 - Math.abs(challenge.difficulty - difficultyTarget) / 10;
  const categoryAffinity = keyedUnit(
    profile.seed,
    "jeopardy-category",
    challenge.category,
  );
  const exploration = keyedUnit(
    profile.seed,
    "jeopardy-challenge",
    challenge.challengeId,
  );
  return (
    difficultyFit * 0.42 +
    categoryAffinity * 0.35 +
    exploration * 0.23
  );
}

export function buildJeopardyCatalog(challengeCatalog) {
  return Object.freeze(normalizedChallengeCatalog(challengeCatalog));
}

export function isJeopardyFocusRound(profile, roundNumber) {
  const round = Number(roundNumber);
  if (
    !profile ||
    typeof profile !== "object" ||
    typeof profile.seed !== "string" ||
    profile.seed.length === 0 ||
    !Number.isSafeInteger(round) ||
    round < 1
  ) {
    throw new TypeError("Jeopardy focus requires a profile seed and positive round");
  }
  for (const field of ["jeopardy", "persistence", "exploration"]) {
    if (!Number.isFinite(profile[field]) || profile[field] < 0 || profile[field] > 1) {
      throw new TypeError(`profile.${field} must be a finite rate`);
    }
  }
  const focusChance = clamp(
    0.08 +
      profile.jeopardy * 0.18 +
      profile.persistence * 0.06 +
      profile.exploration * 0.03,
    0.08,
    0.34,
  );
  return keyedUnit(profile.seed, "jeopardy-round-focus", round) < focusChance;
}

function jeopardyKindInterest(profile, challenge) {
  const interest = keyedUnit(
    profile.seed,
    "jeopardy-kind-interest",
    challenge.challengeId,
  );
  if (challenge.kind === "static") {
    return (
      interest <
      clamp(
        0.42 +
          profile.jeopardy * 0.42 +
          profile.exploration * 0.14 -
          challenge.difficulty * 0.025,
        0.25,
        0.94,
      )
    );
  }
  if (challenge.kind === "attachment") {
    return (
      interest <
      clamp(
        0.2 + profile.jeopardy * 0.46 + profile.exploration * 0.2,
        0.2,
        0.88,
      )
    );
  }
  return (
    interest <
    clamp(
      0.05 +
        profile.jeopardy * 0.25 +
        profile.exploration * 0.12 +
        profile.risk * 0.08,
      0.05,
      0.5,
    )
  );
}

function normalizedJeopardyMemory(memory, challengeId) {
  const raw = memory?.[challengeId] ?? {};
  const attempts = Number(raw.attempts ?? 0);
  const lastAttemptRound = Number(raw.lastAttemptRound ?? 0);
  if (
    !Number.isSafeInteger(attempts) ||
    attempts < 0 ||
    !Number.isSafeInteger(lastAttemptRound) ||
    lastAttemptRound < 0
  ) {
    throw new TypeError("Jeopardy memory counters must be non-negative integers");
  }
  return {
    solved: raw.solved === true,
    viewed: raw.viewed === true,
    downloaded: raw.downloaded === true,
    containerCreated: raw.containerCreated === true,
    containerReady: raw.containerReady === true,
    attempts,
    lastAttemptRound,
  };
}

/**
 * Choose one live Jeopardy action from public challenge metadata and this
 * player's local history. No solve count, challenge subset, winner, or unlock
 * timestamp is assigned by the orchestrator. The deterministic roll makes a
 * run replayable while budgets, attendance, prior attempts, and elapsed play
 * decide which outcomes are actually reached.
 */
export function jeopardyIntent(profile, challengeCatalog, memory, context) {
  if (!profile || typeof profile !== "object" || !context || typeof context !== "object") {
    throw new TypeError("Jeopardy intent requires a player profile and context");
  }
  for (const field of ["jeopardy", "risk", "exploration", "persistence"]) {
    if (!Number.isFinite(profile[field]) || profile[field] < 0 || profile[field] > 1) {
      throw new TypeError(`profile.${field} must be a finite rate`);
    }
  }
  if (typeof profile.seed !== "string" || profile.seed.length === 0) {
    throw new TypeError("profile.seed is required");
  }
  const round = Number(context.round);
  const progress = Number(context.progress);
  const availableCredits = Number(context.availableCredits);
  if (
    !Number.isSafeInteger(round) ||
    round < 1 ||
    !Number.isFinite(progress) ||
    progress < 0 ||
    progress > 1 ||
    !Number.isSafeInteger(availableCredits) ||
    availableCredits < 0
  ) {
    throw new TypeError("Jeopardy context contains invalid round, progress, or credits");
  }

  if (context.actedThisRound === true) {
    return Object.freeze({
      action: "wait",
      reason: "already-acted-this-round",
      challengeId: null,
      kind: null,
      cost: 0,
    });
  }
  if (!isJeopardyFocusRound(profile, round)) {
    return Object.freeze({
      action: "wait",
      reason: "focused-on-another-domain",
      challengeId: null,
      kind: null,
      cost: 0,
    });
  }

  const catalog = normalizedChallengeCatalog(challengeCatalog);
  const ranked = catalog
    .map((challenge) => ({
      challenge,
      state: normalizedJeopardyMemory(memory, challenge.challengeId),
    }))
    .filter(({ challenge, state }) => {
      const attemptLimit =
        1 + Math.floor(1 + profile.persistence * 2.4 + profile.jeopardy * 1.4);
      return (
        !state.solved &&
        state.attempts < attemptLimit &&
        jeopardyKindInterest(profile, challenge)
      );
    })
    .map((candidate) => ({
      ...candidate,
      priority:
        challengePreference(profile, candidate.challenge) +
        keyedUnit(
          profile.seed,
          "jeopardy-live-priority",
          round,
          candidate.challenge.challengeId,
        ) *
          0.14 -
        candidate.state.attempts * 0.045,
    }))
    .sort(
      (left, right) =>
        right.priority - left.priority ||
        left.challenge.challengeId - right.challenge.challengeId,
    );

  for (const { challenge, state } of ranked) {
    const result = (action, reason, extra = {}) =>
      Object.freeze({
        action,
        reason,
        challengeId: challenge.challengeId,
        kind: challenge.kind,
        cost: 0,
        ...extra,
      });
    if (!state.viewed) return result("view", "unread");
    if (challenge.kind === "attachment" && !state.downloaded) {
      if (availableCredits >= PLAYER_ACTION_COSTS.attachmentSolve) {
        return result("download", "attachment-preparation", {
          cost: PLAYER_ACTION_COSTS.attachmentSolve,
        });
      }
      continue;
    }
    if (challenge.kind === "container" && !state.containerCreated) {
      if (availableCredits >= PLAYER_ACTION_COSTS.containerSolve) {
        return result("createContainer", "container-preparation", {
          cost: PLAYER_ACTION_COSTS.containerSolve,
        });
      }
      continue;
    }
    if (challenge.kind === "container" && !state.containerReady) continue;
    if (state.lastAttemptRound === round || availableCredits < PLAYER_ACTION_COSTS.staticSolve) {
      continue;
    }

    const kindPenalty = challenge.kind === "container" ? 0.07 : challenge.kind === "attachment" ? 0.03 : 0;
    const solveChance = clamp(
      0.07 +
        profile.jeopardy * 0.54 +
        progress * 0.24 +
        state.attempts * 0.03 -
        challenge.difficulty * 0.05 -
        kindPenalty,
      0.03,
      0.88,
    );
    const discovered =
      keyedUnit(
        profile.seed,
        "jeopardy-discovery",
        challenge.challengeId,
        round,
        state.attempts,
      ) < solveChance;
    if (discovered) {
      return result("submitCorrect", "solution-discovered", {
        cost: PLAYER_ACTION_COSTS.staticSolve,
        solveChance,
      });
    }
    const guessChance = clamp(
      0.04 + profile.risk * 0.38 + (1 - profile.jeopardy) * 0.12,
      0.04,
      0.5,
    );
    const guesses =
      keyedUnit(
        profile.seed,
        "jeopardy-guess",
        challenge.challengeId,
        round,
        state.attempts,
      ) < guessChance;
    return result(guesses ? "submitWrong" : "research", "attempt-incomplete", {
      cost: PLAYER_ACTION_COSTS.staticSolve,
      solveChance,
    });
  }

  return Object.freeze({
    action: "wait",
    reason: ranked.length === 0 ? "no-interest" : "no-actionable-challenge",
    challengeId: null,
    kind: null,
    cost: 0,
  });
}

export function kothTakeoverTechnique(profile, state, minimumTechnique = 1) {
  const cycleNumber = Number(state?.cycleNumber);
  const cycleTick = Number(state?.cycleTick);
  const minimum = Number(minimumTechnique);
  if (
    typeof profile?.seed !== "string" ||
    !Number.isFinite(profile.koth) ||
    profile.koth < 0 ||
    profile.koth > 1 ||
    !Number.isSafeInteger(cycleNumber) ||
    cycleNumber < 1 ||
    !Number.isSafeInteger(cycleTick) ||
    cycleTick < 1 ||
    !Number.isSafeInteger(minimum) ||
    minimum < 1 ||
    minimum > 3
  ) {
    throw new TypeError("KotH takeover technique requires a valid profile, cycle, and floor");
  }
  const skillLevel = profile.koth >= 0.72 ? 3 : profile.koth >= 0.42 ? 2 : 1;
  const variation = keyedUnit(profile.seed, "koth-technique", cycleNumber, cycleTick) < 0.2 ? -1 : 0;
  return clamp(Math.max(minimum, skillLevel + variation), 1, 3);
}

function nullableParticipationId(value, label) {
  if (value === null) return null;
  const participationId = Number(value);
  if (!Number.isSafeInteger(participationId) || participationId < 1) {
    throw new TypeError(`${label} must be null or a positive integer`);
  }
  return participationId;
}

export function kothControllerParticipationId(state) {
  if (!state || typeof state !== "object") {
    throw new TypeError("KotH state is required to identify its controller");
  }
  const holderParticipationId = nullableParticipationId(
    state.holderParticipationId,
    "holderParticipationId",
  );
  const provisionalParticipationId = nullableParticipationId(
    state.provisionalClaimantParticipationId,
    "provisionalClaimantParticipationId",
  );
  // The provisional token is already the marker in the shared container, so
  // it is the physical controller until confirmation promotes it to holder.
  return provisionalParticipationId ?? holderParticipationId;
}

export function kothPatchIntent(profile, state, context) {
  if (!state || typeof state !== "object" || !context || typeof context !== "object") {
    throw new TypeError("KotH patch intent requires state and context objects");
  }
  const cycleNumber = Number(state.cycleNumber);
  const cycleTick = Number(state.cycleTick);
  const cycleTicks = Number(state.cycleTicks);
  const ownParticipationId = Number(context.ownParticipationId);
  const availableCredits = Number(context.availableCredits);
  if (
    typeof profile?.seed !== "string" ||
    !Number.isFinite(profile.koth) ||
    !Number.isFinite(profile.persistence) ||
    !Number.isFinite(profile.risk) ||
    !Number.isSafeInteger(cycleNumber) ||
    cycleNumber < 0 ||
    !Number.isSafeInteger(cycleTick) ||
    cycleTick < 0 ||
    !Number.isSafeInteger(cycleTicks) ||
    cycleTicks < 1 ||
    cycleTick > cycleTicks ||
    !Number.isSafeInteger(ownParticipationId) ||
    ownParticipationId < 1 ||
    !Number.isSafeInteger(availableCredits) ||
    availableCredits < 0
  ) {
    throw new TypeError("KotH patch intent context is malformed");
  }
  const controllerParticipationId = kothControllerParticipationId(state);
  const result = (attempt, reason, level = 0, incident = "healthy") =>
    Object.freeze({ attempt, reason, level, incident });
  if (
    context.active !== true ||
    state.resetPhase !== "Active" ||
    state.isScorable !== true ||
    state.eligibleNow !== true ||
    cycleNumber < 1 ||
    cycleTick < 1
  ) {
    return result(false, "ineligible");
  }
  if (controllerParticipationId !== ownParticipationId) {
    return result(false, "not-current-controller");
  }
  if (Number(context.patchedCycleNumber) === cycleNumber) {
    return result(false, "already-patched-cycle");
  }
  // A final-tick patch would be destroyed at the boundary before another
  // authoritative scoring observation could prove a healthy defended hold.
  if (cycleTick >= cycleTicks) return result(false, "no-hold-window");
  if (availableCredits < PLAYER_ACTION_COSTS.patch) return result(false, "no-action-credit");
  const patchChance = clamp(0.48 + profile.koth * 0.32 + profile.persistence * 0.14, 0.5, 0.92);
  if (keyedUnit(profile.seed, "koth-patch", cycleNumber) >= patchChance) {
    return result(false, "declined");
  }
  const level = profile.koth >= 0.6 ? 2 : 1;
  const incidentRoll = keyedUnit(profile.seed, "koth-patch-incident", cycleNumber);
  const offlineChance = 0.025 + (1 - profile.koth) * 0.055 + profile.risk * 0.02;
  const mumbleChance = 0.055 + (1 - profile.koth) * 0.08;
  const incident =
    incidentRoll < offlineChance
      ? "offline"
      : incidentRoll < offlineChance + mumbleChance
        ? "mumble"
        : "healthy";
  return result(true, "patch", level, incident);
}

const KOTH_SERVICE_STATUS_PATTERN =
  /^instance=([a-f0-9]{16});patch=([0-2]);state=(healthy|mumble|offline)$/;

export function parseKothServiceStatus(body, instanceHeader) {
  if (typeof body !== "string" || typeof instanceHeader !== "string") return null;
  const match = body.trim().match(KOTH_SERVICE_STATUS_PATTERN);
  const header = instanceHeader.trim();
  if (!match || header.length === 0 || header !== match[1]) return null;
  return Object.freeze({
    instance: match[1],
    patchLevel: Number(match[2]),
    state: match[3],
  });
}

export function isReplacementKothInstance(previousInstance, status) {
  return (
    typeof previousInstance === "string" &&
    previousInstance.length > 0 &&
    status !== null &&
    typeof status === "object" &&
    typeof status.instance === "string" &&
    status.instance.length > 0 &&
    status.instance !== previousInstance
  );
}

export function isPristineKothReplacement(previousInstance, status) {
  return (
    isReplacementKothInstance(previousInstance, status) &&
    status.patchLevel === 0 &&
    status.state === "healthy"
  );
}

export function kothHealthyHoldStatusMatches(patch, status) {
  return (
    patch !== null &&
    typeof patch === "object" &&
    typeof patch.instance === "string" &&
    patch.instance.length > 0 &&
    Number.isSafeInteger(patch.level) &&
    patch.level >= 1 &&
    patch.level <= 2 &&
    patch.controlInterrupted === false &&
    status !== null &&
    typeof status === "object" &&
    status.instance === patch.instance &&
    Number.isSafeInteger(status.patchLevel) &&
    status.patchLevel >= patch.level &&
    status.patchLevel <= 2 &&
    status.state === "healthy"
  );
}

export function kothPatchRepairReady(patchedAtRound, currentRound) {
  const patched = Number(patchedAtRound);
  const current = Number(currentRound);
  if (
    !Number.isSafeInteger(patched) ||
    patched < 0 ||
    !Number.isSafeInteger(current) ||
    current < 0
  ) {
    throw new TypeError("KotH patch repair rounds must be non-negative integers");
  }
  return current > patched;
}

export function kothIntent(profile, state, context) {
  if (!state || typeof state !== "object" || !context || typeof context !== "object") {
    throw new TypeError("KotH intent requires state and context objects");
  }
  const cycleNumber = Number(state.cycleNumber);
  const cycleTick = Number(state.cycleTick);
  const cycleTicks = Number(state.cycleTicks ?? 3);
  const observationsInTick = Number(context.observationsInTick ?? 1);
  if (
    !Number.isSafeInteger(cycleNumber) ||
    cycleNumber < 1 ||
    !Number.isSafeInteger(cycleTick) ||
    cycleTick < 1 ||
    !Number.isSafeInteger(cycleTicks) ||
    cycleTicks < 1 ||
    !Number.isSafeInteger(observationsInTick) ||
    observationsInTick < 1
  ) {
    throw new TypeError("KotH cycle and observation values must be positive integers");
  }
  const competitionSeed = String(context.competitionSeed || "").trim();
  if (!competitionSeed) throw new TypeError("competitionSeed is required");
  const ownParticipationId = Number(context.ownParticipationId);
  const claimantId = kothControllerParticipationId(state);
  const phase = claimantId === null ? "opening" : "takeover";
  const technique = kothTakeoverTechnique(
    profile,
    state,
    Number(context.minimumTechnique ?? 1),
  );
  // The hill is last-write-wins inside a scoring tick. Skilled players do not
  // blindly fire on their first poll: they watch the live marker and time a
  // bounded write closer to the checker boundary. The keyed jitter keeps the
  // decision independent and reproducible without selecting a winner.
  const reactionTiming = clamp(
    profile.koth * 0.6 +
      keyedUnit(profile.seed, "koth-reaction", cycleNumber, cycleTick) * 0.4,
    0,
    0.999_999,
  );
  const reactionObservation =
    1 + Math.floor(reactionTiming * 3);
  const result = (attempt, reason, wantsToAttempt = false) =>
    Object.freeze({
      attempt,
      wantsToAttempt,
      reason,
      phase,
      technique,
      reactionObservation,
    });

  if (context.active !== true) return result(false, "inactive");
  if (
    state.resetPhase !== "Active" ||
    state.isScorable !== true ||
    state.eligibleNow !== true
  ) {
    return result(false, "ineligible");
  }
  if (context.attempted === true) return result(false, "already-attempted");
  if (Number(context.availableCredits ?? 1) < PLAYER_ACTION_COSTS.kothClaim) {
    return result(false, "no-action-credit");
  }
  if (claimantId !== null && claimantId === ownParticipationId) {
    return result(false, "already-controlling");
  }
  if (cycleTick >= cycleTicks) return result(false, "confirmation-window");

  const teamCount = Math.max(2, Number(context.teamCount) || 2);
  const rank = clamp(Number(context.scoreboardRank) || teamCount, 1, teamCount);
  const trailing = (rank - 1) / Math.max(1, teamCount - 1);
  const claimObserved = claimantId !== null;
  const lateTick = cycleTick > 1;
  const kothSkillSquared = profile.koth * profile.koth;
  const kothSkillFourth = kothSkillSquared * kothSkillSquared;
  // Once a provisional claim is visible, most players independently conserve
  // their remaining credits rather than take a low-value late shot. Enough
  // capable rivals still test the new controller's defense. A nonlinear skill
  // term makes KotH expertise affect actual late contests instead of being
  // drowned out by the 80 non-specialists in a 100-team field.
  const chance = claimObserved && lateTick
    ? clamp(
        0.002 +
          kothSkillFourth * 0.03 +
          profile.risk * 0.003 +
          profile.persistence * 0.003 +
          trailing * 0.003,
        0.001,
        0.045,
      )
    : clamp(
        (lateTick ? 0.005 : 0.05) +
          kothSkillSquared * (lateTick ? 0.045 : 0.32) +
          profile.risk * (lateTick ? 0.006 : 0.04) +
          profile.persistence * (lateTick ? 0.005 : 0.03) +
          trailing * (lateTick ? 0.004 : 0.03),
        lateTick ? 0.005 : 0.03,
        lateTick ? 0.075 : 0.5,
      );
  const wantsToAttempt =
    keyedUnit(
      profile.seed,
      "koth-intent",
      competitionSeed,
      cycleNumber,
      cycleTick,
      claimantId ?? "open",
    ) < chance;
  if (!wantsToAttempt) return result(false, "declined");
  if (observationsInTick < reactionObservation) {
    return result(false, "waiting-to-react", true);
  }
  return result(true, "challenge", true);
}

export function classifyKothCaptureOutcome({
  successful,
  resetRace,
  captureWindowClosed,
  stateAvailable,
  eligibleNow,
}) {
  for (const [name, value] of Object.entries({
    successful,
    resetRace,
    captureWindowClosed,
    stateAvailable,
    eligibleNow,
  })) {
    if (typeof value !== "boolean") {
      throw new TypeError(`${name} must be boolean`);
    }
  }
  if (successful) return "success";
  if (resetRace) return "resetRace";
  if (captureWindowClosed) return "windowClosed";
  if (!stateAvailable) return "stateUnavailable";
  if (!eligibleNow) return "ineligibleTransition";
  return "pending";
}

export function classifyKothPendingTransition(pending, state) {
  if (
    !pending ||
    typeof pending !== "object" ||
    !Number.isSafeInteger(pending.cycleNumber) ||
    pending.cycleNumber < 1 ||
    !Number.isSafeInteger(pending.cycleTick) ||
    pending.cycleTick < 1
  ) {
    throw new TypeError(
      "pending capture must have positive cycleNumber and cycleTick values",
    );
  }
  if (state === null) return "stateUnavailable";
  if (
    !state ||
    typeof state !== "object" ||
    !Number.isSafeInteger(state.cycleNumber) ||
    state.cycleNumber < 0 ||
    !Number.isSafeInteger(state.cycleTick) ||
    state.cycleTick < 0 ||
    typeof state.resetPhase !== "string" ||
    typeof state.isScorable !== "boolean" ||
    typeof state.eligibleNow !== "boolean"
  ) {
    throw new TypeError("state must be null or a valid authoritative KotH state");
  }

  const resetRace =
    state.cycleNumber !== pending.cycleNumber ||
    state.resetPhase !== "Active" ||
    !state.isScorable;
  const captureWindowClosed =
    state.cycleNumber === pending.cycleNumber &&
    state.cycleTick !== pending.cycleTick;
  return classifyKothCaptureOutcome({
    successful: false,
    resetRace,
    captureWindowClosed,
    stateAvailable: true,
    eligibleNow: state.eligibleNow,
  });
}

export function isKothTerminalWindow(pending, outcome) {
  if (
    !pending ||
    typeof pending !== "object" ||
    typeof pending.burstExhausted !== "boolean"
  ) {
    throw new TypeError("pending capture must include a burstExhausted boolean");
  }
  if (
    ![
      "pending",
      "resetRace",
      "windowClosed",
      "stateUnavailable",
      "ineligibleTransition",
    ].includes(outcome)
  ) {
    throw new TypeError("outcome must be a pending capture transition");
  }
  return pending.burstExhausted && outcome === "windowClosed";
}

export function kothCapturePendingBalance({
  started,
  recovered,
  resetRaces,
  windowClosed,
  ineligibleTransitions,
  invariantFailures,
  terminalWindows,
}) {
  const counts = {
    started,
    recovered,
    resetRaces,
    windowClosed,
    ineligibleTransitions,
    invariantFailures,
    terminalWindows,
  };
  for (const [name, value] of Object.entries(counts)) {
    if (!Number.isSafeInteger(value) || value < 0) {
      throw new TypeError(`${name} must be a non-negative safe integer`);
    }
  }
  const resolved =
    recovered +
    resetRaces +
    windowClosed +
    ineligibleTransitions +
    invariantFailures;
  const unresolved = started - resolved;
  return {
    started,
    resolved,
    unresolved,
    valid: unresolved === 0 && terminalWindows <= windowClosed,
  };
}

export function kothCaptureStatusBalance({
  attemptFailures,
  networkErrors,
  http4xx,
  http5xx,
  otherStatusFailures,
}) {
  const counts = {
    attemptFailures,
    networkErrors,
    http4xx,
    http5xx,
    otherStatusFailures,
  };
  for (const [name, value] of Object.entries(counts)) {
    if (!Number.isSafeInteger(value) || value < 0) {
      throw new TypeError(`${name} must be a non-negative safe integer`);
    }
  }
  const classified = networkErrors + http4xx + http5xx + otherStatusFailures;
  return {
    attemptFailures,
    classified,
    valid: attemptFailures === classified,
  };
}

export const ENGAGEMENT_TIERS = Object.freeze(
  ENGAGEMENT_SPECS.map(({ engagementTier }) => engagementTier),
);
export const PLAYER_SPECIALTIES = Object.freeze(
  SPECIALTY_SPECS.map(({ specialty }) => specialty),
);
