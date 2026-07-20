// Exhaustive disposable admin-plane acceptance and fixed-rate read simulation.
// This intentionally covers every registered admin operation once with its real
// contract, then hands the live fixture to k6 for replica/read-path pressure.
import { randomUUID } from 'node:crypto';

import * as A from './applib.mjs';
import {
  ADMIN_OPERATIONS,
  ADMIN_SIGNALR_SURFACES,
  PARTICIPATION_STATUS,
  assertBuildImageFixtureInventory,
  assertCompleteCoverage,
  assertDirectAdminOriginBindings,
  assertDisposableComposeTopology,
  assertExactFailedBuildPruneCandidates,
  assertSafeAdminTarget,
  assertStableZeroResidualSnapshots,
  stableReplicaProjection,
  validateAdminResponse,
} from './admin-lifecycle.js';
import {
  adminApi,
  acquireAdminLifecycleDatabaseLock,
  createWorkerCsr,
  deleteDisposableAdminGame,
  deleteDisposableLoadGame,
  disposableAdminGameRuntimeIds,
  expectStatus,
  inspectUnchangedServerRuntimeIdentity,
  insertBuildRecord,
  inspectUniformServerRuntimeIdentity,
  multipartRequest,
  originalServerRuntimeLogTargets,
  persistRecovery,
  positiveId,
  rawRequest,
  removeRecovery,
  repositoryCleanupRescheduleSql,
  shouldRetainLifecycleManifest,
  scratchChallengeArchive,
  sqlLiteral,
  teamByName,
  unwrap,
  userByEmail,
} from './admin-fixtures.mjs';
import { countContainerFatalLogs } from './log-audit.mjs';
import { docker, mintJwt, PG, RSCTF, runK6, sql, TARGET } from './lib.mjs';
import {
  acquireExclusiveProcessLock,
  loadOrchestrationLockPath,
} from './process-control.mjs';

const tag = `adm${Date.now().toString(36)}`;
const recoveryPath = `/tmp/rsctf-admin-lifecycle-${tag}.json`;
const rawWebTargets = String(process.env.WEB_TARGETS || '').trim();
const webTargets = (rawWebTargets.startsWith('[') ? JSON.parse(rawWebTargets) : rawWebTargets.split(','))
  .map((target) => target.trim().replace(/\/$/, ''))
  .filter(Boolean);
const controlTarget = String(process.env.CONTROL_TARGET || TARGET).replace(/\/$/, '');
const containerImage = process.env.ADMIN_CONTAINER_IMAGE || 'nginx:alpine';
const repositoryUrl = process.env.ADMIN_REPOSITORY_URL || 'https://github.com/dimasma0305/rsctf-challenges.git';
const repositoryRef = process.env.ADMIN_REPOSITORY_REF || 'main';
const repositoryExpectedCommit = String(process.env.ADMIN_REPOSITORY_EXPECTED_COMMIT || '').trim();
const reportableAcceptance = process.env.RSCTF_ACCEPTANCE_REPORTABLE === '1';
if (repositoryExpectedCommit && !/^[0-9a-f]{40}$/i.test(repositoryExpectedCommit)) {
  throw new Error('ADMIN_REPOSITORY_EXPECTED_COMMIT must be a full 40-character Git commit');
}
if (reportableAcceptance && !repositoryExpectedCommit) {
  throw new Error('RSCTF_ACCEPTANCE_REPORTABLE=1 requires ADMIN_REPOSITORY_EXPECTED_COMMIT');
}
const redisContainer = process.env.REDIS_CONTAINER || PG.replace(/-db-(\d+)$/, '-redis-$1');
const runStartedAt = Date.now();
const serverContainers = [...new Set([
  RSCTF,
  ...String(process.env.ADMIN_RSCTF_CONTAINERS || '')
    .split(',')
    .map((name) => name.trim())
    .filter(Boolean),
])];

const state = {
  schemaVersion: 2,
  tag,
  target: TARGET,
  startedAt: runStartedAt,
  completed: false,
  reportable: reportableAcceptance,
  gameIds: [],
  userIds: [],
  teamIds: [],
  workerIds: [],
  repoBindingIds: [],
  buildRecordIds: [],
  buildImageFixtures: [],
  credentialCacheKeys: [],
  participationIds: [],
  containerIds: [],
  runtimeContainerIds: [],
  originalGlobalConfig: null,
  evidence: {},
};
const covered = new Set();
const timing = [];
let lock;
let databaseLock;
let originalGlobalConfig = null;
let fixtureGame = null;
let authorizationGameId = null;
let fixtureChallenge = null;
let fixtureAdChallenge = null;
let fixtureContainerChallenge = null;
let fixtureParticipation = null;
let fixtureUsers = null;
let workerId = null;
let repoBindingId = null;
let repoGameId = null;
let repoChallengeId = null;
let antiCheatBlockId = null;
let containerGuid = null;
let containerRuntimeId = null;

function saveRecovery() {
  persistRecovery(recoveryPath, state);
}

function canonicalPath(path) {
  return String(path).split('?')[0].replace(/\{[^}]+\}/g, '{}');
}

function operationFor(method, template) {
  const wantedMethod = String(method).toUpperCase();
  const wantedPath = canonicalPath(template);
  const matches = ADMIN_OPERATIONS.filter(
    (operation) =>
      String(operation.method).toUpperCase() === wantedMethod &&
      canonicalPath(operation.path) === wantedPath,
  );
  if (matches.length !== 1) {
    throw new Error(
      `admin catalog lookup ${wantedMethod} ${template} matched ${matches.length} operations`,
    );
  }
  return matches[0];
}

function recordCoverage(method, template, response) {
  const operation = operationFor(method, template);
  requireCondition(
    validateAdminResponse(operation.id, response),
    `${operation.id} returned a malformed ${response.status} response`,
  );
  covered.add(operation.id);
  return operation;
}

async function call(method, template, path, options = {}) {
  const started = performance.now();
  const response = await adminApi(method, path, options);
  const operation = recordCoverage(method, template, response);
  timing.push({ id: operation.id, ms: Math.round((performance.now() - started) * 100) / 100 });
  console.log(`  ✓ ${operation.id} (${response.status}, ${timing.at(-1).ms} ms)`);
  return response;
}

async function callRaw(method, template, path, options = {}, expected = 200) {
  const started = performance.now();
  const response = await rawRequest(method, path, options);
  expectStatus(response, expected, `${method} ${path}`);
  const operation = recordCoverage(method, template, response);
  timing.push({ id: operation.id, ms: Math.round((performance.now() - started) * 100) / 100 });
  console.log(`  ✓ ${operation.id} (${response.status}, ${timing.at(-1).ms} ms)`);
  return response;
}

function requireCondition(condition, message) {
  if (!condition) throw new Error(message);
}

function inspectComposeContainer(container, label) {
  const inspected = docker(['inspect', container]);
  requireCondition(inspected.status === 0, `cannot inspect declared disposable ${label} ${container}`);
  let records;
  try {
    records = JSON.parse(inspected.stdout);
  } catch (error) {
    throw new Error(`cannot parse ${label} ${container} inspection: ${error.message}`);
  }
  requireCondition(Array.isArray(records) && records.length === 1, `${label} ${container} inspection is ambiguous`);
  const record = records[0];
  return {
    name: container,
    environment: record?.Config?.Env,
    project: record?.Config?.Labels?.['com.docker.compose.project'],
    service: record?.Config?.Labels?.['com.docker.compose.service'],
    networkAddresses: Object.values(record?.NetworkSettings?.Networks || {}).flatMap((network) =>
      [network?.IPAddress, network?.GlobalIPv6Address].filter(Boolean),
    ),
  };
}

function assertDisposableRuntimeMarker(targets) {
  // This is intentionally the first backing-service inspection in main(). No
  // sql(), Redis mutation, or authenticated application request may precede it.
  const servers = serverContainers.map((container) => inspectComposeContainer(container, 'server'));
  assertDisposableComposeTopology({
    marker: process.env.ADMIN_LIFECYCLE_STACK_MARKER,
    servers,
    postgres: inspectComposeContainer(PG, 'PostgreSQL'),
    redis: inspectComposeContainer(redisContainer, 'Redis'),
  });
  assertDirectAdminOriginBindings({
    webTargets: targets.webTargets,
    controlTarget: targets.controlTarget,
    servers,
  });
  for (const server of servers) {
    requireCondition(
      server.environment.includes('RSCTF_STORAGE_BACKEND=local'),
      `${server.name} must use the disposable local blob backend for exact leak auditing`,
    );
  }
}

async function assertRuntimeRoles() {
  const expected = [
    [TARGET, 'web'],
    ...webTargets.map((target) => [target, 'web']),
    [controlTarget, 'control'],
  ];
  for (const [endpoint, role] of expected) {
    const health = await rawRequest('GET', '/healthz', {
      baseUrl: endpoint,
      jwt: null,
      ip: null,
    });
    requireCondition(health.status === 200, `${endpoint} failed /healthz preflight`);
    requireCondition(
      health.headers.get('x-rsctf-role') === role,
      `${endpoint} reports role ${health.headers.get('x-rsctf-role') || '<missing>'}, expected ${role}`,
    );
  }
}

async function assertGlobalAdminMutationBaseline() {
  const failed = Number(sql('SELECT count(*) FROM "BuildRecords" WHERE status=2'));
  const active = Number(sql('SELECT count(*) FROM "BuildRecords" WHERE status IN (3,5)'));
  requireCondition(
    failed === 0 && active === 0,
    `global build-prune baseline is not empty (failed=${failed}, building/queued=${active})`,
  );
  const inventory = await adminApi('GET', '/api/admin/builds/images');
  expectStatus(inventory, 200, 'global owned-image baseline');
  requireCondition(
    validateAdminResponse('admin_build_images_get', inventory),
    'global owned-image baseline returned a malformed inventory',
  );
  const orphaned = (inventory.json || []).filter(
    (image) => !Array.isArray(image.referencedBy) || image.referencedBy.length === 0,
  );
  requireCondition(
    orphaned.length === 0,
    `global image-prune baseline contains ${orphaned.length} pre-existing unreferenced image(s)`,
  );
  state.globalMutationBaseline = { failedBuilds: failed, activeBuilds: active, orphanedImages: 0 };
  saveRecovery();
}

function buildRecordInventory(predicate = 'TRUE') {
  const raw = sql(
    `SELECT COALESCE(json_agg(json_build_object(` +
      `'id',id,'gameId',game_id,'status',status) ORDER BY id),'[]'::json)::text ` +
      `FROM "BuildRecords" WHERE ${predicate}`,
  );
  const parsed = JSON.parse(raw || '[]');
  requireCondition(Array.isArray(parsed), 'build-record inventory is not an array');
  return parsed;
}

function sameJson(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function normalizedManagedImageTag(value) {
  let image = String(value || '').trim().toLowerCase();
  image = image.replace(/^(?:docker\.io|index\.docker\.io)\//, '');
  const slash = image.lastIndexOf('/');
  if (!image.slice(slash + 1).includes(':')) image += ':latest';
  return image;
}

function canonicalManagedImageTag(value) {
  const normalized = normalizedManagedImageTag(value);
  return normalized ? `docker.io/${normalized}` : '';
}

function imageSlug(title) {
  const slug = String(title)
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
  return slug || 'challenge';
}

function scratchBuildImagePlan(gameId) {
  const id = positiveId(gameId, 'scratch build game');
  return [
    { role: 'delete', title: `Admin Image Delete ${tag}`, flag: `flag{admin_image_delete_${tag}}` },
    { role: 'prune', title: `Admin Image Prune ${tag}`, flag: `flag{admin_image_prune_${tag}}` },
  ].map((fixture) => ({
    ...fixture,
    gameId: id,
    challengeId: null,
    imageRef: `rsctf/${id}/${imageSlug(fixture.title)}:latest`,
    archiveHash: null,
    definitionDeleted: false,
    imageRemoved: false,
  }));
}

function imageInventoryHas(records, imageRef) {
  const wanted = normalizedManagedImageTag(imageRef);
  return Array.isArray(records) && records.some((image) =>
    Array.isArray(image.tags) && image.tags.some((candidate) =>
      normalizedManagedImageTag(candidate) === wanted));
}

async function ownedImageInventory() {
  const response = await adminApi('GET', '/api/admin/builds/images');
  expectStatus(response, 200, 'owned build image inventory');
  requireCondition(
    validateAdminResponse('admin_build_images_get', response),
    'owned build image inventory returned malformed records',
  );
  return response.json;
}

function scratchBuildRows(fixtures) {
  const titles = fixtures.map((fixture) => sqlLiteral(fixture.title)).join(',');
  return JSON.parse(sql(
    `SELECT COALESCE(json_agg(json_build_object(` +
      `'id',id,'title',title,'status',build_status,'imageRef',container_image,` +
      `'digest',build_image_digest,'archiveHash',original_archive_blob_path,` +
      `'log',last_build_log) ORDER BY id),'[]'::json)::text ` +
      `FROM "GameChallenges" WHERE game_id=${positiveId(fixtures[0].gameId, 'scratch build game')} ` +
      `AND title IN (${titles})`,
  ) || '[]');
}

async function waitForOwnedScratchBuilds(fixtures, timeoutMs = 180_000) {
  const deadline = Date.now() + timeoutMs;
  let last = 'no challenge rows';
  while (Date.now() <= deadline) {
    const rows = scratchBuildRows(fixtures);
    const failed = rows.find((row) => [2, 4, 6].includes(Number(row.status)));
    if (failed) throw new Error(`scratch image build failed for ${failed.title}: ${failed.log || `status ${failed.status}`}`);
    if (
      rows.length === fixtures.length &&
      rows.every((row) => Number(row.status) === 1 && typeof row.digest === 'string' && row.digest.length > 0)
    ) {
      for (const fixture of fixtures) {
        const row = rows.find((candidate) => candidate.title === fixture.title);
        requireCondition(row, `scratch build row omitted ${fixture.title}`);
        requireCondition(
          normalizedManagedImageTag(row.imageRef) === normalizedManagedImageTag(fixture.imageRef),
          `${fixture.title} published unexpected image ${row.imageRef}`,
        );
        fixture.challengeId = positiveId(row.id, `${fixture.role} scratch challenge`);
        fixture.imageRef = row.imageRef;
        requireCondition(
          typeof row.archiveHash === 'string' && /^[a-f0-9]{64}$/.test(row.archiveHash),
          `${fixture.title} did not retain its trusted source archive`,
        );
        fixture.archiveHash = row.archiveHash;
      }
      saveRecovery();
      const inventory = await ownedImageInventory();
      try {
        assertBuildImageFixtureInventory(inventory, fixtures, { referenced: true });
        return inventory;
      } catch (error) {
        last = error.message;
      }
    } else {
      last = JSON.stringify(rows);
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error(`scratch builds did not publish owned inventory within ${timeoutMs} ms: ${last}`);
}

async function deleteScratchBuildDefinition(fixture) {
  const rows = String(sql(
    `SELECT id FROM "GameChallenges" WHERE game_id=${positiveId(fixture.gameId, 'scratch build game')} ` +
      `AND title=${sqlLiteral(fixture.title)} ORDER BY id`,
  ) || '').split('\n').filter(Boolean).map(Number);
  requireCondition(rows.length <= 1, `scratch challenge title is ambiguous: ${fixture.title}`);
  if (rows.length === 0) {
    fixture.definitionDeleted = true;
    return false;
  }
  const challengeId = positiveId(rows[0], `${fixture.role} scratch challenge`);
  if (fixture.challengeId) {
    requireCondition(challengeId === fixture.challengeId, `${fixture.title} identity changed before deletion`);
  }
  const response = await A.api(
    'DELETE',
    `/api/edit/games/${positiveId(fixture.gameId, 'scratch build game')}/challenges/${challengeId}`,
    { jwt: A.adminJwt(), timeoutMs: 180_000 },
  );
  expectStatus(response, 200, `delete scratch challenge ${fixture.title}`);
  requireCondition(
    Number(sql(`SELECT count(*) FROM "GameChallenges" WHERE id=${challengeId}`)) === 0,
    `scratch challenge ${fixture.title} survived normal application deletion`,
  );
  if (fixture.archiveHash) {
    requireCondition(
      Number(sql(`SELECT count(*) FROM "Files" WHERE hash=${sqlLiteral(fixture.archiveHash)}`)) === 0,
      `scratch challenge ${fixture.title} retained source-archive metadata`,
    );
    const blobPath = `/data/files/${fixture.archiveHash.slice(0, 2)}/${fixture.archiveHash.slice(2, 4)}/${fixture.archiveHash}`;
    requireCondition(
      docker(['exec', RSCTF, 'test', '!', '-e', blobPath]).status === 0,
      `scratch challenge ${fixture.title} retained source-archive bytes`,
    );
  }
  fixture.challengeId = challengeId;
  fixture.definitionDeleted = true;
  saveRecovery();
  return true;
}

async function cleanupOwnedScratchImage(fixture) {
  const inventory = await ownedImageInventory();
  if (!imageInventoryHas(inventory, fixture.imageRef)) {
    requireCondition(
      docker(['image', 'inspect', fixture.imageRef]).status !== 0,
      `${fixture.imageRef} exists outside the installation-owned inventory`,
    );
    fixture.imageRemoved = true;
    return false;
  }
  const response = await adminApi(
    'DELETE',
    `/api/admin/builds/images?tag=${encodeURIComponent(fixture.imageRef)}&force=false`,
    { timeoutMs: 180_000 },
  );
  requireCondition(
    response.status === 200 && response.json?.removed === 1,
    `cleanup could not remove owned image ${fixture.imageRef}: ${response.text}`,
  );
  requireCondition(
    docker(['image', 'inspect', fixture.imageRef]).status !== 0,
    `owned image ${fixture.imageRef} survived application cleanup`,
  );
  fixture.imageRemoved = true;
  saveRecovery();
  return true;
}

function websocketUrl(baseUrl, path) {
  const url = new URL(path, `${baseUrl}/`);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  return url.toString();
}

async function expectAdminWebSocketRejected(baseUrl, token, label) {
  await new Promise((resolve, reject) => {
    const suffix = token ? `?access_token=${encodeURIComponent(token)}` : '';
    const socket = new WebSocket(websocketUrl(baseUrl, `/hub/admin${suffix}`));
    let settled = false;
    const finish = (action, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      action(value);
    };
    const timeout = setTimeout(() => {
      socket.close();
      finish(reject, new Error(`${label} admin SignalR connection did not terminate`));
    }, 5_000);
    socket.addEventListener('open', () => {
      socket.close();
      finish(reject, new Error(`admin SignalR accepted ${label} credentials`));
    }, { once: true });
    socket.addEventListener('error', () => finish(resolve), { once: true });
    socket.addEventListener('close', () => finish(resolve), { once: true });
  });
}

async function assertAdminSignalRAuth(baseUrl, { adminToken, ordinaryToken, monitorToken }) {
  const negotiatePath = '/hub/admin/negotiate?negotiateVersion=1';
  for (const [label, jwt, expected] of [
    ['missing', null, 401],
    ['ordinary', ordinaryToken, 403],
    ['Monitor', monitorToken, 403],
  ]) {
    const response = await rawRequest('POST', negotiatePath, {
      baseUrl,
      jwt,
      ip: `10.253.4.${expected === 401 ? 1 : expected === 403 && label === 'ordinary' ? 2 : 3}`,
      headers: { 'content-type': 'text/plain;charset=UTF-8', connection: 'close' },
      body: '',
      timeoutMs: 10_000,
      // This probe follows a five-minute child k6 process. One stale Undici
      // socket may race Caddy's idle close; retry only an approved pre-response
      // transport error, never a timeout or an HTTP response.
      networkRetries: 1,
    });
    state.evidence.signalRNegotiateNetworkRetries =
      (state.evidence.signalRNegotiateNetworkRetries || 0) + response.attempts - 1;
    requireCondition(
      response.status === expected,
      `${label} admin SignalR negotiate returned ${response.status}, expected ${expected}`,
    );
  }

  await expectAdminWebSocketRejected(baseUrl, null, 'missing');
  await expectAdminWebSocketRejected(baseUrl, ordinaryToken, 'ordinary');
  await expectAdminWebSocketRejected(baseUrl, monitorToken, 'Monitor');

  await new Promise((resolve, reject) => {
    const path = `/hub/admin?access_token=${encodeURIComponent(adminToken)}`;
    const socket = new WebSocket(websocketUrl(baseUrl, path));
    const timeout = setTimeout(() => {
      socket.close();
      reject(new Error('authenticated admin SignalR handshake timed out'));
    }, 5_000);
    let opened = false;
    socket.addEventListener('open', () => {
      opened = true;
      socket.send('{"protocol":"json","version":1}\u001e');
    }, { once: true });
    socket.addEventListener('message', (event) => {
      const frames = String(event.data).split('\u001e').map((frame) => frame.trim()).filter(Boolean);
      if (!frames.includes('{}')) return;
      clearTimeout(timeout);
      socket.close();
      resolve();
    });
    socket.addEventListener('error', () => {
      clearTimeout(timeout);
      reject(new Error('authenticated admin SignalR connection failed'));
    }, { once: true });
    socket.addEventListener('close', () => {
      if (!opened) {
        clearTimeout(timeout);
        reject(new Error('authenticated admin SignalR connection closed before upgrade'));
      }
    });
  });
}

function exactJson(response, label) {
  requireCondition(response.json !== undefined, `${label} did not return JSON`);
  return unwrap(response);
}

function materializeCatalogPath(template, fixture) {
  const values = {
    id: fixture.gameId,
    gameid: fixture.gameId,
    game_id: fixture.gameId,
    userid: fixture.userId,
    challengeid: fixture.challengeId,
    auditid: fixture.auditId,
  };
  let path = template.replace(/\{([^}]+)\}/g, (_, key) => {
    const normalized = key.toLowerCase();
    if (normalized === 'id' && template.includes('/instances/')) return fixture.containerId;
    if (normalized === 'id' && template.includes('/anticheatblocks/')) return fixture.antiCheatId;
    if (normalized === 'id' && template.includes('/repobindings/')) return fixture.bindingId;
    if (normalized === 'id' && template.includes('/teams/')) return fixture.teamId;
    if (normalized === 'id' && template.includes('/participation/')) return fixture.participationId;
    if (normalized === 'id' && template.includes('/workers/')) return fixture.workerId;
    return values[normalized] ?? fixture.gameId;
  });
  if (template === '/api/admin/builds/images') path += '?tag=rsctf/admin-auth-probe:none&force=false';
  return path;
}

async function authorizationMatrix(fixture) {
  console.log('\nauthorization matrix (every operation)…');
  const unrelated = mintJwt(fixture.userId, undefined, 1);
  const alternateUnrelated = mintJwt(fixture.alternateUserId, undefined, 1);
  const monitor = mintJwt(fixture.monitorUserId, fixture.monitorStamp, 2);
  const alternateMonitor = mintJwt(
    fixture.alternateMonitorUserId,
    fixture.alternateMonitorStamp,
    2,
  );
  const crossGameManager = mintJwt(
    fixture.crossGameManagerUserId,
    fixture.crossGameManagerStamp,
    1,
  );
  let index = 0;
  let monitorChecks = 0;
  for (const operation of ADMIN_OPERATIONS) {
    if (operation.path === '/api/workers/enroll') continue;
    const path = materializeCatalogPath(operation.path, fixture);
    const body = operation.method === 'GET' || operation.method === 'DELETE' ? undefined : {};
    const unauthenticated = await A.api(operation.method, path, {
      body,
      ip: `10.253.1.${(index % 240) + 1}`,
      timeoutMs: 30_000,
    });
    requireCondition(
      unauthenticated.status === 401,
      `${operation.id} missing-token authorization returned ${unauthenticated.status}`,
    );
    // The two diagnostic routes share the intentionally tight Concurrency
    // bucket. Use a second ordinary account for the latter so this auth test
    // measures authorization rather than tripping a 10-second rate limit.
    const unprivilegedJwt = operation.id === 'admin_email_test' ? alternateUnrelated : unrelated;
    const unprivileged = await A.api(operation.method, path, {
      body,
      jwt: unprivilegedJwt,
      ip: `10.253.2.${(index % 240) + 1}`,
      timeoutMs: 30_000,
    });
    requireCondition(
      unprivileged.status === 403,
      `${operation.id} unprivileged authorization returned ${unprivileged.status}`,
    );
    if (operation.auth === 'admin') {
      const monitorJwt = operation.id === 'admin_email_test' ? alternateMonitor : monitor;
      const monitorResponse = await A.api(operation.method, path, {
        body,
        jwt: monitorJwt,
        ip: `10.253.5.${(index % 240) + 1}`,
        timeoutMs: 30_000,
      });
      requireCondition(
        monitorResponse.status === 403,
        `${operation.id} Monitor authorization returned ${monitorResponse.status}`,
      );
      monitorChecks += 1;
    }
    index += 1;
  }

  const crossGameMutation = await A.api(
    'PUT',
    `/api/admin/participation/${fixture.participationId}`,
    {
      body: { status: 'Suspended' },
      jwt: crossGameManager,
      ip: '10.253.6.1',
      timeoutMs: 30_000,
    },
  );
  requireCondition(
    crossGameMutation.status === 403,
    `cross-game manager authorization returned ${crossGameMutation.status}`,
  );
  requireCondition(
    Number(sql(`SELECT status FROM "Participations" WHERE id=${fixture.participationId}`)) ===
      PARTICIPATION_STATUS.Accepted,
    'cross-game manager changed participation state despite rejection',
  );

  const invalidEnrollment = await A.api('POST', '/api/workers/enroll', {
    body: { token: 'invalid-admin-lifecycle-token', csrPem: 'invalid-csr' },
    ip: '10.253.3.1',
  });
  requireCondition(
    invalidEnrollment.status === 401,
    `invalid worker enrollment returned ${invalidEnrollment.status}, expected 401 after issuer setup`,
  );
  console.log(
    `  ✓ ${index} admin operations reject missing and ordinary credentials; ` +
      `${monitorChecks} Admin-only operations reject Monitor; cross-game manager rejected`,
  );
}

async function identityLifecycle() {
  console.log('\nidentity, team, and configuration lifecycle…');
  const names = {
    profileEmail: `${tag}.profile@admin.invalid`,
    managerEmail: `${tag}.manager@admin.invalid`,
    captainEmail: `${tag}.captain@admin.invalid`,
    pollerEmail: `${tag}.poller@admin.invalid`,
    monitorEmail: `${tag}.monitor@admin.invalid`,
    importEmail: `${tag}.import@admin.invalid`,
    cacheDeleteEmail: `${tag}.cache-delete@admin.invalid`,
    team: `ADMINLT-${tag}`,
  };
  await call('POST', '/api/admin/users', '/api/admin/users', {
    body: [
      {
        userName: `${tag}profile`,
        password: `Adm-${tag}-Profile!9`,
        email: names.profileEmail,
        realName: 'Admin lifecycle profile',
      },
      {
        userName: `${tag}manager`,
        password: `Adm-${tag}-Manager!9`,
        email: names.managerEmail,
        realName: 'Admin lifecycle manager',
      },
      {
        userName: `${tag}captain`,
        password: `Adm-${tag}-Captain!9`,
        email: names.captainEmail,
        realName: 'Admin lifecycle captain',
        teamName: names.team,
      },
      {
        userName: `${tag}poller`,
        password: `Adm-${tag}-Poller!9`,
        email: names.pollerEmail,
        realName: 'Admin lifecycle poller',
      },
      {
        userName: `${tag}monitor`,
        password: `Adm-${tag}-Monitor!9`,
        email: names.monitorEmail,
        realName: 'Admin lifecycle alternate monitor',
      },
    ],
  });

  const profile = userByEmail(names.profileEmail);
  const manager = userByEmail(names.managerEmail);
  const captain = userByEmail(names.captainEmail);
  const poller = userByEmail(names.pollerEmail);
  const monitor = userByEmail(names.monitorEmail);
  const team = teamByName(names.team);
  state.userIds.push(profile.id, manager.id, captain.id, poller.id, monitor.id);
  state.teamIds.push(team.id);
  saveRecovery();

  const importedResponse = await call('POST', '/api/admin/users/import', '/api/admin/users/import', {
    body: {
      rows: [
        {
          email: names.importEmail,
          realName: 'Admin lifecycle imported user',
          userNameOverride: `${tag}import`,
        },
        {
          email: names.cacheDeleteEmail,
          realName: 'Admin lifecycle cached credential deletion user',
          userNameOverride: `${tag}cachedelete`,
        },
      ],
      teamMode: 'none',
      emailConfirmed: true,
    },
  });
  const importedModel = exactJson(importedResponse, 'user import');
  requireCondition(importedModel.created === 2, 'user import did not create exactly two users');
  requireCondition(
    importedResponse.json?.users?.length === 2 && importedResponse.json.users.every(
      (user) => typeof user.password === 'string' && user.password.length >= 8,
    ),
    'user import did not return both one-time passwords',
  );
  const imported = userByEmail(names.importEmail);
  const cacheDelete = userByEmail(names.cacheDeleteEmail);
  state.userIds.push(imported.id, cacheDelete.id);
  state.credentialCacheKeys = [names.importEmail, names.cacheDeleteEmail]
    .map((email) => `credimport:${email.trim().toUpperCase()}`);
  requireCondition(
    redisKeyExists(state.credentialCacheKeys[1]) === 1,
    'import did not publish the deletion-regression credential into shared Redis',
  );
  state.evidence.cachedCredentialUserId = cacheDelete.id;
  // Publish the fixture as soon as every identity exists so cleanup remains
  // possible when a later assertion fails before role promotion completes.
  fixtureUsers = { names, profile, manager, captain, poller, monitor, imported, cacheDelete, team };
  saveRecovery();

  const list = await call('GET', '/api/admin/users', `/api/admin/users?count=500&search=${tag}`);
  requireCondition(
    Array.isArray(list.json?.data) && list.json.data.length >= 7,
    'admin user list did not include the identity fixture',
  );
  const search = await call(
    'POST',
    '/api/admin/users/search',
    `/api/admin/users/search?hint=${encodeURIComponent(tag)}`,
  );
  requireCondition(search.json?.total >= 7, 'admin user search did not use the query hint');
  const detail = await call(
    'GET',
    '/api/admin/users/{userid}',
    `/api/admin/users/${profile.id}`,
  );
  requireCondition(detail.json?.userId === profile.id, 'admin user detail returned the wrong user');
  await call('PUT', '/api/admin/users/{userid}', `/api/admin/users/${profile.id}`, {
    body: { bio: `profile updated by ${tag}`, phone: '+6200000000' },
  });

  const reset = await callRaw(
    'DELETE',
    '/api/admin/users/{userid}/password',
    `/api/admin/users/${profile.id}/password`,
  );
  requireCondition(
    /private/i.test(reset.headers.get('cache-control') || '') &&
      /no-store/i.test(reset.headers.get('cache-control') || ''),
    'password reset response is missing private, no-store',
  );
  requireCondition(typeof reset.json === 'string' && reset.json.length >= 8, 'password reset did not return a password');

  await call('DELETE', '/api/admin/users/{userid}', `/api/admin/users/${profile.id}`);
  requireCondition(
    Number(sql(`SELECT count(*) FROM "AspNetUsers" WHERE id=${sqlLiteral(profile.id)}::uuid`)) === 0,
    'admin user delete left the profile fixture behind',
  );
  // Keep every created identity in the recovery ledger after its ordinary
  // delete so the final audit can prove the exact UUID remains absent.

  const credentialSend = await callRaw(
    'POST',
    '/api/admin/users/credentials/send',
    '/api/admin/users/credentials/send',
    {
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        items: [{
          email: names.importEmail,
          userName: importedModel.users.find((user) => user.email === names.importEmail)?.userName,
        }],
      }),
    },
  );
  requireCondition(
    /private/i.test(credentialSend.headers.get('cache-control') || '') &&
      /no-store/i.test(credentialSend.headers.get('cache-control') || ''),
    'credential delivery response is missing private, no-store',
  );
  requireCondition(
    Number(credentialSend.json?.sent) + Number(credentialSend.json?.failed) === 1,
    'credential delivery did not return one truthful per-recipient result',
  );
  if (process.env.ADMIN_REQUIRE_SMTP === '1') {
    requireCondition(credentialSend.json.sent === 1, `required SMTP delivery failed: ${credentialSend.text}`);
  } else {
    requireCondition(
      credentialSend.json.sent === 0 && credentialSend.json.failed === 1,
      'an unconfigured SMTP sender falsely reported credential delivery',
    );
  }

  const teamCount = Number(sql('SELECT count(*) FROM "Teams"'));
  const teamSkip = Math.max(0, teamCount - 500);
  const teams = await call(
    'GET',
    '/api/admin/teams',
    `/api/admin/teams?count=500&skip=${teamSkip}`,
  );
  requireCondition(teams.json?.data?.some((item) => item.id === team.id), 'admin team list omitted fixture team');
  const teamSearch = await call(
    'POST',
    '/api/admin/teams/search',
    `/api/admin/teams/search?hint=${encodeURIComponent(tag)}`,
  );
  requireCondition(teamSearch.json?.data?.some((item) => item.id === team.id), 'team search omitted fixture team');
  await call('PUT', '/api/admin/teams/{id}', `/api/admin/teams/${team.id}`, {
    body: { bio: `team updated by ${tag}`, locked: false },
  });

  // A distinct admin identity keeps fixed-rate k6 polling out of the mutation
  // identity's distributed rate-limit bucket.
  await adminApi('PUT', `/api/admin/users/${poller.id}`, { body: { role: 'Admin' } });
  const promotedPoller = userByEmail(names.pollerEmail);
  requireCondition(promotedPoller.role === 3, 'polling identity was not promoted to Admin');

  await adminApi('PUT', `/api/admin/users/${imported.id}`, { body: { role: 'Monitor' } });
  await adminApi('PUT', `/api/admin/users/${monitor.id}`, { body: { role: 'Monitor' } });
  const promotedImported = userByEmail(names.importEmail);
  const promotedMonitor = userByEmail(names.monitorEmail);
  requireCondition(
    promotedImported.role === 2 && promotedMonitor.role === 2,
    'authorization identities were not promoted to Monitor',
  );

  fixtureUsers = {
    names,
    profile,
    manager,
    captain,
    poller: promotedPoller,
    monitor: promotedMonitor,
    imported: promotedImported,
    cacheDelete,
    team,
  };
  return fixtureUsers;
}

async function configurationLifecycle() {
  const initial = await call('GET', '/api/admin/config', '/api/admin/config');
  const model = exactJson(initial, 'admin config');
  requireCondition(model.globalConfig, 'admin config omitted globalConfig');
  originalGlobalConfig = structuredClone(model.globalConfig);
  // Persist the restore value before the first global mutation. A hard-killed
  // runner cannot execute finally-cleanup, so its mode-0600 manifest must
  // retain enough exact state for a later operator recovery.
  state.originalGlobalConfig = structuredClone(originalGlobalConfig);
  saveRecovery();
  requireCondition(
    !originalGlobalConfig.logoHash && !originalGlobalConfig.faviconHash,
    'branding lifecycle requires an empty disposable branding slot',
  );

  const changed = {
    ...originalGlobalConfig,
    title: `RSCTF admin lifecycle ${tag}`,
    slogan: `replica convergence ${tag}`,
  };
  await call('PUT', '/api/admin/config', '/api/admin/config', { body: { globalConfig: changed } });
  for (const [index, baseUrl] of webTargets.entries()) {
    const replica = await adminApi('GET', '/api/admin/config', {
      baseUrl,
      ip: `10.252.10.${index + 1}`,
    });
    requireCondition(
      replica.json?.globalConfig?.title === changed.title,
      `web replica ${index + 1} did not converge on the config mutation`,
    );
  }

  const onePixelPng = Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=',
    'base64',
  );
  const uploaded = await multipartRequest('/api/admin/config/logo', {
    filename: `${tag}.png`,
    content: onePixelPng,
    contentType: 'image/png',
  });
  recordCoverage('POST', '/api/admin/config/logo', uploaded);
  console.log('  ✓ admin.config.logo.upload');
  const afterUpload = await adminApi('GET', '/api/admin/config');
  const logoHash = afterUpload.json?.globalConfig?.logoHash;
  requireCondition(
    typeof logoHash === 'string' && logoHash === afterUpload.json?.globalConfig?.faviconHash,
    'logo upload did not atomically publish matching logo/favicon hashes',
  );
  const asset = await rawRequest('GET', `/assets/${logoHash}/${tag}.png`, { jwt: null, ip: null });
  requireCondition(asset.status === 200 && asset.bytes.length === onePixelPng.length, 'uploaded logo is not servable');

  await call('DELETE', '/api/admin/config/logo', '/api/admin/config/logo');
  const afterDelete = await adminApi('GET', '/api/admin/config');
  requireCondition(
    afterDelete.json?.globalConfig?.logoHash === null &&
      afterDelete.json?.globalConfig?.faviconHash === null,
    'logo delete did not clear both branding hashes',
  );

  const myIp = await call('GET', '/api/admin/MyIp', '/api/admin/MyIp');
  requireCondition(typeof myIp.json?.detectedIp === 'string', 'MyIp did not resolve an address');
  await call('POST', '/api/admin/captcha/test', '/api/admin/captcha/test', {
    body: { config: { provider: 'HashPow' } },
  });
  const diagnosticJwt = mintJwt(
    fixtureUsers.poller.id,
    userByEmail(fixtureUsers.names.pollerEmail).stamp,
    3,
  );
  const email = await call('POST', '/api/admin/email/test', '/api/admin/email/test', {
    jwt: diagnosticJwt,
    body: {
      config: { senderAddress: 'not-a-mailbox', smtp: { host: '127.0.0.1', port: 1 } },
      recipient: `${tag}@admin.invalid`,
    },
    expected: 400,
  });
  requireCondition(/not sent|failed|smtp/i.test(email.text), 'email diagnostic did not explain its rejected delivery');
}

async function eventFixture() {
  console.log('\nevent, participation, submission, writeup, A&D, and container fixture…');
  const now = A.nowMs();
  fixtureGame = await A.createGame({
    title: `ADMIN-LIFECYCLE-${tag}`,
    hidden: false,
    practiceMode: false,
    acceptWithoutReview: true,
    start: now - 3_600_000,
    end: now + 3_600_000,
    teamMemberCountLimit: 0,
    containerCountLimit: 3,
    allowUserSubmissions: false,
    adTickSeconds: 30,
    adFlagLifetimeTicks: 5,
  });
  state.gameIds.push(fixtureGame);
  saveRecovery();
  sql(
    `UPDATE "Games" SET writeup_required=TRUE, writeup_deadline=clock_timestamp()+interval '1 hour', ` +
      `writeup_note=${sqlLiteral(`admin lifecycle ${tag}`)}, ad_warmup_seconds=1, ad_tick_seconds=30 ` +
      `WHERE id=${fixtureGame}`,
  );

  const cohort = A.seedCohort(fixtureGame, 2);
  state.userIds.push(...cohort.userIds);
  state.teamIds.push(...cohort.teamIds);
  state.participationIds.push(...cohort.partIds);
  fixtureParticipation = cohort.partIds[0];
  const playerId = cohort.userIds[0];
  const playerStamp = sql(
    `SELECT security_stamp FROM "AspNetUsers" WHERE id=${sqlLiteral(playerId)}::uuid`,
  );
  const playerJwt = mintJwt(playerId, playerStamp, 1);
  sql(
    `INSERT INTO "GameManagers"(game_id,user_id) VALUES (` +
      `${fixtureGame},${sqlLiteral(fixtureUsers.manager.id)}::uuid)`,
  );
  const managerJwt = mintJwt(fixtureUsers.manager.id, fixtureUsers.manager.stamp, 1);
  const unrelatedCheck = await A.api(
    'PUT',
    `/api/admin/participation/${fixtureParticipation}`,
    { jwt: playerJwt, body: { status: 'Suspended' }, ip: '10.252.20.1' },
  );
  requireCondition(unrelatedCheck.status === 403, 'unrelated player could mutate participation state');
  await call(
    'PUT',
    '/api/admin/participation/{id}',
    `/api/admin/participation/${fixtureParticipation}`,
    { jwt: managerJwt, body: { status: 'Suspended' }, ip: '10.252.20.2' },
  );
  requireCondition(
    Number(sql(`SELECT status FROM "Participations" WHERE id=${fixtureParticipation}`)) ===
      PARTICIPATION_STATUS.Suspended,
    'same-game manager mutation returned success without persisting Suspended',
  );
  await adminApi('PUT', `/api/admin/participation/${fixtureParticipation}`, {
    body: { status: 'Accepted' },
    ip: '10.252.20.3',
  });
  requireCondition(
    Number(sql(`SELECT status FROM "Participations" WHERE id=${fixtureParticipation}`)) ===
      PARTICIPATION_STATUS.Accepted,
    'participation did not return to Accepted',
  );

  authorizationGameId = await A.createGame({
    title: `ADMIN-AUTHORIZATION-${tag}`,
    hidden: true,
    practiceMode: false,
    acceptWithoutReview: true,
    start: now + 86_400_000,
    end: now + 90_000_000,
    teamMemberCountLimit: 0,
    containerCountLimit: 0,
    allowUserSubmissions: false,
  });
  state.gameIds.push(authorizationGameId);
  sql(
    `BEGIN; ` +
      `DELETE FROM "GameManagers" WHERE game_id=${fixtureGame} ` +
        `AND user_id=${sqlLiteral(fixtureUsers.manager.id)}::uuid; ` +
      `INSERT INTO "GameManagers"(game_id,user_id) VALUES (` +
        `${authorizationGameId},${sqlLiteral(fixtureUsers.manager.id)}::uuid); ` +
      `COMMIT;`,
  );
  saveRecovery();

  fixtureChallenge = await A.createChallenge(fixtureGame, {
    title: `admin-static-${tag}`,
    category: 'Web',
    type: 'StaticAttachment',
  });
  await A.setChallenge(fixtureGame, fixtureChallenge, {
    content: 'admin lifecycle challenge',
    originalScore: 1000,
    minScoreRate: 0.25,
    difficulty: 2,
  });
  const fixtureFlag = `flag{admin_lifecycle_${tag}}`;
  await A.addFlags(fixtureGame, fixtureChallenge, [fixtureFlag]);
  await A.setChallenge(fixtureGame, fixtureChallenge, { isEnabled: true });

  fixtureContainerChallenge = await A.createChallenge(fixtureGame, {
    title: `admin-box-${tag}`,
    category: 'Pwn',
    type: 'StaticContainer',
  });
  await A.setChallenge(fixtureGame, fixtureContainerChallenge, {
    containerImage,
    memoryLimit: 64,
    cpuCount: 1,
    exposePort: 80,
    enableTrafficCapture: false,
  });
  await A.rebuildChallengeImage(
    fixtureGame,
    fixtureContainerChallenge,
    containerImage,
    'admin lifecycle container challenge',
  );
  await A.addFlags(fixtureGame, fixtureContainerChallenge, [`flag{admin_box_${tag}}`]);
  await A.setChallenge(fixtureGame, fixtureContainerChallenge, { isEnabled: true });

  fixtureAdChallenge = await A.createChallenge(fixtureGame, {
    title: `admin-ad-${tag}`,
    category: 'Pwn',
    type: 'AttackDefense',
  });
  const checkerDirectory = A.prepareExactChecker(fixtureGame, fixtureAdChallenge);
  await A.setChallenge(fixtureGame, fixtureAdChallenge, {
    adSelfHosted: true,
    adAllowSelfReset: true,
    adCheckerImage: checkerDirectory,
  });
  await A.addFlags(fixtureGame, fixtureAdChallenge, ['flag{admin_ad_placeholder}']);
  await A.setChallenge(fixtureGame, fixtureAdChallenge, { isEnabled: true });

  const solve = await A.api(
    'POST',
    `/api/game/${fixtureGame}/challenges/${fixtureChallenge}`,
    { jwt: playerJwt, ip: '10.252.21.1', body: { flag: fixtureFlag } },
  );
  expectStatus(solve, 200, 'fixture challenge solve');
  await adminApi(
    'POST',
    `/api/game/${fixtureGame}/challenges/${fixtureChallenge}/review`,
    {
      jwt: playerJwt,
      ip: '10.252.21.2',
      body: { rating: 4, comment: `admin lifecycle review ${tag}` },
    },
  );
  const minimalPdf = Buffer.from(
    `%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\ntrailer<</Root 1 0 R>>\n` +
      `% ADMIN-LIFECYCLE-${tag}\n%%EOF\n`,
  );
  await multipartRequest(`/api/game/${fixtureGame}/writeup`, {
    filename: `${tag}.pdf`,
    content: minimalPdf,
    contentType: 'application/pdf',
    jwt: playerJwt,
    ip: '10.252.21.3',
  });
  const writeupArtifact = JSON.parse(
    sql(
      `SELECT json_build_object(` +
        `'id',file.id,'hash',file.hash,'referenceCount',file.reference_count` +
      `)::text FROM "Participations" participation ` +
      `JOIN "Files" file ON file.id=participation.writeup_id ` +
      `WHERE participation.id=${fixtureParticipation} AND participation.game_id=${fixtureGame}`,
    ),
  );
  requireCondition(
    Number.isSafeInteger(writeupArtifact.id) &&
      /^[0-9a-f]{64}$/.test(writeupArtifact.hash) &&
      writeupArtifact.referenceCount === 1,
    `writeup fixture did not create one uniquely owned blob: ${JSON.stringify(writeupArtifact)}`,
  );
  state.evidence.writeup = writeupArtifact;
  saveRecovery();

  const containerResponse = await A.api(
    'POST',
    `/api/game/${fixtureGame}/container/${fixtureContainerChallenge}`,
    { jwt: playerJwt, ip: '10.252.21.4', timeoutMs: 120_000 },
  );
  expectStatus(containerResponse, 200, 'fixture container create');
  containerGuid = containerResponse.json?.id;
  requireCondition(
    /^[0-9a-f-]{36}$/i.test(containerGuid || ''),
    `container create returned an invalid id: ${containerResponse.text}`,
  );
  state.containerIds.push(containerGuid);
  containerRuntimeId = sql(
    `SELECT container_id FROM "Containers" WHERE id=${sqlLiteral(containerGuid)}::uuid`,
  );
  requireCondition(containerRuntimeId.length > 0, 'fixture container row omitted its runtime id');
  state.runtimeContainerIds.push(containerRuntimeId);

  const submissionId = positiveId(
    sql(
      `SELECT id FROM "Submissions" WHERE game_id=${fixtureGame} ` +
        `AND participation_id=${fixtureParticipation} AND challenge_id=${fixtureChallenge} ` +
        `ORDER BY id DESC LIMIT 1`,
    ),
    'fixture submission',
  );
  state.evidence.submissionId = submissionId;
  state.evidence.suspicionId = positiveId(
    sql(
      `WITH inserted AS (` +
        `INSERT INTO "SuspicionEvents"(` +
          `game_id,participation_id,challenge_id,kind,evidence_key,score_delta,created_at) VALUES (` +
          `${fixtureGame},${fixtureParticipation},${fixtureChallenge},0,` +
          `${sqlLiteral(`submission:${submissionId}`)},10,clock_timestamp()) RETURNING id` +
        `) SELECT id FROM inserted`,
    ),
    'suspicion event',
  );
  state.evidence.flagEgressId = positiveId(
    sql(
      `WITH inserted AS (` +
        `INSERT INTO "FlagEgressEvents"(` +
          `game_id,participation_id,challenge_id,container_id,remote_ip,remote_port,hit_count,` +
          `first_seen_utc,last_seen_utc) VALUES (` +
          `${fixtureGame},${fixtureParticipation},${fixtureChallenge},NULL,'203.0.113.10',31337,2,` +
          `clock_timestamp(),clock_timestamp()) RETURNING id` +
        `) SELECT id FROM inserted`,
    ),
    'flag egress event',
  );
  antiCheatBlockId = positiveId(
    sql(
      `WITH inserted AS (` +
        `INSERT INTO "AntiCheatBlocks"(` +
          `user_id,user_name,conflict_user_id,conflict_user_name,kind,conflicting_value,occurred_at_utc) ` +
          `VALUES (${sqlLiteral(playerId)}::uuid,${sqlLiteral(`${tag}-player`)},` +
          `${sqlLiteral(cohort.userIds[1])}::uuid,${sqlLiteral(`${tag}-peer`)},'Ip','203.0.113.10',` +
          `clock_timestamp()) RETURNING id` +
        `) SELECT id FROM inserted`,
    ),
    'anti-cheat block',
  );
  state.evidence.antiCheatBlockId = antiCheatBlockId;
  saveRecovery();

  return { cohort, playerId, playerJwt, fixtureFlag, minimalPdf };
}

async function observabilityAndRuntime() {
  console.log('\nadmin observability and runtime endpoints…');
  const dashboard = await call('GET', '/api/admin/dashboard', '/api/admin/dashboard');
  requireCondition(dashboard.json?.systemStats?.userCount > 0, 'dashboard user count is not populated');

  const egress = await call(
    'GET',
    '/api/admin/Games/{id}/FlagEgress',
    `/api/admin/Games/${fixtureGame}/FlagEgress?count=20&skip=0`,
  );
  requireCondition(
    egress.json?.data?.some((item) => item.id === state.evidence.flagEgressId),
    'flag-egress feed omitted the fixture event',
  );

  const trend = await call(
    'GET',
    '/api/admin/submissiontrend',
    '/api/admin/submissiontrend?range=Day',
  );
  requireCondition(Array.isArray(trend.json) && trend.json.some((bucket) => bucket.count > 0), 'Day trend is empty');
  for (const range of ['Week', 'Month', 'Year']) {
    const response = await adminApi('GET', `/api/admin/submissiontrend?range=${range}`);
    requireCondition(Array.isArray(response.json), `${range} trend is not an array`);
  }

  const reviews = await call('GET', '/api/admin/reviews', '/api/admin/reviews?count=100&skip=0');
  requireCondition(
    reviews.json?.some((review) => review.challengeId === fixtureChallenge),
    'review feed omitted the fixture review',
  );
  const cheats = await call(
    'GET',
    '/api/admin/cheat-reports',
    '/api/admin/cheat-reports?count=100&skip=0',
  );
  requireCondition(
    cheats.json?.some((report) => report.submitTeam?.id === fixtureParticipation),
    'cheat report feed omitted the fixture suspicion',
  );
  const writeups = await call('GET', '/api/admin/writeups', '/api/admin/writeups?count=100&skip=0');
  requireCondition(
    writeups.json?.some((writeup) => writeup.id === fixtureParticipation),
    'global writeup feed omitted the fixture PDF',
  );
  const gameWriteups = await call(
    'GET',
    '/api/admin/writeups/{id}',
    `/api/admin/writeups/${fixtureGame}`,
  );
  requireCondition(
    gameWriteups.json?.writeups?.some((writeup) => writeup.id === fixtureParticipation),
    'game writeup feed omitted the fixture PDF',
  );
  const archive = await callRaw(
    'GET',
    '/api/admin/writeups/{id}/all',
    `/api/admin/writeups/${fixtureGame}/all`,
  );
  requireCondition(
    archive.headers.get('content-type') === 'application/zip' &&
      archive.bytes[0] === 0x50 && archive.bytes[1] === 0x4b,
    'writeup archive is not a ZIP payload',
  );

  const logs = await call(
    'GET',
    '/api/admin/logs',
    `/api/admin/logs?count=100&skip=0&search=${encodeURIComponent('AdminController')}`,
  );
  requireCondition(Array.isArray(logs.json), 'admin logs response is not an array');
  const files = await call('GET', '/api/admin/files', '/api/admin/files?count=100&skip=0');
  requireCondition(
    files.json?.data?.some((file) => /Writeup-/i.test(file.name)),
    'file inventory omitted the fixture writeup',
  );

  const instances = await call('GET', '/api/admin/instances', '/api/admin/instances?count=100&skip=0');
  requireCondition(
    instances.json?.data?.some((instance) => instance.containerGuid === containerGuid),
    'instance inventory omitted the live fixture container',
  );
  const stats = await call(
    'GET',
    '/api/admin/instances/{id}/stats',
    `/api/admin/instances/${containerGuid}/stats`,
  );
  requireCondition(Number.isFinite(stats.json?.cpuPercent), 'instance stats omitted cpuPercent');

  const antiCheat = await call(
    'GET',
    '/api/admin/anticheatblocks',
    '/api/admin/anticheatblocks?count=100',
  );
  requireCondition(
    antiCheat.json?.some((block) => block.id === antiCheatBlockId),
    'anti-cheat inventory omitted the fixture block',
  );
  await call(
    'DELETE',
    '/api/admin/anticheatblocks/{id}',
    `/api/admin/anticheatblocks/${antiCheatBlockId}`,
  );
  requireCondition(
    Number(sql(`SELECT count(*) FROM "AntiCheatBlocks" WHERE id=${antiCheatBlockId}`)) === 0,
    'anti-cheat block delete did not persist',
  );
  antiCheatBlockId = null;

  // Read consistency is asserted on stable projections; volatile timestamps and
  // request-origin fields are deliberately excluded by the catalog helper.
  for (const operation of [
    operationFor('GET', '/api/admin/dashboard'),
    operationFor('GET', '/api/admin/config'),
    operationFor('GET', '/api/admin/teams'),
    operationFor('GET', '/api/admin/builds'),
  ]) {
    const actualPath = operation.path === '/api/admin/teams'
      ? '/api/admin/teams?count=100&skip=0'
      : operation.path === '/api/admin/builds'
        ? `/api/admin/builds?count=100&skip=0&gameId=${fixtureGame}`
        : operation.path;
    const projections = [];
    for (const [index, baseUrl] of webTargets.entries()) {
      const response = await adminApi('GET', actualPath, {
        baseUrl,
        ip: `10.252.30.${index + 1}`,
      });
      projections.push(stableReplicaProjection(operation.id, response.json));
    }
    requireCondition(
      projections.every((projection) => JSON.stringify(projection) === JSON.stringify(projections[0])),
      `${operation.id} diverged between web replicas`,
    );
  }

  // Keep this exact instance live for the all-origin read matrix. Its
  // catalogued DELETE is executed first during cleanup, before the disposable
  // event graph can erase the retry identity.
  saveRecovery();
}

async function buildLifecycle() {
  console.log('\nbuild history and installation-owned image lifecycle…');
  // Bulk rebuild is a retry action, not a rebuild-all action. Force the exact
  // disposable challenge into Failed and require a real new attempt.
  const beforeBulk = Number(
    sql(`SELECT count(*) FROM "BuildRecords" WHERE challenge_id=${fixtureContainerChallenge}`),
  );
  sql(
    `UPDATE "GameChallenges" SET build_status=2 WHERE id=${fixtureContainerChallenge} ` +
      `AND game_id=${fixtureGame}`,
  );
  const bulk = await call(
    'POST',
    '/api/admin/games/{gameId}/bulkrebuild',
    `/api/admin/games/${fixtureGame}/bulkrebuild`,
    { timeoutMs: 180_000 },
  );
  requireCondition(bulk.json?.enqueued >= 1, `bulk rebuild remained a no-op: ${bulk.text}`);
  requireCondition(
    Number(sql(`SELECT count(*) FROM "BuildRecords" WHERE challenge_id=${fixtureContainerChallenge}`)) > beforeBulk,
    'bulk rebuild did not create a durable audit row',
  );

  const queuedId = insertBuildRecord({
    challengeId: fixtureContainerChallenge,
    gameId: fixtureGame,
    title: `admin-queued-${tag}`,
    status: 5,
    attempt: 40,
    logTail: `queued ${tag}`,
  });
  const failedId = insertBuildRecord({
    challengeId: fixtureContainerChallenge,
    gameId: fixtureGame,
    title: `admin-failed-${tag}`,
    status: 2,
    attempt: 41,
    logTail: `failed ${tag}`,
  });
  const deleteId = insertBuildRecord({
    challengeId: fixtureContainerChallenge,
    gameId: fixtureGame,
    title: `admin-delete-${tag}`,
    status: 4,
    attempt: 42,
  });
  const bulkDeleteIds = [43, 44].map((attempt) =>
    insertBuildRecord({
      challengeId: fixtureContainerChallenge,
      gameId: fixtureGame,
      title: `admin-bulk-delete-${tag}-${attempt}`,
      status: 4,
      attempt,
    }),
  );
  state.buildRecordIds.push(queuedId, failedId, deleteId, ...bulkDeleteIds);
  saveRecovery();

  const builds = await call(
    'GET',
    '/api/admin/builds',
    `/api/admin/builds?count=500&skip=0&gameId=${fixtureGame}`,
  );
  requireCondition(builds.json?.some((record) => record.id === failedId), 'build history omitted fixture rows');
  const inProgress = await call('GET', '/api/admin/builds/inprogress', '/api/admin/builds/inprogress');
  requireCondition(
    inProgress.json?.some((record) => record.auditId === queuedId),
    'in-progress build feed omitted queued fixture',
  );

  const retrySource = positiveId(
    sql(
      `SELECT id FROM "BuildRecords" WHERE challenge_id=${fixtureContainerChallenge} ` +
        `AND id<>${queuedId} ORDER BY id DESC LIMIT 1`,
    ),
    'retry source build',
  );
  const retry = await call(
    'POST',
    '/api/admin/builds/{auditId}/reenqueue',
    `/api/admin/builds/${retrySource}/reenqueue`,
    { timeoutMs: 180_000 },
  );
  requireCondition(retry.json?.attempt >= 2, 'build re-enqueue did not advance the attempt');
  state.buildRecordIds.push(retry.json.id);

  await call('DELETE', '/api/admin/builds/{auditId}', `/api/admin/builds/${deleteId}`);
  requireCondition(Number(sql(`SELECT count(*) FROM "BuildRecords" WHERE id=${deleteId}`)) === 0, 'single build delete failed');

  const bulkDelete = await call(
    'POST',
    '/api/admin/builds/bulkdelete',
    '/api/admin/builds/bulkdelete',
    { body: bulkDeleteIds },
  );
  requireCondition(bulkDelete.json?.removed === bulkDeleteIds.length, 'bulk build delete count is wrong');

  // The endpoint is intentionally installation-global. Make its complete
  // candidate set exact immediately before the request, and ensure no build is
  // still capable of transitioning into Failed while that request is in flight.
  sql(`UPDATE "BuildRecords" SET status=1, finished_at_utc=clock_timestamp() WHERE id=${queuedId}`);
  assertExactFailedBuildPruneCandidates(buildRecordInventory(), failedId);
  const unrelatedBuildSnapshot = buildRecordInventory(`game_id<>${fixtureGame}`);
  const pruneFailed = await call(
    'POST',
    '/api/admin/builds/prunefailed',
    '/api/admin/builds/prunefailed',
  );
  requireCondition(pruneFailed.json?.removed === 1, 'failed-build pruning did not remove exactly one row');
  requireCondition(
    buildRecordInventory('status=2').length === 0,
    'failed-build pruning left a failed row after its exact candidate snapshot',
  );
  requireCondition(
    sameJson(buildRecordInventory(`game_id<>${fixtureGame}`), unrelatedBuildSnapshot),
    'failed-build pruning changed unrelated build inventory',
  );

  // Build two real installation-owned images through the trusted import path.
  // The future authorization game is deliberately unstarted, so normal
  // challenge deletion can later release each reference without SQL shortcuts.
  const imageFixtures = scratchBuildImagePlan(authorizationGameId);
  state.buildImageFixtures = imageFixtures;
  saveRecovery();
  const sourceArchive = scratchChallengeArchive(imageFixtures);
  const importedImages = await multipartRequest(
    `/api/edit/games/${authorizationGameId}/challenges/import`,
    {
      filename: `${tag}-owned-images.zip`,
      content: sourceArchive,
      contentType: 'application/zip',
      timeoutMs: 300_000,
      label: 'trusted FROM-scratch image import',
    },
  );
  requireCondition(
    importedImages.json?.imported === 2 && importedImages.json?.updated === 0 && importedImages.json?.failed === 0,
    `trusted scratch import failed: ${importedImages.text}`,
  );
  await waitForOwnedScratchBuilds(imageFixtures);

  const images = await call('GET', '/api/admin/builds/images', '/api/admin/builds/images');
  assertBuildImageFixtureInventory(images.json, imageFixtures, { referenced: true });
  const visibleTags = new Set((images.json || []).flatMap((image) => image.tags || []).map(normalizedManagedImageTag));
  requireCondition(
    !visibleTags.has(normalizedManagedImageTag('rsctf/67/twin-tokens:latest')),
    'image inventory crossed the installation ownership boundary',
  );

  const deleteFixture = imageFixtures.find((fixture) => fixture.role === 'delete');
  const pruneFixture = imageFixtures.find((fixture) => fixture.role === 'prune');
  requireCondition(deleteFixture && pruneFixture, 'scratch image roles are incomplete');

  // `force=true` is advisory only: an exact live definition must still block
  // deletion, and global pruning must preserve both referenced images.
  const protectedDelete = await adminApi(
    'DELETE',
    `/api/admin/builds/images?tag=${encodeURIComponent(deleteFixture.imageRef)}&force=true`,
    { timeoutMs: 180_000 },
  );
  requireCondition(
    validateAdminResponse('admin_build_image_delete', protectedDelete) &&
      protectedDelete.json?.removed === 0 &&
      protectedDelete.json?.messages?.some((message) =>
        /still referenced/i.test(message) && message.includes(deleteFixture.title)),
    `force bypassed a referenced image or returned weak evidence: ${protectedDelete.text}`,
  );
  const protectedPrune = await adminApi('POST', '/api/admin/builds/pruneimages', { timeoutMs: 180_000 });
  const protectedMessages = (protectedPrune.json?.messages || []).join('\n');
  requireCondition(
    validateAdminResponse('admin_build_images_prune', protectedPrune) &&
      protectedPrune.json?.removed === 0 &&
      imageFixtures.every((fixture) => protectedMessages.includes(fixture.title)),
    `prune did not protect both referenced images: ${protectedPrune.text}`,
  );
  for (const fixture of imageFixtures) {
    requireCondition(
      docker(['image', 'inspect', fixture.imageRef]).status === 0,
      `referenced image ${fixture.imageRef} disappeared during protection checks`,
    );
  }

  for (const fixture of imageFixtures) await deleteScratchBuildDefinition(fixture);
  const orphanedInventory = await ownedImageInventory();
  assertBuildImageFixtureInventory(orphanedInventory, imageFixtures, { referenced: false });

  const deletedImage = await call(
    'DELETE',
    '/api/admin/builds/images',
    `/api/admin/builds/images?tag=${encodeURIComponent(deleteFixture.imageRef)}&force=false`,
  );
  requireCondition(deletedImage.json?.removed === 1, `exact owned image delete failed: ${deletedImage.text}`);
  requireCondition(
    docker(['image', 'inspect', deleteFixture.imageRef]).status !== 0,
    'exactly deleted image tag still exists',
  );
  deleteFixture.imageRemoved = true;
  saveRecovery();

  const prunedImages = await call(
    'POST',
    '/api/admin/builds/pruneimages',
    '/api/admin/builds/pruneimages',
  );
  requireCondition(prunedImages.json?.removed === 1, `owned orphan image prune was not exact: ${prunedImages.text}`);
  requireCondition(
    docker(['image', 'inspect', pruneFixture.imageRef]).status !== 0,
    'pruned image tag still exists',
  );
  pruneFixture.imageRemoved = true;
  const finalInventory = await ownedImageInventory();
  requireCondition(
    imageFixtures.every((fixture) => !imageInventoryHas(finalInventory, fixture.imageRef)),
    'owned image inventory retained a deleted scratch fixture',
  );
  const ownershipRefs = imageFixtures.map((fixture) => sqlLiteral(canonicalManagedImageTag(fixture.imageRef))).join(',');
  requireCondition(
    Number(sql(`SELECT count(*) FROM "BuildImageOwnerships" WHERE canonical_ref IN (${ownershipRefs})`)) === 0,
    'owned image cleanup retained a durable ownership row',
  );
  saveRecovery();
}

async function repositoryLifecycle() {
  console.log('\nrepository binding lifecycle…');
  const before = Number(sql('SELECT COALESCE(MAX(id),0) FROM "RepoBindings"'));
  const created = await call('POST', '/api/admin/repobindings', '/api/admin/repobindings', {
    body: {
      repoUrl: repositoryUrl,
      ref: repositoryRef,
      intervalSeconds: 300,
      runImmediately: false,
    },
  });
  requireCondition(created.json?.failures === 0, `repository binding create failed: ${created.text}`);
  repoBindingId = positiveId(
    sql(`SELECT id FROM "RepoBindings" WHERE id>${before} ORDER BY id DESC LIMIT 1`),
    'repository binding',
  );
  state.repoBindingIds.push(repoBindingId);
  saveRecovery();

  // Build one solved event around the repository's stable manifest identities.
  // The first real HTTP scan must update this challenge in place; the historical
  // delete-and-recreate implementation erased the rows asserted below.
  const now = A.nowMs();
  const repoTitle = `LOADTEST-ADMIN-REPO-${tag}`;
  repoGameId = await A.createGame({
    title: repoTitle,
    hidden: false,
    practiceMode: false,
    acceptWithoutReview: true,
    start: now - 60_000,
    end: now + 3_600_000,
    teamMemberCountLimit: 0,
    containerCountLimit: 0,
    allowUserSubmissions: false,
  });
  state.gameIds.push(repoGameId);
  repoChallengeId = await A.createChallenge(repoGameId, {
    title: `repo-solve-${tag}`,
    category: 'Misc',
    type: 'StaticAttachment',
  });
  await A.setChallenge(repoGameId, repoChallengeId, {
    content: `repository solve preservation ${tag}`,
    originalScore: 777,
    minScoreRate: 0.4,
    difficulty: 9,
    submissionLimit: 7,
    disableBloodBonus: true,
  });
  const repoFlag = `flag{repository_preservation_${tag}}`;
  await A.addFlags(repoGameId, repoChallengeId, [repoFlag]);
  await A.setChallenge(repoGameId, repoChallengeId, { isEnabled: true });
  const repoCohort = A.seedCohort(repoGameId, 1);
  state.userIds.push(...repoCohort.userIds);
  state.teamIds.push(...repoCohort.teamIds);
  state.participationIds.push(...repoCohort.partIds);
  const repoPlayerStamp = sql(
    `SELECT security_stamp FROM "AspNetUsers" WHERE id=${sqlLiteral(repoCohort.userIds[0])}::uuid`,
  );
  const repoPlayerJwt = mintJwt(repoCohort.userIds[0], repoPlayerStamp, 1);
  const solved = await A.api(
    'POST',
    `/api/game/${repoGameId}/challenges/${repoChallengeId}`,
    { jwt: repoPlayerJwt, ip: '10.252.24.1', body: { flag: repoFlag } },
  );
  expectStatus(solved, 200, 'repository preservation solve');
  const repoSubmissionId = positiveId(solved.json?.data ?? solved.json, 'repository submission');
  const sourceIdentity = `binding/${repoBindingId}/Jeopardy/Misc/static-handout/challenge.yaml`;
  sql(
    `UPDATE "Games" SET repo_binding_id=${repoBindingId}, event_manifest_path='.gzevent' ` +
      `WHERE id=${repoGameId}; ` +
      `UPDATE "GameChallenges" SET source_yaml_path=${sqlLiteral(sourceIdentity)} ` +
      `WHERE id=${repoChallengeId} AND game_id=${repoGameId};`,
  );
  state.evidence.repositorySolve = {
    gameId: repoGameId,
    challengeId: repoChallengeId,
    participationId: repoCohort.partIds[0],
    teamId: repoCohort.teamIds[0],
    submissionId: repoSubmissionId,
    sourceIdentity,
    title: repoTitle,
  };
  const solveSnapshot = async () => {
    const database = JSON.parse(
      sql(
        `SELECT json_build_object(` +
          `'challengeId',challenge.id,` +
          `'acceptedCount',challenge.accepted_count,` +
          `'submissionCount',challenge.submission_count,` +
          `'submissionRows',(SELECT count(*) FROM "Submissions" submission ` +
            `WHERE submission.game_id=${repoGameId} AND submission.challenge_id=challenge.id),` +
          `'submissionId',(SELECT min(id) FROM "Submissions" submission ` +
            `WHERE submission.game_id=${repoGameId} AND submission.challenge_id=challenge.id),` +
          `'firstSolveRows',(SELECT count(*) FROM "FirstSolves" first_solve ` +
            `WHERE first_solve.challenge_id=challenge.id),` +
          `'firstSolveSubmissionId',(SELECT min(submission_id) FROM "FirstSolves" first_solve ` +
            `WHERE first_solve.challenge_id=challenge.id),` +
          `'flags',COALESCE((SELECT json_agg(flag.flag ORDER BY flag.flag) FROM "FlagContexts" flag ` +
            `WHERE flag.challenge_id=challenge.id),'[]'::json),` +
          `'sourceIdentity',challenge.source_yaml_path` +
        `)::text FROM "GameChallenges" challenge ` +
        `WHERE challenge.id=${repoChallengeId} AND challenge.game_id=${repoGameId}`,
      ),
    );
    const board = await A.api('GET', `/api/game/${repoGameId}/scoreboard`, {
      jwt: A.adminJwt(),
      ip: '10.252.24.2',
    });
    expectStatus(board, 200, 'repository preservation scoreboard');
    const item = board.json?.items?.find((candidate) => candidate.id === repoCohort.teamIds[0]);
    const cell = item?.solvedChallenges?.find((candidate) => candidate.id === repoChallengeId);
    requireCondition(item && cell, 'repository preservation solve is missing from the scoreboard');
    return {
      database,
      scoreboard: {
        teamId: item.id,
        score: item.score,
        solvedCount: item.solvedCount,
        challengeId: cell.id,
        challengeScore: cell.score,
      },
    };
  };
  const beforeScan = await solveSnapshot();
  requireCondition(
    beforeScan.database.submissionId === repoSubmissionId &&
      beforeScan.database.firstSolveSubmissionId === repoSubmissionId &&
      beforeScan.database.acceptedCount === 1 &&
      beforeScan.database.submissionCount === 1,
    `repository solve fixture is incomplete: ${JSON.stringify(beforeScan)}`,
  );
  saveRecovery();

  const bindings = await call('GET', '/api/admin/repobindings', '/api/admin/repobindings');
  requireCondition(bindings.json?.some((binding) => binding.id === repoBindingId), 'binding list omitted fixture');
  const paused = await call(
    'PUT',
    '/api/admin/repobindings/{id}',
    `/api/admin/repobindings/${repoBindingId}`,
    { body: { intervalSeconds: 600, status: 'Paused', pushOnEdit: false } },
  );
  requireCondition(paused.json?.status === 'Paused', 'repository binding did not pause');
  await adminApi('PUT', `/api/admin/repobindings/${repoBindingId}`, {
    body: { status: 'Active' },
  });

  const scan = await call(
    'POST',
    '/api/admin/repobindings/{id}/scan',
    `/api/admin/repobindings/${repoBindingId}/scan`,
    { timeoutMs: 300_000 },
  );
  requireCondition(
    scan.json?.gamesUpdated === 1 &&
      scan.json?.challengesUpdated >= 1 &&
      scan.json?.failures === 1 &&
      scan.json?.messages?.some((message) =>
        message.includes(`challenge #${repoChallengeId}`) && /grading\/scoring changes were retained/i.test(message)),
    `repository scan did not report the one intentional solved-grading fence: ${scan.text}`,
  );
  const observedCommit = sql(
    `SELECT COALESCE(last_commit_sha,'') FROM "RepoBindings" WHERE id=${repoBindingId}`,
  );
  requireCondition(/^[0-9a-f]{40}$/i.test(observedCommit), 'repository scan omitted its exact commit identity');
  requireCondition(
    !repositoryExpectedCommit || observedCommit.toLowerCase() === repositoryExpectedCommit.toLowerCase(),
    `repository scan resolved ${observedCommit}, expected ${repositoryExpectedCommit}`,
  );
  const afterFirstScan = await solveSnapshot();
  requireCondition(
    JSON.stringify(afterFirstScan) === JSON.stringify(beforeScan),
    `repository scan changed solve evidence: ${JSON.stringify({ beforeScan, afterFirstScan })}`,
  );

  // A failed-to-apply grading mutation is deliberately retryable at the same
  // commit. Exercise that real HTTP retry too; it must remain idempotent and
  // preserve the exact evidence a second time.
  const retriedScan = await adminApi('POST', `/api/admin/repobindings/${repoBindingId}/scan`, {
    timeoutMs: 300_000,
  });
  requireCondition(
    validateAdminResponse('admin_repo_binding_scan', retriedScan) &&
      retriedScan.json?.failures === 1 &&
      retriedScan.json?.gamesUpdated === 1,
    `same-commit repository retry was not controlled: ${retriedScan.text}`,
  );
  const afterRetry = await solveSnapshot();
  requireCondition(
    JSON.stringify(afterRetry) === JSON.stringify(beforeScan),
    `same-commit repository retry changed solve evidence: ${JSON.stringify({ beforeScan, afterRetry })}`,
  );
  state.evidence.repositorySolve = {
    ...state.evidence.repositorySolve,
    beforeScan,
    afterFirstScan,
    afterRetry,
    firstScan: scan.json,
    retriedScan: retriedScan.json,
    commit: observedCommit,
  };
  const history = await call(
    'GET',
    '/api/admin/repobindings/{id}/scans',
    `/api/admin/repobindings/${repoBindingId}/scans`,
  );
  requireCondition(history.json?.length >= 2, 'repository scan retry history is incomplete');
  // Retain the scanned binding through the all-origin read matrix. Cleanup
  // performs the catalogued delete and verifies its checkout is gone.
  saveRecovery();
}

async function adAdminLifecycle() {
  console.log('\nA&D admin lifecycle…');
  const service = await call(
    'POST',
    '/api/ad/admin/{game_id}/Services',
    `/api/ad/admin/${fixtureGame}/Services`,
    {
      body: {
        participationId: fixtureParticipation,
        challengeId: fixtureAdChallenge,
        host: 'admin-lifecycle.invalid',
        port: 31337,
      },
    },
  );
  requireCondition(service.json?.participationId === fixtureParticipation, 'A&D service registration returned wrong owner');
  const serviceId = service.json.adTeamServiceId;
  await adminApi('POST', `/api/ad/admin/${fixtureGame}/Services`, {
    body: {
      participationId: fixtureParticipation,
      challengeId: fixtureAdChallenge,
      host: 'admin-lifecycle-updated.invalid',
      port: 31338,
    },
  });
  requireCondition(
    Number(
      sql(
        `SELECT count(*) FROM "AdTeamServices" WHERE game_id=${fixtureGame} ` +
          `AND participation_id=${fixtureParticipation} AND challenge_id=${fixtureAdChallenge}`,
      ),
    ) === 1,
    'A&D service upsert created duplicates',
  );
  const services = await call(
    'GET',
    '/api/ad/admin/{game_id}/Services',
    `/api/ad/admin/${fixtureGame}/Services`,
  );
  requireCondition(
    services.json?.some((item) => item.adTeamServiceId === serviceId && item.port === 31338),
    'A&D service inventory did not return the upserted endpoint',
  );
  const rounds = await call(
    'GET',
    '/api/ad/admin/{game_id}/Rounds',
    `/api/ad/admin/${fixtureGame}/Rounds`,
  );
  requireCondition(Array.isArray(rounds.json), 'A&D rounds response is not an array');
  const beforeRoundCount = Number(sql(`SELECT count(*) FROM "AdRounds" WHERE game_id=${fixtureGame}`));
  const advance = await call(
    'POST',
    '/api/ad/admin/{game_id}/Round/Advance',
    `/api/ad/admin/${fixtureGame}/Round/Advance`,
    { expected: 400 },
  );
  requireCondition(/disabled/i.test(advance.text), 'manual round advance did not explain its rejection');
  requireCondition(
    Number(sql(`SELECT count(*) FROM "AdRounds" WHERE game_id=${fixtureGame}`)) === beforeRoundCount,
    'manual round advance wrote an official round',
  );
}

async function workerLifecycle() {
  console.log('\ntrusted worker administration and enrollment…');
  const created = await call('POST', '/api/admin/workers', '/api/admin/workers', {
    body: { name: `admin-lifecycle-${tag}` },
  });
  workerId = created.json?.worker?.id;
  requireCondition(/^[0-9a-f-]{36}$/i.test(workerId || ''), `worker create failed: ${created.text}`);
  requireCondition(typeof created.json?.enrollment?.token === 'string', 'worker create omitted enrollment token');
  state.workerIds.push(workerId);
  saveRecovery();

  const workers = await call('GET', '/api/admin/workers', '/api/admin/workers');
  requireCondition(workers.json?.some((worker) => worker.id === workerId), 'worker inventory omitted fixture');
  const directWorkers = await adminApi('GET', '/api/admin/workers', {
    baseUrl: controlTarget,
    ip: '10.252.40.10',
  });
  requireCondition(
    directWorkers.json?.some((worker) => worker.id === workerId),
    'direct control replica does not share the public worker inventory',
  );
  const rotated = await call(
    'POST',
    '/api/admin/workers/{id}/token',
    `/api/admin/workers/${workerId}/token`,
  );
  requireCondition(
    typeof rotated.json?.token === 'string' && rotated.json.token !== created.json.enrollment.token,
    'worker token rotation did not replace the one-use token',
  );

  const csrPem = createWorkerCsr();
  const enrollment = await call(
    'POST',
    '/api/workers/enroll',
    '/api/workers/enroll',
    {
      jwt: null,
      body: { token: rotated.json.token, csrPem },
      ip: '10.252.40.1',
    },
  );
  requireCondition(
    enrollment.json?.workerId === workerId && /BEGIN CERTIFICATE/.test(enrollment.json?.certificatePem || ''),
    'worker enrollment did not return its signed client certificate',
  );
  const replay = await A.api('POST', '/api/workers/enroll', {
    body: { token: rotated.json.token, csrPem },
    ip: '10.252.40.2',
  });
  requireCondition(replay.status === 401, `consumed worker token replay returned ${replay.status}`);

  const draining = await call(
    'PUT',
    '/api/admin/workers/{id}/state',
    `/api/admin/workers/${workerId}/state`,
    { body: { state: 'Draining' } },
  );
  requireCondition(draining.json?.administrativeState === 'Draining', 'worker did not enter Draining');
  for (const desired of ['Disabled', 'Enabled']) {
    const response = await adminApi('PUT', `/api/admin/workers/${workerId}/state`, {
      body: { state: desired },
    });
    requireCondition(response.json?.administrativeState === desired, `worker did not enter ${desired}`);
  }
}

async function signalRAndLoadSimulation() {
  console.log('\nadmin SignalR and fixed-rate replica simulation…');
  const negotiate = await rawRequest(
    'POST',
    '/hub/admin/negotiate?negotiateVersion=1',
    { headers: { 'content-type': 'text/plain;charset=UTF-8' }, body: '' },
  );
  requireCondition(
    negotiate.status === 200 && typeof negotiate.json?.connectionToken === 'string',
    `admin SignalR negotiate failed: ${negotiate.status} ${negotiate.text.slice(0, 200)}`,
  );
  covered.add('admin_signalr_negotiate');
  requireCondition(ADMIN_SIGNALR_SURFACES.length >= 2, 'admin SignalR catalog is incomplete');

  const pollerJwt = mintJwt(
    fixtureUsers.poller.id,
    userByEmail(fixtureUsers.names.pollerEmail).stamp,
    3,
  );
  const ordinaryJwt = mintJwt(
    fixtureUsers.captain.id,
    userByEmail(fixtureUsers.names.captainEmail).stamp,
    1,
  );
  const monitorJwt = mintJwt(
    fixtureUsers.monitor.id,
    userByEmail(fixtureUsers.names.monitorEmail).stamp,
    2,
  );
  const summaryPath = process.env.SUMMARY_JSON || `/tmp/rsctf-admin-lifecycle-${tag}-k6.json`;
  const before = docker([
    'stats',
    '--no-stream',
    '--format',
    '{{.Name}}|{{.CPUPerc}}|{{.MemUsage}}',
    ...[RSCTF, ...String(process.env.ADMIN_RSCTF_CONTAINERS || '').split(',').filter(Boolean)],
  ]).stdout.trim();
  const loadStartedAt = Date.now();
  state.load = {
    summaryPath,
    startedAt: loadStartedAt,
    resourcesBefore: before.split('\n'),
  };
  saveRecovery();
  const status = runK6('admin-lifecycle.js', {
    TARGET,
    ADMIN_LIFECYCLE_DISPOSABLE: process.env.ADMIN_LIFECYCLE_DISPOSABLE,
    CONFIRM_ADMIN_TARGET: process.env.CONFIRM_ADMIN_TARGET,
    WEB_TARGETS: webTargets.join(','),
    CONTROL_TARGET: controlTarget,
    CONFIRM_ADMIN_WEB_TARGETS: process.env.CONFIRM_ADMIN_WEB_TARGETS,
    CONFIRM_ADMIN_CONTROL_TARGET: process.env.CONFIRM_ADMIN_CONTROL_TARGET,
    ...(process.env.ALLOW_REMOTE_ADMIN_LIFECYCLE
      ? { ALLOW_REMOTE_ADMIN_LIFECYCLE: process.env.ALLOW_REMOTE_ADMIN_LIFECYCLE }
      : {}),
    ADMIN_TOKEN: pollerJwt,
    ADMIN_CONTEXT: JSON.stringify({
      gameId: fixtureGame,
      adGameId: fixtureGame,
      userId: fixtureUsers.captain.id,
      instanceId: containerGuid,
      bindingId: repoBindingId,
    }),
    DURATION: process.env.DURATION || '30s',
    RATE: process.env.RATE || 1,
    VUS: process.env.VUS || 4,
    SUMMARY_JSON: summaryPath,
  });
  requireCondition(status === 0, `admin k6 simulation failed with status ${status}`);
  const after = docker([
    'stats',
    '--no-stream',
    '--format',
    '{{.Name}}|{{.CPUPerc}}|{{.MemUsage}}',
    ...[RSCTF, ...String(process.env.ADMIN_RSCTF_CONTAINERS || '').split(',').filter(Boolean)],
  ]).stdout.trim();
  state.load = {
    ...state.load,
    completedAt: Date.now(),
    resourcesAfter: after.split('\n'),
  };
  saveRecovery();
  await assertAdminSignalRAuth(TARGET, {
    adminToken: pollerJwt,
    ordinaryToken: ordinaryJwt,
    monitorToken: monitorJwt,
  });
  covered.add('admin_signalr_connect');
  console.log('  ✓ admin_signalr_connect (unauthorized rejected; authenticated handshake completed)');
}

async function deleteIdentityFixture() {
  if (!fixtureUsers) return;
  const tryDeleteUser = async (id) => {
    if (!id || Number(sql(`SELECT count(*) FROM "AspNetUsers" WHERE id=${sqlLiteral(id)}::uuid`)) === 0) return;
    await adminApi('DELETE', `/api/admin/users/${id}`, { timeoutMs: 120_000 });
  };

  if (fixtureUsers.team?.id && Number(sql(`SELECT count(*) FROM "Teams" WHERE id=${fixtureUsers.team.id}`)) > 0) {
    await call('DELETE', '/api/admin/teams/{id}', `/api/admin/teams/${fixtureUsers.team.id}`, {
      timeoutMs: 120_000,
    });
  }
  // Directly demote the temporary polling admin; the API correctly forbids one
  // admin from mutating another administrator.
  sql(
    `UPDATE "AspNetUsers" SET role=1, security_stamp=gen_random_uuid()::text ` +
      `WHERE id=${sqlLiteral(fixtureUsers.poller.id)}::uuid AND role=3`,
  );
  for (const id of [
    fixtureUsers.captain.id,
    fixtureUsers.manager.id,
    fixtureUsers.imported.id,
    fixtureUsers.monitor.id,
    fixtureUsers.cacheDelete.id,
    fixtureUsers.poller.id,
  ]) {
    await tryDeleteUser(id);
  }
}

function integerArraySql(values) {
  const normalized = [...new Set((values || []).map((value) => positiveId(value, 'residual integer id')))];
  return normalized.length ? `ARRAY[${normalized.join(',')}]::integer[]` : 'ARRAY[]::integer[]';
}

function uuidArraySql(values) {
  const normalized = [...new Set((values || []).map(String))];
  for (const value of normalized) {
    requireCondition(/^[0-9a-f]{8}-[0-9a-f-]{27}$/i.test(value), `invalid residual UUID ${value}`);
  }
  return normalized.length
    ? `ARRAY[${normalized.map((value) => `${sqlLiteral(value)}::uuid`).join(',')}]`
    : 'ARRAY[]::uuid[]';
}

function exactIdCount(table, ids, { uuid = false } = {}) {
  const array = uuid ? uuidArraySql(ids) : integerArraySql(ids);
  return Number(sql(`SELECT count(*) FROM "${table}" WHERE id=ANY(${array})`));
}

function redisKeyExists(key) {
  requireCondition(
    typeof key === 'string' && key.startsWith('credimport:') && !/[\r\n\0]/.test(key),
    `invalid credential cache key ${key}`,
  );
  const result = docker(['exec', redisContainer, 'redis-cli', '--raw', 'EXISTS', key]);
  requireCondition(result.status === 0, `Redis EXISTS failed for ${key}: ${result.stderr.trim()}`);
  const exists = Number(result.stdout.trim());
  requireCondition(exists === 0 || exists === 1, `Redis returned an invalid EXISTS result for ${key}`);
  return exists;
}

function containerPathExists(path) {
  const present = docker(['exec', RSCTF, 'test', '-e', path]);
  if (present.status === 0) return true;
  const absent = docker(['exec', RSCTF, 'test', '!', '-e', path]);
  requireCondition(absent.status === 0, `could not inspect ${path}: ${present.stderr.trim()}`);
  return false;
}

function exactResidualSnapshot() {
  const gameIds = integerArraySql(state.gameIds);
  const workerIds = uuidArraySql(state.workerIds);
  const evidence = state.evidence || {};
  const buildImageFixtures = Array.isArray(state.buildImageFixtures) ? state.buildImageFixtures : [];
  const buildTitles = buildImageFixtures.map((fixture) => sqlLiteral(fixture.title));
  const buildCanonicalRefs = buildImageFixtures.map((fixture) =>
    sqlLiteral(canonicalManagedImageTag(fixture.imageRef)));
  const buildArchiveHashes = buildImageFixtures
    .map((fixture) => String(fixture.archiveHash || ''))
    .filter((hash) => /^[a-f0-9]{64}$/.test(hash));
  const writeupHash = String(evidence.writeup?.hash || '');
  const writeupPath = /^[0-9a-f]{64}$/.test(writeupHash)
    ? `/data/files/${writeupHash.slice(0, 2)}/${writeupHash.slice(2, 4)}/${writeupHash}`
    : null;
  return Object.freeze({
    games: exactIdCount('Games', state.gameIds),
    gameNamespace: Number(
      sql(
        `SELECT count(*) FROM "Games" WHERE title IN (` +
          `${sqlLiteral(`ADMIN-LIFECYCLE-${tag}`)},${sqlLiteral(`ADMIN-AUTHORIZATION-${tag}`)})`,
      ),
    ),
    users: exactIdCount('AspNetUsers', state.userIds, { uuid: true }),
    userNamespace: Number(
      sql(`SELECT count(*) FROM "AspNetUsers" WHERE email LIKE ${sqlLiteral(`${tag}.%@admin.invalid`)}`),
    ),
    teams: exactIdCount('Teams', state.teamIds),
    teamNamespace: Number(
      sql(
        `SELECT count(*) FROM "Teams" WHERE id=ANY(${integerArraySql(state.teamIds)}) ` +
          `OR name=${sqlLiteral(`ADMINLT-${tag}`)}`,
      ),
    ),
    participations: exactIdCount('Participations', state.participationIds),
    workers: exactIdCount('WorkerNodes', state.workerIds, { uuid: true }),
    workerNamespace: Number(
      sql(`SELECT count(*) FROM "WorkerNodes" WHERE name=${sqlLiteral(`admin-lifecycle-${tag}`)}`),
    ),
    workerWorkloads: Number(
      sql(`SELECT count(*) FROM "WorkerWorkloads" WHERE worker_id=ANY(${workerIds})`),
    ),
    bindings: exactIdCount('RepoBindings', state.repoBindingIds),
    repositoryCheckouts: state.repoBindingIds.filter(
      (id) => containerPathExists(`/data/files/repos/${positiveId(id, 'repository binding')}`),
    ).length,
    buildRecords: exactIdCount('BuildRecords', state.buildRecordIds),
    gameBuildRecords: Number(
      sql(`SELECT count(*) FROM "BuildRecords" WHERE game_id=ANY(${gameIds})`),
    ),
    scratchBuildDefinitions: buildTitles.length
      ? Number(sql(`SELECT count(*) FROM "GameChallenges" WHERE title IN (${buildTitles.join(',')})`))
      : 0,
    scratchBuildOwnerships: buildCanonicalRefs.length
      ? Number(sql(
          `SELECT count(*) FROM "BuildImageOwnerships" ` +
            `WHERE canonical_ref IN (${buildCanonicalRefs.join(',')})`,
        ))
      : 0,
    scratchBuildDockerTags: buildImageFixtures.filter(
      (fixture) => docker(['image', 'inspect', fixture.imageRef]).status === 0,
    ).length,
    scratchBuildArchiveFiles: buildArchiveHashes.length
      ? Number(sql(`SELECT count(*) FROM "Files" WHERE hash IN (${buildArchiveHashes.map(sqlLiteral).join(',')})`))
      : 0,
    physicalScratchBuildArchives: buildArchiveHashes.filter((hash) =>
      containerPathExists(`/data/files/${hash.slice(0, 2)}/${hash.slice(2, 4)}/${hash}`),
    ).length,
    containers: exactIdCount('Containers', state.containerIds, { uuid: true }),
    runtimeContainers: state.runtimeContainerIds.filter(
      (id) => docker(['container', 'inspect', id]).status === 0,
    ).length,
    submissions: evidence.submissionId
      ? exactIdCount('Submissions', [evidence.submissionId])
      : 0,
    suspicionEvents: evidence.suspicionId
      ? exactIdCount('SuspicionEvents', [evidence.suspicionId])
      : 0,
    flagEgressEvents: evidence.flagEgressId
      ? exactIdCount('FlagEgressEvents', [evidence.flagEgressId])
      : 0,
    antiCheatBlocks: evidence.antiCheatBlockId
      ? exactIdCount('AntiCheatBlocks', [evidence.antiCheatBlockId])
      : 0,
    writeupFiles: evidence.writeup?.id ? exactIdCount('Files', [evidence.writeup.id]) : 0,
    physicalWriteupBlobs: writeupPath && containerPathExists(writeupPath) ? 1 : 0,
    checkerDirectories: state.gameIds.filter(
      (id) => containerPathExists(`/data/files/checkers/load/${positiveId(id, 'checker game')}`),
    ).length,
    credentialRedisKeys: state.credentialCacheKeys.reduce(
      (count, key) => count + redisKeyExists(key),
      0,
    ),
  });
}

async function assertStableExactCleanup() {
  const delayMs = Number(process.env.ADMIN_CLEANUP_STABILITY_MS || 2_000);
  requireCondition(
    Number.isSafeInteger(delayMs) && delayMs >= 1_000 && delayMs <= 10_000,
    'ADMIN_CLEANUP_STABILITY_MS must be an integer from 1000 through 10000',
  );
  const passes = [];
  for (let pass = 0; pass < 2; pass += 1) {
    await new Promise((resolve) => setTimeout(resolve, delayMs));
    const snapshot = exactResidualSnapshot();
    passes.push(snapshot);
  }
  state.cleanupAudit = { delayMs, passes };
  saveRecovery();
  assertStableZeroResidualSnapshots(passes);
}

async function cleanup() {
  console.log('\ncleanup and leak audit…');
  const errors = [];
  const attempt = async (label, action) => {
    try {
      await action();
    } catch (error) {
      errors.push(`${label}: ${error.message}`);
    }
  };

  await attempt('live fixture container', async () => {
    if (!containerGuid) return;
    if (covered.has('admin_instance_delete')) {
      await adminApi('DELETE', `/api/admin/instances/${containerGuid}`, { timeoutMs: 120_000 });
    } else {
      await call(
        'DELETE',
        '/api/admin/instances/{id}',
        `/api/admin/instances/${containerGuid}`,
        { timeoutMs: 120_000 },
      );
    }
    requireCondition(
      Number(sql(`SELECT count(*) FROM "Containers" WHERE id=${sqlLiteral(containerGuid)}::uuid`)) === 0,
      'admin instance destroy left its database row',
    );
    requireCondition(
      docker(['container', 'inspect', containerRuntimeId]).status !== 0,
      'admin instance destroy left its Docker runtime behind',
    );
    containerGuid = null;
    containerRuntimeId = null;
  });
  await attempt('repository binding', async () => {
    if (!repoBindingId) return;
    const deletingId = repoBindingId;
    let bindingDeleted = false;
    if (repoGameId && Number(sql(`SELECT count(*) FROM "Games" WHERE id=${repoGameId}`)) > 0) {
      const currentTitle = sql(`SELECT title FROM "Games" WHERE id=${repoGameId}`);
      requireCondition(
        currentTitle === `LOADTEST-ADMIN-REPO-${tag}`,
        `repository fixture ${repoGameId} changed identity to ${currentTitle}`,
      );
      requireCondition(
        Number(
          sql(
            `SELECT count(*) FROM "Games" WHERE id=${repoGameId} ` +
              `AND repo_binding_id=${deletingId}`,
          ),
        ) === 1,
        `repository fixture ${repoGameId} does not exclusively own binding ${deletingId}`,
      );
      const foreignBoundGames = Number(
        sql(
          `SELECT count(*) FROM "Games" WHERE repo_binding_id=${deletingId} ` +
            `AND id<>${repoGameId}`,
        ),
      );
      requireCondition(
        foreignBoundGames === 0,
        `repository binding ${deletingId} unexpectedly owns ${foreignBoundGames} other game(s)`,
      );

      const imported = JSON.parse(
        sql(
          `SELECT COALESCE(json_agg(json_build_object(` +
            `'id',id,'sourcePath',source_yaml_path,'imageRef',container_image,` +
            `'archiveHash',original_archive_blob_path,` +
            `'attachmentHash',(SELECT file.hash FROM "Attachments" attachment ` +
              `JOIN "Files" file ON file.id=attachment.local_file_id ` +
              `WHERE attachment.id="GameChallenges".attachment_id)` +
          `) ORDER BY id),'[]'::json)::text FROM "GameChallenges" ` +
          `WHERE game_id=${repoGameId} AND source_yaml_path LIKE ` +
            `${sqlLiteral(`binding/${deletingId}/%`)}`,
        ) || '[]',
      );
      requireCondition(Array.isArray(imported), 'repository cleanup inventory is malformed');
      const totalChallengeCount = Number(
        sql(`SELECT count(*) FROM "GameChallenges" WHERE game_id=${repoGameId}`),
      );
      requireCondition(
        imported.length > 0 && imported.length === totalChallengeCount,
        `repository fixture ${repoGameId} has ${totalChallengeCount} challenge(s), but only ` +
          `${imported.length} belong to binding ${deletingId}`,
      );
      requireCondition(
        imported.filter((challenge) => Number(challenge.id) === Number(repoChallengeId)).length === 1,
        `repository fixture ${repoGameId} does not contain exactly one protected challenge ` +
          `${repoChallengeId}`,
      );
      requireCondition(
        imported.every((challenge) =>
          String(challenge.sourcePath || '').startsWith(`binding/${deletingId}/`)),
        `repository fixture ${repoGameId} contains an unexpected source path`,
      );
      state.evidence.repositoryArtifacts = imported;
      saveRecovery();

      // Stop every future scan and detach the retained game before changing
      // its disposable cleanup schedule or any imported definition.
      if (covered.has('admin_repo_binding_delete')) {
        await adminApi('DELETE', `/api/admin/repobindings/${deletingId}`, { timeoutMs: 300_000 });
      } else {
        await call(
          'DELETE',
          '/api/admin/repobindings/{id}',
          `/api/admin/repobindings/${deletingId}`,
          { timeoutMs: 300_000 },
        );
      }
      requireCondition(
        Number(sql(`SELECT count(*) FROM "RepoBindings" WHERE id=${deletingId}`)) === 0,
        'repository binding delete did not persist',
      );
      requireCondition(
        Number(sql(`SELECT count(*) FROM "Games" WHERE id=${repoGameId} AND repo_binding_id IS NULL`)) === 1,
        'repository binding delete did not detach its retained fixture game',
      );
      const checkout = docker([
        'exec', RSCTF, 'test', '!', '-e', `/data/files/repos/${deletingId}`,
      ]);
      requireCondition(
        checkout.status === 0,
        `repository checkout ${deletingId} survived binding delete`,
      );
      bindingDeleted = true;

      // Challenge hard-deletion correctly protects every Jeopardy definition
      // once its event has started. This exact, manifest-owned fixture still
      // needs application deletion to release unsolved attachments and source
      // archives before the evidence-preserving SQL fallback can remove the
      // one solved challenge. Move only this disposable schedule back to the
      // pre-start state; the solved challenge remains protected by its durable
      // submission and first-solve evidence.
      const rescheduled = sql(
        repositoryCleanupRescheduleSql(repoGameId, deletingId, repoChallengeId, tag),
      );
      requireCondition(
        Number(rescheduled) === repoGameId,
        `repository fixture ${repoGameId} could not be safely rescheduled for cleanup`,
      );

      for (const challenge of imported) {
        const challengeId = positiveId(challenge.id, 'repository cleanup challenge');
        if (challengeId === repoChallengeId) continue;
        const response = await A.api(
          'DELETE',
          `/api/edit/games/${repoGameId}/challenges/${challengeId}`,
          { jwt: A.adminJwt(), timeoutMs: 180_000 },
        );
        expectStatus(response, 200, `repository challenge ${challengeId} cleanup`);
        requireCondition(
          Number(sql(`SELECT count(*) FROM "GameChallenges" WHERE id=${challengeId}`)) === 0,
          `repository challenge ${challengeId} survived application cleanup`,
        );
      }

      if (
        repoChallengeId &&
        Number(sql(`SELECT count(*) FROM "GameChallenges" WHERE id=${repoChallengeId}`)) > 0
      ) {
        const detached = await A.api(
          'POST',
          `/api/edit/games/${repoGameId}/challenges/${repoChallengeId}/attachment`,
          { jwt: A.adminJwt(), body: { attachmentType: 'None' }, timeoutMs: 120_000 },
        );
        expectStatus(detached, 200, 'repository solved-challenge attachment detach');
        requireCondition(
          Number(
            sql(
              `SELECT count(*) FROM "GameChallenges" ` +
                `WHERE id=${repoChallengeId} AND attachment_id IS NOT NULL`,
            ),
          ) === 0,
          'repository solved challenge retained attachment metadata',
        );
      }

      const imageRefs = [...new Set(imported.map((row) => row.imageRef).filter(Boolean))];
      for (const imageRef of imageRefs) {
        await cleanupOwnedScratchImage({ imageRef, imageRemoved: false });
      }
      for (const row of imported) {
        for (const hash of [row.archiveHash, row.attachmentHash].map(String)) {
          if (!/^[0-9a-f]{64}$/.test(hash)) continue;
          requireCondition(
            Number(sql(`SELECT count(*) FROM "Files" WHERE hash=${sqlLiteral(hash)}`)) === 0,
            `repository challenge cleanup retained blob metadata ${hash}`,
          );
          requireCondition(
            docker([
              'exec',
              RSCTF,
              'test',
              '!',
              '-e',
              `/data/files/${hash.slice(0, 2)}/${hash.slice(2, 4)}/${hash}`,
            ]).status === 0,
            `repository challenge cleanup retained blob bytes ${hash}`,
          );
        }
      }
      const checkerCleanup = docker([
        'exec',
        RSCTF,
        'rm',
        '-rf',
        `/data/files/checkers/load/${repoGameId}`,
      ]);
      requireCondition(
        checkerCleanup.status === 0,
        `repository checker cleanup failed: ${checkerCleanup.stderr.trim()}`,
      );
    }
    if (!bindingDeleted) {
      if (covered.has('admin_repo_binding_delete')) {
        await adminApi('DELETE', `/api/admin/repobindings/${deletingId}`, { timeoutMs: 300_000 });
      } else {
        await call(
          'DELETE',
          '/api/admin/repobindings/{id}',
          `/api/admin/repobindings/${deletingId}`,
          { timeoutMs: 300_000 },
        );
      }
      requireCondition(
        Number(sql(`SELECT count(*) FROM "RepoBindings" WHERE id=${deletingId}`)) === 0,
        'repository binding delete did not persist',
      );
      const checkout = docker([
        'exec', RSCTF, 'test', '!', '-e', `/data/files/repos/${deletingId}`,
      ]);
      requireCondition(
        checkout.status === 0,
        `repository checkout ${deletingId} survived binding delete`,
      );
    }
    repoBindingId = null;
  });
  await attempt('worker rows', async () => {
    for (const id of state.workerIds) {
      sql(`DELETE FROM "WorkerWorkloads" WHERE worker_id=${sqlLiteral(id)}::uuid`);
      sql(`DELETE FROM "WorkerNodes" WHERE id=${sqlLiteral(id)}::uuid`);
    }
  });
  await attempt('admin build records', async () => {
    const ownedGameIds = integerArraySql(state.gameIds);
    sql(`DELETE FROM "BuildRecords" WHERE game_id=ANY(${ownedGameIds})`);
  });
  await attempt('owned scratch build images', async () => {
    for (const fixture of state.buildImageFixtures) {
      await deleteScratchBuildDefinition(fixture);
    }
    for (const fixture of state.buildImageFixtures) {
      await cleanupOwnedScratchImage(fixture);
    }
  });
  await attempt('repository solve event', async () => {
    if (!repoGameId) return;
    const deletingId = repoGameId;
    const expectedTitle = `LOADTEST-ADMIN-REPO-${tag}`;
    const currentTitle = sql(`SELECT title FROM "Games" WHERE id=${deletingId}`);
    if (!currentTitle) {
      repoGameId = null;
      repoChallengeId = null;
      return;
    }
    requireCondition(
      currentTitle === expectedTitle,
      `repository solve game ${deletingId} is ${currentTitle}, not ${expectedTitle}`,
    );
    const remainingChallenges = JSON.parse(
      sql(
        `SELECT COALESCE(json_agg(json_build_object('id',id,'sourcePath',source_yaml_path) ` +
          `ORDER BY id),'[]'::json)::text FROM "GameChallenges" ` +
          `WHERE game_id=${deletingId}`,
      ) || '[]',
    );
    const capturedChallenge = state.evidence.repositoryArtifacts?.filter(
      (challenge) => Number(challenge.id) === Number(repoChallengeId),
    ) ?? [];
    requireCondition(
      repoChallengeId &&
        capturedChallenge.length === 1 &&
        remainingChallenges.length === 1 &&
        Number(remainingChallenges[0]?.id) === Number(repoChallengeId) &&
        remainingChallenges[0]?.sourcePath === capturedChallenge[0]?.sourcePath,
      `repository solve fallback does not exclusively own its remaining challenge: ` +
        `${JSON.stringify(remainingChallenges)}`,
    );
    const protectedDeletion = await A.deleteGame(deletingId);
    requireCondition(
      protectedDeletion.status === 400 &&
        /cannot be permanently deleted after it has started/i.test(protectedDeletion.text),
      `repository solve retention policy changed: ${protectedDeletion.status} ` +
        `${protectedDeletion.text.slice(0, 300)}`,
    );
    deleteDisposableLoadGame(deletingId, expectedTitle);
    repoGameId = null;
    repoChallengeId = null;
  });
  await attempt('cross-game manager fixture', async () => {
    if (!authorizationGameId) return;
    const deletingId = authorizationGameId;
    const currentTitle = sql(`SELECT title FROM "Games" WHERE id=${deletingId}`);
    if (!currentTitle) {
      authorizationGameId = null;
      return;
    }
    requireCondition(
      currentTitle === `ADMIN-AUTHORIZATION-${tag}`,
      `authorization game ${deletingId} is not the exact disposable fixture`,
    );
    const deleted = await A.deleteGame(deletingId);
    requireCondition(
      deleted.status === 200,
      `authorization game delete returned ${deleted.status}: ${deleted.text.slice(0, 300)}`,
    );
    requireCondition(
      Number(sql(`SELECT count(*) FROM "Games" WHERE id=${deletingId}`)) === 0,
      'authorization game survived its application delete',
    );
    authorizationGameId = null;
  });
  await attempt('event namespace', async () => {
    if (fixtureGame && Number(sql(`SELECT count(*) FROM "Games" WHERE id=${fixtureGame}`)) > 0) {
      // Wait for any in-flight checker pass and prevent another round from
      // starting while the disposable checker tree and database graph vanish.
      await A.setAdScoringPaused(fixtureGame, true);
      // Preserve blob reference accounting: the disposable fixture's writeup
      // is the only file owner it creates, and must be released by application
      // code before the exact SQL fallback can remove competition rows.
      await adminApi('DELETE', `/api/edit/games/${fixtureGame}/writeups`, {
        timeoutMs: 120_000,
      });
      const writeupResidual = Number(
        sql(
          `SELECT count(*) FROM "Files" file WHERE ` +
            (state.evidence.writeup
              ? `file.id=${positiveId(state.evidence.writeup.id, 'writeup file')} AND ` +
                `file.hash=${sqlLiteral(state.evidence.writeup.hash)}`
              : `file.name LIKE ${sqlLiteral(`Writeup-${fixtureGame}-%`)}`),
        ),
      );
      requireCondition(
        writeupResidual === 0,
        `writeup release left ${writeupResidual} blob artifact(s); refusing SQL fallback`,
      );
      if (state.evidence.writeup) {
        const hash = String(state.evidence.writeup.hash);
        requireCondition(/^[0-9a-f]{64}$/.test(hash), 'recovery manifest contains an invalid writeup hash');
        const storedBlob = `/data/files/${hash.slice(0, 2)}/${hash.slice(2, 4)}/${hash}`;
        const physicalResidual = docker(['exec', RSCTF, 'test', '!', '-e', storedBlob]);
        requireCondition(
          physicalResidual.status === 0,
          `writeup release left physical blob ${hash}; refusing SQL fallback`,
        );
      }

      const protectedDeletion = await A.deleteGame(fixtureGame);
      requireCondition(
        protectedDeletion.status === 400 &&
          /cannot be permanently deleted after it has started/i.test(protectedDeletion.text),
        `started fixture delete policy changed: ${protectedDeletion.status} ${protectedDeletion.text.slice(0, 300)}`,
      );

      // The SQL fallback is permitted only after every exact runtime identity is
      // absent. Never erase the database lookup needed to retry external cleanup.
      const runtimeIds = [
        ...state.runtimeContainerIds,
        ...disposableAdminGameRuntimeIds(fixtureGame, tag),
      ];
      requireCondition(
        Number(sql(`SELECT count(*) FROM "GameChallenges" WHERE game_id=${fixtureGame} AND "Type"=5`)) === 0,
        'admin fixture unexpectedly owns a KotH runtime',
      );
      const checkerCleanup = docker([
        'exec',
        RSCTF,
        'rm',
        '-rf',
        `/data/files/checkers/load/${fixtureGame}`,
      ]);
      requireCondition(
        checkerCleanup.status === 0,
        `checker cleanup failed: ${checkerCleanup.stderr.trim()}`,
      );
      deleteDisposableAdminGame(fixtureGame, tag, { runtimeIds });
      fixtureGame = null;
    }
  });
  await attempt('identity fixture', deleteIdentityFixture);
  await attempt('remaining identity namespace', async () => {
    const remainingTeamId = sql(
      `SELECT id FROM "Teams" WHERE name=${sqlLiteral(`ADMINLT-${tag}`)} ORDER BY id DESC LIMIT 1`,
    );
    if (remainingTeamId) {
      await adminApi('DELETE', `/api/admin/teams/${positiveId(remainingTeamId, 'cleanup team')}`, {
        timeoutMs: 120_000,
      });
    }
    const remainingUserIds = String(
      sql(
        `SELECT COALESCE(string_agg(id::text, ','), '') FROM "AspNetUsers" ` +
          `WHERE email LIKE ${sqlLiteral(`${tag}.%@admin.invalid`)}`,
      ),
    ).split(',').filter(Boolean);
    for (const id of remainingUserIds) {
      sql(
        `UPDATE "AspNetUsers" SET role=1, security_stamp=gen_random_uuid()::text ` +
          `WHERE id=${sqlLiteral(id)}::uuid AND role=3`,
      );
      await adminApi('DELETE', `/api/admin/users/${id}`, { timeoutMs: 120_000 });
    }
  });
  await attempt('profile user', async () => {
    if (!fixtureUsers?.profile?.id) return;
    if (Number(sql(`SELECT count(*) FROM "AspNetUsers" WHERE id=${sqlLiteral(fixtureUsers.profile.id)}::uuid`)) > 0) {
      await adminApi('DELETE', `/api/admin/users/${fixtureUsers.profile.id}`);
    }
  });
  await attempt('global configuration restore', async () => {
    const restoreConfig = originalGlobalConfig || state.originalGlobalConfig;
    if (restoreConfig) {
      // update_config deliberately ignores branding hashes. The logo endpoint
      // is the only operation that clears the references and purges the blob,
      // so run its idempotent delete even if the main scenario failed between
      // upload and its ordinary delete step.
      await adminApi('DELETE', '/api/admin/config/logo');
      await adminApi('PUT', '/api/admin/config', { body: { globalConfig: restoreConfig } });
    }
  });
  await attempt('remaining namespaced evidence', async () => {
    if (antiCheatBlockId) sql(`DELETE FROM "AntiCheatBlocks" WHERE id=${antiCheatBlockId}`);
  });
  await attempt('stable exact residual audit', assertStableExactCleanup);
  if (errors.length) throw new Error(errors.join('; '));
  console.log('  ✓ exact database, Redis, blob, checkout, worker, and container resources stayed absent');
}

async function main() {
  const targets = assertSafeAdminTarget(process.env);
  assertDisposableRuntimeMarker(targets);
  state.evidence.runtimeIdentity = {
    before: inspectUniformServerRuntimeIdentity(serverContainers),
    after: null,
  };
  requireCondition(webTargets.length >= 2, 'WEB_TARGETS must name at least two direct web replicas');
  saveRecovery();
  lock = await acquireExclusiveProcessLock(loadOrchestrationLockPath, {
    label: 'RSCTF admin lifecycle',
    metadata: { target: TARGET, tag },
  });
  databaseLock = await acquireAdminLifecycleDatabaseLock();

  console.log(`RSCTF exhaustive admin lifecycle → ${TARGET}`);
  console.log(`  web replicas: ${webTargets.join(', ')}`);
  console.log(`  control endpoint: ${controlTarget}`);
  console.log(`  PostgreSQL: ${PG}; runtime owner: ${RSCTF}`);
  await A.preflight();
  await assertRuntimeRoles();
  await assertGlobalAdminMutationBaseline();

  let primaryError = null;
  try {
    await identityLifecycle();
    await configurationLifecycle();
    await eventFixture();
    await observabilityAndRuntime();
    await buildLifecycle();
    await repositoryLifecycle();
    await adAdminLifecycle();
    // Measure the shared read plane before one-time worker-token mutations and
    // the destructive authorization matrix. This keeps before/after runs on an
    // identical, stable fixture while the later release gate still covers both.
    await signalRAndLoadSimulation();
    await workerLifecycle();
    await authorizationMatrix({
      gameId: fixtureGame,
      challengeId: fixtureChallenge,
      participationId: fixtureParticipation,
      userId: fixtureUsers.captain.id,
      alternateUserId: fixtureUsers.manager.id,
      monitorUserId: fixtureUsers.imported.id,
      monitorStamp: fixtureUsers.imported.stamp,
      alternateMonitorUserId: fixtureUsers.monitor.id,
      alternateMonitorStamp: fixtureUsers.monitor.stamp,
      crossGameManagerUserId: fixtureUsers.manager.id,
      crossGameManagerStamp: fixtureUsers.manager.stamp,
      teamId: fixtureUsers.team.id,
      containerId: randomUUID(),
      antiCheatId: state.evidence.antiCheatBlockId,
      auditId: state.buildRecordIds.find(Boolean) || 1,
      bindingId: state.repoBindingIds.find(Boolean) || 1,
      workerId,
    });
  } catch (error) {
    primaryError = error;
  }

  let cleanupError = null;
  try {
    await cleanup();
  } catch (error) {
    cleanupError = error;
  }
  if (primaryError || cleanupError) {
    const parts = [];
    if (primaryError) parts.push(`scenario: ${primaryError.stack || primaryError.message}`);
    if (cleanupError) parts.push(`cleanup: ${cleanupError.stack || cleanupError.message}`);
    throw new Error(parts.join('\n'));
  }

  const completion = assertCompleteCoverage(covered, { includeSignalR: true });
  requireCondition(completion !== false, 'admin operation coverage is incomplete');
  const startingRuntimeIdentity = state.evidence.runtimeIdentity.before;
  state.evidence.runtimeIdentity.after = inspectUnchangedServerRuntimeIdentity(
    startingRuntimeIdentity,
    serverContainers,
  );
  saveRecovery();
  const fatalLogsByContainer = Object.fromEntries(
    originalServerRuntimeLogTargets(startingRuntimeIdentity).map(({ name, containerId }) => [
      name,
      {
        containerId,
        fatalLineCount: countContainerFatalLogs(containerId, runStartedAt),
      },
    ]),
  );
  state.evidence.fatalLogAudit = fatalLogsByContainer;
  const fatalLogs = Object.values(fatalLogsByContainer)
    .reduce((count, value) => count + value.fatalLineCount, 0);
  requireCondition(fatalLogs === 0, `runtime log audit found ${fatalLogs} panic/fatal records`);
  // Close the replacement/restart race around the log read itself. Names must
  // still resolve to the exact starting container ids after their immutable-id
  // logs have been consumed.
  state.evidence.runtimeIdentity.after = inspectUnchangedServerRuntimeIdentity(
    startingRuntimeIdentity,
    serverContainers,
  );
  state.completed = true;
  state.completedAt = Date.now();
  state.coveredOperations = [...covered].sort();
  state.timing = timing;
  saveRecovery();

  const values = timing.map((sample) => sample.ms).sort((left, right) => left - right);
  const percentile = (fraction) => values[Math.min(values.length - 1, Math.floor(values.length * fraction))] || 0;
  console.log('\nadmin lifecycle passed');
  console.log(`  operations and realtime surfaces: ${covered.size}/${completion.required}`);
  console.log(
    `  one-shot route latency: median=${percentile(0.5)} ms p95=${percentile(0.95)} ms ` +
      `max=${values.at(-1) || 0} ms`,
  );
  console.log(`  k6 summary: ${state.load?.summaryPath || 'not exported'}`);
  console.log(`  recovery/audit manifest: ${recoveryPath}`);
}

try {
  await main();
} finally {
  await databaseLock?.release();
  await lock?.release();
  if (!shouldRetainLifecycleManifest({
    completed: state.completed,
    cleanupVerified: state.completed,
    keep: process.env.KEEP_ADMIN_MANIFEST,
  })) removeRecovery(recoveryPath);
}
