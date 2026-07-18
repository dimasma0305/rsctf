const LABEL_PREFIX = "rsctf.load.lifecycle";

export const fleetLabelKeys = Object.freeze({
  owner: `${LABEL_PREFIX}.owner`,
  game: `${LABEL_PREFIX}.game`,
  challenge: `${LABEL_PREFIX}.challenge`,
  role: `${LABEL_PREFIX}.role`,
  participation: `${LABEL_PREFIX}.participation`,
});

export const fleetOwner = "byoc-fleet-v1";
export const teamClientOwner = "team-client-v2";

export const teamClientLabelKeys = Object.freeze({
  owner: `${LABEL_PREFIX}.owner`,
  game: `${LABEL_PREFIX}.game`,
  run: `${LABEL_PREFIX}.run`,
  role: `${LABEL_PREFIX}.role`,
  participation: `${LABEL_PREFIX}.participation`,
  index: `${LABEL_PREFIX}.index`,
});

function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer (got ${value})`);
  }
  return parsed;
}

export function normalizeFleetScope(gameId, challengeId = null, participationIds = []) {
  const game = positiveInteger(gameId, "fleet game id");
  const challenge = challengeId == null ? null : positiveInteger(challengeId, "fleet challenge id");
  if (!Array.isArray(participationIds)) throw new Error("fleet participation ids must be an array");
  const participations = participationIds.map((id) => positiveInteger(id, "fleet participation id"));
  if (new Set(participations).size !== participations.length) {
    throw new Error("fleet participation ids must be distinct");
  }
  return Object.freeze({ gameId: game, challengeId: challenge, participationIds: Object.freeze(participations) });
}

export function fleetLabels(scope, role, participationId = null) {
  if (!scope || !Number.isSafeInteger(scope.gameId) || scope.gameId <= 0) {
    throw new Error("fleet labels require a normalized game scope");
  }
  if (typeof role !== "string" || !/^[a-z][a-z-]{1,31}$/.test(role)) {
    throw new Error(`invalid fleet resource role ${role}`);
  }
  if (scope.challengeId == null) throw new Error("fleet resource labels require a challenge id");
  const labels = {
    [fleetLabelKeys.owner]: fleetOwner,
    [fleetLabelKeys.game]: String(scope.gameId),
    [fleetLabelKeys.challenge]: String(scope.challengeId),
    [fleetLabelKeys.role]: role,
  };
  if (participationId != null) {
    const pid = positiveInteger(participationId, "fleet resource participation id");
    if (scope.participationIds.length && !scope.participationIds.includes(pid)) {
      throw new Error(`participation ${pid} is outside the fleet ownership scope`);
    }
    labels[fleetLabelKeys.participation] = String(pid);
  }
  return Object.freeze(labels);
}

export function dockerLabelArgs(labels) {
  return Object.entries(labels).flatMap(([key, value]) => ["--label", `${key}=${value}`]);
}

export function dockerOwnershipFilterArgs(scope) {
  if (!scope || !Number.isSafeInteger(scope.gameId) || scope.gameId <= 0) {
    throw new Error("fleet discovery requires a normalized game scope");
  }
  const labels = {
    [fleetLabelKeys.owner]: fleetOwner,
    [fleetLabelKeys.game]: String(scope.gameId),
  };
  if (scope.challengeId != null) labels[fleetLabelKeys.challenge] = String(scope.challengeId);
  return Object.entries(labels).flatMap(([key, value]) => ["--filter", `label=${key}=${value}`]);
}

export function ownsFleetResource(labels, scope, role, participationId = null) {
  if (!labels || typeof labels !== "object") return false;
  const expected = fleetLabels(scope, role, participationId);
  return Object.entries(expected).every(([key, value]) => labels[key] === value);
}

export function fleetParticipantBindings(scope, resources) {
  if (!Array.isArray(resources)) throw new Error("fleet resources must be an array");
  const bindings = new Map();
  for (const resource of resources) {
    const labels = resource?.labels;
    const kind = resource?.kind || "resource";
    const gameId = Number(labels?.[fleetLabelKeys.game]);
    const challengeId = Number(labels?.[fleetLabelKeys.challenge]);
    const role = labels?.[fleetLabelKeys.role];
    const rawParticipationId = labels?.[fleetLabelKeys.participation];
    if (
      gameId !== scope.gameId ||
      !Number.isSafeInteger(challengeId) ||
      challengeId <= 0 ||
      (scope.challengeId != null && challengeId !== scope.challengeId)
    ) {
      throw new Error(`owned lifecycle ${kind} has malformed game/challenge labels`);
    }
    if (role === "shared-service" && rawParticipationId == null) continue;
    if (!["relay", "isolated-service", "flag-volume"].includes(role)) {
      throw new Error(`owned lifecycle ${kind} has an unsupported role label ${role}`);
    }
    const participationId = Number(rawParticipationId);
    if (!Number.isSafeInteger(participationId) || participationId <= 0) {
      throw new Error(`owned lifecycle ${kind} has a malformed participation label`);
    }
    bindings.set(`${challengeId}:${participationId}`, { challengeId, participationId });
  }
  return [...bindings.values()];
}

function normalizedRunId(value) {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(value)) {
    throw new Error(`team-client run id must be a safe 1-64 character identifier (got ${value})`);
  }
  return value;
}

export function normalizeTeamClientScope(gameId, runId, participationIds) {
  const game = positiveInteger(gameId, "team-client game id");
  const run = normalizedRunId(runId);
  if (!Array.isArray(participationIds) || participationIds.length < 2) {
    throw new Error("team-client participation ids must contain at least two entries");
  }
  const participations = participationIds.map((id) =>
    positiveInteger(id, "team-client participation id"),
  );
  if (new Set(participations).size !== participations.length) {
    throw new Error("team-client participation ids must be distinct");
  }
  return Object.freeze({
    gameId: game,
    runId: run,
    participationIds: Object.freeze(participations),
  });
}

export function teamClientLabels(scope, participationId, index) {
  if (!scope || !Number.isSafeInteger(scope.gameId) || scope.gameId <= 0) {
    throw new Error("team-client labels require a normalized scope");
  }
  const participation = positiveInteger(participationId, "team-client participation id");
  const position = Number(index);
  if (!Number.isSafeInteger(position) || position < 0 || position >= scope.participationIds.length) {
    throw new Error(`team-client index is outside the ownership scope (got ${index})`);
  }
  if (scope.participationIds[position] !== participation) {
    throw new Error(
      `team-client index ${position} is bound to participation ${scope.participationIds[position]}, not ${participation}`,
    );
  }
  return Object.freeze({
    [teamClientLabelKeys.owner]: teamClientOwner,
    [teamClientLabelKeys.game]: String(scope.gameId),
    [teamClientLabelKeys.run]: normalizedRunId(scope.runId),
    [teamClientLabelKeys.role]: "team-client",
    [teamClientLabelKeys.participation]: String(participation),
    [teamClientLabelKeys.index]: String(position),
  });
}

export function dockerTeamClientFilterArgs(scope) {
  if (!scope || !Number.isSafeInteger(scope.gameId) || scope.gameId <= 0) {
    throw new Error("team-client discovery requires a normalized scope");
  }
  const labels = {
    [teamClientLabelKeys.owner]: teamClientOwner,
    [teamClientLabelKeys.game]: String(scope.gameId),
    [teamClientLabelKeys.run]: normalizedRunId(scope.runId),
    [teamClientLabelKeys.role]: "team-client",
  };
  return Object.entries(labels).flatMap(([key, value]) => ["--filter", `label=${key}=${value}`]);
}

export function ownsTeamClient(labels, scope, participationId, index) {
  if (!labels || typeof labels !== "object") return false;
  const expected = teamClientLabels(scope, participationId, index);
  return Object.entries(expected).every(([key, value]) => labels[key] === value);
}

export function selectTeamClientOwnershipRecord(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  if (Object.prototype.hasOwnProperty.call(value, "teamClientOwnership")) {
    return value.teamClientOwnership;
  }
  if (Object.prototype.hasOwnProperty.call(value, "ownership")) {
    return value.ownership;
  }
  return value.owner === teamClientOwner ? value : null;
}

export function sameTeamClientScope(left, right) {
  if (!left || !right || typeof left !== "object" || typeof right !== "object") return false;
  return (
    left.gameId === right.gameId &&
    left.runId === right.runId &&
    Array.isArray(left.participationIds) &&
    Array.isArray(right.participationIds) &&
    left.participationIds.length === right.participationIds.length &&
    left.participationIds.every((id, index) => id === right.participationIds[index])
  );
}
