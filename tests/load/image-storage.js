// Shared validation and result reduction for the on-demand image stress test.

export function positiveInteger(value, label, maximum = Number.MAX_SAFE_INTEGER) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0 || parsed > maximum) {
    throw new Error(`${label} must be an integer from 1 through ${maximum}`);
  }
  return parsed;
}

export function parseImageStorageContext(raw) {
  let value;
  try {
    value = JSON.parse(raw);
  } catch {
    throw new Error('IMAGE_STORAGE_CONTEXT must be valid JSON');
  }
  const gameId = positiveInteger(value?.gameId, 'game id');
  const challengeId = positiveInteger(value?.challengeId, 'challenge id');
  const tokens = Array.isArray(value?.tokens) ? value.tokens.map(String) : [];
  if (tokens.length < 2 || tokens.length > 256) {
    throw new Error('IMAGE_STORAGE_CONTEXT must contain 2 through 256 player tokens');
  }
  if (new Set(tokens).size !== tokens.length || tokens.some((token) => token.split('.').length !== 3)) {
    throw new Error('IMAGE_STORAGE_CONTEXT player tokens must be unique JWTs');
  }
  return Object.freeze({ gameId, challengeId, tokens: Object.freeze(tokens) });
}

export function parseDockerStat(line) {
  const [name = '', cpu = '', memory = ''] = String(line).trim().split('|');
  const cpuPercent = Number(cpu.replace('%', ''));
  const memoryMatch = memory.trim().match(/^(\d+(?:\.\d+)?)\s*(B|KiB|MiB|GiB|TiB)$/i);
  const units = { b: 1, kib: 1024, mib: 1024 ** 2, gib: 1024 ** 3, tib: 1024 ** 4 };
  const memoryBytes = memoryMatch
    ? Number(memoryMatch[1]) * units[memoryMatch[2].toLowerCase()]
    : Number.NaN;
  if (!name || !Number.isFinite(cpuPercent) || cpuPercent < 0 || !Number.isFinite(memoryBytes) || memoryBytes < 0) {
    throw new Error(`invalid Docker resource sample: ${line}`);
  }
  return { name, cpuPercent, memoryBytes };
}

export function parseProcessStat(line, pid) {
  const processId = positiveInteger(pid, 'RSCTF_PROCESS_PID');
  const fields = String(line).trim().split(/\s+/).map(Number);
  if (
    fields.length !== 2 ||
    !Number.isFinite(fields[0]) ||
    fields[0] < 0 ||
    !Number.isSafeInteger(fields[1]) ||
    fields[1] <= 0
  ) {
    throw new Error(`invalid process resource sample: ${line}`);
  }
  return { name: `pid:${processId}`, cpuPercent: fields[0], memoryBytes: fields[1] * 1024 };
}

export function parseFilesystemStat(raw) {
  const lines = String(raw).trim().split(/\r?\n/);
  const fields = (lines.at(-1) || '').trim().split(/\s+/).map(Number);
  if (fields.length !== 2 || fields.some((value) => !Number.isSafeInteger(value) || value < 0)) {
    throw new Error(`invalid filesystem resource sample: ${raw}`);
  }
  return { totalBytes: fields[0], availableBytes: fields[1] };
}

export function summarizeResourceSamples(samples) {
  const rows = samples.flatMap((sample) => sample.resources || []);
  return {
    samples: samples.length,
    maxCpuPercent: rows.reduce((maximum, row) => Math.max(maximum, row.cpuPercent), 0),
    maxMemoryBytes: rows.reduce((maximum, row) => Math.max(maximum, row.memoryBytes), 0),
    minimumFilesystemAvailableBytes: samples.reduce(
      (minimum, sample) => Math.min(minimum, sample.filesystem?.availableBytes ?? minimum),
      Number.MAX_SAFE_INTEGER,
    ),
    healthFailures: samples.filter((sample) => sample.healthStatus !== 200 || sample.healthBody !== 'ok').length,
  };
}
