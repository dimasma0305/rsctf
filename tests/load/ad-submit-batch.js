export const MAX_AD_BATCH = 100;

export const AD_SUBMIT_STATUSES = [
  'accepted',
  'duplicate',
  'wrong',
  'expired',
  'self_attack',
  'not_started',
  'ended',
  'paused',
  'rejected',
];

export const AD_SUBMIT_BATCH_SHAPES = ['repeated', 'distinct', 'distinct-known'];

const STATUS_SET = new Set(AD_SUBMIT_STATUSES);

function required(environment, name) {
  const value = environment?.[name];
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error(`${name} is required`);
  }
  return value;
}

function boundedInteger(value, label, minimum, maximum) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${label} must be an integer from ${minimum} to ${maximum}`);
  }
  return parsed;
}

function boundedDuration(value, label, minimumMs, maximumMs) {
  const duration = String(value);
  const match = /^(\d+)(ms|s|m)$/.exec(duration);
  if (!match) throw new Error(`${label} must use an integer ms, s, or m duration`);
  const multiplier = match[2] === 'ms' ? 1 : match[2] === 's' ? 1_000 : 60_000;
  const milliseconds = Number(match[1]) * multiplier;
  if (!Number.isSafeInteger(milliseconds) || milliseconds < minimumMs || milliseconds > maximumMs) {
    throw new Error(`${label} must be between ${minimumMs}ms and ${maximumMs}ms`);
  }
  return duration;
}

function normalizedTarget(value) {
  const match = /^(https?):\/\/(\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9._-]+)(?::(\d{1,5}))?\/?$/.exec(
    String(value),
  );
  if (!match) throw new Error('TARGET must be a valid HTTP(S) origin without credentials or a path');
  if (match[3]) boundedInteger(match[3], 'TARGET port', 1, 65_535);
  return `${match[1].toLowerCase()}://${match[2]}${match[3] ? `:${match[3]}` : ''}`;
}

/**
 * Validate the deliberately explicit configuration for the mutating A&D batch
 * load. Nothing in this path discovers a game, flag, or credential.
 */
export function parseAdSubmitBatchConfig(environment) {
  if (environment?.CONFIRM_MUTATING_LOAD !== '1') {
    throw new Error('set CONFIRM_MUTATING_LOAD=1 to acknowledge that a valid flag can alter scoring');
  }

  const token = required(environment, 'TOKEN');
  if (/\s/.test(token) || token.length > 8_192) {
    throw new Error('TOKEN must be a single non-whitespace bearer credential');
  }
  const flag = required(environment, 'FLAG');
  if (!flag.trim() || flag.length > 4_096) {
    throw new Error('FLAG must contain between 1 and 4096 characters');
  }

  const rate = boundedInteger(environment.RATE || '1', 'RATE', 1, 20);
  const batchShape = String(environment.BATCH_SHAPE || 'repeated').toLowerCase();
  if (!AD_SUBMIT_BATCH_SHAPES.includes(batchShape)) {
    throw new Error(`BATCH_SHAPE must be one of ${AD_SUBMIT_BATCH_SHAPES.join(', ')}`);
  }
  let explicitFlags = null;
  if (batchShape === 'distinct-known') {
    try {
      explicitFlags = JSON.parse(required(environment, 'FLAGS_JSON'));
    } catch (error) {
      throw new Error(`FLAGS_JSON must be valid JSON: ${error.message}`);
    }
    if (
      !Array.isArray(explicitFlags) ||
      explicitFlags.length !== MAX_AD_BATCH ||
      new Set(explicitFlags).size !== MAX_AD_BATCH ||
      explicitFlags.some(
        (value) => typeof value !== 'string' || !/^flag\{[A-Za-z0-9_-]{32}\}$/.test(value),
      )
    ) {
      throw new Error('FLAGS_JSON must contain exactly 100 distinct engine-shaped flags');
    }
  }
  const vus = boundedInteger(environment.VUS || String(Math.max(4, rate * 2)), 'VUS', 1, 100);
  const maxVus = boundedInteger(
    environment.MAX_VUS || String(Math.max(vus, rate * 4)),
    'MAX_VUS',
    vus,
    200,
  );

  return {
    target: normalizedTarget(environment.TARGET || 'http://127.0.0.1:8080'),
    gameId: boundedInteger(required(environment, 'GAME'), 'GAME', 1, 2_147_483_647),
    token,
    flag,
    batchShape,
    explicitFlags,
    rate,
    vus,
    maxVus,
    duration: boundedDuration(environment.DURATION || '30s', 'DURATION', 1_000, 600_000),
    requestTimeout: boundedDuration(
      environment.REQUEST_TIMEOUT || '10s',
      'REQUEST_TIMEOUT',
      1_000,
      60_000,
    ),
  };
}

/// Generate deterministic, valid-shape flags that cannot be discovered from the
/// database. They exercise the distinct unknown-flag lookup ceiling without
/// mutating scoring state.
export function distinctPlausibleFlags(batchSize = MAX_AD_BATCH) {
  return Array.from({ length: batchSize }, (_, index) => {
    const payload = `audit${index.toString(36).padStart(4, '0')}`.padEnd(32, 'x');
    return `flag{${payload}}`;
  });
}

function emptyStatusCounts() {
  const counts = {};
  for (const status of AD_SUBMIT_STATUSES) counts[status] = 0;
  return counts;
}

function invalid(counts, reason, acceptedCount = null) {
  return { valid: false, counts, acceptedCount, reason };
}

/**
 * Validate one raw Ad/Submit response for a batch containing the same known
 * flag 100 times. A correctly configured attacker can accept it at most once;
 * every other row must be the same attack's duplicate.
 */
export function inspectKnownFlagBatch(model, expectedFlag, batchSize = MAX_AD_BATCH) {
  const counts = emptyStatusCounts();
  if (
    model === null ||
    typeof model !== 'object' ||
    Array.isArray(model) ||
    !Number.isSafeInteger(model.acceptedCount) ||
    model.acceptedCount < 0 ||
    model.acceptedCount > 1 ||
    !Array.isArray(model.results) ||
    model.results.length !== batchSize
  ) {
    return invalid(counts, 'invalid batch response shape');
  }

  for (const result of model.results) {
    if (result === null || typeof result !== 'object' || Array.isArray(result)) {
      return invalid(counts, 'invalid result row', model.acceptedCount);
    }
    if (result.flag !== expectedFlag || !STATUS_SET.has(result.status)) {
      return invalid(counts, 'unexpected flag echo or status', model.acceptedCount);
    }
    counts[result.status]++;
    if (
      !Number.isSafeInteger(result.flagPlantedAtRound) ||
      result.flagPlantedAtRound < 1
    ) {
      return invalid(counts, 'known flag has no valid planted round', model.acceptedCount);
    }
  }

  if (
    counts.accepted !== model.acceptedCount ||
    counts.accepted + counts.duplicate !== batchSize
  ) {
    return invalid(counts, 'known flag was not accepted or duplicate', model.acceptedCount);
  }
  return { valid: true, counts, acceptedCount: model.acceptedCount, reason: null };
}


/** Validate a response for 100 distinct, plausible, deliberately unknown flags. */
export function inspectDistinctFlagBatch(model, expectedFlags) {
  const counts = emptyStatusCounts();
  if (
    model === null ||
    typeof model !== 'object' ||
    Array.isArray(model) ||
    model.acceptedCount !== 0 ||
    !Array.isArray(model.results) ||
    model.results.length !== expectedFlags.length
  ) {
    return invalid(counts, 'invalid distinct batch response shape');
  }

  for (let index = 0; index < expectedFlags.length; index++) {
    const result = model.results[index];
    if (
      result === null ||
      typeof result !== 'object' ||
      Array.isArray(result) ||
      result.flag !== expectedFlags[index] ||
      result.status !== 'wrong' ||
      result.flagPlantedAtRound !== null
    ) {
      return invalid(counts, 'distinct unknown flag was not rejected', model.acceptedCount);
    }
    counts.wrong++;
  }
  return { valid: true, counts, acceptedCount: 0, reason: null };
}

/** Validate a response for 100 distinct, known flags already captured by the team. */
export function inspectDistinctKnownFlagBatch(model, expectedFlags) {
  const counts = emptyStatusCounts();
  if (
    model === null ||
    typeof model !== 'object' ||
    Array.isArray(model) ||
    model.acceptedCount !== 0 ||
    !Array.isArray(model.results) ||
    model.results.length !== expectedFlags.length
  ) {
    return invalid(counts, 'invalid distinct-known batch response shape');
  }

  for (let index = 0; index < expectedFlags.length; index++) {
    const result = model.results[index];
    if (
      result === null ||
      typeof result !== 'object' ||
      Array.isArray(result) ||
      result.flag !== expectedFlags[index] ||
      result.status !== 'duplicate' ||
      !Number.isSafeInteger(result.flagPlantedAtRound) ||
      result.flagPlantedAtRound < 1
    ) {
      return invalid(counts, 'distinct known flag was not duplicate', model.acceptedCount);
    }
    counts.duplicate++;
  }
  return { valid: true, counts, acceptedCount: 0, reason: null };
}
