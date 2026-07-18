import assert from 'node:assert/strict';
import { test } from 'node:test';

import { auditRequiredResourceSamples } from '../worker-plane.js';
import { sampleWorkerResources } from '../worker-resource-sampler.mjs';

const BASE = ['run-rsctf-1', 'run-db-1', 'run-redis-1'];
const WORKER_ID = '018f3c6a-d79b-7cc0-8f68-8fdbad0f57bb';
const AGENT_PID = 4321;

function statsRows(names) {
  return `${names.map((name, index) => JSON.stringify({
    Name: name,
    CPUPerc: `${index + 1}.25%`,
    MemUsage: `${index + 2}MiB / 1GiB`,
    MemPerc: '0.50%',
  })).join('\n')}\n`;
}

function result(overrides = {}) {
  return { status: 0, signal: null, stdout: '', stderr: '', ...overrides };
}

function samplerStub({
  base = result({ stdout: statsRows(BASE) }),
  agent = result({ stdout: '2.50 8192\n' }),
  discovery = result({ stdout: 'worker-a\n' }),
  workload = result({ stdout: statsRows(['worker-a']) }),
} = {}) {
  const calls = [];
  const spawnCommand = (command, args) => {
    calls.push({ command, args });
    if (command === 'ps') return agent;
    if (command === 'docker' && args[0] === 'ps') return discovery;
    if (command === 'docker' && args[0] === 'stats') {
      return args.slice(4).some((name) => name.startsWith('worker-')) ? workload : base;
    }
    throw new Error(`unexpected command: ${command} ${args.join(' ')}`);
  };
  return { calls, spawnCommand };
}

function snapshot(stub, timestampMs = 1) {
  return sampleWorkerResources({
    baseContainers: BASE,
    workerId: WORKER_ID,
    agentPid: AGENT_PID,
    spawnCommand: stub.spawnCommand,
    now: () => timestampMs,
  });
}

test('ephemeral workload stats EOF cannot erase a complete stable sample', () => {
  const stub = samplerStub({
    workload: result({ status: 1, stderr: 'error from daemon: unexpected EOF\n' }),
  });
  const sampled = snapshot(stub);

  assert.deepEqual(sampled.containers.map(({ name }) => name), BASE);
  assert.deepEqual(sampled.agent, {
    pid: AGENT_PID,
    cpuPercent: 2.5,
    memoryBytes: 8 * 1024 * 1024,
  });
  assert.equal(sampled.errors, undefined);
  assert.match(sampled.workloadSamplingWarnings.join('\n'), /unexpected EOF/);
  assert.deepEqual(
    auditRequiredResourceSamples([sampled, { ...sampled, timestampMs: 2 }], BASE),
    { valid: true, errors: [] },
  );

  const statsCalls = stub.calls.filter(({ command, args }) => (
    command === 'docker' && args[0] === 'stats'
  ));
  assert.equal(statsCalls.length, 2);
  assert.deepEqual(statsCalls[0].args.slice(4), BASE);
  assert.deepEqual(statsCalls[1].args.slice(4), ['worker-a']);
});

test('partial or malformed workload output is non-fatal and preserves valid rows', () => {
  const stub = samplerStub({
    discovery: result({ stdout: 'worker-a\nworker-b\n' }),
    workload: result({
      status: null,
      error: new Error('docker stats stream ended with EOF'),
      stdout: `${statsRows(['worker-a'])}{not-json}\n`,
    }),
  });
  const sampled = snapshot(stub);

  assert.deepEqual(sampled.containers.map(({ name }) => name), [...BASE, 'worker-a']);
  assert.equal(sampled.errors, undefined);
  assert.equal(sampled.workloadSamplingWarnings.length, 2);
  assert.match(sampled.workloadSamplingWarnings.join('\n'), /ended with EOF/);
  assert.match(sampled.workloadSamplingWarnings.join('\n'), /docker stats parse/);
  assert.equal(
    auditRequiredResourceSamples([sampled, { ...sampled, timestampMs: 2 }], BASE).valid,
    true,
  );
});

test('missing or failed stable service and agent samples remain fatal', () => {
  const missingBaseStub = samplerStub({
    base: result({ stdout: statsRows(BASE.slice(0, 2)) }),
  });
  const missingBase = snapshot(missingBaseStub);
  const missingBaseAudit = auditRequiredResourceSamples(
    [missingBase, { ...missingBase, timestampMs: 2 }],
    BASE,
  );
  assert.equal(missingBaseAudit.valid, false);
  assert.match(missingBaseAudit.errors.join('\n'), /run-redis-1 has no complete CPU\/RAM sample/);

  const failedBaseStub = samplerStub({
    base: result({ status: 1, stderr: 'unexpected EOF', stdout: statsRows(BASE) }),
  });
  const failedBase = snapshot(failedBaseStub);
  assert.match(failedBase.errors.join('\n'), /base docker stats: unexpected EOF/);
  assert.equal(
    auditRequiredResourceSamples([failedBase, { ...failedBase, timestampMs: 2 }], BASE).valid,
    false,
  );

  const failedAgentStub = samplerStub({
    agent: result({ status: 1, stderr: 'process disappeared' }),
  });
  const failedAgent = snapshot(failedAgentStub);
  assert.match(failedAgent.errors.join('\n'), /agent ps: process disappeared/);
  assert.equal(
    auditRequiredResourceSamples([failedAgent, { ...failedAgent, timestampMs: 2 }], BASE).valid,
    false,
  );
});
