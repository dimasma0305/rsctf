const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const WORKER_HANDLE = /^rsctf-worker:([0-9a-f-]{36}):([0-9a-f-]{36}):([1-9][0-9]*)$/i;
const SHA256 = /^sha256:[0-9a-f]{64}$/i;
const ISOLATED_PROJECT = /^[a-z0-9][a-z0-9_-]{0,47}$/;
const DAEMON_OWNER_LABEL = 'io.rsctf.worker.daemon-owner';
const SAFE_COMPOSE_HOST_ENV = [
  'HOME',
  'LOGNAME',
  'PATH',
  'TMPDIR',
  'USER',
  'XDG_RUNTIME_DIR',
];
const DEFAULT_MAX_PROXY_RESPONSE_BYTES = 64 * 1024;

export function createProxyResponseTracker(
  marker = '',
  maxBytes = DEFAULT_MAX_PROXY_RESPONSE_BYTES,
) {
  if (!Number.isSafeInteger(maxBytes) || maxBytes <= 0) {
    throw new Error('proxy response byte limit must be a positive integer');
  }
  return {
    marker: String(marker),
    maxBytes,
    body: '',
    sawPayload: false,
    valid: false,
    truncated: false,
  };
}

export function appendProxyResponse(tracker, chunk) {
  const text = proxyChunkText(chunk);
  if (!text.length) return tracker;
  tracker.sawPayload = true;
  const remaining = Math.max(0, tracker.maxBytes - tracker.body.length);
  if (text.length > remaining) tracker.truncated = true;
  if (remaining > 0) tracker.body += text.slice(0, remaining);
  tracker.valid = tracker.marker ? tracker.body.includes(tracker.marker) : true;
  return tracker;
}

function proxyChunkText(chunk) {
  if (typeof chunk === 'string') return chunk;
  let bytes;
  if (chunk instanceof ArrayBuffer) {
    bytes = new Uint8Array(chunk);
  } else if (ArrayBuffer.isView(chunk)) {
    bytes = new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength);
  } else if (Array.isArray(chunk)) {
    bytes = Uint8Array.from(chunk);
  } else {
    return '';
  }
  let text = '';
  for (const byte of bytes) text += String.fromCharCode(byte);
  return text;
}

export function validateIsolatedProject(project) {
  const normalized = String(project || '').trim();
  if (!ISOLATED_PROJECT.test(normalized)) {
    throw new Error(
      'E2E_PROJECT must contain 1-48 lowercase letters, digits, underscores, or hyphens',
    );
  }
  if (normalized === 'rsctf') {
    throw new Error('E2E_PROJECT must not use the reserved live project name rsctf');
  }
  return normalized;
}

export function assertFreshIsolatedProject(project, { resources = [], imageTags = [] } = {}) {
  const normalized = validateIsolatedProject(project);
  const collisions = [...resources, ...imageTags].map(String).filter(Boolean);
  if (collisions.length) {
    throw new Error(
      `isolated project ${normalized} already owns Docker artifacts: ${collisions.join(', ')}`,
    );
  }
  return normalized;
}

export function canCleanupComposeProject(project, claimedProject) {
  try {
    return validateIsolatedProject(project) === validateIsolatedProject(claimedProject);
  } catch {
    return false;
  }
}

export function isolatedComposeEnvironment(hostEnvironment, pinnedEnvironment) {
  const result = {};
  for (const key of SAFE_COMPOSE_HOST_ENV) {
    if (hostEnvironment[key]) result[key] = hostEnvironment[key];
  }
  for (const [key, value] of Object.entries(pinnedEnvironment)) {
    if (value !== undefined) result[key] = String(value);
  }
  return result;
}

export function requireMatchingSha256(label, expected, actual) {
  const normalizedExpected = String(expected || '').toLowerCase();
  const normalizedActual = String(actual || '').toLowerCase();
  if (!SHA256.test(normalizedExpected) || !SHA256.test(normalizedActual)) {
    throw new Error(`${label} must have exact sha256 identities`);
  }
  if (normalizedExpected !== normalizedActual) {
    throw new Error(`${label} identity changed: expected ${normalizedExpected}, got ${normalizedActual}`);
  }
  return normalizedActual;
}

export function auditRequiredResourceSamples(samples, baseContainers, { requireAgent = true } = {}) {
  const errors = [];
  if (!Array.isArray(samples) || samples.length < 2) {
    return { valid: false, errors: ['resource time series must contain at least two samples'] };
  }
  const required = new Set(baseContainers.map(String));
  samples.forEach((sample, index) => {
    if (!sample || !Number.isFinite(Number(sample.timestampMs))) {
      errors.push(`sample ${index}: invalid timestamp`);
      return;
    }
    if (Array.isArray(sample.errors) && sample.errors.length) {
      errors.push(`sample ${index}: ${sample.errors.join('; ')}`);
    }
    const containers = Array.isArray(sample.containers) ? sample.containers : [];
    const byName = new Map(containers.map((container) => [String(container?.name || ''), container]));
    for (const name of required) {
      const container = byName.get(name);
      if (
        !container ||
        !Number.isFinite(container.cpuPercent) ||
        !Number.isFinite(container.memoryBytes)
      ) {
        errors.push(`sample ${index}: required container ${name} has no complete CPU/RAM sample`);
      }
    }
    if (
      requireAgent &&
      (!sample.agent ||
        !Number.isSafeInteger(sample.agent.pid) ||
        !Number.isFinite(sample.agent.cpuPercent) ||
        !Number.isFinite(sample.agent.memoryBytes))
    ) {
      errors.push(`sample ${index}: worker agent has no complete CPU/RAM sample`);
    }
  });
  return { valid: errors.length === 0, errors };
}

export function ownsDaemonSentinel(labels, workerId) {
  return Boolean(
    labels &&
      typeof labels === 'object' &&
      !Array.isArray(labels) &&
      UUID.test(String(workerId || '')) &&
      labels[DAEMON_OWNER_LABEL] === workerId,
  );
}

export function canRemoveDaemonSentinel(labels, workerId, preexisting) {
  return preexisting === false && ownsDaemonSentinel(labels, workerId);
}

export function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer (got ${value})`);
  }
  return parsed;
}

export function boundedPositiveInteger(value, label, maximum) {
  const parsed = positiveInteger(value, label);
  const upperBound = positiveInteger(maximum, `${label} maximum`);
  if (parsed > upperBound) {
    throw new Error(`${label} must be at most ${upperBound} (got ${value})`);
  }
  return parsed;
}

export function assertProxyIdentityRateBudget(
  rate,
  identityTokens,
  maximumPerIdentity = 2,
) {
  const parsedRate = Number(rate);
  const parsedMaximum = Number(maximumPerIdentity);
  if (!Number.isFinite(parsedRate) || parsedRate <= 0) {
    throw new Error(`proxy RATE must be positive (got ${rate})`);
  }
  if (!Number.isFinite(parsedMaximum) || parsedMaximum <= 0) {
    throw new Error(
      `MAX_PROXY_RATE_PER_IDENTITY must be positive (got ${maximumPerIdentity})`,
    );
  }
  if (!Array.isArray(identityTokens) || identityTokens.length === 0) {
    throw new Error('proxy load needs at least one authenticated identity');
  }
  const identities = new Set(identityTokens.map(String).filter(Boolean));
  if (identities.size !== identityTokens.length) {
    throw new Error('each proxy endpoint must use a distinct authenticated identity');
  }
  const perIdentity = parsedRate / identities.size;
  if (perIdentity > parsedMaximum) {
    throw new Error(
      `RATE=${parsedRate} across ${identities.size} authenticated identities offers ` +
        `${perIdentity.toFixed(3)} request/s per identity; keep it at or below ` +
        `${parsedMaximum} to stay clear of the 150 request/60s authenticated limiter ` +
        '(increase FLEET or lower RATE)',
    );
  }
  return perIdentity;
}

export function unwrapResponse(value) {
  if (
    value &&
    typeof value === 'object' &&
    !Array.isArray(value) &&
    Object.hasOwn(value, 'data')
  ) {
    return value.data;
  }
  return value;
}

export function parseContainerInfo(value) {
  const container = unwrapResponse(value);
  if (!container || typeof container !== 'object' || Array.isArray(container)) {
    throw new Error('container response must be an object');
  }
  if (!UUID.test(String(container.id || ''))) {
    throw new Error('container response has an invalid id');
  }
  if (!UUID.test(String(container.entry || ''))) {
    throw new Error('worker-backed container entry must be a proxy UUID');
  }
  if (container.status !== 'Running') {
    throw new Error(`container is not running (status ${container.status})`);
  }
  if (!Number.isFinite(Number(container.startedAt)) || !Number.isFinite(Number(container.expectStopAt))) {
    throw new Error('container response has invalid timestamps');
  }
  return {
    id: String(container.id).toLowerCase(),
    entry: String(container.entry).toLowerCase(),
    status: container.status,
    startedAt: Number(container.startedAt),
    expectStopAt: Number(container.expectStopAt),
  };
}

export function parseWorkerHandle(value) {
  const match = WORKER_HANDLE.exec(String(value || ''));
  if (!match || !UUID.test(match[1]) || !UUID.test(match[2])) {
    throw new Error('container has an invalid worker backend handle');
  }
  const generation = positiveInteger(match[3], 'workload generation');
  return {
    workloadId: match[1].toLowerCase(),
    assignmentId: match[2].toLowerCase(),
    generation,
  };
}

export function advanceWorkloadHandles(handles) {
  return handles.map((handle) => {
    const generation = positiveInteger(handle?.generation, 'workload generation');
    if (generation >= Number.MAX_SAFE_INTEGER) {
      throw new Error('workload generation is exhausted');
    }
    return { ...handle, generation: generation + 1 };
  });
}

export function proxyWebSocketUrl(target, entry) {
  if (!UUID.test(String(entry || ''))) throw new Error('proxy entry must be a UUID');
  const url = new URL(target);
  if (url.protocol === 'http:') url.protocol = 'ws:';
  else if (url.protocol === 'https:') url.protocol = 'wss:';
  else throw new Error(`worker proxy target must use HTTP or HTTPS (got ${url.protocol})`);
  url.pathname = `${url.pathname.replace(/\/$/, '')}/api/proxy/${entry}`;
  url.search = '';
  url.hash = '';
  return url.toString();
}

function validateWorker(worker) {
  if (!worker || typeof worker !== 'object' || Array.isArray(worker)) {
    throw new Error('worker list contains a non-object row');
  }
  if (!UUID.test(String(worker.id || ''))) throw new Error('worker list contains an invalid id');
  if (!['Enabled', 'Draining', 'Disabled'].includes(worker.administrativeState)) {
    throw new Error(`worker ${worker.id} has an invalid administrative state`);
  }
  if (typeof worker.online !== 'boolean') {
    throw new Error(`worker ${worker.id} has an invalid online state`);
  }
  if (worker.online && !UUID.test(String(worker.sessionId || ''))) {
    throw new Error(`online worker ${worker.id} has an invalid session id`);
  }
  for (const field of ['sessionEpoch', 'heartbeatAt', 'leaseExpiresAt']) {
    const value = worker[field];
    if (value != null && !Number.isFinite(Number(value))) {
      throw new Error(`worker ${worker.id} has an invalid ${field}`);
    }
  }
  return { ...worker, id: String(worker.id).toLowerCase() };
}

export function selectOnlineWorkers(
  value,
  { minimum = 1, workerIds = [], platformOs = undefined } = {},
) {
  const rows = unwrapResponse(value);
  if (!Array.isArray(rows)) throw new Error('admin worker response must be an array');
  const workers = rows.map(validateWorker);
  const requested = new Set(workerIds.map((id) => String(id).toLowerCase()));
  for (const id of requested) {
    if (!UUID.test(id)) throw new Error(`WORKER_IDS contains an invalid UUID: ${id}`);
  }
  const selected = workers.filter((worker) => {
    if (requested.size && !requested.has(worker.id)) return false;
    if (platformOs && worker.platformOs !== platformOs) return false;
    return (
      worker.online &&
      worker.administrativeState === 'Enabled' &&
      worker.runtimeKind === 'docker' &&
      Number(worker.capacity?.slots || 0) > 0 &&
      Number(worker.capabilities?.maxWorkloadReplicas || 0) > 0
    );
  });
  const seen = new Set(workers.map((worker) => worker.id));
  const missing = [...requested].filter((id) => !seen.has(id));
  if (missing.length) throw new Error(`requested workers were not found: ${missing.join(', ')}`);
  if (selected.length < positiveInteger(minimum, 'minimum worker count')) {
    throw new Error(
      `need at least ${minimum} online enabled Docker worker(s), found ${selected.length}`,
    );
  }
  return selected;
}

export function auditWorkerContinuity(
  beforeValue,
  afterValue,
  workerIds,
  { expectedReconnectIds = [], allowSessionChanges = false, now = Date.now() } = {},
) {
  const before = new Map(
    unwrapResponse(beforeValue).map(validateWorker).map((worker) => [worker.id, worker]),
  );
  const after = new Map(
    unwrapResponse(afterValue).map(validateWorker).map((worker) => [worker.id, worker]),
  );
  const reconnects = new Set(expectedReconnectIds.map((id) => String(id).toLowerCase()));
  const errors = [];
  for (const rawId of workerIds) {
    const id = String(rawId).toLowerCase();
    const first = before.get(id);
    const last = after.get(id);
    if (!first || !last) {
      errors.push(`${id}: missing from ${first ? 'after' : 'before'} snapshot`);
      continue;
    }
    if (!last.online) errors.push(`${id}: offline after the run`);
    if (Number(last.leaseExpiresAt || 0) <= now) errors.push(`${id}: lease is not current`);
    if (Number(last.heartbeatAt || 0) < Number(first.heartbeatAt || 0)) {
      errors.push(`${id}: heartbeat moved backwards`);
    }
    if (Number(last.sessionEpoch || 0) < Number(first.sessionEpoch || 0)) {
      errors.push(`${id}: session epoch moved backwards`);
    }
    const changed = first.sessionId !== last.sessionId;
    if (reconnects.has(id)) {
      if (!changed || Number(last.sessionEpoch) <= Number(first.sessionEpoch)) {
        errors.push(`${id}: expected a new fenced session`);
      }
    } else if (changed && !allowSessionChanges) {
      errors.push(`${id}: reconnected unexpectedly during the run`);
    }
  }
  return { valid: errors.length === 0, errors };
}

export function auditWorkloadRows(value, expectedHandles, expectedState) {
  if (!Array.isArray(value)) throw new Error('workload snapshot must be an array');
  const expected = new Map(expectedHandles.map((handle) => [handle.workloadId, handle]));
  const seen = new Set();
  const errors = [];
  for (const row of value) {
    const id = String(row?.workloadId || '').toLowerCase();
    const handle = expected.get(id);
    if (!handle) {
      errors.push(`${id || '<missing>'}: unexpected workload row`);
      continue;
    }
    if (seen.has(id)) errors.push(`${id}: duplicate workload row`);
    seen.add(id);
    if (String(row.assignmentId).toLowerCase() !== handle.assignmentId) {
      errors.push(`${id}: assignment fence changed`);
    }
    if (Number(row.generation) !== handle.generation) {
      errors.push(`${id}: generation fence changed`);
    }
    if (!Number.isSafeInteger(Number(row.observedSessionEpoch)) || Number(row.observedSessionEpoch) <= 0) {
      errors.push(`${id}: observed status has no valid session epoch fence`);
    }
    if (expectedState === 'Ready') {
      if (row.desiredState !== 'Present' || row.observedState !== 'Ready') {
        errors.push(`${id}: expected Present/Ready, got ${row.desiredState}/${row.observedState}`);
      }
    } else if (expectedState === 'Absent') {
      if (row.desiredState !== 'Absent' || row.observedState !== 'Absent') {
        errors.push(`${id}: expected Absent/Absent, got ${row.desiredState}/${row.observedState}`);
      }
    } else {
      throw new Error(`unsupported expected workload state ${expectedState}`);
    }
  }
  for (const id of expected.keys()) {
    if (!seen.has(id)) errors.push(`${id}: workload row is missing`);
  }
  return { valid: errors.length === 0, errors };
}

export function workloadShape(value, label = 'workload') {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  if (!Array.isArray(value.services) || value.services.length === 0) {
    throw new Error(`${label} must contain at least one service`);
  }
  const names = new Set();
  const services = value.services.map((service, index) => {
    const name = String(service?.name || '').trim();
    if (!name || names.has(name)) {
      throw new Error(`${label} service ${index} has an empty or duplicate name`);
    }
    names.add(name);
    const replicas = positiveInteger(service.replicas, `${label} ${name} replicas`);
    return { name, replicas };
  });
  return {
    spec: value,
    serviceCount: services.length,
    replicaCount: services.reduce((total, service) => total + service.replicas, 0),
    services,
  };
}

export function auditReplicaLabels(value, expectedHandles, expectedServices) {
  if (!Array.isArray(value)) throw new Error('replica label snapshot must be an array');
  const expected = new Map(expectedHandles.map((handle) => [handle.workloadId, handle]));
  const expectedPairs = new Set();
  for (const service of expectedServices) {
    for (let replica = 0; replica < service.replicas; replica += 1) {
      expectedPairs.add(`${service.name}:${replica}`);
    }
  }
  const seen = new Map([...expected.keys()].map((id) => [id, new Set()]));
  const errors = [];
  for (const labels of value) {
    if (!labels || typeof labels !== 'object' || Array.isArray(labels)) {
      errors.push('replica has malformed Docker labels');
      continue;
    }
    const workloadId = String(labels['io.rsctf.workload.id'] || '').toLowerCase();
    const handle = expected.get(workloadId);
    if (!handle) {
      errors.push(`${workloadId || '<missing>'}: unexpected Docker replica`);
      continue;
    }
    if (labels['io.rsctf.worker.managed'] !== 'true') {
      errors.push(`${workloadId}: replica is missing the managed label`);
    }
    if (String(labels['io.rsctf.assignment.id'] || '').toLowerCase() !== handle.assignmentId) {
      errors.push(`${workloadId}: replica assignment fence changed`);
    }
    if (Number(labels['io.rsctf.workload.generation']) !== handle.generation) {
      errors.push(`${workloadId}: replica generation fence changed`);
    }
    if (
      handle.specHash &&
      String(labels['io.rsctf.workload.spec-hash'] || '').toLowerCase() !== handle.specHash
    ) {
      errors.push(`${workloadId}: replica spec-hash fence changed`);
    }
    if (
      handle.workerId &&
      String(labels['io.rsctf.worker.id'] || '').toLowerCase() !== handle.workerId
    ) {
      errors.push(`${workloadId}: replica worker identity changed`);
    }
    const pair = `${String(labels['io.rsctf.workload.service'] || '')}:` +
      `${String(labels['io.rsctf.workload.replica'] || '')}`;
    if (!expectedPairs.has(pair)) {
      errors.push(`${workloadId}: unexpected replica ${pair}`);
      continue;
    }
    if (seen.get(workloadId).has(pair)) {
      errors.push(`${workloadId}: duplicate replica ${pair}`);
    }
    seen.get(workloadId).add(pair);
  }
  for (const [workloadId, pairs] of seen) {
    for (const pair of expectedPairs) {
      if (!pairs.has(pair)) errors.push(`${workloadId}: replica ${pair} is missing`);
    }
  }
  return { valid: errors.length === 0, errors };
}

export function percentile(values, fraction) {
  if (!Array.isArray(values) || values.length === 0) return 0;
  if (!Number.isFinite(fraction) || fraction < 0 || fraction > 1) {
    throw new Error('percentile fraction must be between zero and one');
  }
  const sorted = values.map(Number).sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * fraction) - 1)];
}
