const MAX_ROSTER = 2_000;
const MIN_WAVES = 2;
const MAX_ACTIVE_FLEET = 128;
const DEFAULT_ADMISSION_PER_MINUTE = 30_000;

function positiveInteger(value, name) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new TypeError(`${name} must be a positive integer (got ${value})`);
  }
  return parsed;
}

export function managedKothSummaryMetric(summary, name, field) {
  const metric = summary?.metrics?.[name] || {};
  const values = metric.values || metric;
  const direct = Number(values[field]);
  if (Number.isFinite(direct)) return direct;
  if (field !== 'rate') return Number.NaN;

  // k6 2.x calls the aggregate for a Rate metric `value`, while k6 1.x
  // nested it under `values.rate`. Retained evidence must accept both shapes.
  const value = Number(values.value);
  if (Number.isFinite(value)) return value;
  const passes = Number(values.passes);
  const fails = Number(values.fails);
  const samples = passes + fails;
  return Number.isFinite(passes) && Number.isFinite(fails) && samples > 0
    ? passes / samples
    : Number.NaN;
}

export function managedKothLoadPlan({
  rosterSize = MAX_ROSTER,
  activeFleet = 64,
  rate = 100,
  vus = 128,
  durationSeconds = 20,
} = {}) {
  const roster = positiveInteger(rosterSize, 'managed KotH roster size');
  const active = positiveInteger(activeFleet, 'managed KotH active fleet');
  const arrivalsPerSecond = positiveInteger(rate, 'managed KotH arrival rate');
  const virtualUsers = positiveInteger(vus, 'managed KotH VUs');
  const seconds = positiveInteger(durationSeconds, 'managed KotH duration');
  if (roster !== MAX_ROSTER) {
    throw new RangeError(`managed KotH acceptance requires the exact ${MAX_ROSTER}-team roster`);
  }
  if (active < 2 || active > MAX_ACTIVE_FLEET || active >= roster) {
    throw new RangeError(`managed KotH active fleet must be between 2 and ${MAX_ACTIVE_FLEET}`);
  }
  if (arrivalsPerSecond > 2_000 || virtualUsers > 500 || seconds > 600) {
    throw new RangeError('managed KotH rate, VUs, and duration exceed the bounded acceptance limits');
  }
  const defaultAdmissionRefillPerSecond = DEFAULT_ADMISSION_PER_MINUTE / 60;
  if (arrivalsPerSecond > defaultAdmissionRefillPerSecond) {
    throw new RangeError('managed KotH valid rate exceeds the default capability-admission refill');
  }
  const scheduledIterations = arrivalsPerSecond * seconds;
  if (scheduledIterations !== roster) {
    throw new RangeError(
      `managed KotH valid phase must schedule exactly ${roster} plays (got ${scheduledIterations})`,
    );
  }
  return Object.freeze({
    rosterSize: roster,
    activeFleet: active,
    rate: arrivalsPerSecond,
    vus: virtualUsers,
    durationSeconds: seconds,
    scheduledIterations,
    minimumWaves: MIN_WAVES,
    defaultAdmissionPerMinute: DEFAULT_ADMISSION_PER_MINUTE,
    defaultAdmissionRefillPerSecond,
  });
}

export function managedKothAbusePlan({
  rate = 200,
  vus = 128,
  durationSeconds = 30,
  admissionPerMinute = 3_000,
} = {}) {
  const arrivalsPerSecond = positiveInteger(rate, 'managed KotH abuse arrival rate');
  const virtualUsers = positiveInteger(vus, 'managed KotH abuse VUs');
  const seconds = positiveInteger(durationSeconds, 'managed KotH abuse duration');
  const admission = positiveInteger(admissionPerMinute, 'managed KotH IP admission');
  if (admission < 3_000 || admission > 1_000_000) {
    throw new RangeError('managed KotH IP admission must be between 3000 and 1000000 per minute');
  }
  if (arrivalsPerSecond > 2_000 || virtualUsers > 500 || seconds > 600) {
    throw new RangeError('managed KotH abuse rate, VUs, and duration exceed bounded acceptance limits');
  }
  const scheduledIterations = arrivalsPerSecond * seconds;
  const maximumAdmittedFromFullBucket = admission + Math.ceil((admission * seconds) / 60);
  if (scheduledIterations <= maximumAdmittedFromFullBucket) {
    throw new RangeError(
      `managed KotH abuse phase cannot exhaust a full ${admission}/minute admission bucket`,
    );
  }
  return Object.freeze({
    rate: arrivalsPerSecond,
    vus: virtualUsers,
    durationSeconds: seconds,
    admissionPerMinute: admission,
    scheduledIterations,
    maximumAdmittedFromFullBucket,
  });
}

export function managedKothOperationCycleId(operation) {
  const match = String(operation || '').match(
    /^koth-cycle:(\d+):attempt:\d+(?::managed-reporter-v\d+:[0-9a-f]{16}:[0-9a-f]{32})?$/,
  );
  if (!match) return null;
  const cycleId = Number(match[1]);
  return Number.isSafeInteger(cycleId) && cycleId > 0 ? cycleId : null;
}

export function managedKothHarnessConfig(env = {}) {
  if (env.MANAGED_KOTH_STRESS_ACK !== '1' || env.MANAGED_KOTH_DISPOSABLE !== '1') {
    throw new Error('managed KotH load requires both destructive disposable acknowledgements');
  }
  let target;
  try {
    target = new URL(String(env.TARGET || ''));
  } catch {
    throw new Error('managed KotH TARGET must be an absolute loopback HTTP origin');
  }
  const loopback = ['127.0.0.1', 'localhost', '::1', '[::1]'].includes(target.hostname);
  if (
    !['http:', 'https:'].includes(target.protocol) ||
    !loopback ||
    target.username ||
    target.password ||
    target.pathname !== '/' ||
    target.search ||
    target.hash
  ) {
    throw new Error('managed KotH TARGET must be an absolute loopback HTTP origin');
  }
  for (const name of ['SUMMARY_JSON', 'RESOURCE_JSON']) {
    if (!String(env[name] || '').startsWith('/')) {
      throw new Error(`${name} must be an absolute retained artifact path`);
    }
  }
  return Object.freeze({
    target: target.origin,
    summaryPath: String(env.SUMMARY_JSON),
    resourcePath: String(env.RESOURCE_JSON),
  });
}

export function validateManagedReporterEnvironment(entries, {
  gameId,
  challengeId,
  platformUrl,
} = {}) {
  if (!Array.isArray(entries)) throw new Error('managed reporter environment is missing');
  const expectedNames = [
    'RSCTF_KOTH_GAME_ID',
    'RSCTF_KOTH_CHALLENGE_ID',
    'RSCTF_KOTH_PLATFORM_URL',
    'RSCTF_KOTH_CONTEXT_URL',
    'RSCTF_KOTH_OBSERVATION_URL',
    'RSCTF_KOTH_REPORTER_SECRET',
  ];
  const reporterEntries = entries.filter((entry) => String(entry).startsWith('RSCTF_KOTH_'));
  const values = new Map(reporterEntries.map((entry) => {
    const separator = String(entry).indexOf('=');
    return [String(entry).slice(0, separator), String(entry).slice(separator + 1)];
  }));
  if (
    reporterEntries.length !== expectedNames.length ||
    values.size !== expectedNames.length ||
    expectedNames.some((name) => !values.has(name))
  ) {
    throw new Error('managed target did not receive the exact six reporter variables');
  }
  const base = String(platformUrl || '').replace(/\/+$/, '');
  const scope = `${base}/api/v1/koth/games/${Number(gameId)}/challenges/${Number(challengeId)}`;
  if (
    values.get('RSCTF_KOTH_GAME_ID') !== String(gameId) ||
    values.get('RSCTF_KOTH_CHALLENGE_ID') !== String(challengeId) ||
    values.get('RSCTF_KOTH_PLATFORM_URL') !== base ||
    values.get('RSCTF_KOTH_CONTEXT_URL') !== `${scope}/context` ||
    values.get('RSCTF_KOTH_OBSERVATION_URL') !== `${scope}/observations` ||
    !/^koth_target_[A-Za-z0-9_-]{32,128}$/.test(values.get('RSCTF_KOTH_REPORTER_SECRET') || '')
  ) {
    throw new Error('managed target reporter environment is inconsistent');
  }
  return Object.freeze(Object.fromEntries(values));
}

export function validateManagedKothRecovery(before, after) {
  if (
    !before ||
    !after ||
    Number(after.cycleId) !== Number(before.cycleId) ||
    Number(after.resetAttempt) !== Number(before.resetAttempt) + 1 ||
    String(after.containerId || '') === String(before.containerId || '') ||
    String(after.credentialRevision || '') === String(before.credentialRevision || '') ||
    managedKothOperationCycleId(after.operation) !== Number(after.cycleId)
  ) {
    throw new Error(`managed KotH recovery identity failed: ${JSON.stringify({ before, after })}`);
  }
  return after;
}

export function validateManagedReporterStatus(model, expected) {
  if (!model || typeof model !== 'object' || Array.isArray(model)) {
    throw new Error('managed KotH reporter status must be an object');
  }
  const forbidden = Object.keys(model).filter((key) => /secret|token|credential/i.test(key));
  if (forbidden.length > 0) {
    throw new Error(`managed KotH reporter status exposes forbidden fields: ${forbidden.join(',')}`);
  }
  const integerFields = [
    'successfulReports',
    'submittedWaves',
    'contextRefreshes',
    'eligibleRoster',
    'uniqueAuthenticated',
    'uniqueActivePlayed',
    'invalidAuthentications',
    'lastRound',
  ];
  if (
    model.reporterConfigured !== true ||
    typeof model.reporterHealthy !== 'boolean' ||
    integerFields.some((field) => !Number.isSafeInteger(model[field]) || model[field] < 0) ||
    (model.lastContext !== null && !/^[0-9a-f]{64}$/.test(model.lastContext)) ||
    (model.lastError !== null && (
      typeof model.lastError !== 'string' ||
      model.lastError.length < 1 ||
      model.lastError.length > 300 ||
      /[\r\n]/.test(model.lastError)
    )) ||
    model.reporterHealthy !== (model.lastError === null)
  ) {
    throw new Error('managed KotH reporter status is malformed');
  }
  if (expected) {
    const plan = managedKothLoadPlan(expected);
    const minimumReports = positiveInteger(
      expected.minimumReports ?? plan.minimumWaves,
      'managed KotH minimum reporter reports',
    );
    const requireAbuse = expected.requireAbuse ?? false;
    if (!model.reporterHealthy) {
      throw new Error(`managed KotH reporter is unhealthy: ${model.lastError}`);
    }
    if (model.successfulReports < minimumReports || model.submittedWaves < minimumReports) {
      throw new Error(`managed KotH reporter submitted only ${model.submittedWaves} waves`);
    }
    if (model.uniqueAuthenticated !== plan.rosterSize) {
      throw new Error(
        `managed KotH reporter authenticated ${model.uniqueAuthenticated}/${plan.rosterSize} capabilities`,
      );
    }
    if (model.uniqueActivePlayed !== plan.activeFleet) {
      throw new Error(
        `managed KotH active fleet is ${model.uniqueActivePlayed}/${plan.activeFleet}`,
      );
    }
    if (requireAbuse && model.invalidAuthentications < 1) {
      throw new Error('managed KotH abuse path did not reject an invalid capability');
    }
  }
  return model;
}

export function validateManagedKothIntegrity(model, {
  rosterSize = MAX_ROSTER,
  activeFleet = 64,
  minimumScorableRounds = 1,
  minimumResetAttempts = 0,
} = {}) {
  const roster = positiveInteger(rosterSize, 'managed KotH roster size');
  const active = positiveInteger(activeFleet, 'managed KotH active fleet');
  const scorable = positiveInteger(minimumScorableRounds, 'managed KotH scorable rounds');
  if (!Number.isSafeInteger(minimumResetAttempts) || minimumResetAttempts < 0) {
    throw new TypeError('managed KotH reset-attempt minimum must be a non-negative integer');
  }
  const expectedDenseRows = Number(model?.scorableRounds) * roster;
  const expectedZeroRows = Number(model?.scorableRounds) * (roster - active);
  if (
    !model ||
    Number(model.rosterCount) !== roster ||
    Number(model.capabilityCount) !== roster ||
    Number(model.pendingRevocations) !== 0 ||
    Number(model.scorableRounds) < scorable ||
    Number(model.denseRows) !== expectedDenseRows ||
    Number(model.zeroRows) !== expectedZeroRows ||
    Number(model.positiveRows) !== Number(model.scorableRounds) * active ||
    Number(model.crownRows) !== Number(model.scorableRounds) ||
    Number(model.uniqueCrownRounds) !== Number(model.scorableRounds) ||
    Number(model.crownMismatches) !== 0 ||
    Number(model.denseRounds) !== Number(model.scorableRounds) ||
    Number(model.fullRosterWaves) < MIN_WAVES ||
    Number(model.invalidRows) !== 0 ||
    Number(model.exclusiveRows) !== 0 ||
    Number(model.duplicateRows) !== 0 ||
    Number(model.snapshotRows) !== roster ||
    Number(model.snapshotWaves) !== 1 ||
    Number(model.reporterResetAttempt) < minimumResetAttempts ||
    model.reporterUsed !== true
  ) {
    throw new Error(`managed KotH integrity failed: ${JSON.stringify(model)}`);
  }
  return model;
}

export const MANAGED_KOTH_MAX_ROSTER = MAX_ROSTER;
export const MANAGED_KOTH_MIN_WAVES = MIN_WAVES;
export const MANAGED_KOTH_MAX_ACTIVE_FLEET = MAX_ACTIVE_FLEET;
export const MANAGED_KOTH_DEFAULT_ADMISSION_PER_MINUTE = DEFAULT_ADMISSION_PER_MINUTE;
