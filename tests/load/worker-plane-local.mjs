// Isolated current-tree trusted-worker acceptance runner.
//
// It builds rsctf plus the native Linux worker agent, starts a uniquely named
// Compose project on loopback-only random ports, enrolls one agent, provisions a
// minimal real game, and delegates the actual lifecycle/proxy gate to
// worker-plane.mjs. Cleanup is scoped to the unique Compose project and worker
// identity; the normal `rsctf` project is never addressed.
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { once } from 'node:events';
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  assertFreshIsolatedProject,
  auditRequiredResourceSamples,
  canCleanupComposeProject,
  canRemoveDaemonSentinel,
  isolatedComposeEnvironment,
  requireMatchingSha256,
  validateIsolatedProject,
} from './worker-plane.js';
import {
  assertRepositorySourceFingerprints,
  expectedSourceFingerprints,
} from './source-fingerprint.mjs';
import { sampleWorkerResources } from './worker-resource-sampler.mjs';

const LOAD_ROOT = dirname(fileURLToPath(import.meta.url));
const REPOSITORY_ROOT = resolve(LOAD_ROOT, '../..');
const DEPLOY_COMPOSE = resolve(REPOSITORY_ROOT, 'deploy/compose.yml');
const WORKER_COMPOSE = resolve(REPOSITORY_ROOT, 'deploy/compose.workers.yml');
const FIXTURE_CONTEXT = resolve(
  REPOSITORY_ROOT,
  'examples/challenge-repository/Jeopardy/Web/static-flag-service/src',
);

function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer (got ${value})`);
  }
  return parsed;
}

const FLEET = positiveInteger(process.env.FLEET || process.env.N || 5, 'FLEET');
const WORKLOAD_SERVICE_COUNT = 2;
const WORKLOAD_REPLICA_COUNT = 3;
const SCALED_WORKLOAD_REPLICA_COUNT = 5;
const PROJECT = validateIsolatedProject(
  process.env.E2E_PROJECT ||
    `rsctf-worker-e2e-${process.pid}-${randomBytes(4).toString('hex')}`,
);
const EXPECTED_SOURCE_FINGERPRINTS = expectedSourceFingerprints(process.env);

const TEMP_ROOT = mkdtempSync(resolve(tmpdir(), `${PROJECT}-`));
chmodSync(TEMP_ROOT, 0o700);
const PKI_ROOT = resolve(TEMP_ROOT, 'pki');
const AGENT_STATE = resolve(TEMP_ROOT, 'agent');
const BUILT_RSCTF_IMAGE = `${PROJECT}:current`;
const RSCTF_IMAGE = process.env.E2E_RSCTF_IMAGE || BUILT_RSCTF_IMAGE;
const BUILT_FIXTURE_IMAGE = `${PROJECT}-fixture:current`;
const FIXTURE_IMAGE = process.env.E2E_FIXTURE_IMAGE || BUILT_FIXTURE_IMAGE;
const AGENT_BINARY = resolve(
  REPOSITORY_ROOT,
  process.env.E2E_AGENT_BIN || 'agents/worker-agent/target/release/rsctf-worker-agent',
);
const BUILDS_RSCTF = !process.env.E2E_RSCTF_IMAGE;
const BUILDS_FIXTURE = !process.env.E2E_FIXTURE_IMAGE;
const BUILDS_AGENT = !process.env.E2E_AGENT_BIN;
const KEEP_IMAGES = process.env.E2E_KEEP_IMAGES === '1';
const POSTGRES_IMAGE = process.env.E2E_POSTGRES_IMAGE || 'postgres:18.4-alpine3.24';
const REDIS_IMAGE = process.env.E2E_REDIS_IMAGE || 'redis:7-alpine';
const DOCKER_CONTEXT = process.env.E2E_DOCKER_CONTEXT || 'default';
const DOCKER_ENVIRONMENT = isolatedComposeEnvironment(process.env, {
  DOCKER_CONTEXT,
});
const RESOURCE_JSON = String(process.env.RESOURCE_JSON || '').trim();
const RESOURCE_INTERVAL_MS = positiveInteger(
  process.env.RESOURCE_INTERVAL_MS || 1_000,
  'RESOURCE_INTERVAL_MS',
);

let composeEnvironment;
let agentProcess;
let workerId;
let daemonSentinelPreexisting = true;
let interruptedSignal;
let composeCleanupClaim;
let builtRsctfImage = false;
let builtFixtureImage = false;

function command(commandName, args, options = {}) {
  const output = execFileSync(commandName, args, {
    cwd: REPOSITORY_ROOT,
    encoding: 'utf8',
    env: DOCKER_ENVIRONMENT,
    ...options,
  });
  return typeof output === 'string' ? output.trim() : '';
}

function bestEffort(commandName, args, options = {}) {
  return spawnSync(commandName, args, {
    cwd: REPOSITORY_ROOT,
    encoding: 'utf8',
    stdio: 'ignore',
    env: DOCKER_ENVIRONMENT,
    ...options,
  });
}

function runStreaming(commandName, args, options = {}) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(commandName, args, {
      cwd: REPOSITORY_ROOT,
      stdio: 'inherit',
      ...options,
    });
    child.once('error', reject);
    child.once('exit', (code, signal) => {
      if (code === 0) resolveRun();
      else reject(new Error(`${commandName} exited with ${code ?? signal}`));
    });
  });
}

function compose(args, options = {}) {
  return command(
    'docker',
    ['compose', '--project-name', PROJECT, '--file', DEPLOY_COMPOSE, '--file', WORKER_COMPOSE, ...args],
    { env: composeEnvironment, ...options },
  );
}

function dockerLines(args) {
  return command('docker', args).split(/\s+/).filter(Boolean);
}

function imageTagExists(image) {
  const inspected = spawnSync('docker', ['image', 'inspect', image], {
    cwd: REPOSITORY_ROOT,
    encoding: 'utf8',
    env: DOCKER_ENVIRONMENT,
    stdio: 'ignore',
  });
  if (inspected.status === 0) return true;
  if (inspected.status === 1) return false;
  throw new Error(`could not verify whether image tag ${image} already exists`);
}

function preflightIsolatedProject() {
  const label = `com.docker.compose.project=${PROJECT}`;
  const resources = [
    ...dockerLines(['ps', '--all', '--format', '{{.Names}}', '--filter', `label=${label}`]),
    ...dockerLines(['network', 'ls', '--format', '{{.Name}}', '--filter', `label=${label}`]),
    ...dockerLines(['volume', 'ls', '--format', '{{.Name}}', '--filter', `label=${label}`]),
  ];
  const imageTags = [
    ...(BUILDS_RSCTF && imageTagExists(RSCTF_IMAGE) ? [RSCTF_IMAGE] : []),
    ...(BUILDS_FIXTURE && imageTagExists(FIXTURE_IMAGE) ? [FIXTURE_IMAGE] : []),
  ];
  assertFreshIsolatedProject(PROJECT, { resources, imageTags });
}

function sha256File(path) {
  return `sha256:${createHash('sha256').update(readFileSync(path)).digest('hex')}`;
}

function dockerImageId(image) {
  return command('docker', ['image', 'inspect', image, '--format', '{{.Id}}']);
}

function runningContainerImageId(container) {
  return command('docker', ['inspect', container, '--format', '{{.Image}}']);
}

async function reservePort(explicit, label) {
  if (explicit) return positiveInteger(explicit, label);
  const server = createServer();
  server.unref();
  await new Promise((resolveListen, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolveListen);
  });
  const address = server.address();
  const port = typeof address === 'object' && address ? address.port : 0;
  await new Promise((resolveClose, reject) => server.close((error) => (error ? reject(error) : resolveClose())));
  return positiveInteger(port, label);
}

function openssl(args) {
  command('openssl', args, { stdio: ['ignore', 'ignore', 'inherit'] });
}

function createWorkerPki() {
  mkdirSync(PKI_ROOT, { recursive: true, mode: 0o700 });
  const caKey = resolve(PKI_ROOT, 'worker-ca.key');
  const caCert = resolve(PKI_ROOT, 'worker-ca.crt');
  const serverKey = resolve(PKI_ROOT, 'worker-server.key');
  const serverCsr = resolve(PKI_ROOT, 'worker-server.csr');
  const serverCert = resolve(PKI_ROOT, 'worker-server.crt');

  openssl(['genpkey', '-algorithm', 'EC', '-pkeyopt', 'ec_paramgen_curve:P-256', '-out', caKey]);
  openssl([
    'req', '-x509', '-new', '-sha256', '-days', '7', '-key', caKey,
    '-subj', '/CN=RSCTF isolated worker CA',
    '-addext', 'basicConstraints=critical,CA:TRUE',
    '-addext', 'keyUsage=critical,keyCertSign,cRLSign',
    '-out', caCert,
  ]);
  openssl(['genpkey', '-algorithm', 'EC', '-pkeyopt', 'ec_paramgen_curve:P-256', '-out', serverKey]);
  openssl([
    'req', '-new', '-sha256', '-key', serverKey,
    '-subj', '/CN=127.0.0.1',
    '-addext', 'basicConstraints=critical,CA:FALSE',
    '-addext', 'keyUsage=critical,digitalSignature,keyEncipherment',
    '-addext', 'extendedKeyUsage=serverAuth',
    '-addext', 'subjectAltName=IP:127.0.0.1',
    '-out', serverCsr,
  ]);
  openssl([
    'x509', '-req', '-sha256', '-days', '7', '-in', serverCsr,
    '-CA', caCert, '-CAkey', caKey, '-CAcreateserial',
    '-copy_extensions', 'copy', '-out', serverCert,
  ]);
  chmodSync(caKey, 0o600);
  chmodSync(serverKey, 0o600);
  return { caKey, caCert, serverKey, serverCert };
}

async function waitFor(check, timeoutMs, description) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    if (interruptedSignal) throw new Error(`interrupted by ${interruptedSignal}`);
    try {
      const value = await check();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolveWait) => setTimeout(resolveWait, 500));
  }
  throw new Error(
    `timed out waiting for ${description}${lastError ? `: ${lastError.message}` : ''}`,
  );
}

async function api(target, method, path, { token, body, timeoutMs = 30_000 } = {}) {
  const headers = {};
  if (token) headers.authorization = `Bearer ${token}`;
  if (body !== undefined) headers['content-type'] = 'application/json';
  const response = await fetch(`${target}${path}`, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const text = await response.text();
  let json;
  try {
    json = text ? JSON.parse(text) : undefined;
  } catch {
    json = undefined;
  }
  if (response.status >= 300) {
    throw new Error(`${method} ${path} returned ${response.status}: ${json?.title || text.slice(0, 300)}`);
  }
  return json;
}

function startAgent() {
  const tail = [];
  const args = [
    'run', '--config', resolve(AGENT_STATE, 'worker.json'),
    '--accept-host-network-boundary',
    '--allow-unbounded-storage',
    '--slots', String(FLEET),
    '--label', `e2e-project=${PROJECT}`,
  ];
  const child = spawn(AGENT_BINARY, args, {
    cwd: REPOSITORY_ROOT,
    env: { ...process.env, RUST_LOG: process.env.E2E_AGENT_LOG || 'info' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const relay = (stream, destination) => {
    stream.on('data', (chunk) => {
      const text = chunk.toString();
      destination.write(text);
      tail.push(text);
      if (tail.length > 40) tail.shift();
    });
  };
  relay(child.stdout, process.stdout);
  relay(child.stderr, process.stderr);
  child.logTail = tail;
  return child;
}

async function stopAgent() {
  if (!agentProcess || agentProcess.exitCode !== null) return;
  agentProcess.kill('SIGTERM');
  const exited = Promise.race([
    once(agentProcess, 'exit'),
    new Promise((resolveWait) => setTimeout(() => resolveWait(false), 5_000)),
  ]);
  if ((await exited) === false && agentProcess.exitCode === null) {
    agentProcess.kill('SIGKILL');
    await once(agentProcess, 'exit');
  }
}

function cleanupWorkerResources() {
  if (!workerId) return;
  try {
    const filter = `label=io.rsctf.worker.id=${workerId}`;
    const containers = command('docker', ['ps', '--all', '--quiet', '--filter', filter]);
    if (containers) bestEffort('docker', ['rm', '--force', ...containers.split(/\s+/)]);
    const networks = command('docker', ['network', 'ls', '--quiet', '--filter', filter]);
    if (networks) bestEffort('docker', ['network', 'rm', ...networks.split(/\s+/)]);
    const volumes = command('docker', ['volume', 'ls', '--quiet', '--filter', filter]);
    if (volumes) bestEffort('docker', ['volume', 'rm', ...volumes.split(/\s+/)]);

    const sentinel = spawnSync(
      'docker',
      ['volume', 'inspect', '--format', '{{json .Labels}}', 'rsctf-worker-owner'],
      { cwd: REPOSITORY_ROOT, encoding: 'utf8', env: DOCKER_ENVIRONMENT },
    );
    if (sentinel.status === 0) {
      const labels = JSON.parse(String(sentinel.stdout || '{}'));
      if (canRemoveDaemonSentinel(labels, workerId, daemonSentinelPreexisting)) {
        bestEffort('docker', ['volume', 'rm', 'rsctf-worker-owner']);
      }
    }
  } catch (error) {
    console.error(`worker resource cleanup failed: ${error.message}`);
  }
}

function resourceSnapshot(baseContainers) {
  return sampleWorkerResources({
    baseContainers,
    workerId,
    agentPid: agentProcess?.pid,
    cwd: REPOSITORY_ROOT,
    env: DOCKER_ENVIRONMENT,
  });
}

async function runWorkerGate(childEnvironment, resourceMetadata) {
  if (!RESOURCE_JSON) {
    await runStreaming(process.execPath, [resolve(LOAD_ROOT, 'worker-plane.mjs')], {
      cwd: LOAD_ROOT,
      env: childEnvironment,
    });
    return;
  }

  const baseContainers = [
    childEnvironment.RSCTF_CONTAINER,
    childEnvironment.PG_CONTAINER,
    `${PROJECT}-redis-1`,
  ];
  const samples = [resourceSnapshot(baseContainers)];
  const timer = setInterval(() => samples.push(resourceSnapshot(baseContainers)), RESOURCE_INTERVAL_MS);
  timer.unref();
  let outcome = 'passed';
  let gateError;
  try {
    await runStreaming(process.execPath, [resolve(LOAD_ROOT, 'worker-plane.mjs')], {
      cwd: LOAD_ROOT,
      env: childEnvironment,
    });
  } catch (error) {
    outcome = 'failed';
    gateError = error;
  }
  clearInterval(timer);
  samples.push(resourceSnapshot(baseContainers));
  const metrics = auditRequiredResourceSamples(samples, baseContainers);
  if (!metrics.valid && outcome === 'passed') outcome = 'invalid-metrics';
  const destination = resolve(LOAD_ROOT, RESOURCE_JSON);
  mkdirSync(dirname(destination), { recursive: true });
  writeFileSync(
    destination,
    `${JSON.stringify({
      schemaVersion: 1,
      project: PROJECT,
      target: childEnvironment.TARGET,
      workerId,
      outcome,
      metrics,
      intervalMs: RESOURCE_INTERVAL_MS,
      load: {
        fleet: Number(childEnvironment.FLEET),
        rate: Number(childEnvironment.RATE),
        vus: Number(childEnvironment.VUS),
        duration: childEnvironment.DURATION,
        servicesPerWorkload: WORKLOAD_SERVICE_COUNT,
        baseReplicasPerWorkload: WORKLOAD_REPLICA_COUNT,
        scaledReplicasPerWorkload: SCALED_WORKLOAD_REPLICA_COUNT,
      },
      artifacts: resourceMetadata,
      samples,
    }, null, 2)}\n`,
    { mode: 0o600 },
  );
  console.log(`  resource time series: ${destination} (${samples.length} samples)`);
  if (gateError) throw gateError;
  if (!metrics.valid) {
    throw new Error(`resource time series is incomplete: ${metrics.errors.join('; ')}`);
  }
}

async function cleanup() {
  await stopAgent();
  cleanupWorkerResources();
  if (
    composeEnvironment &&
    canCleanupComposeProject(PROJECT, composeCleanupClaim)
  ) {
    bestEffort(
      'docker',
      [
        'compose', '--project-name', PROJECT, '--file', DEPLOY_COMPOSE,
        '--file', WORKER_COMPOSE, 'down', '--volumes', '--remove-orphans', '--timeout', '10',
      ],
      { env: composeEnvironment },
    );
  }
  cleanupWorkerResources();
  if (!KEEP_IMAGES) {
    if (builtFixtureImage) bestEffort('docker', ['image', 'rm', FIXTURE_IMAGE]);
    if (builtRsctfImage) bestEffort('docker', ['image', 'rm', RSCTF_IMAGE]);
  }
  rmSync(TEMP_ROOT, { recursive: true, force: true });
}

function printFailureDiagnostics() {
  if (
    !composeEnvironment ||
    !canCleanupComposeProject(PROJECT, composeCleanupClaim)
  ) return;
  const status = bestEffort(
    'docker',
    [
      'compose', '--project-name', PROJECT, '--file', DEPLOY_COMPOSE,
      '--file', WORKER_COMPOSE, 'ps', '--all',
    ],
    { env: composeEnvironment, stdio: ['ignore', 'pipe', 'pipe'] },
  );
  const logs = bestEffort(
    'docker',
    [
      'compose', '--project-name', PROJECT, '--file', DEPLOY_COMPOSE,
      '--file', WORKER_COMPOSE, 'logs', '--no-color', '--tail', '160', 'rsctf',
    ],
    { env: composeEnvironment, stdio: ['ignore', 'pipe', 'pipe'] },
  );
  if (status.stdout) console.error(`isolated Compose status:\n${status.stdout.trim()}`);
  if (logs.stdout || logs.stderr) {
    console.error(`isolated rsctf logs:\n${`${logs.stdout || ''}${logs.stderr || ''}`.trim()}`);
  }
  if (workerId) {
    const ids = dockerLines([
      'ps', '--all', '--quiet', '--filter', `label=io.rsctf.worker.id=${workerId}`,
    ]);
    if (ids.length) {
      const states = bestEffort(
        'docker',
        [
          'inspect', '--format',
          '{{.Name}} status={{.State.Status}} exit={{.State.ExitCode}} error={{json .State.Error}}',
          ...ids,
        ],
        { stdio: ['ignore', 'pipe', 'pipe'] },
      );
      if (states.stdout) console.error(`isolated workload states:\n${states.stdout.trim()}`);
    }
  }
}

async function buildArtifacts() {
  if (BUILDS_AGENT) {
    console.log('building native worker agent (--release)…');
    await runStreaming('cargo', [
      'build', '--release', '--locked', '--manifest-path',
      resolve(REPOSITORY_ROOT, 'agents/worker-agent/Cargo.toml'),
    ]);
  }
  if (BUILDS_RSCTF) {
    console.log(`building current rsctf image ${RSCTF_IMAGE}…`);
    await runStreaming('docker', ['build', '--tag', RSCTF_IMAGE, REPOSITORY_ROOT], {
      env: DOCKER_ENVIRONMENT,
    });
    builtRsctfImage = true;
  }
  if (BUILDS_FIXTURE) {
    console.log(`building worker-local probe image ${FIXTURE_IMAGE}…`);
    await runStreaming('docker', ['build', '--tag', FIXTURE_IMAGE, FIXTURE_CONTEXT], {
      env: DOCKER_ENVIRONMENT,
    });
    builtFixtureImage = true;
  }
}

async function main() {
  for (const signal of ['SIGINT', 'SIGTERM']) {
    process.once(signal, () => {
      interruptedSignal = signal;
      if (agentProcess?.exitCode === null) agentProcess.kill('SIGTERM');
    });
  }

  const preBuildSource = assertRepositorySourceFingerprints(
    REPOSITORY_ROOT,
    EXPECTED_SOURCE_FINGERPRINTS,
    'pre-build',
  );
  preflightIsolatedProject();
  await buildArtifacts();
  assertRepositorySourceFingerprints(
    REPOSITORY_ROOT,
    EXPECTED_SOURCE_FINGERPRINTS,
    'post-build',
  );
  const expectedAgentSha256 = sha256File(AGENT_BINARY);
  const expectedRsctfImageId = dockerImageId(RSCTF_IMAGE);
  const fixtureImageId = dockerImageId(FIXTURE_IMAGE);
  requireMatchingSha256('rsctf image', expectedRsctfImageId, expectedRsctfImageId);
  requireMatchingSha256('fixture image', fixtureImageId, fixtureImageId);
  const httpPort = await reservePort(process.env.E2E_HTTP_PORT, 'E2E_HTTP_PORT');
  const workerPort = await reservePort(process.env.E2E_WORKER_PORT, 'E2E_WORKER_PORT');
  if (httpPort === workerPort) throw new Error('HTTP and worker listener ports must differ');
  const pki = createWorkerPki();
  const password = randomBytes(24).toString('hex');
  const jwtSecret = randomBytes(32).toString('hex');
  const target = `http://127.0.0.1:${httpPort}`;
  composeEnvironment = isolatedComposeEnvironment(process.env, {
    DOCKER_CONTEXT,
    COMPOSE_PROJECT_NAME: PROJECT,
    COMPOSE_DISABLE_ENV_FILE: '1',
    POSTGRES_IMAGE,
    POSTGRES_USER: 'postgres',
    POSTGRES_PASSWORD: password,
    POSTGRES_DB: 'rsctf',
    REDIS_IMAGE,
    REDIS_MAXMEMORY: '256mb',
    RSCTF_JWT_SECRET: jwtSecret,
    RSCTF_PUBLIC_URL: target,
    RSCTF_COOKIE_SECURE: 'false',
    RSCTF_HTTP_BIND_IP: '127.0.0.1',
    RSCTF_HTTP_PORT: String(httpPort),
    RSCTF_IMAGE,
    RSCTF_WORKER_BIND_IP: '127.0.0.1',
    RSCTF_WORKER_PORT: String(workerPort),
    RSCTF_WORKER_PUBLIC_ENDPOINT: `127.0.0.1:${workerPort}`,
    RSCTF_WORKER_SERVER_NAME: '127.0.0.1',
    RSCTF_WORKER_LOCAL_BACKEND: 'none',
    RSCTF_WORKER_LOCAL_TRAFFIC_CAPTURE_ENABLED: 'false',
    RSCTF_WORKER_DEFAULT_OS: 'linux',
    RSCTF_WORKER_DEFAULT_ARCH: 'amd64',
    RSCTF_WORKER_CA_CERT_HOST: pki.caCert,
    RSCTF_WORKER_CA_KEY_HOST: pki.caKey,
    RSCTF_WORKER_SERVER_CERT_HOST: pki.serverCert,
    RSCTF_WORKER_SERVER_KEY_HOST: pki.serverKey,
    RSCTF_ROLE: 'all',
    RSCTF_MIGRATE: '1',
    RSCTF_STORAGE_BACKEND: 'local',
    RSCTF_S3_BUCKET: '',
    RSCTF_S3_REGION: '',
    RSCTF_S3_ENDPOINT: '',
    RSCTF_S3_PREFIX: 'assets',
    RSCTF_S3_ACCESS_KEY: '',
    RSCTF_S3_SECRET_KEY: '',
    RSCTF_DISTRIBUTED_RATELIMIT: 'false',
    RSCTF_TRAFFIC_CAPTURE_ENABLED: 'false',
    RUST_LOG: process.env.E2E_RSCTF_LOG || 'info',
  });

  console.log(`starting isolated Compose project ${PROJECT} on ${target} / worker :${workerPort}…`);
  composeCleanupClaim = PROJECT;
  try {
    compose(['up', '--detach', '--wait', '--wait-timeout', '180']);
  } catch (error) {
    printFailureDiagnostics();
    throw error;
  }
  const pgContainer = `${PROJECT}-db-1`;
  const rsctfContainer = `${PROJECT}-rsctf-1`;
  const redisContainer = `${PROJECT}-redis-1`;
  const network = `${PROJECT}_default`;
  const rsctfImageId = requireMatchingSha256(
    'running rsctf image',
    expectedRsctfImageId,
    runningContainerImageId(rsctfContainer),
  );
  const postgresImageId = requireMatchingSha256(
    'running PostgreSQL image',
    dockerImageId(POSTGRES_IMAGE),
    runningContainerImageId(pgContainer),
  );
  const redisImageId = requireMatchingSha256(
    'running Redis image',
    dockerImageId(REDIS_IMAGE),
    runningContainerImageId(redisContainer),
  );

  // `lib.mjs` and the delegated gate invoke Docker directly. Pin them to the
  // same audited context as Compose instead of inheriting an ambient daemon.
  for (const key of ['DOCKER_HOST', 'DOCKER_CONFIG', 'DOCKER_TLS_VERIFY', 'DOCKER_CERT_PATH']) {
    delete process.env[key];
  }
  process.env.DOCKER_CONTEXT = DOCKER_CONTEXT;
  process.env.TARGET = target;
  process.env.PG_CONTAINER = pgContainer;
  process.env.PG_USER = 'postgres';
  process.env.PG_DATABASE = 'rsctf';
  process.env.RSCTF_CONTAINER = rsctfContainer;
  process.env.NET = network;
  process.env.RSCTF_JWT_SECRET = jwtSecret;
  const A = await import('./applib.mjs');
  const L = await import('./lib.mjs');

  await api(target, 'POST', '/api/account/register', {
    body: {
      userName: `worker-e2e-${process.pid}`,
      password: randomBytes(16).toString('base64url'),
      email: `worker-e2e-${process.pid}@load.test`,
    },
  });
  const adminToken = A.adminJwt();
  await A.preflight();

  const created = await api(target, 'POST', '/api/admin/workers', {
    token: adminToken,
    body: { name: `${PROJECT}-agent` },
  });
  workerId = String(created.worker.id);
  daemonSentinelPreexisting = command(
    'docker',
    ['volume', 'ls', '--quiet', '--filter', 'name=^rsctf-worker-owner$'],
  )
    .split(/\s+/)
    .filter(Boolean)
    .includes('rsctf-worker-owner');
  const enrollmentToken = String(created.enrollment.token);
  const enrollment = spawnSync(
    AGENT_BINARY,
    [
      'enroll', '--server-url', target, '--allow-insecure-enrollment',
      '--token-stdin', '--state-dir', AGENT_STATE,
    ],
    {
      cwd: REPOSITORY_ROOT,
      env: { ...process.env, RUST_LOG: process.env.E2E_AGENT_LOG || 'info' },
      input: `${enrollmentToken}\n`,
      encoding: 'utf8',
      stdio: ['pipe', 'inherit', 'inherit'],
    },
  );
  if (enrollment.status !== 0) {
    throw new Error(`worker enrollment failed with exit code ${enrollment.status}`);
  }
  agentProcess = startAgent();
  agentProcess.once('error', (error) => {
    console.error(`worker agent process error: ${error.message}`);
  });
  await waitFor(async () => {
    if (agentProcess.exitCode !== null) {
      throw new Error(`worker agent exited early (${agentProcess.exitCode})`);
    }
    const workers = await api(target, 'GET', '/api/admin/workers', { token: adminToken });
    return workers.find((worker) => worker.id === workerId && worker.online === true);
  }, 45_000, 'native worker to become online');
  requireMatchingSha256('worker agent path', expectedAgentSha256, sha256File(AGENT_BINARY));
  const agentSha256 = requireMatchingSha256(
    'running worker agent',
    expectedAgentSha256,
    sha256File(`/proc/${agentProcess.pid}/exe`),
  );
  const workerImage = `worker://${workerId}/${fixtureImageId}`;
  const workerLocalImage = {
    type: 'workerLocal',
    workerId,
    imageId: fixtureImageId,
  };
  const baseWorkloadSpec = {
    gameKind: 'jeopardy',
    platform: {
      operatingSystem: 'linux',
      architecture: 'amd64',
    },
    services: [
      {
        name: 'primary',
        image: workerLocalImage,
        resources: { cpuMillis: 100, memoryBytes: 64 * 1024 * 1024 },
        replicas: 2,
        stateless: true,
        environment: { RSCTF_SERVICE_ROLE: 'primary' },
        ports: [{ name: 'http', containerPort: 8080, protocol: 'tcp' }],
      },
      {
        name: 'sidecar',
        image: workerLocalImage,
        resources: { cpuMillis: 100, memoryBytes: 64 * 1024 * 1024 },
        replicas: 1,
        stateless: true,
        environment: { RSCTF_SERVICE_ROLE: 'sidecar' },
        ports: [{ name: 'http', containerPort: 8080, protocol: 'tcp' }],
      },
    ],
    primaryEndpoint: { service: 'primary', port: 'http' },
    flagTarget: { service: 'primary', path: '/tmp/rsctf-flag' },
  };
  const scaledWorkloadSpec = structuredClone(baseWorkloadSpec);
  scaledWorkloadSpec.services[0].replicas = 3;
  scaledWorkloadSpec.services[1].replicas = 2;
  const now = Number(L.sql(`SELECT (extract(epoch from now())*1000)::bigint`));
  const gameId = await A.createGame({
    title: `WORKER-E2E-${Date.now()}`,
    hidden: false,
    practiceMode: false,
    acceptWithoutReview: true,
    start: now - 60_000,
    end: now + 3_600_000,
    teamMemberCountLimit: 1,
    containerCountLimit: 1,
    allowUserSubmissions: false,
  });
  const challengeId = await A.createChallenge(gameId, {
    title: 'worker-proxy-probe',
    category: 'Web',
    type: 'StaticContainer',
  });
  await A.setChallenge(gameId, challengeId, {
    content: 'isolated worker-plane acceptance probe',
    originalScore: 1000,
    minScoreRate: 0.25,
    difficulty: 1,
    containerImage: workerImage,
    memoryLimit: 64,
    cpuCount: 1,
    exposePort: 8080,
    enableTrafficCapture: false,
    workloadSpec: baseWorkloadSpec,
  });
  await A.addFlags(gameId, challengeId, [`flag{worker_e2e_${challengeId}}`]);
  L.sql(
    `UPDATE "GameChallenges" SET build_status=1, build_image_digest='${workerImage}', ` +
      `review_status=0 WHERE game_id=${gameId} AND id=${challengeId}`,
  );
  await A.setChallenge(gameId, challengeId, { isEnabled: true });
  const storedShape = JSON.parse(
    L.sql(
      `SELECT json_build_object(` +
        `'serviceCount',jsonb_array_length(workload_spec->'services'),` +
        `'replicaCount',(SELECT sum((replica_service->>'replicas')::int) ` +
        `FROM jsonb_array_elements(workload_spec->'services') AS services(replica_service))` +
        `)::text FROM "GameChallenges" WHERE game_id=${gameId} AND id=${challengeId}`,
    ),
  );
  if (
    storedShape.serviceCount !== WORKLOAD_SERVICE_COUNT ||
    storedShape.replicaCount !== WORKLOAD_REPLICA_COUNT
  ) {
    throw new Error(
      `stored aggregate workload shape is ${storedShape.serviceCount} service(s)/` +
        `${storedShape.replicaCount} replica(s), expected ` +
        `${WORKLOAD_SERVICE_COUNT}/${WORKLOAD_REPLICA_COUNT}`,
    );
  }
  A.seedCohort(gameId, FLEET);

  console.log(
    `isolated worker gate → project=${PROJECT} game=${gameId} challenge=${challengeId} ` +
      `worker=${workerId} fleet=${FLEET}`,
  );
  console.log(`  rsctf image: ${RSCTF_IMAGE} (${rsctfImageId})`);
  console.log(`  PostgreSQL image: ${POSTGRES_IMAGE} (${postgresImageId})`);
  console.log(`  Redis image: ${REDIS_IMAGE} (${redisImageId})`);
  console.log(`  agent: ${AGENT_BINARY} (${agentSha256})`);
  console.log(`  fixture: ${workerImage}`);
  console.log(
    `  source fingerprints: tracked=${preBuildSource.tracked} ` +
      `untracked=${preBuildSource.untracked}`,
  );

  const childEnvironment = {
    ...process.env,
    TARGET: target,
    GAME: String(gameId),
    CID: String(challengeId),
    FLEET: String(FLEET),
    MIN_WORKERS: '1',
    WORKER_IDS: workerId,
    WORKER_OS: 'linux',
    EXPECTED_SERVICE_COUNT: String(WORKLOAD_SERVICE_COUNT),
    EXPECTED_REPLICA_COUNT: String(WORKLOAD_REPLICA_COUNT),
    ROLLOUT_UP_SPEC_JSON: JSON.stringify(scaledWorkloadSpec),
    ROLLOUT_DOWN_SPEC_JSON: JSON.stringify(baseWorkloadSpec),
    LOCAL_DOCKER_REPLICA_AUDIT: '1',
    RATE: process.env.RATE || '10',
    VUS: process.env.VUS || '10',
    DURATION: process.env.DURATION || '10s',
    CYCLES: process.env.CYCLES || '1',
    EXPECTED_RESPONSE_MARKER:
      process.env.EXPECTED_RESPONSE_MARKER || 'Shared rsctf demo service',
    PG_CONTAINER: pgContainer,
    PG_USER: 'postgres',
    PG_DATABASE: 'rsctf',
    RSCTF_CONTAINER: rsctfContainer,
    NET: network,
    RSCTF_JWT_SECRET: jwtSecret,
  };
  const preMeasurementSource = assertRepositorySourceFingerprints(
    REPOSITORY_ROOT,
    EXPECTED_SOURCE_FINGERPRINTS,
    'pre-measurement',
  );
  await runWorkerGate(childEnvironment, {
    rsctfImage: RSCTF_IMAGE,
    rsctfImageId,
    postgresImage: POSTGRES_IMAGE,
    postgresImageId,
    redisImage: REDIS_IMAGE,
    redisImageId,
    agentBinary: AGENT_BINARY,
    agentSha256,
    fixture: workerImage,
    sourceFingerprints: preMeasurementSource,
  });
}

main()
  .then(() => {
    console.log('\nRESULT — isolated current-tree native-worker gate passed');
  })
  .catch((error) => {
    console.error(`error: ${error.message}`);
    printFailureDiagnostics();
    if (agentProcess?.logTail?.length) {
      console.error(`worker log tail:\n${agentProcess.logTail.join('')}`);
    }
    process.exitCode = 1;
  })
  .finally(cleanup);
