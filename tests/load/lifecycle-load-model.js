function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1) {
    throw new Error(`${label} must be a positive integer`);
  }
  return parsed;
}

export function shouldValidateSemanticResponse(status) {
  return Number(status) !== 429;
}

export function reserveLifecycleContainerUsers(userIds, reservedCount) {
  if (!Array.isArray(userIds) || userIds.length === 0) {
    throw new Error("lifecycle Jeopardy users must be a non-empty array");
  }
  const users = userIds.map((value, index) => {
    if (typeof value !== "string" || value.length === 0) {
      throw new Error(`missing lifecycle Jeopardy user at slot ${index}`);
    }
    return value;
  });
  if (new Set(users).size !== users.length) {
    throw new Error("lifecycle Jeopardy users must be distinct");
  }

  const reserved = Number(reservedCount);
  if (!Number.isSafeInteger(reserved) || reserved < 0) {
    throw new Error("container lifecycle user count must be a non-negative integer");
  }
  if (reserved >= users.length) {
    throw new Error(
      "container lifecycle users must leave at least one Jeopardy player identity",
    );
  }

  const splitAt = users.length - reserved;
  return Object.freeze({
    playerUsers: Object.freeze(users.slice(0, splitAt)),
    containerUsers: Object.freeze(users.slice(splitAt)),
  });
}

export function retryAfterDelaySeconds(
  value,
  fallbackSeconds = 1,
  maximumSeconds = 60,
  nowMs = Date.now(),
) {
  const fallback = Number(fallbackSeconds);
  const maximum = Number(maximumSeconds);
  const now = Number(nowMs);
  if (!Number.isFinite(fallback) || fallback <= 0) {
    throw new Error("Retry-After fallback must be a positive number");
  }
  if (!Number.isFinite(maximum) || maximum < fallback) {
    throw new Error("Retry-After maximum must be at least the fallback");
  }
  if (!Number.isFinite(now)) {
    throw new Error("Retry-After clock must be finite");
  }

  const raw = Array.isArray(value) ? value[0] : value;
  const text = typeof raw === "string" ? raw.trim() : String(raw ?? "").trim();
  let seconds = fallback;
  if (/^\d+(?:\.\d+)?$/.test(text)) {
    seconds = Number(text);
  } else if (text.length > 0) {
    const retryAt = Date.parse(text);
    if (Number.isFinite(retryAt)) seconds = Math.max(0, (retryAt - now) / 1_000);
  }

  // Avoid a tight retry loop when a server legitimately returns zero seconds.
  return Math.min(maximum, Math.max(0.1, seconds));
}

export function lifecycleFleetSlot(iterationInInstance, fleetSize) {
  const iteration = Number(iterationInInstance);
  const size = positiveInteger(fleetSize, "lifecycle fleet size");
  if (!Number.isSafeInteger(iteration) || iteration < 0) {
    throw new Error(
      "lifecycle scenario iteration must be a non-negative integer",
    );
  }
  return iteration % size;
}

export function lifecycleFleetSlots(workerCount, fleetSize) {
  const workers = Number(workerCount);
  const size = positiveInteger(fleetSize, "lifecycle fleet size");
  if (!Number.isSafeInteger(workers) || workers < 0) {
    throw new Error("lifecycle worker count must be a non-negative integer");
  }
  return Object.freeze(
    Array.from({ length: workers }, (_, index) => index % size),
  );
}

export function lifecycleFleetIp(index) {
  const slot = Number(index);
  if (!Number.isSafeInteger(slot) || slot < 0 || slot >= 64_516) {
    throw new Error("lifecycle fleet index must be an integer below 64516");
  }
  return `10.240.${Math.floor(slot / 254)}.${(slot % 254) + 1}`;
}

export function selectKothCapacityClaimant(
  fleetParticipationIds,
  cooldownParticipationIds,
  cycleNumber,
) {
  if (!Array.isArray(fleetParticipationIds) || fleetParticipationIds.length === 0) {
    throw new Error("KotH capacity claimant selection requires a non-empty fleet");
  }
  const fleet = fleetParticipationIds.map((value) =>
    positiveInteger(value, "KotH fleet participation id"),
  );
  if (new Set(fleet).size !== fleet.length) {
    throw new Error("KotH fleet participation ids must be distinct");
  }
  const cooldowns = new Set(
    [...cooldownParticipationIds].map((value) =>
      positiveInteger(value, "KotH cooldown participation id"),
    ),
  );
  const cycle = positiveInteger(cycleNumber, "KotH cycle number");
  const eligible = fleet.filter((participationId) => !cooldowns.has(participationId));
  return eligible.length === 0 ? null : eligible[(cycle - 1) % eligible.length];
}

export function buildLifecycleFleet(state, requestedSize) {
  const size = positiveInteger(requestedSize, "lifecycle fleet size");
  const users = Array.isArray(state?.adUsers) ? state.adUsers : [];
  const participations = Array.isArray(state?.adPartIds) ? state.adPartIds : [];
  const flags = Array.isArray(state?.plantedFlags) ? state.plantedFlags : [];
  if (users.length < size || participations.length < size) {
    throw new Error(
      `lifecycle state has ${users.length} users and ${participations.length} participations for a ${size}-team fleet`,
    );
  }

  const flagByParticipation = new Map();
  for (const planted of flags) {
    const participationId = positiveInteger(
      planted?.pid,
      "planted-flag participation id",
    );
    if (flagByParticipation.has(participationId)) {
      throw new Error(
        `duplicate planted flag for participation ${participationId}`,
      );
    }
    if (typeof planted?.flag !== "string" || planted.flag.length === 0) {
      throw new Error(
        `missing planted flag for participation ${participationId}`,
      );
    }
    flagByParticipation.set(participationId, planted.flag);
  }

  const fleetParticipationIds = participations
    .slice(0, size)
    .map((value) => positiveInteger(value, "fleet participation id"));
  if (new Set(fleetParticipationIds).size !== size) {
    throw new Error("lifecycle fleet participation ids must be distinct");
  }

  return Object.freeze(
    fleetParticipationIds.map((participationId, index) => {
      const userId = users[index];
      if (typeof userId !== "string" || userId.length === 0) {
        throw new Error(`missing lifecycle user for fleet slot ${index}`);
      }
      const victimParticipationId = fleetParticipationIds[(index + 1) % size];
      const victimFlag = flagByParticipation.get(victimParticipationId);
      if (!victimFlag) {
        throw new Error(
          `missing planted flag for fleet victim ${victimParticipationId}`,
        );
      }
      return Object.freeze({
        index,
        participationId,
        userId,
        victimParticipationId,
        victimFlag,
      });
    }),
  );
}
