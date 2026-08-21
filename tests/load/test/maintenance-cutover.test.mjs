import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const here = dirname(fileURLToPath(import.meta.url));
const repository = resolve(here, '../../..');
const kubernetesScript = join(repository, 'scripts/kubernetes-maintenance-cutover.sh');
const composeScript = join(repository, 'scripts/compose-maintenance-cutover.sh');
const commandFixture = join(here, 'fixtures/maintenance-cutover-command.mjs');
const digest = `sha256:${'a'.repeat(64)}`;
const repositoryImage = 'registry.example/rsctf';
const immutableImage = `${repositoryImage}@${digest}`;
const oldImage = `${repositoryImage}@sha256:${'b'.repeat(64)}`;
// The scripts retain their deliberately short injected operation timeout. The
// outer runner needs more headroom when all subprocess-heavy tests run together.
const subprocessTimeoutMs = 30_000;

function harness(t, state) {
  const directory = mkdtempSync(join(tmpdir(), 'rsctf-cutover-test-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const statePath = join(directory, 'state.json');
  const stateFile = join(directory, 'compose-cutover.json');
  writeFileSync(statePath, `${JSON.stringify(state, null, 2)}\n`);
  for (const command of ['kubectl', 'helm', 'docker']) {
    const target = join(directory, command);
    copyFileSync(commandFixture, target);
    chmodSync(target, 0o755);
  }
  const env = {
    ...process.env,
    PATH: `${directory}:${process.env.PATH}`,
    RSCTF_CUTOVER_TEST_STATE: statePath,
    RSCTF_IMAGE: immutableImage,
  };
  return {
    directory,
    env,
    stateFile,
    readState: () => JSON.parse(readFileSync(statePath, 'utf8')),
    writeState: (next) => writeFileSync(statePath, `${JSON.stringify(next, null, 2)}\n`),
  };
}

function deployment(release, role, replicas) {
  return {
    release,
    role,
    name: release,
    replicas,
    storedReplicas: replicas,
    statusReplicas: replicas,
    image: oldImage,
  };
}

function kubernetesState() {
  return {
    appName: 'rsctf',
    oldChartLabels: true,
    deployments: [
      deployment('rsctf-web', 'web', 3),
      deployment('rsctf-control', 'control', 1),
    ],
    hpas: [],
    jobs: [],
    events: [],
  };
}

function runKubernetes(
  harness,
  timeout = '5s',
  runtimeReleases = ['rsctf-web', 'rsctf-control'],
) {
  const releaseArgs = runtimeReleases.flatMap((release) => ['--runtime-release', release]);
  return spawnSync('bash', [
    kubernetesScript,
    '--namespace', 'rsctf-system',
    '--chart', './charts/rsctf',
    '--image-repository', repositoryImage,
    '--image-digest', digest,
    '--database-secret', 'rsctf-shared',
    '--migrate-release', 'rsctf-migrate',
    ...releaseArgs,
    '--timeout', timeout,
  ], { encoding: 'utf8', env: harness.env, timeout: subprocessTimeoutMs });
}

function container(id, service, role, countIndex = 0) {
  return {
    id: `${id + countIndex}`.padStart(12, '0'),
    project: 'rsctf-prod',
    service,
    oneoff: false,
    role,
    running: true,
    restart: 'unless-stopped',
    image: oldImage,
  };
}

function composeState({ orphan = false } = {}) {
  const containers = [
    container(100, 'rsctf', 'web'),
    container(101, 'rsctf', 'web'),
    container(200, 'rsctf-control', 'control'),
  ];
  if (orphan) containers.push(container(300, 'renamed-old-runtime', 'engine'));
  return {
    project: 'rsctf-prod',
    configServices: [
      { name: 'rsctf', role: 'web', image: immutableImage },
      { name: 'rsctf-control', role: 'control', image: immutableImage },
    ],
    containers,
    events: [],
  };
}

function runCompose(harness, { env = harness.env, envFile, image } = {}) {
  const args = [
    composeScript,
    '--project-name', 'rsctf-prod',
    '--migrate-service', 'rsctf',
    '--state-file', harness.stateFile,
    '--timeout', '5',
  ];
  if (envFile) args.push('--env-file', envFile);
  if (image) args.push('--image', image);
  return spawnSync('bash', args, { encoding: 'utf8', env, timeout: subprocessTimeoutMs });
}

test('Kubernetes cutover scales every old-chart runtime to zero before waiting for migration', (t) => {
  const context = harness(t, kubernetesState());
  const result = runKubernetes(context);
  assert.equal(result.status, 0, result.stderr);
  const state = context.readState();
  const migrate = state.events.indexOf('helm:migrate:rsctf-migrate:wait-for-jobs');
  assert.ok(migrate > state.events.indexOf('k8s:scale:rsctf-web:0'));
  assert.ok(migrate > state.events.indexOf('k8s:scale:rsctf-control:0'));
  assert.deepEqual(
    state.deployments.map(({ release, replicas, image }) => ({ release, replicas, image })),
    [
      { release: 'rsctf-web', replicas: 3, image: immutableImage },
      { release: 'rsctf-control', replicas: 1, image: immutableImage },
    ],
  );
  const repeat = runKubernetes(context);
  assert.equal(repeat.status, 0, repeat.stderr);
  const repeatedState = context.readState();
  assert.equal(
    repeatedState.events.filter((value) => value === 'helm:migrate:rsctf-migrate:wait-for-jobs').length,
    2,
  );
  assert.match(repeatedState.events.join('\n'), /k8s:delete-job:/);
});

test('Kubernetes restores readiness providers before dependent runtime roles', async (t) => {
  await t.test('control before web regardless of caller order', () => {
    const state = kubernetesState();
    state.requireProviderOrder = true;
    const context = harness(t, state);
    const result = runKubernetes(context);
    assert.equal(result.status, 0, result.stderr);
    const runtimeEvents = context.readState().events.filter((value) => value.startsWith('helm:runtime:'));
    assert.deepEqual(runtimeEvents.map((value) => value.split(':')[2]), [
      'rsctf-control',
      'rsctf-web',
    ]);
  });

  await t.test('network then engine then web regardless of caller order', () => {
    const state = kubernetesState();
    state.requireProviderOrder = true;
    state.deployments = [
      deployment('rsctf-web', 'web', 3),
      deployment('rsctf-engine', 'engine', 2),
      deployment('rsctf-network', 'network', 1),
    ];
    const context = harness(t, state);
    const result = runKubernetes(
      context,
      '5s',
      ['rsctf-web', 'rsctf-engine', 'rsctf-network'],
    );
    assert.equal(result.status, 0, result.stderr);
    const runtimeEvents = context.readState().events.filter((value) => value.startsWith('helm:runtime:'));
    assert.deepEqual(runtimeEvents.map((value) => value.split(':')[2]), [
      'rsctf-network',
      'rsctf-engine',
      'rsctf-web',
    ]);
  });
});

test('Kubernetes cutover rejects an unlisted runtime and an HPA before migration', async (t) => {
  await t.test('unlisted runtime', () => {
    const state = kubernetesState();
    state.deployments.push(deployment('rsctf-engine', 'engine', 2));
    const context = harness(t, state);
    const result = runKubernetes(context);
    assert.notEqual(result.status, 0);
    assert.doesNotMatch(context.readState().events.join('\n'), /helm:migrate/);
  });
  await t.test('HPA-managed runtime', () => {
    const state = kubernetesState();
    state.hpas = ['rsctf-web'];
    const context = harness(t, state);
    const result = runKubernetes(context);
    assert.notEqual(result.status, 0);
    assert.doesNotMatch(context.readState().events.join('\n'), /k8s:scale|helm:migrate/);
  });
});

test('Kubernetes scale-down timeout never starts migration', (t) => {
  const state = kubernetesState();
  state.scaleTimeout = true;
  const context = harness(t, state);
  const result = runKubernetes(context, '1s');
  assert.notEqual(result.status, 0);
  assert.doesNotMatch(context.readState().events.join('\n'), /helm:migrate/);
});

test('Kubernetes migration failure stays at zero and retries from stored Helm values', (t) => {
  const state = kubernetesState();
  state.migrationFail = true;
  const context = harness(t, state);
  const first = runKubernetes(context);
  assert.notEqual(first.status, 0);
  let afterFailure = context.readState();
  assert.ok(afterFailure.deployments.every((item) => item.replicas === 0));
  assert.doesNotMatch(afterFailure.events.join('\n'), /helm:runtime/);

  afterFailure.migrationFail = false;
  context.writeState(afterFailure);
  const retry = runKubernetes(context);
  assert.equal(retry.status, 0, retry.stderr);
  afterFailure = context.readState();
  assert.match(afterFailure.events.join('\n'), /k8s:delete-job:/);
  assert.deepEqual(afterFailure.deployments.map((item) => item.replicas), [3, 1]);
  assert.ok(afterFailure.deployments.every((item) => item.image === immutableImage));
});

test('Kubernetes catches a controller restoring desired replicas during migration', (t) => {
  const state = kubernetesState();
  state.reappearAfterMigrationK8s = true;
  const context = harness(t, state);
  const result = runKubernetes(context);
  assert.notEqual(result.status, 0);
  const after = context.readState();
  assert.match(after.events.join('\n'), /helm:migrate/);
  assert.doesNotMatch(after.events.join('\n'), /helm:runtime/);
});

test('Compose cutover stops orphan runtimes, migrates at zero, then removes old images', (t) => {
  const context = harness(t, composeState({ orphan: true }));
  const result = runCompose(context);
  assert.equal(result.status, 0, result.stderr);
  const state = context.readState();
  const migrate = state.events.indexOf('compose:migrate');
  const orphanStop = state.events.indexOf('docker:stop:000000000300');
  const orphanRemove = state.events.indexOf('docker:rm:000000000300');
  const up = state.events.findIndex((value) => value.startsWith('compose:up:'));
  assert.ok(orphanStop >= 0 && orphanStop < migrate);
  assert.ok(orphanRemove > migrate && orphanRemove < up);
  assert.equal(existsSync(context.stateFile), false);
  assert.deepEqual(
    state.containers.filter((item) => item.service === 'rsctf').map((item) => item.image),
    [immutableImage, immutableImage],
  );
  assert.deepEqual(
    state.containers.filter((item) => item.service === 'rsctf-control').map((item) => item.image),
    [immutableImage],
  );
});

test('Compose cutover resolves the pinned image from only the documented env file', (t) => {
  const context = harness(t, composeState());
  const envFile = join(context.directory, '.env');
  writeFileSync(envFile, `# reviewed release\nRSCTF_IMAGE=${immutableImage}\n`);
  const env = { ...context.env };
  delete env.RSCTF_IMAGE;

  const result = runCompose(context, { env, envFile });
  assert.equal(result.status, 0, result.stderr);
  const state = context.readState();
  assert.match(state.events.join('\n'), /compose:migrate/);
  assert.ok(state.containers.every((item) => item.image === immutableImage));
});

test('Compose explicit image overrides a stale environment and remains digest-pinned', (t) => {
  const context = harness(t, composeState());
  const staleEnv = {
    ...context.env,
    RSCTF_IMAGE: `${repositoryImage}@sha256:${'c'.repeat(64)}`,
  };
  const result = runCompose(context, { env: staleEnv, image: immutableImage });
  assert.equal(result.status, 0, result.stderr);
  assert.match(context.readState().events.join('\n'), /compose:migrate/);
});

test('Compose rejects ambiguous or mutable env-file images before stopping runtimes', async (t) => {
  for (const [label, contents] of [
    ['duplicate', `RSCTF_IMAGE=${immutableImage}\nRSCTF_IMAGE=${immutableImage}\n`],
    ['mutable', 'RSCTF_IMAGE=registry.example/rsctf:latest\n'],
  ]) {
    await t.test(label, () => {
      const context = harness(t, composeState());
      const envFile = join(context.directory, `${label}.env`);
      writeFileSync(envFile, contents);
      const env = { ...context.env };
      delete env.RSCTF_IMAGE;
      const result = runCompose(context, { env, envFile });
      assert.notEqual(result.status, 0);
      assert.doesNotMatch(context.readState().events.join('\n'), /compose:stop|compose:migrate/);
    });
  }
});

test('Compose migration failure does not start runtime and retries from protected state', (t) => {
  const state = composeState();
  state.migrationFail = true;
  const context = harness(t, state);
  const first = runCompose(context);
  assert.notEqual(first.status, 0);
  let afterFailure = context.readState();
  assert.ok(afterFailure.containers.every((item) => item.running === false));
  assert.doesNotMatch(afterFailure.events.join('\n'), /compose:up/);
  assert.equal(existsSync(context.stateFile), true);

  afterFailure.migrationFail = false;
  context.writeState(afterFailure);
  const retry = runCompose(context);
  assert.equal(retry.status, 0, retry.stderr);
  afterFailure = context.readState();
  assert.match(afterFailure.events.join('\n'), /compose:up:rsctf-control=1,rsctf=2/);
  assert.equal(existsSync(context.stateFile), false);
});

test('Compose catches an old runtime that reappears after the migration preflight', (t) => {
  const state = composeState();
  state.reappearAfterMigration = true;
  const context = harness(t, state);
  const result = runCompose(context);
  assert.notEqual(result.status, 0);
  const after = context.readState();
  assert.doesNotMatch(after.events.join('\n'), /compose:up/);
  assert.equal(existsSync(context.stateFile), true);
});

test('Compose refuses a runtime whose restart policy could auto-resume', (t) => {
  const state = composeState();
  state.containers[0].restart = 'always';
  const context = harness(t, state);
  const result = runCompose(context);
  assert.notEqual(result.status, 0);
  assert.doesNotMatch(context.readState().events.join('\n'), /compose:stop|compose:migrate/);
});

test('Compose refuses an unrendered legacy project service with no role metadata', (t) => {
  const state = composeState();
  state.containers.push(container(400, 'legacy-renamed-service', ''));
  const context = harness(t, state);
  const result = runCompose(context);
  assert.notEqual(result.status, 0);
  assert.doesNotMatch(context.readState().events.join('\n'), /compose:migrate/);
});
