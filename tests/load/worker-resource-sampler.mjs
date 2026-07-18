import { spawnSync } from 'node:child_process';

function parsePercent(value) {
  const parsed = Number.parseFloat(String(value || '').replace('%', ''));
  return Number.isFinite(parsed) ? parsed : undefined;
}

function parseBytes(value) {
  const match = String(value || '').trim().match(/^([0-9.]+)\s*([KMGT]?i?B)$/i);
  if (!match) return undefined;
  const amount = Number(match[1]);
  const units = {
    b: 1,
    kb: 1_000,
    kib: 1_024,
    mb: 1_000_000,
    mib: 1_048_576,
    gb: 1_000_000_000,
    gib: 1_073_741_824,
    tb: 1_000_000_000_000,
    tib: 1_099_511_627_776,
  };
  const multiplier = units[match[2].toLowerCase()];
  return Number.isFinite(amount) && multiplier ? Math.round(amount * multiplier) : undefined;
}

function commandFailure(result) {
  const details = [
    result?.error?.message,
    String(result?.stderr || '').trim(),
    result?.signal ? `terminated by ${result.signal}` : undefined,
    Number.isInteger(result?.status) ? `exit ${result.status}` : undefined,
  ].filter(Boolean);
  return details.join('; ') || 'command failed without diagnostics';
}

function run(spawnCommand, command, args, options) {
  try {
    return spawnCommand(command, args, options);
  } catch (error) {
    return { status: null, stdout: '', stderr: '', error };
  }
}

function parseDockerStats(stdout, scope) {
  const containers = [];
  const errors = [];
  for (const line of String(stdout || '').split('\n').filter((value) => value.trim())) {
    try {
      const row = JSON.parse(line);
      const used = String(row.MemUsage || '').split('/')[0].trim();
      const sample = {
        name: row.Name,
        cpuPercent: parsePercent(row.CPUPerc),
        memoryBytes: parseBytes(used),
        memoryPercent: parsePercent(row.MemPerc),
      };
      if (
        !sample.name ||
        !Number.isFinite(sample.cpuPercent) ||
        !Number.isFinite(sample.memoryBytes)
      ) {
        errors.push(`${scope} docker stats row is incomplete: ${line.slice(0, 200)}`);
        continue;
      }
      containers.push(sample);
    } catch (error) {
      errors.push(`${scope} docker stats parse: ${error.message}`);
    }
  }
  return { containers, errors };
}

function sampleContainers(spawnCommand, names, scope, options) {
  const uniqueNames = [...new Set(names.map(String).filter(Boolean))];
  if (!uniqueNames.length) return { containers: [], errors: [] };
  const sampled = run(
    spawnCommand,
    'docker',
    ['stats', '--no-stream', '--format', '{{json .}}', ...uniqueNames],
    options,
  );
  const parsed = parseDockerStats(sampled?.stdout, scope);
  if (!sampled || sampled.error || sampled.status !== 0) {
    parsed.errors.unshift(`${scope} docker stats: ${commandFailure(sampled)}`);
  }
  return parsed;
}

function sampleAgent(spawnCommand, agentPid, options) {
  if (!Number.isSafeInteger(agentPid) || agentPid <= 0) {
    return { error: 'worker agent process is unavailable' };
  }
  const sampled = run(
    spawnCommand,
    'ps',
    ['-p', String(agentPid), '-o', '%cpu=,rss='],
    options,
  );
  const fields = String(sampled?.stdout || '').trim().split(/\s+/).filter(Boolean);
  if (!sampled || sampled.error || sampled.status !== 0 || fields.length < 2) {
    return { error: `agent ps: ${commandFailure(sampled)}` };
  }
  const agent = {
    pid: agentPid,
    cpuPercent: Number(fields[0]),
    memoryBytes: Number(fields[1]) * 1_024,
  };
  if (!Number.isFinite(agent.cpuPercent) || !Number.isFinite(agent.memoryBytes)) {
    return { error: 'agent ps returned incomplete CPU/RAM fields' };
  }
  return { agent };
}

function discoverWorkloadContainers(spawnCommand, workerId, options) {
  if (!workerId) return { names: [], errors: [] };
  const listed = run(
    spawnCommand,
    'docker',
    [
      'ps', '--format', '{{.Names}}',
      '--filter', `label=io.rsctf.worker.id=${workerId}`,
    ],
    options,
  );
  if (!listed || listed.error || listed.status !== 0) {
    return {
      names: [],
      errors: [`worker container discovery: ${commandFailure(listed)}`],
    };
  }
  return {
    names: String(listed.stdout || '').trim().split(/\s+/).filter(Boolean),
    errors: [],
  };
}

export function sampleWorkerResources({
  baseContainers,
  workerId,
  agentPid,
  cwd,
  env,
  spawnCommand = spawnSync,
  now = Date.now,
}) {
  const options = { cwd, encoding: 'utf8', env };

  // Stable services and the native agent are sampled first and independently.
  // Workload replicas may disappear between discovery and `docker stats` during
  // a rollout, so their failures are retained as warnings without invalidating
  // a complete stable sample.
  const stable = sampleContainers(spawnCommand, baseContainers, 'base', options);
  const sampledAgent = sampleAgent(spawnCommand, agentPid, options);
  const stableErrors = [...stable.errors];
  if (sampledAgent.error) stableErrors.push(sampledAgent.error);

  const discovered = discoverWorkloadContainers(spawnCommand, workerId, options);
  const workloads = sampleContainers(spawnCommand, discovered.names, 'workload', options);
  const workloadSamplingWarnings = [...discovered.errors, ...workloads.errors];
  const containers = [...stable.containers];
  const knownNames = new Set(containers.map(({ name }) => name));
  for (const container of workloads.containers) {
    if (!knownNames.has(container.name)) {
      containers.push(container);
      knownNames.add(container.name);
    }
  }

  return {
    timestampMs: now(),
    containers,
    agent: sampledAgent.agent,
    ...(stableErrors.length ? { errors: stableErrors } : {}),
    ...(workloadSamplingWarnings.length ? { workloadSamplingWarnings } : {}),
  };
}
