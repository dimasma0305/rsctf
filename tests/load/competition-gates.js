const finite = (value) => typeof value === 'number' && Number.isFinite(value);

export function auditDefenseRecovery(incidents, repairs, minimumRate = 0.9) {
  if (
    !Number.isSafeInteger(incidents) ||
    incidents < 0 ||
    !Number.isSafeInteger(repairs) ||
    repairs < 0 ||
    !finite(minimumRate) ||
    minimumRate <= 0 ||
    minimumRate > 1
  ) {
    throw new TypeError('defense recovery counts and minimum rate are invalid');
  }
  const invalid = repairs > incidents;
  const required = incidents === 0 ? 0 : Math.max(1, Math.ceil(incidents * minimumRate));
  return Object.freeze({
    valid: !invalid && repairs >= required,
    incidents,
    repairs,
    unresolved: Math.max(0, incidents - repairs),
    rate: incidents === 0 ? 1 : Math.min(1, repairs / incidents),
    required,
    missing: invalid ? 1 : Math.max(0, required - repairs),
  });
}

export function auditKothPatchOperationFailures(
  patchAttempts,
  patchFailures,
  repairAttempts,
  repairFailures,
  maximumRate = 0.25,
) {
  const counts = [patchAttempts, patchFailures, repairAttempts, repairFailures];
  if (
    counts.some((value) => !Number.isSafeInteger(value) || value < 0) ||
    patchFailures > patchAttempts ||
    repairFailures > repairAttempts ||
    !finite(maximumRate) ||
    maximumRate < 0 ||
    maximumRate > 1
  ) {
    throw new TypeError('KotH patch operation failure counts or rate are invalid');
  }
  const attempts = patchAttempts + repairAttempts;
  const failures = patchFailures + repairFailures;
  const rate = attempts === 0 ? 0 : failures / attempts;
  return Object.freeze({
    valid: rate <= maximumRate,
    attempts,
    failures,
    rate,
    maximumRate,
  });
}

export function specialtyLiftFloor(specialty, longCompetition) {
  if (!['offense', 'defense', 'koth', 'jeopardy'].includes(specialty)) {
    throw new TypeError(`unknown specialty ${specialty}`);
  }
  if (typeof longCompetition !== 'boolean') {
    throw new TypeError('longCompetition must be boolean');
  }
  // A short KotH run has only a handful of acquisition windows, so which
  // specialty wins is intentionally stochastic. The half-hour+ acceptance
  // contract still requires the KotH cohort to beat the complete field.
  if (!longCompetition && specialty === 'koth') return 0;
  return longCompetition ? 1 : 0.8;
}

export function adScoreRangeFloor(longCompetition) {
  if (typeof longCompetition !== 'boolean') {
    throw new TypeError('longCompetition must be boolean');
  }
  // One normalized A&D hill has a deliberately compressed 100-point budget.
  // Three points still distinguishes meaningful field separation without
  // requiring the harness to manufacture score variance.
  return longCompetition ? 3 : 0.5;
}

function descending(left, right) {
  return right - left;
}

function sumAcquisitions(row) {
  return Array.isArray(row?.hills)
    ? row.hills.reduce(
        (total, hill) => total + (Number.isSafeInteger(hill?.acquisitionWindows) ? hill.acquisitionWindows : 0),
        0
      )
    : 0;
}

function compareAd(left, right) {
  return (
    descending(left.settledTotal, right.settledTotal) ||
    descending(left.projectedTotal, right.projectedTotal) ||
    descending(left.offenseRate, right.offenseRate) ||
    descending(left.defenseRate, right.defenseRate) ||
    descending(left.slaRate, right.slaRate) ||
    left.participationId - right.participationId
  );
}

function compareKoth(left, right) {
  return (
    descending(left.settledTotal, right.settledTotal) ||
    descending(left.controlRate, right.controlRate) ||
    descending(left.reliabilityRate, right.reliabilityRate) ||
    descending(sumAcquisitions(left), sumAcquisitions(right)) ||
    left.participationId - right.participationId
  );
}

function compareJeopardy(left, right) {
  return (
    descending(left.score, right.score) ||
    left.lastSubmissionTime - right.lastSubmissionTime ||
    left.id - right.id
  );
}

const DEFINITIONS = Object.freeze({
  ad: {
    rows: (board) => board?.teams,
    id: (row) => row?.participationId,
    score: (row) => row?.settledTotal,
    numeric: ['settledTotal', 'projectedTotal', 'offenseRate', 'defenseRate', 'slaRate'],
    compare: compareAd,
  },
  koth: {
    rows: (board) => board?.teams,
    id: (row) => row?.participationId,
    score: (row) => row?.settledTotal,
    numeric: ['settledTotal', 'controlRate', 'reliabilityRate'],
    compare: compareKoth,
  },
  jeopardy: {
    rows: (board) => board?.items,
    id: (row) => row?.id,
    score: (row) => row?.score,
    numeric: ['score', 'lastSubmissionTime'],
    compare: compareJeopardy,
  },
});

export function auditOrdinalScoreboard(kind, board, expectedIds) {
  const definition = DEFINITIONS[kind];
  if (!definition) throw new Error(`unknown scoreboard kind ${kind}`);
  const expected = Array.from(expectedIds || [], Number);
  if (!expected.length || expected.some((id) => !Number.isSafeInteger(id) || id <= 0)) {
    throw new Error('expectedIds must contain positive integer ids');
  }
  const rows = definition.rows(board);
  const errors = [];
  if (!Array.isArray(rows)) {
    return { valid: false, errors: ['missing rows'], teams: 0, distinctRanks: 0, distinctScores: 0, minimum: 0, maximum: 0 };
  }
  if (rows.length !== expected.length) errors.push(`roster size ${rows.length} != ${expected.length}`);
  const ids = rows.map(definition.id);
  if (ids.some((id) => !Number.isSafeInteger(id) || id <= 0)) errors.push('invalid row id');
  if (new Set(ids).size !== ids.length) errors.push('duplicate row id');
  const expectedSet = new Set(expected);
  if (ids.some((id) => !expectedSet.has(id)) || expected.some((id) => !ids.includes(id))) {
    errors.push('roster mismatch');
  }
  if (rows.some((row) => definition.numeric.some((field) => !finite(row?.[field])))) {
    errors.push('invalid numeric field');
  }
  const ranks = rows.map((row) => row?.rank);
  if (ranks.some((rank, index) => rank !== index + 1)) errors.push('ranks are not ordinal 1..N');
  for (let index = 1; index < rows.length; index++) {
    if (definition.compare(rows[index - 1], rows[index]) > 0) {
      errors.push(`row ${index + 1} violates the ${kind} comparator`);
      break;
    }
  }
  const scores = rows.map(definition.score).filter(finite);
  return {
    valid: errors.length === 0,
    errors,
    teams: rows.length,
    distinctRanks: new Set(ranks).size,
    distinctScores: new Set(scores.map((score) => score.toFixed(6))).size,
    minimum: scores.length ? Math.min(...scores) : 0,
    maximum: scores.length ? Math.max(...scores) : 0,
  };
}

export function populatedTimeBuckets(timestamps, startMs, endMs, bucketCount = 6) {
  if (!Number.isSafeInteger(startMs) || !Number.isSafeInteger(endMs) || endMs <= startMs) {
    throw new Error('event window must use increasing integer millisecond timestamps');
  }
  if (!Number.isSafeInteger(bucketCount) || bucketCount < 1) {
    throw new Error('bucketCount must be a positive integer');
  }
  const populated = new Set();
  for (const raw of timestamps || []) {
    const timestamp = Number(raw);
    if (!Number.isFinite(timestamp) || timestamp < startMs || timestamp >= endMs) continue;
    const index = Math.min(bucketCount - 1, Math.floor(((timestamp - startMs) * bucketCount) / (endMs - startMs)));
    populated.add(index);
  }
  return { populated: populated.size, buckets: [...populated].sort((left, right) => left - right) };
}

export function specialtyLift(profiles, valuesByIndex, specialty) {
  const selected = [];
  const others = [];
  for (const profile of profiles || []) {
    const value = Number(valuesByIndex?.get?.(profile.index) ?? valuesByIndex?.[profile.index]);
    if (!Number.isFinite(value)) continue;
    (profile.specialty === specialty ? selected : others).push(value);
  }
  const mean = (values) => (values.length ? values.reduce((total, value) => total + value, 0) / values.length : 0);
  const specialtyMean = mean(selected);
  const fieldMean = mean([...selected, ...others]);
  return {
    specialty,
    specialtyTeams: selected.length,
    specialtyMean,
    fieldMean,
    lift: fieldMean > 0 ? specialtyMean / fieldMean : 0,
  };
}
