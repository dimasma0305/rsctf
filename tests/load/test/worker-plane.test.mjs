import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { test } from 'node:test';

import {
  advanceWorkloadHandles,
  assertFreshIsolatedProject,
  auditReplicaLabels,
  auditRequiredResourceSamples,
  auditWorkerContinuity,
  auditWorkloadRows,
  assertProxyIdentityRateBudget,
  boundedPositiveInteger,
  canCleanupComposeProject,
  canRemoveDaemonSentinel,
  appendProxyResponse,
  createProxyResponseTracker,
  isolatedComposeEnvironment,
  ownsDaemonSentinel,
  parseContainerInfo,
  parseWorkerHandle,
  percentile,
  positiveInteger,
  proxyWebSocketUrl,
  requireMatchingSha256,
  selectOnlineWorkers,
  unwrapResponse,
  workloadShape,
} from '../worker-plane.js';
import {
  assertSourceFingerprints,
  expectedSourceFingerprints,
  repositorySourceFingerprints,
} from '../source-fingerprint.mjs';

const WORKER = '018f3c6a-d79b-7cc0-8f68-8fdbad0f57bb';
const SESSION = 'c4aa2f80-e797-4e25-a844-7285a0c5396b';
const NEXT_SESSION = '832d50e2-54dd-4412-a590-05a71beaf151';
const WORKLOAD = 'd7bee1de-2a38-4b12-81ab-e6e403803b38';
const ASSIGNMENT = '390d5ea6-c9c3-47f4-8b91-6d9dc2594592';
const CONTAINER = '630e2e19-e50d-43e5-8286-352ea179d935';

function digest(value) {
  return createHash('sha256').update(value).digest('hex');
}

test('k6 consumes binary proxy frames and ignores its own close sentinel', () => {
  const source = readFileSync(new URL('../k6/worker-plane.js', import.meta.url), 'utf8');
  assert.match(source, /socket\.on\('binaryMessage', acceptResponse\)/);
  assert.match(source, /reason !== 'websocket: close sent'/);
});

test('proxy response validation handles text, binary, and split markers', () => {
  const text = createProxyResponseTracker('demo service');
  appendProxyResponse(text, 'Shared demo service\n');
  assert.equal(text.sawPayload, true);
  assert.equal(text.valid, true);

  const binary = createProxyResponseTracker('demo service');
  appendProxyResponse(binary, new TextEncoder().encode('Shared demo service\n'));
  assert.equal(binary.valid, true);

  const split = createProxyResponseTracker('demo service');
  appendProxyResponse(split, 'Shared demo ');
  assert.equal(split.valid, false);
  appendProxyResponse(split, new TextEncoder().encode('service\n'));
  assert.equal(split.valid, true);

  const bounded = createProxyResponseTracker('never', 4);
  appendProxyResponse(bounded, 'long payload');
  assert.equal(bounded.body, 'long');
  assert.equal(bounded.truncated, true);
  assert.equal(bounded.valid, false);
});

test('k6 keeps health frequent, inventory sustainable, and 429s explicit', () => {
  const source = readFileSync(new URL('../k6/worker-plane.js', import.meta.url), 'utf8');
  const healthStart = source.indexOf('export function workerHealth');
  const inventoryStart = source.indexOf('export function workerInventory');
  const healthBody = source.slice(healthStart, inventoryStart);
  const inventoryBody = source.slice(inventoryStart);
  assert.ok(healthStart >= 0 && inventoryStart > healthStart);
  assert.doesNotMatch(healthBody, /\/api\/admin\/workers/);
  assert.match(inventoryBody, /\/api\/admin\/workers/);
  assert.match(source, /WORKER_INVENTORY_INTERVAL_SECONDS \|\| 10/);
  assert.match(source, /WORKER_INVENTORY_INTERVAL_SECONDS < 10/);
  assert.match(source, /new Counter\('worker_list_429'\)/);
  assert.match(source, /worker_list_429: \['count==0'\]/);
  assert.match(inventoryBody, /workers\.status === 429/);
});

test('k6 reports upgrade and post-upgrade stream failures separately', () => {
  const source = readFileSync(new URL('../k6/worker-plane.js', import.meta.url), 'utf8');
  assert.match(source, /new Rate\('proxy_handshake_failure'\)/);
  assert.match(source, /new Rate\('proxy_stream_failure'\)/);
  assert.match(source, /new Rate\('proxy_response_invalid'\)/);
  assert.match(source, /proxyHandshakeFailure\.add\(handshakeFailed\)/);
  assert.match(source, /if \(!handshakeFailed\) \{/);
  assert.match(source, /proxyStreamFailure\.add\(streamErrored\)/);
  assert.doesNotMatch(source, /proxyHandshakeFailure\.add\(handshakeFailed \|\| streamErrored\)/);
});

test('k6 reports authenticated proxy limiter rejections separately', () => {
  const source = readFileSync(new URL('../k6/worker-plane.js', import.meta.url), 'utf8');
  assert.match(source, /new Counter\('proxy_upgrade_429'\)/);
  assert.match(source, /proxy_upgrade_429: \['count==0'\]/);
  assert.match(source, /proxyUpgrade429\.add\(response\?\.status === 429 \? 1 : 0\)/);
});

test('proxy load stays below the authenticated identity limiter budget', () => {
  assert.equal(
    assertProxyIdentityRateBudget(20, Array.from({ length: 10 }, (_, i) => `t${i}`)),
    2,
  );
  assert.equal(
    assertProxyIdentityRateBudget(20, Array.from({ length: 12 }, (_, i) => `t${i}`)),
    20 / 12,
  );
  assert.throws(
    () => assertProxyIdentityRateBudget(20, ['a', 'b', 'c', 'd']),
    /150 request\/60s authenticated limiter/,
  );
  assert.throws(() => assertProxyIdentityRateBudget(2, ['same', 'same']), /distinct/);
  assert.throws(() => assertProxyIdentityRateBudget(2, ['a'], 0), /must be positive/);
});

test('local worker runner validates the fixture response marker', () => {
  const source = readFileSync(new URL('../worker-plane-local.mjs', import.meta.url), 'utf8');
  assert.match(source, /EXPECTED_RESPONSE_MARKER:/);
  assert.match(source, /Shared rsctf demo service/);
});

test('local worker runner binds a private per-run bootstrap token exactly twice', () => {
  const source = readFileSync(new URL('../worker-plane-local.mjs', import.meta.url), 'utf8');
  assert.match(
    source,
    /const bootstrapToken = randomBytes\(32\)\.toString\('base64url'\);/,
  );
  assert.match(source, /RSCTF_BOOTSTRAP_TOKEN: bootstrapToken,/);
  assert.match(
    source,
    /\/api\/account\/register[\s\S]*?body: \{[\s\S]*?bootstrapToken,[\s\S]*?\},/,
  );
  assert.equal(
    source.match(/\bbootstrapToken\b/g)?.length,
    3,
    'the secret must only be generated, passed to isolated Compose, and sent to registration',
  );
  assert.doesNotMatch(source, /process\.env\.RSCTF_BOOTSTRAP_TOKEN\s*=/);
});

test('source fingerprints are required and fail closed on either mismatch', () => {
  const tracked = 'ab'.repeat(32);
  const untracked = 'cd'.repeat(32);
  const expected = expectedSourceFingerprints({
    E2E_EXPECTED_TRACKED_SHA256: tracked.toUpperCase(),
    E2E_EXPECTED_UNTRACKED_SHA256: untracked,
  });
  assert.deepEqual(expected, { tracked, untracked });
  assert.deepEqual(assertSourceFingerprints(expected, expected, 'pre-measurement'), expected);
  assert.throws(() => expectedSourceFingerprints({}), /both required/);
  assert.throws(
    () => expectedSourceFingerprints({ E2E_EXPECTED_TRACKED_SHA256: tracked }),
    /both required/,
  );
  assert.throws(
    () => assertSourceFingerprints(expected, { tracked, untracked: 'ef'.repeat(32) }, 'post-build'),
    /post-build source fingerprint changed \(untracked\)/,
  );
});

test('repository fingerprints match the documented git and sha256sum shape', () => {
  const repository = mkdtempSync(resolve(tmpdir(), 'rsctf-worker-fingerprint-'));
  try {
    execFileSync('git', ['init', '--quiet'], { cwd: repository });
    writeFileSync(resolve(repository, 'tracked.txt'), 'before\n');
    execFileSync('git', ['add', 'tracked.txt'], { cwd: repository });
    execFileSync(
      'git',
      [
        '-c',
        'user.name=RSCTF test',
        '-c',
        'user.email=test@rsctf.invalid',
        'commit',
        '--quiet',
        '-m',
        'fixture',
      ],
      { cwd: repository },
    );
    const clean = repositorySourceFingerprints(repository);
    writeFileSync(resolve(repository, 'tracked.txt'), 'after\n');
    execFileSync('git', ['add', 'tracked.txt'], { cwd: repository });
    writeFileSync(resolve(repository, 'z-last.txt'), 'z\n');
    writeFileSync(resolve(repository, 'a-first.txt'), 'a\n');

    const actual = repositorySourceFingerprints(repository);
    const head = execFileSync('git', ['rev-parse', '--verify', 'HEAD'], { cwd: repository });
    const trackedDiff = execFileSync('git', ['diff', 'HEAD', '--binary'], { cwd: repository });
    const submodules = execFileSync('git', ['submodule', 'status', '--recursive'], {
      cwd: repository,
    });
    const trackedMaterial = Buffer.concat([
      Buffer.from('HEAD\n'),
      head,
      Buffer.from('DIFF_HEAD_BINARY\n'),
      trackedDiff,
      Buffer.from('SUBMODULE_STATUS\n'),
      submodules,
    ]);
    const manifest = `${digest('a\n')}  a-first.txt\n${digest('z\n')}  z-last.txt\n`;
    assert.deepEqual(actual, {
      tracked: digest(trackedMaterial),
      untracked: digest(manifest),
    });
    assert.notEqual(actual.tracked, clean.tracked, 'a staged source change must be covered');

    execFileSync(
      'git',
      [
        '-c',
        'user.name=RSCTF test',
        '-c',
        'user.email=test@rsctf.invalid',
        'commit',
        '--quiet',
        '-m',
        'staged change',
      ],
      { cwd: repository },
    );
    assert.notEqual(
      repositorySourceFingerprints(repository).tracked,
      actual.tracked,
      'a HEAD change during a run must be covered even when the worktree content is unchanged',
    );
  } finally {
    rmSync(repository, { recursive: true, force: true });
  }
});

test('repository fingerprint covers recursive submodule commit status', () => {
  const repository = mkdtempSync(resolve(tmpdir(), 'rsctf-worker-superproject-'));
  const fixture = mkdtempSync(resolve(tmpdir(), 'rsctf-worker-submodule-'));
  const commit = (root, message) => execFileSync(
    'git',
    [
      '-c',
      'user.name=RSCTF test',
      '-c',
      'user.email=test@rsctf.invalid',
      'commit',
      '--quiet',
      '-m',
      message,
    ],
    { cwd: root },
  );
  try {
    execFileSync('git', ['init', '--quiet'], { cwd: fixture });
    writeFileSync(resolve(fixture, 'fixture.txt'), 'first\n');
    execFileSync('git', ['add', 'fixture.txt'], { cwd: fixture });
    commit(fixture, 'first');
    const first = execFileSync('git', ['rev-parse', 'HEAD'], {
      cwd: fixture,
      encoding: 'utf8',
    }).trim();
    writeFileSync(resolve(fixture, 'fixture.txt'), 'second\n');
    execFileSync('git', ['add', 'fixture.txt'], { cwd: fixture });
    commit(fixture, 'second');

    execFileSync('git', ['init', '--quiet'], { cwd: repository });
    execFileSync(
      'git',
      ['-c', 'protocol.file.allow=always', 'submodule', 'add', '--quiet', fixture, 'fixture'],
      { cwd: repository },
    );
    execFileSync('git', ['add', '.gitmodules', 'fixture'], { cwd: repository });
    commit(repository, 'superproject');
    execFileSync('git', ['config', 'diff.ignoreSubmodules', 'all'], { cwd: repository });
    const current = repositorySourceFingerprints(repository);

    execFileSync('git', ['checkout', '--quiet', '--detach', first], {
      cwd: resolve(repository, 'fixture'),
    });
    assert.throws(
      () => repositorySourceFingerprints(repository),
      /initialized at their pinned commits/,
    );
    execFileSync('git', ['add', 'fixture'], { cwd: repository });
    const changed = repositorySourceFingerprints(repository);
    assert.notEqual(changed.tracked, current.tracked);

    writeFileSync(resolve(repository, 'fixture/fixture.txt'), 'dirty\n');
    assert.throws(
      () => repositorySourceFingerprints(repository),
      /no staged, unstaged, or untracked changes/,
    );
    writeFileSync(resolve(repository, 'fixture/fixture.txt'), 'first\n');
    writeFileSync(resolve(repository, 'fixture/untracked.txt'), 'untracked\n');
    assert.throws(
      () => repositorySourceFingerprints(repository),
      /no staged, unstaged, or untracked changes/,
    );
  } finally {
    rmSync(repository, { recursive: true, force: true });
    rmSync(fixture, { recursive: true, force: true });
  }
});

test('isolated project ownership rejects live, stale, and mismatched cleanup scopes', () => {
  assert.equal(assertFreshIsolatedProject('rsctf-worker-e2e-a1'), 'rsctf-worker-e2e-a1');
  assert.throws(() => assertFreshIsolatedProject('rsctf'), /reserved live project/);
  assert.throws(
    () => assertFreshIsolatedProject('rsctf-worker-e2e-a1', { resources: ['old-container'] }),
    /already owns Docker artifacts/,
  );
  assert.throws(
    () => assertFreshIsolatedProject('rsctf-worker-e2e-a1', { imageTags: ['old:image'] }),
    /already owns Docker artifacts/,
  );
  assert.equal(
    canCleanupComposeProject('rsctf-worker-e2e-a1', 'rsctf-worker-e2e-a1'),
    true,
  );
  assert.equal(
    canCleanupComposeProject('rsctf-worker-e2e-a1', 'rsctf-worker-e2e-b2'),
    false,
  );
  assert.equal(canCleanupComposeProject('rsctf', 'rsctf'), false);
});

test('isolated Compose environment drops ambient deployment configuration', () => {
  const environment = isolatedComposeEnvironment(
    {
      PATH: '/usr/bin',
      HOME: '/tmp/home',
      RSCTF_ROLE: 'web',
      RSCTF_STORAGE_BACKEND: 's3',
      RSCTF_S3_BUCKET: 'live-bucket',
      POSTGRES_IMAGE: 'live-postgres',
      DOCKER_HOST: 'tcp://live-docker:2376',
    },
    {
      RSCTF_ROLE: 'all',
      RSCTF_STORAGE_BACKEND: 'local',
      POSTGRES_IMAGE: 'postgres:test',
    },
  );
  assert.deepEqual(environment, {
    HOME: '/tmp/home',
    PATH: '/usr/bin',
    RSCTF_ROLE: 'all',
    RSCTF_STORAGE_BACKEND: 'local',
    POSTGRES_IMAGE: 'postgres:test',
  });
  assert.equal(environment.RSCTF_S3_BUCKET, undefined);
  assert.equal(environment.DOCKER_HOST, undefined);
});

test('artifact identities require exact sha256 equality', () => {
  const identity = `sha256:${'ab'.repeat(32)}`;
  assert.equal(requireMatchingSha256('agent', identity, identity.toUpperCase()), identity);
  assert.throws(
    () => requireMatchingSha256('agent', identity, `sha256:${'cd'.repeat(32)}`),
    /identity changed/,
  );
  assert.throws(() => requireMatchingSha256('agent', identity, 'latest'), /exact sha256/);
});

test('reportable resource samples require complete base and agent metrics', () => {
  const base = ['run-rsctf-1', 'run-db-1', 'run-redis-1'];
  const sample = (timestampMs) => ({
    timestampMs,
    containers: base.map((name) => ({ name, cpuPercent: 1, memoryBytes: 1024 })),
    agent: { pid: 123, cpuPercent: 2, memoryBytes: 2048 },
  });
  assert.deepEqual(auditRequiredResourceSamples([sample(1), sample(2)], base), {
    valid: true,
    errors: [],
  });
  assert.equal(
    auditRequiredResourceSamples(
      [sample(1), { ...sample(2), errors: ['docker stats failed'] }],
      base,
    ).valid,
    false,
  );
  const missing = sample(2);
  missing.containers.pop();
  assert.equal(auditRequiredResourceSamples([sample(1), missing], base).valid, false);
  assert.equal(
    auditRequiredResourceSamples([sample(1), { ...sample(2), agent: undefined }], base).valid,
    false,
  );
});

test('daemon sentinel cleanup requires the exact harness worker identity', () => {
  assert.equal(
    ownsDaemonSentinel({ 'io.rsctf.worker.daemon-owner': WORKER }, WORKER),
    true,
  );
  assert.equal(
    ownsDaemonSentinel({ 'io.rsctf.worker.daemon-owner': WORKER }, ASSIGNMENT),
    false,
  );
  assert.equal(ownsDaemonSentinel({}, WORKER), false);
  assert.equal(
    canRemoveDaemonSentinel({ 'io.rsctf.worker.daemon-owner': WORKER }, WORKER, false),
    true,
  );
  assert.equal(
    canRemoveDaemonSentinel({ 'io.rsctf.worker.daemon-owner': WORKER }, WORKER, true),
    false,
  );
});

function worker(overrides = {}) {
  return {
    id: WORKER,
    name: 'edge-1',
    administrativeState: 'Enabled',
    platformOs: 'windows',
    architecture: 'x86_64',
    runtimeKind: 'docker',
    online: true,
    capabilities: { maxWorkloadReplicas: 512 },
    capacity: { cpuMillis: 8000, memoryBytes: 16_000_000_000, slots: 8 },
    sessionId: SESSION,
    sessionEpoch: 3,
    heartbeatAt: 1_000,
    leaseExpiresAt: 20_000,
    ...overrides,
  };
}

test('response and container parsers accept the raw wire model and legacy envelope', () => {
  const model = {
    id: CONTAINER,
    entry: CONTAINER,
    status: 'Running',
    startedAt: 100,
    expectStopAt: 200,
  };
  assert.equal(unwrapResponse(model), model);
  assert.deepEqual(unwrapResponse({ data: model }), model);
  assert.deepEqual(parseContainerInfo(model), model);
  assert.deepEqual(parseContainerInfo({ data: model }), model);
  assert.throws(() => parseContainerInfo({ ...model, entry: 'worker:80' }), /proxy UUID/);
  assert.throws(() => parseContainerInfo({ ...model, status: 'Pending' }), /not running/);
});

test('worker backend handles retain assignment and generation fences', () => {
  assert.deepEqual(parseWorkerHandle(`rsctf-worker:${WORKLOAD}:${ASSIGNMENT}:7`), {
    workloadId: WORKLOAD,
    assignmentId: ASSIGNMENT,
    generation: 7,
  });
  assert.throws(() => parseWorkerHandle(`rsctf-worker:${WORKLOAD}:${ASSIGNMENT}:0`), /invalid/);
  assert.throws(() => parseWorkerHandle(`rsctf-worker:${WORKLOAD}:bad:1`), /invalid/);
  assert.deepEqual(
    advanceWorkloadHandles([{ workloadId: WORKLOAD, assignmentId: ASSIGNMENT, generation: 7 }]),
    [{ workloadId: WORKLOAD, assignmentId: ASSIGNMENT, generation: 8 }],
  );
  assert.throws(
    () => advanceWorkloadHandles([{ generation: Number.MAX_SAFE_INTEGER }]),
    /generation is exhausted/,
  );
});

test('proxy URLs preserve an optional target base path and select ws or wss', () => {
  assert.equal(
    proxyWebSocketUrl('http://127.0.0.1:8080', CONTAINER),
    `ws://127.0.0.1:8080/api/proxy/${CONTAINER}`,
  );
  assert.equal(
    proxyWebSocketUrl('https://ctf.example/platform/', CONTAINER),
    `wss://ctf.example/platform/api/proxy/${CONTAINER}`,
  );
  assert.throws(() => proxyWebSocketUrl('tcp://ctf.example', CONTAINER), /HTTP or HTTPS/);
});

test('online worker selection is exact about state, runtime, platform, and capacity', () => {
  const rows = [
    worker(),
    worker({ id: 'aef776f6-f31d-45d7-9272-5e3754b6171d', online: false }),
    worker({ id: '2588201d-2664-440b-be06-c7cbd402d254', administrativeState: 'Draining' }),
  ];
  assert.deepEqual(
    selectOnlineWorkers(rows, { minimum: 1, platformOs: 'windows' }).map(({ id }) => id),
    [WORKER],
  );
  assert.deepEqual(
    selectOnlineWorkers(rows, { minimum: 1, workerIds: [WORKER.toUpperCase()] }).map(({ id }) => id),
    [WORKER],
  );
  assert.throws(
    () => selectOnlineWorkers([worker({ capabilities: {} })], { minimum: 1 }),
    /need at least 1/,
  );
  assert.throws(() => selectOnlineWorkers(rows, { minimum: 2 }), /need at least 2/);
  assert.throws(
    () => selectOnlineWorkers(rows, { workerIds: ['58b8de93-c144-4425-814b-73e05dde83dc'] }),
    /not found/,
  );
  assert.throws(
    () => selectOnlineWorkers([worker({ sessionId: null })]),
    /invalid session id/,
  );
});

test('continuity audit accepts live lease renewal and rejects silent session churn', () => {
  const before = [worker()];
  const after = [worker({ heartbeatAt: 2_000, leaseExpiresAt: 30_000 })];
  assert.deepEqual(auditWorkerContinuity(before, after, [WORKER], { now: 3_000 }), {
    valid: true,
    errors: [],
  });

  const changed = [
    worker({
      sessionId: NEXT_SESSION,
      sessionEpoch: 4,
      heartbeatAt: 2_000,
      leaseExpiresAt: 30_000,
    }),
  ];
  assert.equal(
    auditWorkerContinuity(before, changed, [WORKER], { now: 3_000 }).valid,
    false,
  );
  assert.equal(
    auditWorkerContinuity(before, changed, [WORKER], {
      expectedReconnectIds: [WORKER],
      now: 3_000,
    }).valid,
    true,
  );
});

test('reconnect audit requires both a changed session and an advanced epoch', () => {
  const before = [worker()];
  const stale = [worker({ sessionId: NEXT_SESSION, heartbeatAt: 2_000, leaseExpiresAt: 30_000 })];
  const audit = auditWorkerContinuity(before, stale, [WORKER], {
    expectedReconnectIds: [WORKER],
    now: 3_000,
  });
  assert.equal(audit.valid, false);
  assert.match(audit.errors.join(' '), /fenced session/);
});

test('workload audit enforces exact assignment/generation and desired-observed convergence', () => {
  const handles = [{ workloadId: WORKLOAD, assignmentId: ASSIGNMENT, generation: 2 }];
  const ready = [
    {
      workloadId: WORKLOAD,
      assignmentId: ASSIGNMENT,
      generation: 2,
      observedSessionEpoch: 9,
      desiredState: 'Present',
      observedState: 'Ready',
    },
  ];
  assert.deepEqual(auditWorkloadRows(ready, handles, 'Ready'), { valid: true, errors: [] });
  assert.equal(
    auditWorkloadRows(
      [{ ...ready[0], desiredState: 'Absent', observedState: 'Absent' }],
      handles,
      'Absent',
    ).valid,
    true,
  );
  assert.equal(
    auditWorkloadRows([{ ...ready[0], generation: 3 }], handles, 'Ready').valid,
    false,
  );
  assert.equal(
    auditWorkloadRows([{ ...ready[0], observedSessionEpoch: null }], handles, 'Ready').valid,
    false,
  );
});

test('workload shapes and Docker replica labels enforce an exact aggregate topology', () => {
  const shape = workloadShape({
    services: [
      { name: 'primary', replicas: 2 },
      { name: 'sidecar', replicas: 1 },
    ],
  });
  assert.equal(shape.serviceCount, 2);
  assert.equal(shape.replicaCount, 3);
  assert.throws(
    () => workloadShape({ services: [{ name: 'primary', replicas: 1 }, { name: 'primary', replicas: 1 }] }),
    /duplicate/,
  );

  const handle = {
    workloadId: WORKLOAD,
    assignmentId: ASSIGNMENT,
    generation: 4,
    workerId: WORKER,
    specHash: 'ab'.repeat(32),
  };
  const labels = (service, replica) => ({
    'io.rsctf.worker.managed': 'true',
    'io.rsctf.worker.id': WORKER,
    'io.rsctf.workload.id': WORKLOAD,
    'io.rsctf.assignment.id': ASSIGNMENT,
    'io.rsctf.workload.generation': '4',
    'io.rsctf.workload.spec-hash': 'ab'.repeat(32),
    'io.rsctf.workload.service': service,
    'io.rsctf.workload.replica': String(replica),
  });
  const complete = [labels('primary', 0), labels('primary', 1), labels('sidecar', 0)];
  assert.deepEqual(auditReplicaLabels(complete, [handle], shape.services), {
    valid: true,
    errors: [],
  });
  assert.equal(auditReplicaLabels(complete.slice(1), [handle], shape.services).valid, false);
  assert.equal(
    auditReplicaLabels([...complete, labels('sidecar', 1)], [handle], shape.services).valid,
    false,
  );
});

test('numeric helpers reject ambiguous knobs and report nearest-rank percentiles', () => {
  assert.equal(positiveInteger('4', 'fleet'), 4);
  assert.throws(() => positiveInteger('2.5', 'fleet'), /positive integer/);
  assert.throws(() => positiveInteger('0', 'fleet'), /positive integer/);
  assert.equal(boundedPositiveInteger('5000', 'readiness delay', 60_000), 5_000);
  assert.throws(
    () => boundedPositiveInteger('60001', 'readiness delay', 60_000),
    /at most 60000/,
  );
  assert.equal(percentile([10, 40, 20, 30], 0.5), 20);
  assert.equal(percentile([10, 40, 20, 30], 0.95), 40);
  assert.equal(percentile([], 0.95), 0);
});

test('worker proxy phases use only an explicitly configured post-Ready delay', () => {
  const source = readFileSync(new URL('../worker-plane.mjs', import.meta.url), 'utf8');
  const functionStart = source.indexOf('async function runProxyLoad');
  const k6Call = source.indexOf("runK6('worker-plane.js'", functionStart);
  const readinessCondition = source.indexOf('if (PROXY_READINESS_DELAY_MS > 0)', functionStart);
  const readinessWait = source.indexOf('await sleep(PROXY_READINESS_DELAY_MS)', functionStart);
  assert.match(source, /process\.env\.PROXY_READINESS_DELAY_MS \|\| 0/);
  assert.ok(
    functionStart >= 0 &&
      readinessCondition > functionStart &&
      readinessWait > readinessCondition &&
      readinessWait < k6Call,
  );
  assert.match(source, /await runProxyLoad\(cycle, 'base'/);
  assert.match(source, /await runProxyLoad\(cycle, 'scaled'/);
  assert.match(source, /await runProxyLoad\(cycle, 'restored'/);
});
