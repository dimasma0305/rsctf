import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  assertByocFixtureImages,
  assertByocRestartTarget,
  assertExactTunnelCount,
  assertOwnedByocContainer,
  byocFixtureLabels,
  byocFixtureNames,
  byocFixtureOwner,
  dockerByocRunFilterArgs,
  normalizeByocRunId,
} from '../byoc-harness.js';

const digest = (repository) => `${repository}@sha256:${'a'.repeat(64)}`;

function restartResource(overrides = {}) {
  return {
    Id: 'b'.repeat(64),
    Name: '/isolated-rsctf-1',
    Config: {
      Labels: {
        'com.docker.compose.project': 'isolated',
        'com.docker.compose.service': 'rsctf',
      },
      Env: ['RSCTF_ADMIN_LIFECYCLE_MARKER=isolated-stack-marker'],
    },
    State: { Running: true, StartedAt: '2026-07-20T12:00:00.000000000Z' },
    ...overrides,
  };
}

test('BYOC fixture run ids produce disjoint service and relay names', () => {
  const first = byocFixtureNames(normalizeByocRunId('accept-a1'));
  const second = byocFixtureNames(normalizeByocRunId('accept-b2'));
  assert.equal(first.service, 'load_svc_accept-a1');
  assert.equal(first.agent(7), 'load_agent_accept-a1_7');
  assert.notEqual(first.service, second.service);
  assert.notEqual(first.agent(7), second.agent(7));
  assert.throws(() => normalizeByocRunId('../ambient'), /DNS-safe identifier/);
  assert.throws(() => normalizeByocRunId('UPPER'), /DNS-safe identifier/);
});

test('BYOC reportable fixtures require immutable agent and service images', () => {
  assert.deepEqual(
    assertByocFixtureImages({
      agentImage: digest('ghcr.io/example/agent'),
      serviceImage: `sha256:${'c'.repeat(64)}`,
      reportable: true,
    }),
    {
      agentImage: digest('ghcr.io/example/agent'),
      serviceImage: `sha256:${'c'.repeat(64)}`,
      reportable: true,
    },
  );
  assert.doesNotThrow(() =>
    assertByocFixtureImages({
      agentImage: 'agent:diagnostic',
      serviceImage: 'nginx:alpine',
      reportable: false,
    }),
  );
  assert.throws(
    () =>
      assertByocFixtureImages({
        agentImage: 'agent:latest',
        serviceImage: digest('nginx'),
        reportable: true,
      }),
    /RSCTF_BYOC_AGENT_IMAGE.*immutable/,
  );
  assert.throws(
    () =>
      assertByocFixtureImages({
        agentImage: digest('agent'),
        serviceImage: 'nginx:alpine',
        reportable: true,
      }),
    /RSCTF_BYOC_SERVICE_IMAGE.*immutable/,
  );
});

test('BYOC ownership labels bind cleanup to the exact run, role, and name', () => {
  const run = 'owned-a1';
  const names = byocFixtureNames(run);
  const labels = byocFixtureLabels(run, 'relay', {
    index: 3,
    participationId: 11,
    challengeId: 22,
  });
  assert.equal(labels['rsctf.load.byoc.owner'], byocFixtureOwner);
  assert.deepEqual(dockerByocRunFilterArgs(run), [
    '--filter',
    `label=rsctf.load.byoc.owner=${byocFixtureOwner}`,
    '--filter',
    `label=rsctf.load.byoc.run=${run}`,
  ]);
  assert.deepEqual(
    assertOwnedByocContainer(
      { Id: 'd'.repeat(64), Name: `/${names.agent(3)}`, Config: { Labels: labels } },
      run,
    ),
    { id: 'd'.repeat(64), name: names.agent(3), role: 'relay' },
  );
  assert.throws(
    () =>
      assertOwnedByocContainer(
        { Id: 'd'.repeat(64), Name: '/load_agent_someone-else_3', Config: { Labels: labels } },
        run,
      ),
    /inconsistent name\/index labels/,
  );
  assert.throws(
    () =>
      assertOwnedByocContainer(
        {
          Id: 'd'.repeat(64),
          Name: `/${names.agent(3)}`,
          Config: { Labels: { ...labels, 'rsctf.load.byoc.run': 'other-a1' } },
        },
        run,
      ),
    /outside the exact BYOC run ownership scope/,
  );
});

test('exact tunnel-count contract rejects partial and oversized fleets', () => {
  assert.equal(assertExactTunnelCount(60, 60), 60);
  assert.throws(() => assertExactTunnelCount(60, 59), /must equal 60, observed 59/);
  assert.throws(() => assertExactTunnelCount(60, 61), /must equal 60, observed 61/);
});

test('reconnect target requires exact acknowledgement and reportable disposable identity', () => {
  const diagnostic = {
    RSCTF_CONTAINER: 'isolated-rsctf-1',
    CONFIRM_RSCTF_RESTART: 'isolated-rsctf-1',
  };
  assert.deepEqual(assertByocRestartTarget(diagnostic, restartResource()), {
    id: 'b'.repeat(64),
    name: 'isolated-rsctf-1',
    project: 'isolated',
    service: 'rsctf',
    startedAt: '2026-07-20T12:00:00.000000000Z',
    reportable: false,
  });
  assert.throws(
    () => assertByocRestartTarget({ ...diagnostic, CONFIRM_RSCTF_RESTART: 'other' }, restartResource()),
    /acknowledge the exact disposable replica/,
  );
  assert.throws(
    () =>
      assertByocRestartTarget(
        { ...diagnostic, COMPOSE_PROJECT_NAME: 'ambient' },
        restartResource(),
      ),
    /not COMPOSE_PROJECT_NAME=ambient/,
  );

  const reportable = {
    ...diagnostic,
    COMPOSE_PROJECT_NAME: 'isolated',
    ADMIN_LIFECYCLE_STACK_MARKER: 'isolated-stack-marker',
    RSCTF_ACCEPTANCE_REPORTABLE: '1',
  };
  assert.equal(assertByocRestartTarget(reportable, restartResource()).reportable, true);
  assert.throws(
    () =>
      assertByocRestartTarget(
        { ...reportable, ADMIN_LIFECYCLE_STACK_MARKER: 'different-stack-marker' },
        restartResource(),
      ),
    /does not carry the one exact disposable-stack marker/,
  );
  assert.throws(
    () =>
      assertByocRestartTarget(
        reportable,
        restartResource({
          Config: {
            Labels: {
              'com.docker.compose.project': 'isolated',
              'com.docker.compose.service': 'rsctf-control',
            },
            Env: ['RSCTF_ADMIN_LIFECYCLE_MARKER=isolated-stack-marker'],
          },
        }),
      ),
    /must be a Compose rsctf web replica/,
  );
});

test('BYOC drivers check destructive commands, exact reconnects, and reportable k6 errors', () => {
  const worstCase = readFileSync(new URL('../worst-case.mjs', import.meta.url), 'utf8');
  const agents = readFileSync(new URL('../byoc-agents.mjs', import.meta.url), 'utf8');
  const byocDriver = readFileSync(new URL('../byoc.mjs', import.meta.url), 'utf8');
  const busyDriver = readFileSync(new URL('../byoc-busy.mjs', import.meta.url), 'utf8');
  const k6 = readFileSync(new URL('../k6/byoc-requests.js', import.meta.url), 'utf8');

  assert.doesNotMatch(worstCase, /rsctf-rsctf-1/);
  assert.match(worstCase, /checkedDocker\(\['restart', RSCTF\]/);
  assert.match(worstCase, /verifyAgentReconnects\(t0, N\)/);
  assert.match(worstCase, /countContainerFatalLogs\(RSCTF, runStartedAt\)/);
  assert.match(agents, /dockerByocRunFilterArgs\(BYOC_RUN_ID\)/);
  assert.doesNotMatch(agents, /--filter', 'name=load_agent_/);
  assert.match(agents, /timed out waiting for exactly \$\{n\} BYOC tunnels/);
  assert.match(byocDriver, /if \(exitCode !== 0\) throw new Error/);
  assert.match(busyDriver, /if \(k6ExitCode !== 0\)/);
  assert.match(k6, /server_5xx: \['rate==0'\]/);
  assert.match(k6, /non_200: \['rate==0'\]/);
  assert.match(k6, /http_req_failed: \['rate==0'\]/);
});
