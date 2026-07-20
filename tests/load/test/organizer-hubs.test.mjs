import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  assertDisposableOrganizerTopology,
  assertNegotiateContract,
  assertOrganizerHubAcknowledgements,
  assertPrivilegedHubCoverage,
  assertPrivilegedHubRuntimeCoverage,
  assertReceivedLog,
  consumeHubFrames,
  decodeReceive,
  hubFrame,
  isDisposableOrigin,
  normalizeOrigin,
  organizerByocMode,
  organizerWebTargets,
  PRIVILEGED_HUB_SURFACES,
  privilegedHubSurfaceId,
  scopedContainerExecPath,
  SIGNALR_RECORD_SEPARATOR,
} from '../organizer-hubs.js';
import { parseAxumRouterOperations } from '../admin-lifecycle.js';

const topology = {
  adminTarget: 'http://172.30.0.11:8080',
  execTarget: 'http://172.30.0.12:8080',
  webTargets: ['http://172.30.0.11:8080', 'http://172.30.0.13:8080'],
  adminContainer: 'rsctf-test-web-1',
  execContainer: 'rsctf-test-control-1',
  webContainers: ['rsctf-test-web-1', 'rsctf-test-web-2'],
  pgContainer: 'rsctf-test-db-1',
  redisContainer: 'rsctf-test-redis-1',
  composeProject: 'rsctf-test',
  network: 'rsctf-test_default',
  adNetwork: 'rsctf-test_ad',
};

function confirmations(overrides = {}) {
  return {
    ORGANIZER_HUBS_DISPOSABLE: '1',
    CONFIRM_ORGANIZER_HUB_ADMIN_TARGET: topology.adminTarget,
    CONFIRM_ORGANIZER_HUB_EXEC_TARGET: topology.execTarget,
    CONFIRM_ORGANIZER_HUB_WEB_TARGETS: topology.webTargets.join(','),
    CONFIRM_ORGANIZER_HUB_ADMIN_CONTAINER: topology.adminContainer,
    CONFIRM_ORGANIZER_HUB_EXEC_CONTAINER: topology.execContainer,
    CONFIRM_ORGANIZER_HUB_WEB_CONTAINERS: topology.webContainers.join(','),
    CONFIRM_ORGANIZER_HUB_PG_CONTAINER: topology.pgContainer,
    CONFIRM_ORGANIZER_HUB_REDIS_CONTAINER: topology.redisContainer,
    CONFIRM_ORGANIZER_HUB_COMPOSE_PROJECT: topology.composeProject,
    CONFIRM_ORGANIZER_HUB_NETWORK: topology.network,
    CONFIRM_ORGANIZER_HUB_AD_NETWORK: topology.adNetwork,
    ...overrides,
  };
}

test('organizer target acknowledgement is exact and limited to disposable origins', () => {
  assert.deepEqual(
    assertOrganizerHubAcknowledgements(confirmations(), topology),
    Object.fromEntries(Object.entries(confirmations()).filter(([key]) => key.startsWith('CONFIRM_'))),
  );
  assert.throws(
    () => assertOrganizerHubAcknowledgements(confirmations({ ORGANIZER_HUBS_DISPOSABLE: '0' }), topology),
    /ORGANIZER_HUBS_DISPOSABLE/,
  );
  assert.throws(
    () => assertOrganizerHubAcknowledgements(
      confirmations({ CONFIRM_ORGANIZER_HUB_EXEC_TARGET: 'http://172.30.0.99:8080' }),
      topology,
    ),
    /CONFIRM_ORGANIZER_HUB_EXEC_TARGET/,
  );
  const remote = { ...topology, adminTarget: 'https://ctf.example.com' };
  assert.throws(
    () => assertOrganizerHubAcknowledgements(
      confirmations({ CONFIRM_ORGANIZER_HUB_ADMIN_TARGET: remote.adminTarget }),
      remote,
    ),
    /loopback or RFC1918/,
  );
  assert.equal(isDisposableOrigin('http://127.0.0.1:8080'), true);
  assert.equal(isDisposableOrigin('http://10.2.3.4'), true);
  assert.equal(isDisposableOrigin('https://192.168.2.9:9443'), true);
  assert.equal(isDisposableOrigin('https://example.com'), false);
  assert.equal(normalizeOrigin('http://127.0.0.1:8080/'), 'http://127.0.0.1:8080');
});

test('topology gate binds marked app, PostgreSQL, Redis, direct targets, roles, and Docker socket', () => {
  const marker = 'organizer-stack-test-20260720';
  const marked = (entries = []) => [
    ...entries,
    `RSCTF_ADMIN_LIFECYCLE_MARKER=${marker}`,
  ];
  const model = {
    adminTarget: topology.adminTarget,
    execTarget: topology.execTarget,
    composeProject: topology.composeProject,
    marker,
    admin: {
      name: topology.adminContainer,
      project: topology.composeProject,
      service: 'rsctf',
      environment: marked(['RSCTF_ROLE=web']),
      addresses: ['172.30.0.11'],
      hostPorts: [],
      mounts: [],
    },
    exec: {
      name: topology.execContainer,
      project: topology.composeProject,
      service: 'rsctf-control',
      environment: marked(['RSCTF_ROLE=control']),
      addresses: ['172.30.0.12'],
      hostPorts: [],
      mounts: [{ source: '/var/run/docker.sock', destination: '/var/run/docker.sock', rw: true }],
    },
    postgres: {
      name: topology.pgContainer,
      project: topology.composeProject,
      service: 'db',
      environment: marked(),
      addresses: ['172.30.0.10'],
      hostPorts: [],
      mounts: [],
    },
    redis: {
      name: topology.redisContainer,
      project: topology.composeProject,
      service: 'redis',
      environment: marked(),
      addresses: ['172.30.0.9'],
      hostPorts: [],
      mounts: [],
    },
  };
  const secondWeb = {
    ...model.admin,
    name: topology.webContainers[1],
    addresses: ['172.30.0.13'],
  };
  model.webReplicas = [
    { target: topology.adminTarget, container: model.admin },
    { target: topology.webTargets[1], container: secondWeb },
  ];
  assert.deepEqual(assertDisposableOrganizerTopology(model), {
    adminRole: 'web',
    execRole: 'control',
    composeProject: topology.composeProject,
  });
  assert.throws(
    () => assertDisposableOrganizerTopology({
      ...model,
      exec: { ...model.exec, mounts: [{ ...model.exec.mounts[0], rw: false }] },
    }),
    /writable \/var\/run\/docker\.sock/,
  );
  assert.throws(
    () => assertDisposableOrganizerTopology({
      ...model,
      exec: { ...model.exec, project: 'production' },
    }),
    /not rsctf-test/,
  );
  assert.throws(
    () => assertDisposableOrganizerTopology({
      ...model,
      execTarget: 'http://172.30.0.99:8080',
    }),
    /not bound directly/,
  );
  assert.throws(
    () => assertDisposableOrganizerTopology({
      ...model,
      redis: { ...model.redis, environment: ['RSCTF_ADMIN_LIFECYCLE_MARKER=another-stack'] },
    }),
    /exact disposable marker/,
  );
  assert.throws(
    () => assertDisposableOrganizerTopology({ ...model, redis: undefined }),
    /Redis inspections are required/,
  );
});

test('web replica targets are canonical, distinct, disposable, and include the admin origin', () => {
  assert.deepEqual(
    organizerWebTargets(
      '["http://172.30.0.11:8080/","http://172.30.0.13:8080"]',
      topology.adminTarget,
    ),
    [topology.adminTarget, 'http://172.30.0.13:8080'],
  );
  assert.throws(
    () => organizerWebTargets('', topology.adminTarget),
    /at least two/,
  );
  assert.throws(
    () => organizerWebTargets(
      'http://172.30.0.13:8080,http://172.30.0.14:8080',
      topology.adminTarget,
    ),
    /must include/,
  );
  assert.throws(
    () => organizerWebTargets(`${topology.adminTarget},${topology.adminTarget}`, topology.adminTarget),
    /distinct/,
  );
  assert.throws(
    () => organizerWebTargets(`${topology.adminTarget},https://example.com`, topology.adminTarget),
    /loopback or RFC1918/,
  );
});

test('SignalR framing preserves split and packed records', () => {
  const first = hubFrame({ protocol: 'json', version: 1 });
  const second = hubFrame({ type: 6 });
  const split = first.indexOf(SIGNALR_RECORD_SEPARATOR) - 3;
  const partial = consumeHubFrames({ remainder: '' }, first.slice(0, split));
  assert.deepEqual(partial.frames, []);
  const completed = consumeHubFrames(
    { remainder: partial.remainder },
    `${first.slice(split)}${second}`,
  );
  assert.deepEqual(completed.frames, [
    { protocol: 'json', version: 1 },
    { type: 6 },
  ]);
  assert.equal(completed.remainder, '');
});

test('game-scoped terminal paths require one positive game id', () => {
  assert.equal(scopedContainerExecPath(37), '/hub/containerExec/games/37');
  for (const invalid of [0, -1, 1.5, Number.NaN]) {
    assert.throws(() => scopedContainerExecPath(invalid), /positive integer/);
  }
});

test('required BYOC execution cannot be silently skipped', () => {
  assert.deepEqual(organizerByocMode({}), { required: true, skipped: false });
  assert.deepEqual(
    organizerByocMode({ ORGANIZER_HUB_REQUIRE_BYOC: '1' }),
    { required: true, skipped: false },
  );
  assert.deepEqual(
    organizerByocMode({ ORGANIZER_HUB_DIAGNOSTIC_SKIP_BYOC: '1' }),
    { required: false, skipped: true },
  );
  assert.throws(
    () => organizerByocMode({
      ORGANIZER_HUB_REQUIRE_BYOC: '1',
      ORGANIZER_HUB_DIAGNOSTIC_SKIP_BYOC: '1',
    }),
    /conflicts/,
  );
  assert.throws(
    () => organizerByocMode({ SKIP_ORGANIZER_HUB_BYOC: '1' }),
    /no longer accepted/,
  );
  assert.throws(
    () => organizerByocMode({ ORGANIZER_HUB_REQUIRE_BYOC: '0' }),
    /ambiguous/,
  );
});

test('privileged hub catalog exactly matches all six Axum method-path operations', () => {
  const adminHub = readFileSync(new URL('../../../src/hubs/admin.rs', import.meta.url), 'utf8');
  const execHub = readFileSync(new URL('../../../src/hubs/container.rs', import.meta.url), 'utf8');
  const actual = [
    ...parseAxumRouterOperations(adminHub).filter(({ path }) => path.startsWith('/hub/admin')),
    ...parseAxumRouterOperations(execHub).filter(({ path }) => path.startsWith('/hub/containerExec')),
  ];
  assert.equal(assertPrivilegedHubCoverage(actual), 6);
  assert.equal(PRIVILEGED_HUB_SURFACES.length, 6);
  assert.equal(
    assertPrivilegedHubRuntimeCoverage(new Set(PRIVILEGED_HUB_SURFACES.map(({ id }) => id))),
    6,
  );
  assert.throws(
    () => assertPrivilegedHubRuntimeCoverage(PRIVILEGED_HUB_SURFACES.slice(1).map(({ id }) => id)),
    /missing: admin_upgrade/,
  );
  assert.throws(
    () => assertPrivilegedHubRuntimeCoverage([
      ...PRIVILEGED_HUB_SURFACES.map(({ id }) => id),
      'unknown_surface',
    ]),
    /unknown: unknown_surface/,
  );
  assert.equal(
    privilegedHubSurfaceId('GET', '/hub/containerExec/games/37?access_token=redacted'),
    'scoped_container_exec_upgrade',
  );
  assert.equal(
    privilegedHubSurfaceId('post', '/hub/containerExec/games/37/negotiate?negotiateVersion=1'),
    'scoped_container_exec_negotiate',
  );
  assert.throws(
    () => privilegedHubSurfaceId('GET', '/hub/containerExec/games/0'),
    /unknown privileged hub runtime surface/,
  );

  const compact = execHub.replaceAll(/\s/g, '').replaceAll(',)', ')');
  for (const [path, handler] of [
    ['/hub/containerExec', 'get(container_hub)'],
    ['/hub/containerExec/negotiate', 'post(signalr::admin_negotiate)'],
    ['/hub/containerExec/games/{game_id}', 'get(scoped_container_hub)'],
    ['/hub/containerExec/games/{game_id}/negotiate', 'post(scoped_negotiate)'],
  ]) {
    assert.ok(
      compact.includes(
        `.route("${path}",limited(Policy::PrivilegedHubAdmission,${handler})`,
      ),
      `${path} is not wrapped by privileged hub admission`,
    );
  }
});

test('negotiate and ReceivedLog validators enforce the real wire contract', () => {
  const negotiate = {
    status: 200,
    json: {
      negotiateVersion: 1,
      connectionId: 'connection-token',
      connectionToken: 'connection-token',
      availableTransports: [{ transport: 'WebSockets', transferFormats: ['Text', 'Binary'] }],
    },
  };
  assert.equal(assertNegotiateContract(negotiate), 'connection-token');
  assert.throws(
    () => assertNegotiateContract({ ...negotiate, json: { ...negotiate.json, connectionToken: 'alias' } }),
    /invalid WebSocket transport contract/,
  );

  const received = {
    type: 1,
    target: 'ReceivedLog',
    arguments: [{
      time: 1_752_000_000_000,
      level: 'Information',
      msg: 'Successfully added 1 users',
      name: 'fixture-admin',
      status: 'Success',
      ip: null,
      fingerprint: null,
    }],
  };
  assert.equal(
    assertReceivedLog(received, {
      message: 'Successfully added 1 users',
      userName: 'fixture-admin',
    }).msg,
    'Successfully added 1 users',
  );
  assert.throws(
    () => assertReceivedLog(received, { message: 'different', userName: 'fixture-admin' }),
    /exact semantic audit action/,
  );

  const receive = {
    type: 1,
    target: 'Receive',
    arguments: ['abc', Buffer.from('terminal output').toString('base64')],
  };
  assert.equal(decodeReceive(receive, 'abc'), 'terminal output');
  assert.equal(decodeReceive(receive, 'other'), null);
});

test('acceptance driver retains every privileged hub branch and exact cleanup gate', () => {
  const driver = readFileSync(new URL('../organizer-hubs.mjs', import.meta.url), 'utf8');
  for (const invocation of ['Open', 'Input', 'Resize', 'Close']) {
    assert.match(driver, new RegExp(`invoke\\('${invocation}'`), `${invocation} invocation missing`);
  }
  for (const requirement of [
    'ReceivedLog',
    'arbitrary raw-container rejection notice',
    'revokeOrganizerSessions',
    'startFleetService',
    'startFleetForPids',
    'waitForFleetReady',
    'teardownFleet',
    'ORGANIZER_HUBS_DISPOSABLE',
    'cleanupAuditRows',
    'scopedManagerExecLifecycle',
    'cross-game public container',
    'arbitrary raw host container',
    'revoke exact game manager membership',
    'scoped session authorization-revoked Closed event',
    'post-revocation manager Input side effect',
    'MANAGER_TOKEN',
    'SCOPED_TARGET',
    'residual snapshots',
    'persistRecovery',
    'deleteDisposableLoadGame',
    'historyRetentionCleanup',
    'KEEP_ORGANIZER_HUB_MANIFEST',
    'coveredSurfaces',
    'assertPrivilegedHubRuntimeCoverage',
    'inverseTopologyPreflight',
    'stateful-route absence',
    'control ordinary route',
    'missing scoped negotiate',
    'unassigned ordinary user scoped upgrade',
    'cross-game manager scoped upgrade',
    'deletion-pending game Admin scoped negotiate',
    'deletion-pending game Admin scoped upgrade',
    'ORGANIZER_HUB_DIAGNOSTIC_SKIP_BYOC',
    'ADMIN_LIFECYCLE_STACK_MARKER',
    'ORGANIZER_HUB_REDIS_CONTAINER',
    'ORGANIZER_HUB_WEB_TARGETS',
    'ORGANIZER_HUB_WEB_CONTAINERS',
    'topologyAuthorized',
    'acquireAdminLifecycleDatabaseLock',
    'inspectUniformServerRuntimeIdentity',
    'inspectUnchangedServerRuntimeIdentity',
    'originalServerRuntimeLogTargets',
    'countContainerFatalLogs',
    'RSCTF_ACCEPTANCE_REPORTABLE=1 rejects',
    'ORGANIZER_HUB_CLEANUP_STABILITY_MS',
  ]) {
    assert.ok(driver.includes(requirement), `${requirement} acceptance branch missing`);
  }

  const snapshot = driver.slice(
    driver.indexOf('function recoverySnapshot()'),
    driver.indexOf('function saveRecovery()'),
  );
  assert.doesNotMatch(snapshot, /organizerToken|primaryToken|security_stamp|\.stamp/);
  assert.match(snapshot, /cleanupVerified/);
  assert.match(snapshot, /scenarioFailure/);
  assert.match(snapshot, /cleanupFailure/);
  assert.match(snapshot, /verificationFailure/);
  assert.match(snapshot, /reportable/);
  assert.match(snapshot, /leaseFailures/);
  assert.match(driver, /state\.scenarioFailure = String/);
  assert.match(driver, /state\.cleanupFailure = String/);
  assert.match(driver, /state\.verificationFailure = String/);
  assert.match(driver, /state\.leaseFailures\.push/);
  const main = driver.slice(driver.indexOf('async function main()'));
  assert.ok(
    main.indexOf('const topology = assertSafeTopology()') < main.indexOf('const primary = resolvePrimaryAdmin()'),
    'disposable marker topology must run before the first SQL-backed identity lookup',
  );
  assert.ok(
    main.indexOf('inspectUniformServerRuntimeIdentity(serverContainers)') <
      main.indexOf('databaseLock = await acquireAdminLifecycleDatabaseLock()') &&
      main.indexOf('databaseLock = await acquireAdminLifecycleDatabaseLock()') <
      main.indexOf('const primary = resolvePrimaryAdmin()'),
    'runtime identity and the shared database lease must precede mutable organizer work',
  );
  assert.match(driver, /runtimeIdentity\.after = inspectUnchangedServerRuntimeIdentity/);
  assert.match(driver, /originalServerRuntimeLogTargets\(startingRuntimeIdentity\)/);
  assert.match(driver, /countContainerFatalLogs\(containerId, runStartedAt\)/);
  assert.ok(
    main.indexOf('await inverseTopologyPreflight()') < main.indexOf('const primary = resolvePrimaryAdmin()'),
    'inverse topology must be proven before the first SQL-backed identity lookup',
  );
  assert.ok(
    main.indexOf('await inverseTopologyPreflight()') < main.indexOf('createDockerFixtures('),
    'inverse topology must be proven before creating Docker fixtures',
  );
  const topologyGate = driver.slice(
    driver.indexOf('function assertSafeTopology()'),
    driver.indexOf('async function request('),
  );
  assert.doesNotMatch(topologyGate, /\bsql\s*\(/);
  assert.match(driver, /if \(!state\.topologyAuthorized\)/);
  assert.match(driver, /\['database', databaseLock\], \['process', lock\]/);
  assert.equal(
    (driver.match(/await sleep\(delayMs\)/g) || []).length,
    2,
    'both organizer residual snapshots must wait the configured interval',
  );
  assert.match(driver, /cleanupResidualSnapshots = \{ delayMs, first, second \}/);

  const adminHub = readFileSync(new URL('../../../src/hubs/admin.rs', import.meta.url), 'utf8');
  const execHub = readFileSync(new URL('../../../src/hubs/container.rs', import.meta.url), 'utf8');
  assert.match(adminHub, /\["ReceivedLog"\]/);
  assert.match(adminHub, /admin_negotiate/);
  assert.match(execHub, /"Open"\s*=>/);
  assert.match(execHub, /"Input"\s*=>/);
  assert.match(execHub, /"Resize"\s*=>/);
  assert.match(execHub, /"Close"\s*=>/);
  assert.match(execHub, /is_game_container/);
  assert.match(execHub, /open_byoc_exec/);
  assert.match(execHub, /\/hub\/containerExec\/games\/\{game_id\}/);
  const scopedExec = readFileSync(new URL('../../../src/hubs/container/scoped.rs', import.meta.url), 'utf8');
  assert.match(scopedExec, /resolved\.owner_count = 1/);
  assert.match(scopedExec, /exercise_instance_id IS NULL/);
  assert.match(scopedExec, /challenge\.ad_self_hosted = TRUE/);

  const load = readFileSync(new URL('../k6/organizer-hubs.js', import.meta.url), 'utf8');
  assert.match(load, /scoped_container_exec_session_ms/);
  assert.match(load, /MANAGER_TOKEN/);
  assert.match(load, /SCOPED_TARGET/);
  assert.match(load, /ScopedContainerExecHub/);
  assert.match(load, /public container UUID/);
  assert.match(load, /\[1-5\]\[0-9a-f\]\{3\}/);
});

test('shared SQL helper returns only row data for exact DML ownership checks', () => {
  const source = readFileSync(new URL('../lib.mjs', import.meta.url), 'utf8');
  assert.ok(source.includes("'-qAtc'"), 'psql command tags can corrupt INSERT/DELETE RETURNING identities');
});
