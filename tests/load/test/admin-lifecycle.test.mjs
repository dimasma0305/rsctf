import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import test from 'node:test';

import {
  ADMIN_OPERATION_IDS,
  ADMIN_OPERATIONS,
  ADMIN_READ_OPERATIONS,
  ADMIN_SIGNALR_SURFACES,
  assertCompleteCoverage,
  assertDirectAdminOriginBindings,
  assertDisposableComposeTopology,
  assertExactFailedBuildPruneCandidates,
  assertRouterCoverage,
  assertSafeAdminTarget,
  assertStableZeroResidualSnapshots,
  buildAdminReadOriginMatrix,
  parseAdminRouterOperations,
  positiveInteger,
  resolveOperationPath,
  stableReplicaProjection,
  validateAdminResponse,
} from '../admin-lifecycle.js';

const REPOSITORY = fileURLToPath(new URL('../../..', import.meta.url));

function rustFiles(root) {
  const files = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) files.push(...rustFiles(path));
    else if (entry.isFile() && entry.name.endsWith('.rs') && !entry.name.endsWith('_tests.rs')) files.push(path);
  }
  return files.sort();
}

function repositoryRouterSources() {
  const adminRoot = join(REPOSITORY, 'src/controllers/admin');
  const adPath = join(adminRoot, 'ad.rs');
  return {
    // Include every production module recursively. If the admin router is
    // split again, a route in the new file is still discovered automatically.
    admin: rustFiles(adminRoot)
      .filter((path) => path !== adPath)
      .map((path) => readFileSync(path, 'utf8')),
    ad: readFileSync(adPath, 'utf8'),
    workers: readFileSync(join(REPOSITORY, 'src/controllers/workers.rs'), 'utf8'),
    adminHub: readFileSync(join(REPOSITORY, 'src/hubs/admin.rs'), 'utf8'),
  };
}

const worker = {
  id: '018f3c6a-d79b-7cc0-8f68-8fdbad0f57bb',
  name: 'admin-lifecycle-worker',
  administrativeState: 'Enabled',
  online: true,
  capacity: { cpuMillis: 1000, memoryBytes: 536870912, slots: 2 },
};

function privateHeaders() {
  return { 'Cache-Control': 'private, no-store', Pragma: 'no-cache' };
}

function sampleBody(kind, status) {
  switch (kind) {
    case 'array': return [];
    case 'page': return { data: [], total: 0, length: 0 };
    case 'message': return { title: '', status };
    case 'string':
    case 'private-string': return 'one-time-secret';
    case 'my-ip':
      return {
        detectedIp: '127.0.0.1',
        rawConnectionIp: '127.0.0.1',
        forwardedFor: '',
        proxyTrusted: true,
        trustedNetworks: ['127.0.0.0/8'],
      };
    case 'config': return { accountPolicy: {}, globalConfig: {}, containerPolicy: {}, proxyTrust: {} };
    case 'dashboard':
      return { systemStats: { userCount: 0, teamCount: 0, activeContainerCount: 0 }, topGames: [] };
    case 'game-writeups': return { divisions: {}, writeups: [] };
    case 'zip': return new Uint8Array([0x50, 0x4b]);
    case 'import': return { total: 0, created: 0, updated: 0, skipped: 0, users: [] };
    case 'credential-send': return { sent: 0, failed: 0, results: [] };
    case 'user': return { userId: worker.id, role: 'User' };
    case 'instance-stats':
      return {
        cpuPercent: 1.5,
        memoryUsedBytes: 1024,
        memoryLimitBytes: 4096,
        netRxBytes: 10,
        netTxBytes: 20,
        sampledAt: Date.now(),
      };
    case 'bulk-rebuild': return { enqueued: 0, skipped: 0, messages: [] };
    case 'prune': return { removed: 0, messages: [] };
    case 'build': return { id: 1, challengeId: 2, status: 'Queued' };
    case 'repo-scan-result':
      return {
        gamesCreated: 0,
        gamesUpdated: 0,
        challengesImported: 0,
        challengesUpdated: 0,
        failures: 0,
        messages: [],
      };
    case 'repo-binding': return { id: 1, repoUrl: 'https://example.invalid/repo.git', status: 'Active', games: [] };
    case 'ad-service':
      return { adTeamServiceId: 1, participationId: 2, challengeId: 3, host: '127.0.0.1', port: 31337 };
    case 'error': return { title: 'Manual round advance is disabled', status };
    case 'workers': return [worker];
    case 'worker': return worker;
    case 'worker-created':
      return { worker, enrollment: { workerId: worker.id, token: 'secret', expiresAt: Date.now() + 60_000 } };
    case 'enrollment-token': return { workerId: worker.id, token: 'secret', expiresAt: Date.now() + 60_000 };
    case 'enrollment':
      return {
        workerId: worker.id,
        controlAddress: 'worker.example.invalid:7443',
        dataAddress: 'worker.example.invalid:7443',
        serverName: 'worker.example.invalid',
        certificatePem: '-----BEGIN CERTIFICATE-----',
        caPem: '-----BEGIN CERTIFICATE-----',
      };
    case 'object': return {};
    default: throw new Error(`missing test sample for ${kind}`);
  }
}

function sampleResponse(operation) {
  const status = operation.expectedStatuses[0];
  const headers = operation.responseKind === 'zip'
    ? { 'Content-Type': 'application/zip' }
    : [
        'private-string',
        'import',
        'credential-send',
        'worker-created',
        'enrollment-token',
        'enrollment',
      ].includes(operation.responseKind)
      ? privateHeaders()
      : {};
  return { status, body: sampleBody(operation.responseKind, status), headers };
}

test('catalog covers all 61 HTTP operations and keeps SignalR as separate surfaces', () => {
  assert.equal(ADMIN_OPERATIONS.length, 61);
  assert.equal(new Set(ADMIN_OPERATION_IDS).size, 61);
  assert.deepEqual(
    ADMIN_OPERATIONS.reduce((counts, operation) => {
      counts[operation.method] = (counts[operation.method] || 0) + 1;
      return counts;
    }, {}),
    { GET: 26, PUT: 6, POST: 20, DELETE: 9 },
  );
  const enroll = ADMIN_OPERATIONS.find(({ id }) => id === 'worker_enroll');
  assert.deepEqual(
    { method: enroll.method, path: enroll.path, auth: enroll.auth, surface: enroll.surface },
    { method: 'POST', path: '/api/workers/enroll', auth: 'enrollment-token', surface: 'control' },
  );
  assert.deepEqual(
    ADMIN_SIGNALR_SURFACES.map(({ method, path }) => `${method} ${path}`),
    ['GET /hub/admin', 'POST /hub/admin/negotiate'],
  );
  assert.ok(ADMIN_SIGNALR_SURFACES.every(({ id }) => !ADMIN_OPERATION_IDS.includes(id)));
});

test('read-load subset is GET-only, non-destructive, and includes web and control surfaces', () => {
  assert.equal(ADMIN_READ_OPERATIONS.length, 25);
  assert.ok(ADMIN_READ_OPERATIONS.every(({ method, poll, mutation }) => method === 'GET' && poll && !mutation));
  assert.deepEqual(new Set(ADMIN_READ_OPERATIONS.map(({ surface }) => surface)), new Set(['web', 'control']));
  assert.ok(!ADMIN_READ_OPERATIONS.some(({ id }) => id === 'admin_writeups_download'));
});

test('authorization classes keep Admin, manager, and enrollment-token surfaces explicit', () => {
  assert.equal(ADMIN_OPERATIONS.filter(({ auth }) => auth === 'admin').length, 59);
  assert.deepEqual(
    ADMIN_OPERATIONS.filter(({ auth }) => auth !== 'admin').map(({ id, auth }) => [id, auth]),
    [
      ['admin_participation_update', 'admin-or-manager'],
      ['worker_enroll', 'enrollment-token'],
    ],
  );
});

test('global failed-build prune guard requires one exact fixture and no active build', () => {
  assert.deepEqual(
    assertExactFailedBuildPruneCandidates([
      { id: 1, status: 1 },
      { id: 42, status: 2 },
      { id: 90, status: 4 },
    ], 42),
    { expectedId: 42, candidates: [42] },
  );
  assert.throws(
    () => assertExactFailedBuildPruneCandidates([{ id: 42, status: 2 }, { id: 43, status: 2 }], 42),
    /not the exact fixture/,
  );
  assert.throws(
    () => assertExactFailedBuildPruneCandidates([{ id: 42, status: 2 }, { id: 44, status: 5 }], 42),
    /active candidates/,
  );
});

test('cleanup acceptance requires two identical all-zero snapshots', () => {
  assert.deepEqual(
    assertStableZeroResidualSnapshots([{ users: 0, blobs: 0 }, { users: 0, blobs: 0 }]),
    { passes: 2, resources: 2 },
  );
  assert.throws(
    () => assertStableZeroResidualSnapshots([{ users: 1 }, { users: 0 }]),
    /retained users/,
  );
  assert.throws(() => assertStableZeroResidualSnapshots([{ users: 0 }]), /exactly two/);
});

test('read-origin matrix covers every live read on every eligible replica exactly once', () => {
  const context = {
    gameId: 1,
    adGameId: 1,
    userId: worker.id,
    instanceId: worker.id,
    bindingId: 1,
  };
  const webOrigins = ['http://public.test', 'http://web-1.test', 'http://web-2.test'];
  const controlOrigins = ['http://public.test', 'http://control.test'];
  const matrix = buildAdminReadOriginMatrix(context, webOrigins, controlOrigins);

  assert.equal(matrix.length, 74);
  assert.equal(new Set(matrix.map(({ operation, selectedOrigin }) =>
    `${operation.id}|${selectedOrigin}`)).size, matrix.length);
  for (const operation of ADMIN_READ_OPERATIONS) {
    const expected = operation.surface === 'control' ? controlOrigins : webOrigins;
    assert.deepEqual(
      matrix.filter((item) => item.operation.id === operation.id).map((item) => item.selectedOrigin),
      expected,
    );
  }
  assert.throws(
    () => buildAdminReadOriginMatrix({ gameId: 1 }, webOrigins, controlOrigins),
    /requires admin context/,
  );
});

test('repository router source and lifecycle catalog have exact bidirectional coverage', () => {
  const sources = repositoryRouterSources();
  assert.deepEqual(assertRouterCoverage(sources), { operations: 61, signalR: 2 });
  const parsed = parseAdminRouterOperations(sources);
  assert.equal(parsed.operations.length, 61);
  assert.equal(parsed.signalR.length, 2);
});

test('router-source regression catches a future uncovered operation and a removed route', () => {
  const sources = repositoryRouterSources();
  assert.throws(
    () => assertRouterCoverage({
      ...sources,
      admin: [...sources.admin, '.route("/api/admin/future", get(future))'],
    }),
    /uncovered: GET \/api\/admin\/future/,
  );
  assert.throws(
    () => assertRouterCoverage({
      ...sources,
      admin: sources.admin.map((source) => source.replace('.route("/api/admin/MyIp", get(my_ip))', '')),
    }),
    /missing: GET \/api\/admin\/MyIp/,
  );
});

test('router parser ignores route-like text in Rust comments and strings', () => {
  const parsed = parseAdminRouterOperations({
    admin: `
      // .route("/api/admin/comment", get(comment))
      const EXAMPLE: &str = ".route(\\"/api/admin/string\\", post(string))";
      const RAW: &str = r#"example \" .route("/api/admin/raw", delete(raw))"#;
      Router::new().route("/api/admin/live", get(live))
    `,
    ad: '',
    workers: '.route("/api/workers/enroll", post(enroll))',
  });
  assert.deepEqual(
    parsed.operations.map(({ method, path }) => `${method} ${path}`),
    ['GET /api/admin/live', 'POST /api/workers/enroll'],
  );
});

test('coverage accounting rejects omissions, duplicates, and unknown operations', () => {
  assert.deepEqual(assertCompleteCoverage(ADMIN_OPERATION_IDS), {
    covered: 61,
    required: 61,
    missing: [],
    extra: [],
  });
  assert.throws(() => assertCompleteCoverage(ADMIN_OPERATION_IDS.slice(1)), /missing: admin_my_ip_get/);
  assert.throws(() => assertCompleteCoverage([...ADMIN_OPERATION_IDS, ADMIN_OPERATION_IDS[0]]), /duplicate/);
  assert.throws(() => assertCompleteCoverage([...ADMIN_OPERATION_IDS, 'admin_unknown']), /unknown: admin_unknown/);

  const allSurfaces = [...ADMIN_OPERATION_IDS, ...ADMIN_SIGNALR_SURFACES.map(({ id }) => id)];
  assert.deepEqual(assertCompleteCoverage(allSurfaces, { includeSignalR: true }), {
    covered: 63,
    required: 63,
    missing: [],
    extra: [],
  });
});

test('destructive lifecycle safety guard requires an exact disposable topology acknowledgement', () => {
  const env = {
    ADMIN_LIFECYCLE_DISPOSABLE: '1',
    TARGET: 'http://127.0.0.1:58080',
    CONFIRM_ADMIN_TARGET: 'http://127.0.0.1:58080',
    WEB_TARGETS: '["http://127.0.0.1:58081","http://127.0.0.1:58082"]',
    CONFIRM_ADMIN_WEB_TARGETS: '["http://127.0.0.1:58081","http://127.0.0.1:58082"]',
    CONTROL_TARGET: 'http://127.0.0.1:58083',
    CONFIRM_ADMIN_CONTROL_TARGET: 'http://127.0.0.1:58083',
  };
  assert.deepEqual(assertSafeAdminTarget(env), {
    target: env.TARGET,
    webTargets: ['http://127.0.0.1:58081', 'http://127.0.0.1:58082'],
    controlTarget: env.CONTROL_TARGET,
  });
  assert.throws(() => assertSafeAdminTarget({ ...env, ADMIN_LIFECYCLE_DISPOSABLE: '0' }), /DISPOSABLE=1/);
  assert.throws(() => assertSafeAdminTarget({ ...env, CONFIRM_ADMIN_TARGET: 'http://127.0.0.1:1' }), /exact disposable target/);
  assert.throws(() => assertSafeAdminTarget({ ...env, WEB_TARGETS: `${env.TARGET},${env.TARGET}` }), /distinct/);
  assert.throws(
    () => assertSafeAdminTarget({ ...env, WEB_TARGETS: `${env.TARGET},http://127.0.0.1:58082` }),
    /TARGET must be distinct/,
  );
  assert.throws(() => assertSafeAdminTarget({ ...env, CONTROL_TARGET: env.TARGET }), /CONTROL_TARGET/);
  assert.throws(() => assertSafeAdminTarget({ ...env, CONTROL_TARGET: 'http://127.0.0.1:58081' }), /CONTROL_TARGET/);
  assert.throws(
    () => assertSafeAdminTarget({ ...env, CONFIRM_ADMIN_WEB_TARGETS: '' }),
    /CONFIRM_ADMIN_WEB_TARGETS/,
  );
  assert.throws(
    () => assertSafeAdminTarget({
      ...env,
      CONFIRM_ADMIN_WEB_TARGETS: '["http://127.0.0.1:58082","http://127.0.0.1:58081"]',
    }),
    /CONFIRM_ADMIN_WEB_TARGETS/,
  );
  assert.throws(
    () => assertSafeAdminTarget({ ...env, CONFIRM_ADMIN_CONTROL_TARGET: '' }),
    /CONFIRM_ADMIN_CONTROL_TARGET/,
  );
  assert.throws(
    () => assertSafeAdminTarget({ ...env, CONFIRM_ADMIN_CONTROL_TARGET: 'http://127.0.0.1:58084' }),
    /CONFIRM_ADMIN_CONTROL_TARGET/,
  );
  const remote = { ...env, TARGET: 'https://192.0.2.20', CONFIRM_ADMIN_TARGET: 'https://192.0.2.20' };
  assert.throws(() => assertSafeAdminTarget(remote), /ALLOW_REMOTE_ADMIN_LIFECYCLE/);
  assert.equal(
    assertSafeAdminTarget({ ...remote, ALLOW_REMOTE_ADMIN_LIFECYCLE: remote.TARGET }).target,
    remote.TARGET,
  );
});

test('destructive backing services require the same marker and Compose project before SQL', () => {
  const marker = 'admin-stack-test-1234';
  const resource = (name, service, overrides = {}) => ({
    name,
    service,
    project: 'rsctf-admin-test',
    environment: [`RSCTF_ADMIN_LIFECYCLE_MARKER=${marker}`],
    ...overrides,
  });
  const topology = {
    marker,
    servers: [
      resource('rsctf-admin-rsctf-1', 'rsctf'),
      resource('rsctf-admin-rsctf-2', 'rsctf'),
      resource('rsctf-admin-control-1', 'rsctf-control'),
    ],
    postgres: resource('rsctf-admin-db-1', 'db'),
    redis: resource('rsctf-admin-redis-1', 'redis'),
  };

  assert.deepEqual(assertDisposableComposeTopology(topology), {
    composeProject: 'rsctf-admin-test',
    serverCount: 3,
  });
  assert.throws(
    () => assertDisposableComposeTopology({
      ...topology,
      postgres: { ...topology.postgres, environment: [] },
    }),
    /PostgreSQL.*exact disposable marker/,
  );
  assert.throws(
    () => assertDisposableComposeTopology({
      ...topology,
      redis: { ...topology.redis, environment: ['RSCTF_ADMIN_LIFECYCLE_MARKER=another-stack'] },
    }),
    /Redis.*exact disposable marker/,
  );
  assert.throws(
    () => assertDisposableComposeTopology({
      ...topology,
      postgres: { ...topology.postgres, project: 'some-other-project' },
    }),
    /PostgreSQL.*not the db service in Compose project/,
  );
  assert.throws(
    () => assertDisposableComposeTopology({
      ...topology,
      redis: { ...topology.redis, service: 'db' },
    }),
    /Redis.*not the redis service in Compose project/,
  );

  const orchestrator = readFileSync(join(REPOSITORY, 'tests/load/admin-lifecycle.mjs'), 'utf8');
  const main = orchestrator.slice(orchestrator.indexOf('async function main()'));
  assert.ok(main.indexOf('assertDisposableRuntimeMarker(targets);') < main.indexOf('acquireAdminLifecycleDatabaseLock()'));
});

test('direct admin origins bind one-to-one to declared server IPs, roles, and port 8080', () => {
  const servers = [
    {
      name: 'rsctf-admin-rsctf-1',
      service: 'rsctf',
      networkAddresses: ['172.28.0.11', 'fd00::11'],
    },
    {
      name: 'rsctf-admin-rsctf-2',
      service: 'rsctf',
      networkAddresses: ['172.28.0.12'],
    },
    {
      name: 'rsctf-admin-control-1',
      service: 'rsctf-control',
      networkAddresses: ['172.28.0.13'],
    },
  ];
  const valid = {
    webTargets: ['http://172.28.0.11:8080', 'http://172.28.0.12:8080'],
    controlTarget: 'http://172.28.0.13:8080',
    servers,
  };
  assert.deepEqual(assertDirectAdminOriginBindings(valid), [
    {
      origin: 'http://172.28.0.11:8080',
      container: 'rsctf-admin-rsctf-1',
      service: 'rsctf',
    },
    {
      origin: 'http://172.28.0.12:8080',
      container: 'rsctf-admin-rsctf-2',
      service: 'rsctf',
    },
    {
      origin: 'http://172.28.0.13:8080',
      container: 'rsctf-admin-control-1',
      service: 'rsctf-control',
    },
  ]);
  assert.throws(
    () => assertDirectAdminOriginBindings({
      ...valid,
      webTargets: ['http://172.28.0.11:80', valid.webTargets[1]],
    }),
    /exactly http:\/\/<declared-container-ip>:8080/,
  );
  assert.throws(
    () => assertDirectAdminOriginBindings({
      ...valid,
      webTargets: ['http://172.28.0.99:8080', valid.webTargets[1]],
    }),
    /maps to 0 declared server containers/,
  );
  assert.throws(
    () => assertDirectAdminOriginBindings({
      ...valid,
      controlTarget: 'http://172.28.0.11:8080',
    }),
    /expected rsctf-control/,
  );
  assert.throws(
    () => assertDirectAdminOriginBindings({
      ...valid,
      webTargets: [valid.webTargets[0]],
    }),
    /must name every declared rsctf replica/,
  );
});

test('disposable started-game fallback is exact, blob-safe, and rejects forged namespaces', async () => {
  const previousSecret = process.env.RSCTF_JWT_SECRET;
  process.env.RSCTF_JWT_SECRET = previousSecret || 'admin-fixture-unit-test-secret';
  try {
    const { disposableAdminGameCleanupSql } = await import('../admin-fixtures.mjs');
    const cleanup = disposableAdminGameCleanupSql(42, 'admabc123');
    assert.match(cleanup, /id=42 AND title='ADMIN-LIFECYCLE-admabc123' FOR UPDATE/);
    assert.match(cleanup, /PERFORM pg_advisory_xact_lock\(-?\d+\)/);
    assert.match(cleanup, /writeup_id IS NOT NULL/);
    assert.match(cleanup, /original_archive_blob_path IS NOT NULL/);
    assert.match(cleanup, /RAISE EXCEPTION 'disposable admin fixture % still owns blob metadata'/);
    assert.match(
      cleanup,
      /DELETE FROM "Games" WHERE id=42 AND title='ADMIN-LIFECYCLE-admabc123'/,
    );
    assert.doesNotMatch(cleanup, /DELETE FROM "Games" WHERE id=42;/);
    assert.throws(() => disposableAdminGameCleanupSql(0, 'admabc123'), /positive integer/);
    assert.throws(() => disposableAdminGameCleanupSql(42, 'ADMIN'), /invalid disposable admin namespace/);
    assert.throws(() => disposableAdminGameCleanupSql(42, "admabc' OR TRUE--"), /invalid disposable admin namespace/);
  } finally {
    if (previousSecret === undefined) delete process.env.RSCTF_JWT_SECRET;
    else process.env.RSCTF_JWT_SECRET = previousSecret;
  }
});

test('operation paths bind and encode disposable context without leaving placeholders', () => {
  assert.equal(
    resolveOperationPath('admin_flag_egress_get', { gameId: 12 }),
    '/api/admin/Games/12/FlagEgress?count=25&skip=0',
  );
  assert.equal(
    resolveOperationPath('admin_user_get', { userId: 'user/with space' }),
    '/api/admin/users/user%2Fwith%20space',
  );
  assert.throws(() => resolveOperationPath('admin_user_get', {}), /requires admin context userId/);
  assert.throws(() => resolveOperationPath('not-an-operation', {}), /unknown admin operation/);
  assert.equal(positiveInteger('42', 'teams'), 42);
  assert.throws(() => positiveInteger('0', 'teams'), /positive integer/);
});

test('every catalog operation has a passing status/body/header response contract', () => {
  for (const operation of ADMIN_OPERATIONS) {
    assert.equal(
      validateAdminResponse(operation.id, sampleResponse(operation)),
      true,
      `${operation.id} (${operation.responseKind}) lacks a complete validator fixture`,
    );
  }
});

test('response validation rejects malformed pages, leaked credential caching, and wrong manual-advance status', () => {
  assert.equal(
    validateAdminResponse('admin_users_get', { status: 200, body: { data: [], total: 0, length: 1 } }),
    false,
  );
  assert.equal(
    validateAdminResponse('admin_users_import', {
      status: 200,
      body: sampleBody('import', 200),
      headers: { 'Cache-Control': 'public, max-age=3600' },
    }),
    false,
  );
  assert.equal(
    validateAdminResponse('admin_credentials_send', {
      status: 200,
      body: sampleBody('credential-send', 200),
      headers: {},
    }),
    false,
  );
  assert.equal(
    validateAdminResponse('ad_admin_round_advance_rejected', {
      status: 200,
      body: { title: '', status: 200 },
    }),
    false,
  );
  assert.equal(
    validateAdminResponse('admin_email_test', {
      status: 400,
      body: { title: 'Email test failed: connection refused', status: 400 },
    }),
    true,
  );
  assert.equal(
    validateAdminResponse('admin_writeups_download', {
      status: 200,
      bytes: new Uint8Array([0x50, 0x4b]),
      headers: new Headers({ 'Content-Type': 'application/zip' }),
    }),
    true,
  );
  assert.equal(
    validateAdminResponse('admin_user_password_reset', {
      status: 200,
      json: 'one-time-secret',
      headers: new Headers(privateHeaders()),
    }),
    true,
  );
  for (const operationId of ['admin_worker_create', 'admin_worker_token_issue', 'worker_enroll']) {
    const operation = ADMIN_OPERATIONS.find(({ id }) => id === operationId);
    assert.equal(
      validateAdminResponse(operationId, { ...sampleResponse(operation), headers: {} }),
      false,
      `${operationId} accepted cacheable one-time worker material`,
    );
  }
});

test('stable replica projections remove only request-local and sampled volatility', () => {
  const firstWorker = [{
    ...worker,
    heartbeatAt: 100,
    leaseExpiresAt: 200,
    updatedAt: 300,
    currentActivity: 'replica-a',
  }];
  const secondWorker = [{
    ...worker,
    heartbeatAt: 101,
    leaseExpiresAt: 201,
    updatedAt: 301,
    currentActivity: 'replica-b',
  }];
  assert.deepEqual(
    stableReplicaProjection('admin_workers_get', firstWorker),
    stableReplicaProjection('admin_workers_get', secondWorker),
  );

  const stats = sampleBody('instance-stats', 200);
  assert.deepEqual(
    stableReplicaProjection('admin_instance_stats_get', stats),
    stableReplicaProjection('admin_instance_stats_get', {
      ...stats,
      cpuPercent: 99,
      memoryUsedBytes: 2048,
      netRxBytes: 999,
      netTxBytes: 888,
      sampledAt: stats.sampledAt + 1000,
    }),
  );
  assert.notDeepEqual(
    stableReplicaProjection('admin_workers_get', firstWorker),
    stableReplicaProjection('admin_workers_get', [{ ...secondWorker[0], administrativeState: 'Disabled' }]),
  );
  assert.deepEqual(
    stableReplicaProjection('admin_my_ip_get', {
      detectedIp: '198.51.100.1', rawConnectionIp: '10.0.0.1', forwardedFor: '198.51.100.1',
      proxyTrusted: true, trustedNetworks: ['10.0.0.0/8', '127.0.0.0/8'],
    }),
    stableReplicaProjection('admin_my_ip_get', {
      detectedIp: '198.51.100.2', rawConnectionIp: '10.0.0.2', forwardedFor: '198.51.100.2',
      proxyTrusted: true, trustedNetworks: ['127.0.0.0/8', '10.0.0.0/8'],
    }),
  );
});

test('k6 admin scenario holds a fixed rate, polls shared contracts only, and fails on any error', () => {
  const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '../k6/admin-lifecycle.js'), 'utf8');
  assert.match(source, /ADMIN_READ_OPERATIONS,[\s\S]*validateAdminResponse/);
  assert.match(source, /executor: 'constant-arrival-rate'/);
  assert.match(source, /exec\.scenario\.iterationInTest/);
  assert.match(source, /const WEB_TARGETS/);
  assert.match(source, /const CONTROL_TARGET/);
  assert.match(source, /assertAdminOriginAcknowledgements\(__ENV/);
  assert.match(source, /RATE > 1/);
  assert.match(source, /74-request setup matrix shares the 150\/min admin quota/);
  assert.match(source, /new Trend\(`\$\{operation\.id\}_ms`/);
  assert.match(source, /server_5xx: \['rate==0'\]/);
  assert.match(source, /invalid_admin_response: \['rate==0'\]/);
  assert.match(source, /admin_429: \['count==0'\]/);
  assert.match(source, /dropped_iterations: \['count==0'\]/);
  assert.match(source, /\/livez/);
  assert.match(source, /\/healthz/);
  assert.match(source, /ws\.connect\(/);
  assert.match(source, /"protocol":"json","version":1/);
  assert.match(source, /signalr_failure: \['rate==0'\]/);
  assert.match(source, /http\.get\(`/);
  assert.doesNotMatch(source, /http\.(?:post|put|patch|del)\s*\(/);
});

test('orchestrator holds a database lease and exercises every privileged authorization class', () => {
  const root = join(dirname(fileURLToPath(import.meta.url)), '..');
  const fixtureSource = readFileSync(join(root, 'admin-fixtures.mjs'), 'utf8');
  const lifecycleSource = readFileSync(join(root, 'admin-lifecycle.mjs'), 'utf8');
  assert.match(fixtureSource, /pg_try_advisory_lock/);
  assert.match(fixtureSource, /pg_advisory_unlock/);
  assert.match(lifecycleSource, /operation\.auth === 'admin'/);
  assert.match(lifecycleSource, /Monitor authorization returned/);
  assert.match(lifecycleSource, /cross-game manager authorization returned/);
  assert.match(lifecycleSource, /invalid worker enrollment returned[\s\S]*expected 401/);
  assert.match(lifecycleSource, /\['missing', null, 401\]/);
  assert.match(lifecycleSource, /\['ordinary', ordinaryToken, 403\]/);
  assert.match(lifecycleSource, /\['Monitor', monitorToken, 403\]/);
  assert.match(lifecycleSource, /assertStableExactCleanup/);
  assert.doesNotMatch(lifecycleSource, /sql\(`DELETE FROM "RepoBindings"/);
});

test('orchestrator persists global configuration before its first mutation', () => {
  const source = readFileSync(
    join(dirname(fileURLToPath(import.meta.url)), '../admin-lifecycle.mjs'),
    'utf8',
  );
  const snapshot = source.indexOf('state.originalGlobalConfig = structuredClone(originalGlobalConfig)');
  const persisted = source.indexOf('saveRecovery();', snapshot);
  const mutation = source.indexOf("await call('PUT', '/api/admin/config'", snapshot);
  assert.ok(snapshot >= 0 && persisted > snapshot && mutation > persisted);
  assert.match(source, /originalGlobalConfig \|\| state\.originalGlobalConfig/);
});
