import { spawnSync } from 'node:child_process';

export function fatalLogLineCount(...chunks) {
  return chunks
    .join('\n')
    .split('\n')
    .filter((line) => /panic|fatal/i.test(line)).length;
}

export function countContainerFatalLogs(container, sinceMs, spawn = spawnSync) {
  if (typeof container !== 'string' || container.trim() === '') {
    throw new Error('RSCTF container name is required for the fatal-log audit');
  }
  if (!Number.isSafeInteger(sinceMs) || sinceMs <= 0) {
    throw new Error(`fatal-log audit start must be a positive Unix millisecond timestamp (got ${sinceMs})`);
  }

  const since = new Date(sinceMs).toISOString();
  const result = spawn('docker', ['logs', '--since', since, container], {
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const detail = String(result.stderr || result.stdout || '').trim().slice(0, 500);
    throw new Error(`could not audit ${container} logs${detail ? `: ${detail}` : ''}`);
  }
  return fatalLogLineCount(result.stdout || '', result.stderr || '');
}
