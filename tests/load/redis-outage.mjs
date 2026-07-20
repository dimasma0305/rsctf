// Exercise the Redis-independent request fallback against a disposable stack.
// The exact container name must be repeated as an acknowledgement so invoking
// this npm script cannot silently stop an ambient production dependency.
import { spawnSync } from 'node:child_process';

const TARGET = process.env.TARGET || 'http://127.0.0.1:8080';
const REDIS = process.env.REDIS_CONTAINER || '';
const ACK = process.env.CONFIRM_REDIS_OUTAGE || '';

if (!REDIS || ACK !== REDIS) {
  throw new Error(
    'set REDIS_CONTAINER to a disposable Redis container and repeat its exact name in CONFIRM_REDIS_OUTAGE'
  );
}

const targetUrl = new URL(TARGET);
const loopback = new Set(['127.0.0.1', 'localhost', '::1']);
if (!loopback.has(targetUrl.hostname) && process.env.CONFIRM_REMOTE_REDIS_OUTAGE !== targetUrl.origin) {
  throw new Error(`remote TARGET requires CONFIRM_REMOTE_REDIS_OUTAGE=${targetUrl.origin}`);
}

function command(program, args, options = {}) {
  const result = spawnSync(program, args, { encoding: 'utf8', ...options });
  if (result.status !== 0) {
    throw new Error(`${program} ${args.join(' ')} failed: ${(result.stderr || result.stdout).trim()}`);
  }
  return result.stdout.trim();
}

const identity = command('docker', [
  'inspect',
  '--format',
  '{{index .Config.Labels "com.docker.compose.service"}}|{{.State.Running}}',
  REDIS,
]);
if (identity !== 'redis|true') {
  throw new Error(`${REDIS} is not a running Compose redis service (${identity || 'unknown'})`);
}

async function status(path) {
  try {
    return (await fetch(new URL(path, targetUrl), { signal: AbortSignal.timeout(1500) })).status;
  } catch {
    return 0;
  }
}

async function waitFor(predicate, label, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  let observed;
  while (Date.now() < deadline) {
    observed = await predicate();
    if (observed.ok) return observed;
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`${label} did not settle; last observation: ${JSON.stringify(observed)}`);
}

let stopped = false;
let runError;
try {
  command('docker', ['stop', '--time', '5', REDIS]);
  stopped = true;
  const outage = await waitFor(async () => {
    const [livez, healthz] = await Promise.all([status('/livez'), status('/healthz')]);
    return { ok: livez === 200 && healthz !== 200, livez, healthz };
  }, 'Redis outage health state');
  console.log(`outage_ready livez=${outage.livez} healthz=${outage.healthz}`);

  const args = ['run'];
  if (process.env.SUMMARY_JSON) args.push('--summary-export', process.env.SUMMARY_JSON);
  args.push(new URL('./k6/redis-outage.js', import.meta.url).pathname);
  const result = spawnSync('k6', args, { stdio: 'inherit', env: process.env });
  if (result.status !== 0) runError = new Error(`k6 exited with status ${result.status ?? 'unknown'}`);
} finally {
  if (stopped) {
    command('docker', ['start', REDIS]);
    const recovered = await waitFor(async () => {
      const [livez, healthz] = await Promise.all([status('/livez'), status('/healthz')]);
      return { ok: livez === 200 && healthz === 200, livez, healthz };
    }, 'Redis recovery');
    console.log(`recovered livez=${recovered.livez} healthz=${recovered.healthz}`);
  }
}

if (runError) throw runError;
