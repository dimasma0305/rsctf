// End-to-end trusted-worker load gate against an already-enrolled outbound
// worker fleet. It provisions real per-team Jeopardy workloads, drives fresh
// proxy streams at a fixed arrival rate, verifies durable workload fences, and
// destroys everything it created. No worker private key or enrollment token is
// read by this harness.
import { execFileSync } from 'node:child_process';

import {
  GAME,
  TARGET,
  mintJwt,
  runK6,
  sleep,
  sql,
  stat,
} from './lib.mjs';
import {
  advanceWorkloadHandles,
  auditReplicaLabels,
  auditWorkerContinuity,
  auditWorkloadRows,
  assertProxyIdentityRateBudget,
  parseContainerInfo,
  parseWorkerHandle,
  percentile,
  positiveInteger,
  proxyWebSocketUrl,
  selectOnlineWorkers,
  unwrapResponse,
  workloadShape,
} from './worker-plane.js';

const GAME_ID = positiveInteger(GAME, 'GAME');
const FLEET = positiveInteger(process.env.FLEET || process.env.N || 10, 'FLEET');
const PROXY_RATE = positiveInteger(process.env.RATE || 20, 'RATE');
const MAX_PROXY_RATE_PER_IDENTITY = Number(process.env.MAX_PROXY_RATE_PER_IDENTITY || 2);
const CYCLES = positiveInteger(process.env.CYCLES || 1, 'CYCLES');
const MIN_WORKERS = positiveInteger(process.env.MIN_WORKERS || 1, 'MIN_WORKERS');
const OPERATION_COOLDOWN_MS = positiveInteger(
  process.env.OPERATION_COOLDOWN_MS || 11_000,
  'OPERATION_COOLDOWN_MS',
);
const ABSENT_TIMEOUT_MS = positiveInteger(
  process.env.ABSENT_TIMEOUT_MS || 30_000,
  'ABSENT_TIMEOUT_MS',
);
const RECONNECT_TIMEOUT_MS = positiveInteger(
  process.env.RECONNECT_TIMEOUT_MS || 90_000,
  'RECONNECT_TIMEOUT_MS',
);
const API_TIMEOUT_MS = positiveInteger(process.env.API_TIMEOUT_MS || 120_000, 'API_TIMEOUT_MS');
const PROXY_READINESS_DELAY_MS = Number(process.env.PROXY_READINESS_DELAY_MS || 0);
if (
  !Number.isSafeInteger(PROXY_READINESS_DELAY_MS) ||
  PROXY_READINESS_DELAY_MS < 0 ||
  PROXY_READINESS_DELAY_MS > 60_000
) {
  throw new Error(
    `PROXY_READINESS_DELAY_MS must be an integer from 0 to 60000 ` +
      `(got ${process.env.PROXY_READINESS_DELAY_MS})`,
  );
}
const WORKER_IDS = String(process.env.WORKER_IDS || '')
  .split(',')
  .map((value) => value.trim())
  .filter(Boolean);
const WORKER_OS = String(process.env.WORKER_OS || '').trim().toLowerCase() || undefined;
if (WORKER_OS && !['linux', 'windows'].includes(WORKER_OS)) {
  throw new Error(`WORKER_OS must be linux or windows (got ${WORKER_OS})`);
}
const DB_AUDIT = process.env.SKIP_DB_AUDIT !== '1';
const RECONNECT_WORKER = process.env.RECONNECT_WORKER === '1';
const ALLOW_ACTIVE_RECONNECT = process.env.ALLOW_ACTIVE_RECONNECT === '1';
const ALLOW_SESSION_CHANGES = process.env.ALLOW_SESSION_CHANGES === '1';
const SUMMARY_JSON = String(process.env.SUMMARY_JSON || '').trim();
const LOCAL_DOCKER_REPLICA_AUDIT = process.env.LOCAL_DOCKER_REPLICA_AUDIT === '1';
const EXPECTED_SERVICE_COUNT = process.env.EXPECTED_SERVICE_COUNT
  ? positiveInteger(process.env.EXPECTED_SERVICE_COUNT, 'EXPECTED_SERVICE_COUNT')
  : undefined;
const EXPECTED_REPLICA_COUNT = process.env.EXPECTED_REPLICA_COUNT
  ? positiveInteger(process.env.EXPECTED_REPLICA_COUNT, 'EXPECTED_REPLICA_COUNT')
  : undefined;
if ((EXPECTED_SERVICE_COUNT === undefined) !== (EXPECTED_REPLICA_COUNT === undefined)) {
  throw new Error('EXPECTED_SERVICE_COUNT and EXPECTED_REPLICA_COUNT must be set together');
}

function rolloutShape(variable, label) {
  const raw = String(process.env[variable] || '').trim();
  if (!raw) return undefined;
  try {
    return workloadShape(JSON.parse(raw), label);
  } catch (error) {
    throw new Error(`${variable} is invalid: ${error.message}`);
  }
}

const ROLLOUT_UP = rolloutShape('ROLLOUT_UP_SPEC_JSON', 'scale-up workload');
const ROLLOUT_DOWN = rolloutShape('ROLLOUT_DOWN_SPEC_JSON', 'scale-down workload');
if (Boolean(ROLLOUT_UP) !== Boolean(ROLLOUT_DOWN)) {
  throw new Error('ROLLOUT_UP_SPEC_JSON and ROLLOUT_DOWN_SPEC_JSON must be set together');
}
if (ROLLOUT_UP && (!DB_AUDIT || EXPECTED_SERVICE_COUNT === undefined)) {
  throw new Error('live rollout requires database audit and the expected base workload shape');
}
if (LOCAL_DOCKER_REPLICA_AUDIT && !ROLLOUT_UP) {
  throw new Error('local Docker replica audit requires live rollout workload specifications');
}
if (
  ROLLOUT_DOWN &&
  (ROLLOUT_DOWN.serviceCount !== EXPECTED_SERVICE_COUNT ||
    ROLLOUT_DOWN.replicaCount !== EXPECTED_REPLICA_COUNT)
) {
  throw new Error(
    'scale-down workload must restore EXPECTED_SERVICE_COUNT/EXPECTED_REPLICA_COUNT',
  );
}
if (ROLLOUT_UP && ROLLOUT_UP.replicaCount <= ROLLOUT_DOWN.replicaCount) {
  throw new Error('scale-up workload must contain more replicas than the scale-down workload');
}

let adminToken;
let workerToRestore;
let rolloutChallengeToRestore;
const liveContainers = new Map();
const createLatencies = [];
const deleteLatencies = [];
const scaleUpLatencies = [];
const scaleDownLatencies = [];

function cycleSummaryPath(cycle, phase) {
  if (!SUMMARY_JSON) return SUMMARY_JSON;
  const suffix = [
    ...(CYCLES > 1 ? [`cycle-${cycle}`] : []),
    ...(ROLLOUT_UP ? [phase] : []),
  ];
  if (!suffix.length) return SUMMARY_JSON;
  const slash = Math.max(SUMMARY_JSON.lastIndexOf('/'), SUMMARY_JSON.lastIndexOf('\\'));
  const dot = SUMMARY_JSON.lastIndexOf('.');
  const suffixAt = dot > slash ? dot : SUMMARY_JSON.length;
  return `${SUMMARY_JSON.slice(0, suffixAt)}.${suffix.join('.')}${SUMMARY_JSON.slice(suffixAt)}`;
}

async function api(method, path, { token, body, ip, timeoutMs = API_TIMEOUT_MS } = {}) {
  const headers = {};
  if (token) headers.authorization = `Bearer ${token}`;
  if (ip) headers['x-real-ip'] = ip;
  if (body !== undefined) headers['content-type'] = 'application/json';
  const response = await fetch(`${TARGET}${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const text = await response.text();
  let json;
  try {
    json = text ? JSON.parse(text) : undefined;
  } catch {
    json = undefined;
  }
  return { status: response.status, text, json };
}

function responseError(response, operation) {
  const detail = response.json?.title || response.text?.slice(0, 300) || 'empty response';
  return new Error(`${operation} returned ${response.status}: ${detail}`);
}

function resolveAdminToken() {
  if (process.env.ADMIN_TOKEN) return process.env.ADMIN_TOKEN;
  const row = sql(
    `SELECT COALESCE(json_build_object('id',id::text,'stamp',security_stamp)::text,'') ` +
      `FROM "AspNetUsers" WHERE role=3 ORDER BY register_time_utc LIMIT 1`,
  );
  if (!row) throw new Error('no admin exists; set ADMIN_TOKEN or create an admin first');
  const admin = JSON.parse(row);
  return mintJwt(admin.id, admin.stamp, 3);
}

function resolvePlayers() {
  if (process.env.PLAYER_TOKENS) {
    const tokens = process.env.PLAYER_TOKENS.split(',').map((value) => value.trim()).filter(Boolean);
    if (tokens.length < FLEET) {
      throw new Error(`PLAYER_TOKENS has ${tokens.length} token(s), but FLEET=${FLEET}`);
    }
    return tokens.slice(0, FLEET).map((token, index) => ({
      token,
      participationId: index + 1,
      ip: `30.1.${Math.floor(index / 250)}.${(index % 250) + 1}`,
    }));
  }
  const raw = sql(
    `SELECT COALESCE(json_agg(row_to_json(candidate) ORDER BY candidate."participationId"),'[]'::json)::text ` +
      `FROM (` +
      ` SELECT DISTINCT ON (p.id) p.id AS "participationId", u.id::text AS "userId", ` +
      ` u.security_stamp AS "securityStamp" ` +
      ` FROM "Participations" p ` +
      ` JOIN "UserParticipations" up ON up.participation_id=p.id ` +
      ` JOIN "AspNetUsers" u ON u.id=up.user_id ` +
      ` WHERE p.game_id=${GAME_ID} AND p.status=1 ` +
      ` ORDER BY p.id, up.user_id` +
      `) candidate`,
  );
  const players = JSON.parse(raw || '[]');
  if (players.length < FLEET) {
    throw new Error(
      `worker load needs ${FLEET} distinct Accepted participations in game ${GAME_ID}, found ${players.length}`,
    );
  }
  return players.slice(0, FLEET).map((player, index) => ({
    token: mintJwt(player.userId, player.securityStamp, 1),
    participationId: Number(player.participationId),
    ip: `30.1.${Math.floor(index / 250)}.${(index % 250) + 1}`,
  }));
}

function resolveChallenge() {
  if (process.env.CID) return positiveInteger(process.env.CID, 'CID');
  const id = sql(
    `SELECT id FROM "GameChallenges" ` +
      `WHERE game_id=${GAME_ID} AND "Type" IN (1,3) AND is_enabled ` +
      `AND review_status=0 AND NOT enable_shared_container AND expose_port>0 ` +
      `AND (workload_spec IS NOT NULL OR (build_status=1 AND build_image_digest IS NOT NULL)) ` +
      `ORDER BY id LIMIT 1`,
  );
  if (!id) {
    throw new Error(
      `game ${GAME_ID} has no enabled per-team Jeopardy container with an immutable worker workload; set CID explicitly`,
    );
  }
  return positiveInteger(id, 'discovered CID');
}

function assertStoredChallengeShape(challengeId) {
  if (!DB_AUDIT || EXPECTED_SERVICE_COUNT === undefined) return;
  const raw = sql(
    `SELECT COALESCE(json_build_object(` +
      `'serviceCount',jsonb_array_length(workload_spec->'services'),` +
      `'replicaCount',(SELECT COALESCE(sum((replica_service->>'replicas')::int),0) ` +
      `FROM jsonb_array_elements(workload_spec->'services') AS services(replica_service))` +
      `)::text,'') FROM "GameChallenges" WHERE game_id=${GAME_ID} AND id=${challengeId}`,
  );
  if (!raw) throw new Error(`challenge ${challengeId} has no stored aggregate workload`);
  const shape = JSON.parse(raw);
  if (
    shape.serviceCount !== EXPECTED_SERVICE_COUNT ||
    shape.replicaCount !== EXPECTED_REPLICA_COUNT
  ) {
    throw new Error(
      `challenge ${challengeId} stores ${shape.serviceCount} service(s)/${shape.replicaCount} ` +
        `replica(s), expected ${EXPECTED_SERVICE_COUNT}/${EXPECTED_REPLICA_COUNT}`,
    );
  }
}

async function listWorkers() {
  const response = await api('GET', '/api/admin/workers', { token: adminToken });
  if (response.status !== 200) throw responseError(response, 'list workers');
  return response.json;
}

async function assertHealth(label) {
  const started = Date.now();
  const [live, ready] = await Promise.all([api('GET', '/livez'), api('GET', '/healthz')]);
  if (live.status !== 200 || ready.status !== 200) {
    throw new Error(`${label} health failed: livez=${live.status}, healthz=${ready.status}`);
  }
  return Date.now() - started;
}

function resourceSample() {
  if (process.env.SAMPLE_RESOURCES === '0') return undefined;
  try {
    return stat();
  } catch (error) {
    console.warn(`resource sample unavailable: ${error.message}`);
    return undefined;
  }
}

async function waitFor(check, timeoutMs, description) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await check();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await sleep(500);
  }
  throw new Error(
    `timed out waiting for ${description}${lastError ? `: ${lastError.message}` : ''}`,
  );
}

async function exerciseReconnect(worker) {
  if (DB_AUDIT && !ALLOW_ACTIVE_RECONNECT) {
    const active = Number(
      sql(
        `SELECT count(*) FROM "WorkerWorkloads" WHERE worker_id='${worker.id}'::uuid ` +
          `AND (desired_state='Present' OR observed_state<>'Absent')`,
      ),
    );
    if (active > 0) {
      throw new Error(
        `worker ${worker.id} has ${active} active workload(s); refusing reconnect drill without ALLOW_ACTIVE_RECONNECT=1`,
      );
    }
  }

  const beforeList = await listWorkers();
  workerToRestore = worker.id;
  const disabled = await api('PUT', `/api/admin/workers/${worker.id}/state`, {
    token: adminToken,
    body: { state: 'Disabled' },
  });
  if (disabled.status !== 200) throw responseError(disabled, 'disable worker');
  await waitFor(async () => {
    const row = (await listWorkers()).find((candidate) => candidate.id === worker.id);
    return row?.administrativeState === 'Disabled' && row.online === false;
  }, RECONNECT_TIMEOUT_MS, `worker ${worker.id} to disconnect`);

  const enabled = await api('PUT', `/api/admin/workers/${worker.id}/state`, {
    token: adminToken,
    body: { state: 'Enabled' },
  });
  if (enabled.status !== 200) throw responseError(enabled, 'enable worker');
  const afterList = await waitFor(async () => {
    const rows = await listWorkers();
    const row = rows.find((candidate) => candidate.id === worker.id);
    return row?.online && row.administrativeState === 'Enabled' ? rows : undefined;
  }, RECONNECT_TIMEOUT_MS, `worker ${worker.id} to reconnect`);
  workerToRestore = undefined;

  const audit = auditWorkerContinuity(beforeList, afterList, [worker.id], {
    expectedReconnectIds: [worker.id],
  });
  if (!audit.valid) throw new Error(`reconnect fence failed: ${audit.errors.join('; ')}`);
  const current = afterList.find((candidate) => candidate.id === worker.id);
  console.log(
    `  reconnect drill: ${worker.name} session epoch ${worker.sessionEpoch} → ${current.sessionEpoch}`,
  );
}

async function createContainer(player, challengeId, cycle) {
  const started = Date.now();
  const response = await api('POST', `/api/game/${GAME_ID}/container/${challengeId}`, {
    token: player.token,
    ip: player.ip,
  });
  createLatencies.push(Date.now() - started);
  if (response.status !== 200) throw responseError(response, `create container for participation ${player.participationId}`);
  const container = parseContainerInfo(response.json);
  const record = { ...player, container, createdAt: Date.now(), cycle };
  liveContainers.set(container.id, record);
  return record;
}

async function waitForCooldown(records) {
  const lastMutation = Math.max(...records.map((record) => record.createdAt || 0));
  const remaining = OPERATION_COOLDOWN_MS - (Date.now() - lastMutation);
  if (remaining > 0) await sleep(remaining);
}

async function destroyContainer(record, { cleanup = false } = {}) {
  const started = Date.now();
  let response = await api('DELETE', `/api/game/${GAME_ID}/container/${record.challengeId}`, {
    token: record.token,
    ip: record.ip,
  });
  if (response.status === 429) {
    await sleep(OPERATION_COOLDOWN_MS);
    response = await api('DELETE', `/api/game/${GAME_ID}/container/${record.challengeId}`, {
      token: record.token,
      ip: record.ip,
    });
  }
  deleteLatencies.push(Date.now() - started);
  if (response.status !== 200 && !(cleanup && response.status === 404)) {
    throw responseError(response, `destroy container for participation ${record.participationId}`);
  }
  liveContainers.delete(record.container.id);
  return response;
}

function backendHandles(containerIds) {
  const ids = containerIds.map((id) => `'${id}'::uuid`).join(',');
  const raw = sql(
    `SELECT COALESCE(json_agg(json_build_object(` +
      `'containerId',id::text,'backendHandle',container_id) ORDER BY id),'[]'::json)::text ` +
      `FROM "Containers" WHERE id IN (${ids})`,
  );
  const rows = JSON.parse(raw || '[]');
  if (rows.length !== containerIds.length) {
    throw new Error(`expected ${containerIds.length} persisted containers, found ${rows.length}`);
  }
  return rows.map((row) => parseWorkerHandle(row.backendHandle));
}

function workloadRows(handles) {
  const ids = handles.map((handle) => `'${handle.workloadId}'::uuid`).join(',');
  const raw = sql(
    `SELECT COALESCE(json_agg(json_build_object(` +
      `'workloadId',id::text,'assignmentId',assignment_id::text,'generation',generation,` +
      `'workerId',worker_id::text,'desiredState',desired_state,'observedState',observed_state,` +
      `'observedSessionEpoch',observed_session_epoch,'observedMessage',observed_message,` +
      `'specHash',encode(spec_hash_sha256,'hex'),'reservedSlots',reserved_slots,` +
      `'requiredReplicas',required_replicas,` +
      `'serviceCount',jsonb_array_length(spec->'services'),` +
      `'replicaCount',(SELECT COALESCE(sum((replica_service->>'replicas')::int),0) ` +
      `FROM jsonb_array_elements(spec->'services') AS services(replica_service))` +
      `) ORDER BY id),'[]'::json)::text FROM "WorkerWorkloads" WHERE id IN (${ids})`,
  );
  return JSON.parse(raw || '[]');
}

function assertStoredWorkloadShapes(
  rows,
  serviceCount = EXPECTED_SERVICE_COUNT,
  replicaCount = EXPECTED_REPLICA_COUNT,
) {
  if (serviceCount === undefined) return;
  const mismatch = rows.find(
    (row) =>
      row.serviceCount !== serviceCount ||
      row.replicaCount !== replicaCount ||
      row.reservedSlots !== 1 ||
      row.requiredReplicas !== replicaCount ||
      !/^[0-9a-f]{64}$/.test(String(row.specHash || '')),
  );
  if (mismatch) {
    throw new Error(
      `workload ${mismatch.workloadId} stores ${mismatch.serviceCount} service(s)/` +
        `${mismatch.replicaCount} replica(s), ${mismatch.reservedSlots} workload slot(s), and ` +
        `${mismatch.requiredReplicas} required replica(s); expected ` +
        `${serviceCount}/${replicaCount}/1/${replicaCount}`,
    );
  }
}

function localReplicaLabels(handles) {
  const rows = [];
  for (const handle of handles) {
    const ids = execFileSync(
      'docker',
      [
        'ps', '--all', '--quiet',
        '--filter', `label=io.rsctf.worker.id=${handle.workerId}`,
        '--filter', `label=io.rsctf.workload.id=${handle.workloadId}`,
      ],
      { encoding: 'utf8' },
    ).trim().split(/\s+/).filter(Boolean);
    if (!ids.length) continue;
    const output = execFileSync(
      'docker',
      ['inspect', '--format', '{{json .Config.Labels}}', ...ids],
      { encoding: 'utf8' },
    );
    for (const line of output.split('\n').filter(Boolean)) rows.push(JSON.parse(line));
  }
  return rows;
}

async function requireLocalReplicaTopology(handles, shape) {
  if (!LOCAL_DOCKER_REPLICA_AUDIT) return;
  if (handles.some((handle) => !handle.workerId)) {
    throw new Error('local Docker replica audit requires each workload worker identity');
  }
  await waitFor(() => {
    const audit = auditReplicaLabels(localReplicaLabels(handles), handles, shape.services);
    if (!audit.valid) throw new Error(audit.errors.join('; '));
    return true;
  }, API_TIMEOUT_MS, `${shape.replicaCount}-replica local Docker topology`);
}

async function requireWorkloadState(handles, state) {
  return waitFor(() => {
    const rows = workloadRows(handles);
    const audit = auditWorkloadRows(rows, handles, state);
    return audit.valid ? rows : undefined;
  }, state === 'Ready' ? API_TIMEOUT_MS : ABSENT_TIMEOUT_MS, `workloads to become ${state}`);
}

async function runProxyLoad(cycle, phase, records, selectedWorkers) {
  if (PROXY_READINESS_DELAY_MS > 0) {
    console.log(`    ${phase} optional post-Ready delay: ${PROXY_READINESS_DELAY_MS}ms`);
    await sleep(PROXY_READINESS_DELAY_MS);
  }
  const endpoints = records.map((record) => ({
    url: proxyWebSocketUrl(TARGET, record.container.entry),
    token: record.token,
  }));
  const status = runK6('worker-plane.js', {
    TARGET,
    WORKER_ENDPOINTS: JSON.stringify(endpoints),
    EXPECTED_WORKERS: JSON.stringify(selectedWorkers.map((worker) => worker.id)),
    ADMIN_TOKEN: adminToken,
    RATE: PROXY_RATE,
    VUS: process.env.VUS || Math.max(10, Number(process.env.RATE || 20)),
    MAX_VUS: process.env.MAX_VUS || '',
    HEALTH_POLL_RATE: process.env.HEALTH_POLL_RATE || process.env.WORKER_POLL_RATE || 1,
    WORKER_INVENTORY_INTERVAL_SECONDS:
      process.env.WORKER_INVENTORY_INTERVAL_SECONDS || 10,
    DURATION: process.env.DURATION || '30s',
    STREAM_TIMEOUT_MS: process.env.STREAM_TIMEOUT_MS || 5000,
    PROBE_PAYLOAD: process.env.PROBE_PAYLOAD || '',
    EXPECT_PROXY_RESPONSE: process.env.EXPECT_PROXY_RESPONSE || 1,
    EXPECTED_RESPONSE_MARKER: process.env.EXPECTED_RESPONSE_MARKER || '',
    MAX_PROXY_FAILURE_RATE: process.env.MAX_PROXY_FAILURE_RATE || '',
    MAX_PROXY_STREAM_FAILURE_RATE: process.env.MAX_PROXY_STREAM_FAILURE_RATE || '',
    MAX_PROXY_RESPONSE_MISSING_RATE: process.env.MAX_PROXY_RESPONSE_MISSING_RATE || '',
    DEBUG_PROXY_ERRORS: process.env.DEBUG_PROXY_ERRORS || '',
    MAX_PROXY_RESPONSE_INVALID_RATE: process.env.MAX_PROXY_RESPONSE_INVALID_RATE || '',
    SUMMARY_JSON: cycleSummaryPath(cycle, phase),
  });
  if (status !== 0) {
    throw new Error(`k6 worker-plane ${phase} scenario failed with exit code ${status}`);
  }
}

async function rolloutWorkloads(challengeId, previousHandles, shape, label, latencies) {
  const saved = await api('PUT', `/api/edit/games/${GAME_ID}/challenges/${challengeId}`, {
    token: adminToken,
    body: { workloadSpec: shape.spec },
  });
  if (saved.status !== 200) throw responseError(saved, `save ${label} workload`);
  rolloutChallengeToRestore = shape === ROLLOUT_UP ? challengeId : undefined;

  const started = Date.now();
  const response = await api(
    'POST',
    `/api/edit/games/${GAME_ID}/challenges/${challengeId}/workload/rollout`,
    { token: adminToken },
  );
  if (response.status !== 200) throw responseError(response, `${label} rollout`);
  const result = unwrapResponse(response.json);
  const incomplete =
    Number(result?.stale || 0) +
    Number(result?.incompatible || 0) +
    Number(result?.insufficientCapacity || 0) +
    Number(result?.failed || 0);
  if (
    Number(result?.matched) !== previousHandles.length ||
    Number(result?.updated) !== previousHandles.length ||
    incomplete !== 0
  ) {
    throw new Error(`${label} rollout was incomplete: ${JSON.stringify(result)}`);
  }

  const expectedHandles = advanceWorkloadHandles(previousHandles);
  const rows = await waitFor(() => {
    const snapshot = workloadRows(expectedHandles);
    const audit = auditWorkloadRows(snapshot, expectedHandles, 'Ready');
    if (!audit.valid) throw new Error(audit.errors.join('; '));
    const moved = snapshot.find((row) => {
      const expected = expectedHandles.find((handle) => handle.workloadId === row.workloadId);
      return row.workerId !== expected?.workerId;
    });
    if (moved) throw new Error(`workload ${moved.workloadId} moved workers during rollout`);
    assertStoredWorkloadShapes(snapshot, shape.serviceCount, shape.replicaCount);
    return snapshot;
  }, API_TIMEOUT_MS, `${label} workloads to converge Ready`);
  const currentHandles = expectedHandles.map((handle) => ({
    ...handle,
    specHash: rows.find((row) => row.workloadId === handle.workloadId)?.specHash,
  }));
  await requireLocalReplicaTopology(currentHandles, shape);
  const elapsed = Date.now() - started;
  latencies.push(elapsed);
  console.log(
    `    ${label}: ${rows.length}/${rows.length} Ready at ${shape.serviceCount} service(s)/` +
      `${shape.replicaCount} replica(s) in ${elapsed}ms; Docker topology exact`,
  );
  return currentHandles;
}

async function runCycle(cycle, players, challengeId, selectedWorkers) {
  console.log(`  cycle ${cycle}/${CYCLES}: creating ${players.length} worker workload(s) concurrently`);
  const settled = await Promise.allSettled(
    players.map(async (player) => {
      const record = await createContainer(player, challengeId, cycle);
      record.challengeId = challengeId;
      return record;
    }),
  );
  const records = settled.filter((result) => result.status === 'fulfilled').map((result) => result.value);
  const failures = settled.filter((result) => result.status === 'rejected');
  if (failures.length) {
    throw new Error(
      `${failures.length}/${players.length} container create(s) failed: ${failures
        .slice(0, 3)
        .map((result) => result.reason.message)
        .join('; ')}`,
    );
  }

  let handles = [];
  if (DB_AUDIT) {
    handles = backendHandles(records.map((record) => record.container.id));
    const rows = await requireWorkloadState(handles, 'Ready');
    assertStoredWorkloadShapes(rows);
    const workerIds = new Set(rows.map((row) => row.workerId));
    const selectedIds = new Set(selectedWorkers.map((worker) => worker.id));
    const unexpected = [...workerIds].filter((id) => !selectedIds.has(id));
    if (unexpected.length) {
      throw new Error(
        `scheduler placed workloads outside the selected worker fleet: ${unexpected.join(', ')}`,
      );
    }
    handles = handles.map((handle) => ({
      ...handle,
      workerId: rows.find((row) => row.workloadId === handle.workloadId)?.workerId,
      specHash: rows.find((row) => row.workloadId === handle.workloadId)?.specHash,
    }));
    if (ROLLOUT_DOWN) await requireLocalReplicaTopology(handles, ROLLOUT_DOWN);
    console.log(`    durable status: ${rows.length}/${records.length} Present/Ready across ${workerIds.size} worker(s)`);
  }

  await runProxyLoad(cycle, 'base', records, selectedWorkers);
  if (ROLLOUT_UP) {
    handles = await rolloutWorkloads(
      challengeId,
      handles,
      ROLLOUT_UP,
      'scale-up',
      scaleUpLatencies,
    );
    await runProxyLoad(cycle, 'scaled', records, selectedWorkers);
    handles = await rolloutWorkloads(
      challengeId,
      handles,
      ROLLOUT_DOWN,
      'scale-down',
      scaleDownLatencies,
    );
    await runProxyLoad(cycle, 'restored', records, selectedWorkers);
  }

  await waitForCooldown(records);
  console.log(`    destroying ${records.length} workload(s)`);
  await Promise.all(records.map((record) => destroyContainer(record)));
  if (DB_AUDIT) {
    const absentHandles = advanceWorkloadHandles(handles);
    await requireWorkloadState(absentHandles, 'Absent');
    console.log(`    durable status: ${absentHandles.length}/${absentHandles.length} Absent/Absent`);
  }
}

async function cleanup() {
  if (rolloutChallengeToRestore && adminToken && ROLLOUT_DOWN) {
    try {
      const restored = await api(
        'PUT',
        `/api/edit/games/${GAME_ID}/challenges/${rolloutChallengeToRestore}`,
        { token: adminToken, body: { workloadSpec: ROLLOUT_DOWN.spec } },
      );
      if (restored.status !== 200) throw responseError(restored, 'restore base workload definition');
      rolloutChallengeToRestore = undefined;
    } catch (error) {
      console.error(`failed to restore base workload definition: ${error.message}`);
    }
  }
  if (workerToRestore && adminToken) {
    try {
      await api('PUT', `/api/admin/workers/${workerToRestore}/state`, {
        token: adminToken,
        body: { state: 'Enabled' },
      });
    } catch (error) {
      console.error(`failed to re-enable worker ${workerToRestore}: ${error.message}`);
    }
  }
  const records = [...liveContainers.values()];
  if (!records.length) return;
  try {
    await waitForCooldown(records);
    await Promise.allSettled(records.map((record) => destroyContainer(record, { cleanup: true })));
  } catch (error) {
    console.error(`worker load cleanup failed: ${error.message}`);
  }
}

async function main() {
  adminToken = resolveAdminToken();
  const challengeId = resolveChallenge();
  assertStoredChallengeShape(challengeId);
  const players = resolvePlayers();
  const proxyRatePerIdentity = assertProxyIdentityRateBudget(
    PROXY_RATE,
    players.map((player) => player.token),
    MAX_PROXY_RATE_PER_IDENTITY,
  );
  const initialHealthMs = await assertHealth('preflight');
  const initialWorkerList = await listWorkers();
  let selectedWorkers = selectOnlineWorkers(initialWorkerList, {
    minimum: MIN_WORKERS,
    workerIds: WORKER_IDS,
    platformOs: WORKER_OS,
  });
  const beforeResources = resourceSample();

  console.log(
    `trusted worker load → ${TARGET} game=${GAME_ID} challenge=${challengeId} ` +
      `fleet=${FLEET} cycles=${CYCLES} workers=${selectedWorkers.length}`,
  );
  console.log(
    `  preflight: live/readiness ${initialHealthMs}ms; ` +
      selectedWorkers
        .map(
          (worker) =>
            `${worker.name}(${worker.platformOs}/${worker.architecture}, slots=${worker.capacity.slots}, epoch=${worker.sessionEpoch})`,
        )
        .join(', '),
  );
  console.log(
    `  proxy identity budget: ${proxyRatePerIdentity.toFixed(3)} request/s per player ` +
      `(guard ${MAX_PROXY_RATE_PER_IDENTITY}/s)`,
  );

  if (RECONNECT_WORKER) {
    const reconnectId = process.env.RECONNECT_WORKER_ID?.toLowerCase();
    const worker = reconnectId
      ? selectedWorkers.find((candidate) => candidate.id === reconnectId)
      : selectedWorkers[0];
    if (!worker) throw new Error(`RECONNECT_WORKER_ID ${reconnectId} is not in the selected fleet`);
    await exerciseReconnect(worker);
    selectedWorkers = selectOnlineWorkers(await listWorkers(), {
      minimum: MIN_WORKERS,
      workerIds: WORKER_IDS,
      platformOs: WORKER_OS,
    });
  }

  const continuityBefore = await listWorkers();
  let lastCycleEndedAt = 0;
  for (let cycle = 1; cycle <= CYCLES; cycle += 1) {
    const remaining = OPERATION_COOLDOWN_MS - (Date.now() - lastCycleEndedAt);
    if (cycle > 1 && remaining > 0) await sleep(remaining);
    await runCycle(cycle, players, challengeId, selectedWorkers);
    lastCycleEndedAt = Date.now();
  }
  assertStoredChallengeShape(challengeId);

  const finalHealthMs = await assertHealth('post-run');
  const continuityAfter = await listWorkers();
  const continuity = auditWorkerContinuity(
    continuityBefore,
    continuityAfter,
    selectedWorkers.map((worker) => worker.id),
    { allowSessionChanges: ALLOW_SESSION_CHANGES },
  );
  if (!continuity.valid) {
    throw new Error(`worker lease/session audit failed: ${continuity.errors.join('; ')}`);
  }
  const afterResources = resourceSample();

  console.log('\n  RESULT — trusted worker lifecycle and proxy load passed');
  console.log(
    `    creates: ${createLatencies.length}, p50 ${percentile(createLatencies, 0.5)}ms, ` +
      `p95 ${percentile(createLatencies, 0.95)}ms, max ${Math.max(...createLatencies)}ms`,
  );
  console.log(
    `    destroys: ${deleteLatencies.length}, p50 ${percentile(deleteLatencies, 0.5)}ms, ` +
      `p95 ${percentile(deleteLatencies, 0.95)}ms, max ${Math.max(...deleteLatencies)}ms`,
  );
  if (scaleUpLatencies.length) {
    console.log(
      `    scale-up convergence: ${scaleUpLatencies.length}, ` +
        `p50 ${percentile(scaleUpLatencies, 0.5)}ms, ` +
        `p95 ${percentile(scaleUpLatencies, 0.95)}ms, max ${Math.max(...scaleUpLatencies)}ms`,
    );
    console.log(
      `    scale-down convergence: ${scaleDownLatencies.length}, ` +
        `p50 ${percentile(scaleDownLatencies, 0.5)}ms, ` +
        `p95 ${percentile(scaleDownLatencies, 0.95)}ms, max ${Math.max(...scaleDownLatencies)}ms`,
    );
  }
  console.log(`    post-run live/readiness: ${finalHealthMs}ms; worker leases and session fences valid`);
  if (beforeResources && afterResources) {
    console.log(
      `    rsctf resources (point samples only): ${beforeResources.cpu}%/${beforeResources.mem} → ` +
        `${afterResources.cpu}%/${afterResources.mem}`,
    );
  }
}

main()
  .catch((error) => {
    console.error('error:', error.message);
    process.exitCode = 1;
  })
  .finally(cleanup);
