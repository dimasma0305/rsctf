// Pure contracts for the focused two-service / two-hill acceptance run.
// Keeping validation here makes the destructive orchestrator independently
// testable without PostgreSQL, Docker, credentials, or a running RSCTF stack.

export const multiDomainOwner = 'multi-domain-acceptance-v1';

function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer (got ${value})`);
  }
  return parsed;
}

function exactIds(values, label, expectedCount = 2) {
  if (!Array.isArray(values) || values.length !== expectedCount) {
    throw new Error(`${label} must contain exactly ${expectedCount} entries`);
  }
  const ids = values.map((value) => positiveInteger(value, label));
  if (new Set(ids).size !== ids.length) throw new Error(`${label} must be distinct`);
  return ids;
}

export function normalizeMultiDomainScope(runId, gameId, challengeIds) {
  const run = String(runId || '').trim();
  if (!/^[a-z0-9][a-z0-9-]{0,31}$/.test(run)) {
    throw new Error('multi-domain run id must be a lowercase DNS-safe identifier');
  }
  return Object.freeze({
    runId: run,
    gameId: positiveInteger(gameId, 'multi-domain game id'),
    challengeIds: Object.freeze(exactIds(challengeIds, 'multi-domain challenge ids', 4)),
  });
}

export function multiDomainLabels(scope, challengeId, role) {
  const challenge = positiveInteger(challengeId, 'multi-domain resource challenge id');
  if (!scope?.challengeIds?.includes(challenge)) {
    throw new Error(`challenge ${challenge} is outside the multi-domain scope`);
  }
  if (!['ad-service', 'koth-bootstrap'].includes(role)) {
    throw new Error(`unsupported multi-domain resource role ${role}`);
  }
  return Object.freeze({
    'rsctf.load.multi-domain.owner': multiDomainOwner,
    'rsctf.load.multi-domain.run': scope.runId,
    'rsctf.load.multi-domain.game': String(scope.gameId),
    'rsctf.load.multi-domain.challenge': String(challenge),
    'rsctf.load.multi-domain.role': role,
  });
}

export function ownsMultiDomainResource(labels, scope, challengeId, role) {
  if (!labels || typeof labels !== 'object') return false;
  const expected = multiDomainLabels(scope, challengeId, role);
  return Object.entries(expected).every(([key, value]) => labels[key] === value);
}

function pairKey(challengeId, participationId) {
  return `${challengeId}:${participationId}`;
}

export function validateAdServiceMatrix(rows, challengeIds, participationIds) {
  const challenges = exactIds(challengeIds, 'A&D challenge ids');
  const participations = exactIds(participationIds, 'A&D participation ids');
  if (!Array.isArray(rows)) throw new Error('A&D service matrix must be an array');
  const expected = new Set(challenges.flatMap((challenge) =>
    participations.map((participation) => pairKey(challenge, participation))));
  const observed = new Set();
  const endpoints = new Set();
  for (const row of rows) {
    const challenge = positiveInteger(row?.challengeId, 'A&D service challenge id');
    const participation = positiveInteger(row?.participationId, 'A&D service participation id');
    const key = pairKey(challenge, participation);
    if (!expected.has(key) || observed.has(key)) {
      throw new Error(`unexpected or duplicate A&D service ${key}`);
    }
    if (typeof row?.host !== 'string' || !row.host ||
        !Number.isSafeInteger(Number(row?.port)) || Number(row.port) < 1 || Number(row.port) > 65_535) {
      throw new Error(`A&D service ${key} has an invalid endpoint`);
    }
    const endpoint = `${row.host}:${Number(row.port)}`;
    if (endpoints.has(endpoint)) throw new Error(`A&D services share endpoint ${endpoint}`);
    endpoints.add(endpoint);
    observed.add(key);
  }
  if (observed.size !== expected.size || [...expected].some((key) => !observed.has(key))) {
    throw new Error(`A&D service matrix has ${observed.size}/${expected.size} exact cells`);
  }
  return Object.freeze({ cells: observed.size, uniqueEndpoints: endpoints.size });
}

export function validateManagedRuntimeOwnership(runtime, expectedId, expectedScope) {
  const identity = String(expectedId || '');
  const scope = String(expectedScope || '');
  if (!/^[a-f0-9]{64}$/.test(identity) || !/^[a-f0-9]{32}$/.test(scope)) {
    throw new Error('managed runtime proof requires canonical container and scope identities');
  }
  if (runtime?.Id !== identity || runtime?.State?.Running !== true) {
    throw new Error('managed runtime is absent, replaced, or stopped');
  }
  const labels = runtime?.Config?.Labels || {};
  if (labels['rsctf.managed'] !== scope || labels['rsctf.scope'] !== scope ||
      !/^[a-f0-9]{64}$/.test(labels['rsctf.launch-spec'] || '')) {
    throw new Error('managed runtime ownership labels are incomplete or cross-installation');
  }
  const hostBindings = runtime?.HostConfig?.PortBindings || {};
  const publishedBindings = runtime?.NetworkSettings?.Ports || {};
  if (Object.keys(hostBindings).length !== 0 ||
      Object.values(publishedBindings).some((bindings) => Array.isArray(bindings) && bindings.length > 0)) {
    throw new Error('managed competitive runtime published a host port');
  }
  return true;
}

export function validateFlagMatrix(rows, challengeIds, participationIds) {
  const challenges = exactIds(challengeIds, 'flag challenge ids');
  const participations = exactIds(participationIds, 'flag participation ids');
  if (!Array.isArray(rows)) throw new Error('flag matrix must be an array');
  const expected = new Set(challenges.flatMap((challenge) =>
    participations.map((participation) => pairKey(challenge, participation))));
  const observed = new Set();
  const flags = new Set();
  for (const row of rows) {
    const key = pairKey(
      positiveInteger(row?.challengeId, 'flag challenge id'),
      positiveInteger(row?.participationId, 'flag participation id'),
    );
    if (!expected.has(key) || observed.has(key)) throw new Error(`unexpected or duplicate flag cell ${key}`);
    if (typeof row?.flag !== 'string' || !/^flag\{[^\r\n]{1,240}\}$/.test(row.flag) || flags.has(row.flag)) {
      throw new Error(`flag cell ${key} is malformed or reused`);
    }
    observed.add(key);
    flags.add(row.flag);
  }
  if (observed.size !== expected.size || [...expected].some((key) => !observed.has(key))) {
    throw new Error(`flag matrix has ${observed.size}/${expected.size} exact cells`);
  }
  return Object.freeze({ cells: observed.size, uniqueFlags: flags.size });
}

export function validateRoundCardinality(snapshot, expectedServices = 4, expectedHills = 2) {
  const services = positiveInteger(expectedServices, 'expected round services');
  const hills = positiveInteger(expectedHills, 'expected round hills');
  const exact = {
    flagCount: services,
    flagServiceCount: services,
    checkCount: services,
    checkServiceCount: services,
    deliveryCount: services,
    deliveryServiceCount: services,
    kothResultCount: hills,
    kothResultHillCount: hills,
    cycleCount: hills,
    activeCycleCount: hills,
  };
  for (const [field, expected] of Object.entries(exact)) {
    if (Number(snapshot?.[field]) !== expected) {
      throw new Error(`round cardinality ${field}=${snapshot?.[field]}, expected ${expected}`);
    }
  }
  for (const field of ['duplicateFlags', 'duplicateChecks', 'duplicateDeliveries', 'duplicateKothResults', 'duplicateCycles', 'duplicateActiveCycles']) {
    if (Number(snapshot?.[field]) !== 0) {
      throw new Error(`round cardinality ${field}=${snapshot?.[field]}, expected zero`);
    }
  }
  return Object.freeze({ services, hills, duplicateKeys: 0 });
}

export function validateAdScoreboardReconciliation(model, challengeIds, participationIds) {
  const challenges = exactIds(challengeIds, 'scoreboard challenge ids');
  const participations = exactIds(participationIds, 'scoreboard participation ids');
  if (model?.started !== true || !Array.isArray(model?.challenges) || !Array.isArray(model?.teams)) {
    throw new Error('A&D scoreboard is not a started model');
  }
  const boardChallenges = model.challenges.map(({ challengeId }) => Number(challengeId));
  if (boardChallenges.length !== challenges.length ||
      challenges.some((challenge) => !boardChallenges.includes(challenge))) {
    throw new Error('A&D scoreboard challenge columns do not match the two-service field');
  }
  if (model.teams.length !== participations.length) throw new Error('A&D scoreboard team count is incomplete');
  for (const team of model.teams) {
    if (!participations.includes(Number(team?.participationId)) || !Array.isArray(team?.services) ||
        team.services.length !== challenges.length) {
      throw new Error('A&D scoreboard team row has an invalid service matrix');
    }
    const serviceChallenges = team.services.map(({ challengeId }) => Number(challengeId));
    if (new Set(serviceChallenges).size !== challenges.length ||
        challenges.some((challenge) => !serviceChallenges.includes(challenge))) {
      throw new Error('A&D scoreboard team row duplicated or omitted a challenge');
    }
    const settled = team.services.reduce((sum, service) => sum + Number(service?.settledPoints), 0);
    const projected = team.services.reduce((sum, service) => sum + Number(service?.projectedPoints), 0);
    if (![settled, projected, Number(team?.settledTotal), Number(team?.projectedTotal)].every(Number.isFinite) ||
        Math.abs(settled - Number(team.settledTotal)) >= 1e-6 ||
        Math.abs(projected - Number(team.projectedTotal)) >= 1e-6) {
      throw new Error('A&D scoreboard service totals do not reconcile with the team row');
    }
  }
  return Object.freeze({ teams: participations.length, servicesPerTeam: challenges.length, totalsReconciled: true });
}

export function validateAcceptedAttackAttribution(rows, challengeIds, victimParticipationId) {
  const challenges = exactIds(challengeIds, 'captured challenge ids');
  const victim = positiveInteger(victimParticipationId, 'captured victim participation id');
  if (!Array.isArray(rows) || rows.length !== challenges.length) {
    throw new Error(`expected ${challenges.length} accepted attack rows`);
  }
  const observed = rows.map((row) => {
    if (Number(row?.victimParticipationId) !== victim) {
      throw new Error('accepted attack crossed the exact victim service');
    }
    if (Number(row?.flagChallengeId) !== Number(row?.serviceChallengeId)) {
      throw new Error('accepted flag was attributed to another challenge service');
    }
    return positiveInteger(row.serviceChallengeId, 'accepted attack challenge id');
  });
  if (new Set(observed).size !== challenges.length ||
      challenges.some((challenge) => !observed.includes(challenge))) {
    throw new Error('accepted attacks did not cover both challenge identities exactly once');
  }
  return Object.freeze({ attacks: rows.length, challengeIds: Object.freeze([...observed].sort((a, b) => a - b)) });
}

export function validateKothCapabilityMatrix(rows, challengeIds, participationIds) {
  const challenges = exactIds(challengeIds, 'KotH challenge ids');
  const participations = exactIds(participationIds, 'KotH participation ids');
  if (!Array.isArray(rows)) throw new Error('KotH capability matrix must be an array');
  const expected = new Set(challenges.flatMap((challenge) =>
    participations.map((participation) => pairKey(challenge, participation))));
  const observed = new Set();
  const tokens = new Set();
  for (const row of rows) {
    const key = pairKey(
      positiveInteger(row?.challengeId, 'KotH token challenge id'),
      positiveInteger(row?.participationId, 'KotH token participation id'),
    );
    if (!expected.has(key) || observed.has(key)) throw new Error(`unexpected or duplicate KotH token ${key}`);
    if (typeof row?.token !== 'string' || !/^koth_[A-Za-z0-9_-]{8,128}$/.test(row.token) || tokens.has(row.token)) {
      throw new Error(`KotH token ${key} is malformed or reused across scopes`);
    }
    if (Number(row?.tokenChallengeId) !== Number(row?.challengeId) ||
        Number(row?.targetChallengeId) !== Number(row?.challengeId) ||
        Number(row?.cycleChallengeId) !== Number(row?.challengeId)) {
      throw new Error(`KotH token ${key} crossed target/cycle ownership`);
    }
    observed.add(key);
    tokens.add(row.token);
  }
  if (observed.size !== expected.size || [...expected].some((key) => !observed.has(key))) {
    throw new Error(`KotH capability matrix has ${observed.size}/${expected.size} exact cells`);
  }
  return Object.freeze({ cells: observed.size, uniqueTokens: tokens.size });
}

export function validateCrossHillRejection(result, scope) {
  const source = positiveInteger(scope?.sourceTokenId, 'source KotH token id');
  const sourceChallenge = positiveInteger(scope?.sourceChallengeId, 'source KotH challenge id');
  const destination = positiveInteger(
    scope?.destinationChallengeId,
    'destination KotH challenge id',
  );
  const game = positiveInteger(scope?.gameId, 'destination KotH game id');
  const cycle = positiveInteger(scope?.cycleId, 'destination KotH cycle id');
  const container = String(scope?.containerId || '').trim();
  const resetAttempt = Number(scope?.resetAttempt);
  if (sourceChallenge === destination) {
    throw new Error('cross-hill proof requires two different challenge identities');
  }
  if (!container || !Number.isSafeInteger(resetAttempt) || resetAttempt < 0) {
    throw new Error('destination KotH cycle scope is incomplete');
  }
  if (Number(result?.challengeId) !== destination || result?.markerObserved !== true) {
    throw new Error('cross-hill marker was not observed on the destination hill');
  }
  if (
    Number(result?.gameId) !== game ||
    Number(result?.roundGameId) !== game ||
    Number(result?.cycleId) !== cycle ||
    String(result?.containerId || '') !== container ||
    Number(result?.tokenWindowAttempt) !== resetAttempt
  ) {
    throw new Error('cross-hill result escaped the exact destination cycle scope');
  }
  const adRoundId = Number(result?.adRoundId);
  const roundNumber = Number(result?.roundNumber);
  const plannedStartRound = Number(result?.plannedStartRound);
  const plannedEndRound = Number(result?.plannedEndRound);
  if (
    !Number.isSafeInteger(adRoundId) || adRoundId <= 0 ||
    !Number.isSafeInteger(roundNumber) || roundNumber <= 0 ||
    !Number.isSafeInteger(plannedStartRound) || plannedStartRound <= 0 ||
    !Number.isSafeInteger(plannedEndRound) || plannedEndRound < plannedStartRound ||
    roundNumber < plannedStartRound || roundNumber > plannedEndRound
  ) {
    throw new Error('cross-hill result escaped the destination cycle round window');
  }
  if (Number(result?.status) !== 0 || result?.isScorable !== true) {
    throw new Error('cross-hill rejection was hidden by platform-attributed checker failure');
  }
  if (result?.tokenId != null || result?.controller != null || result?.responsible != null) {
    throw new Error('cross-hill capability was accepted by the destination hill');
  }
  if (Number(result?.sourceTokenMatches) !== source || Number(result?.destinationMatches) !== 0) {
    throw new Error('cross-hill token scope proof is inconsistent');
  }
  return Object.freeze({ rejected: true, resultId: positiveInteger(result.id, 'KotH control result id') });
}

export function validateFaultIsolation({ lowerBefore, higherBefore, failedResponseStatus, lowerAfter, higherAfter, lowerRecovered }) {
  if (Number(failedResponseStatus) < 400 || Number(failedResponseStatus) >= 500) {
    throw new Error('lower-hill fault must return a controlled 4xx recovery response');
  }
  if (lowerBefore?.phase !== 'Active' || higherBefore?.phase !== 'Active') {
    throw new Error('both hills must start Active');
  }
  if (lowerAfter?.phase !== 'ReadinessPending' || Number(lowerAfter?.readinessFailures) <= Number(lowerBefore?.readinessFailures)) {
    throw new Error('lower hill did not persist its isolated readiness failure');
  }
  if (
    higherAfter?.phase !== 'Active' ||
    Number(higherAfter?.updatedAtMs) <= Number(higherBefore?.updatedAtMs) ||
    Number(higherAfter?.readinessFailures) !== Number(higherBefore?.readinessFailures)
  ) {
    throw new Error('later hill was starved behind the lower-id failure');
  }
  if (lowerRecovered?.phase !== 'Active' || Number(lowerRecovered?.readinessFailures) < Number(lowerAfter?.readinessFailures)) {
    throw new Error('lower hill did not recover after its checker was restored');
  }
  return Object.freeze({ failIsolated: true, lowerRecovered: true });
}

export const multiDomainCleanupFields = Object.freeze([
  'games',
  'exactDatabaseRows',
  'runtimeContainers',
  'fixtureImages',
  'checkerDirectories',
]);

export function validateCleanupPasses(passes) {
  if (!Array.isArray(passes) || passes.length !== 2) throw new Error('cleanup requires exactly two snapshots');
  for (const [index, snapshot] of passes.entries()) {
    if (!snapshot || typeof snapshot !== 'object' || Array.isArray(snapshot)) {
      throw new Error(`cleanup snapshot ${index + 1} is invalid`);
    }
    const keys = Object.keys(snapshot);
    if (
      keys.length !== multiDomainCleanupFields.length ||
      multiDomainCleanupFields.some((field) => !Object.hasOwn(snapshot, field)) ||
      keys.some((field) => !multiDomainCleanupFields.includes(field))
    ) {
      throw new Error(
        `cleanup snapshot ${index + 1} must contain exactly ${multiDomainCleanupFields.join(', ')}`,
      );
    }
    for (const resource of multiDomainCleanupFields) {
      const count = snapshot[resource];
      if (!Number.isSafeInteger(count) || count !== 0) {
        throw new Error(`cleanup snapshot ${index + 1} retained ${resource}=${count}`);
      }
    }
  }
  const canonical = (snapshot) => multiDomainCleanupFields.map((field) => snapshot[field]);
  if (JSON.stringify(canonical(passes[0])) !== JSON.stringify(canonical(passes[1]))) {
    throw new Error('cleanup snapshots changed between delayed reads');
  }
  return true;
}
