// Safe, fixed-rate polling for every GET endpoint in the admin lifecycle
// catalog. The destructive lifecycle is performed by the Node orchestrator;
// this scenario only proves that public routing, direct web replicas, and the
// control surface remain responsive and return their documented shapes.
//
// ADMIN_TOKEN=<jwt> \
// TARGET=http://127.0.0.1:8080 \
// WEB_TARGETS='["http://127.0.0.1:18081","http://127.0.0.1:18082"]' \
// CONTROL_TARGET=http://127.0.0.1:18083 \
// ADMIN_CONTEXT='{"gameId":1,"adGameId":1,"userId":"..."}' \
// k6 run k6/admin-lifecycle.js
import http from 'k6/http';
import { check } from 'k6';
import exec from 'k6/execution';
import { Counter, Rate, Trend } from 'k6/metrics';
import ws from 'k6/ws';

import {
  ADMIN_READ_OPERATIONS,
  assertAdminOriginAcknowledgements,
  buildAdminReadOriginMatrix,
  resolveOperationPath,
  validateAdminResponse,
} from '../admin-lifecycle.js';

const TARGET = origin(__ENV.TARGET || 'http://127.0.0.1:8080', 'TARGET');
const WEB_TARGETS = targetList(__ENV.WEB_TARGETS || __ENV.ADMIN_WEB_TARGETS, 'WEB_TARGETS');
const CONTROL_TARGET = origin(__ENV.CONTROL_TARGET || '', 'CONTROL_TARGET');
const ADMIN_TOKEN = __ENV.ADMIN_TOKEN || '';
const ADMIN_CONTEXT = parseContext(__ENV.ADMIN_CONTEXT || '{}');
const RATE = positiveInteger(__ENV.RATE || 1, 'RATE');
const HEALTH_RATE = positiveInteger(__ENV.HEALTH_RATE || 1, 'HEALTH_RATE');
const VUS = positiveInteger(__ENV.VUS || Math.max(4, RATE * 2), 'VUS');
const MAX_VUS = positiveInteger(__ENV.MAX_VUS || Math.max(VUS, RATE * 4), 'MAX_VUS');
const DURATION = __ENV.DURATION || '60s';
const MAX_ADMIN_P95_MS = positiveNumber(__ENV.MAX_ADMIN_P95_MS || 1000, 'MAX_ADMIN_P95_MS');
const MAX_HEALTH_P95_MS = positiveNumber(__ENV.MAX_HEALTH_P95_MS || 500, 'MAX_HEALTH_P95_MS');

if (!ADMIN_TOKEN) throw new Error('ADMIN_TOKEN is required');
if (__ENV.ADMIN_LIFECYCLE_DISPOSABLE !== '1') {
  throw new Error('set ADMIN_LIFECYCLE_DISPOSABLE=1 before sending an admin token');
}
if (!isLoopbackOrigin(TARGET) && __ENV.ALLOW_REMOTE_ADMIN_LIFECYCLE !== TARGET) {
  throw new Error(`remote admin lifecycle requires ALLOW_REMOTE_ADMIN_LIFECYCLE=${TARGET}`);
}
if (WEB_TARGETS.length < 2 || new Set(WEB_TARGETS).size !== WEB_TARGETS.length) {
  throw new Error('WEB_TARGETS must contain at least two distinct direct web-replica origins');
}
if (WEB_TARGETS.includes(TARGET)) {
  throw new Error('TARGET must be distinct from every direct WEB_TARGETS origin');
}
if (CONTROL_TARGET === TARGET || WEB_TARGETS.includes(CONTROL_TARGET)) {
  throw new Error('CONTROL_TARGET must be distinct from TARGET and every direct web target');
}
assertAdminOriginAcknowledgements(__ENV, {
  target: TARGET,
  webTargets: WEB_TARGETS,
  controlTarget: CONTROL_TARGET,
});
if (MAX_VUS < VUS) throw new Error('MAX_VUS must be greater than or equal to VUS');
if (RATE > 2) {
  throw new Error(
    'RATE must stay at or below 2 req/s: the 74-request setup matrix shares the 150/min admin quota',
  );
}

const webOrigins = unique([TARGET, ...WEB_TARGETS]);
const controlOrigins = unique([TARGET, CONTROL_TARGET]);
const healthOrigins = unique([...webOrigins, CONTROL_TARGET]);
const eligibleReads = ADMIN_READ_OPERATIONS.map((operation) =>
  Object.freeze({ operation, path: resolveOperationPath(operation, ADMIN_CONTEXT) }));
const readOriginMatrix = buildAdminReadOriginMatrix(
  ADMIN_CONTEXT,
  webOrigins,
  controlOrigins,
);

http.setResponseCallback(http.expectedStatuses(200));

const server5xx = new Rate('server_5xx');
const unexpectedStatus = new Rate('unexpected_status');
const invalidAdminResponse = new Rate('invalid_admin_response');
const admin429 = new Counter('admin_429');
const healthFailure = new Rate('health_failure');
const signalrFailure = new Rate('signalr_failure');
const adminMatrixFailure = new Rate('admin_matrix_failure');
const adminMatrixSamples = new Counter('admin_matrix_samples');
const adminReadMs = new Trend('admin_read_ms', true);
const adminMatrixMs = new Trend('admin_matrix_ms', true);
const healthMs = new Trend('health_ms', true);
const signalrHandshakeMs = new Trend('signalr_handshake_ms', true);
const endpointTrends = Object.fromEntries(
  ADMIN_READ_OPERATIONS.map((operation) => [operation.id, new Trend(`${operation.id}_ms`, true)]),
);

export const options = {
  setupTimeout: '2m',
  scenarios: {
    admin_reads: {
      executor: 'constant-arrival-rate',
      exec: 'adminReads',
      rate: RATE,
      timeUnit: '1s',
      duration: DURATION,
      preAllocatedVUs: VUS,
      maxVUs: MAX_VUS,
    },
    platform_health: {
      executor: 'constant-arrival-rate',
      exec: 'platformHealth',
      rate: HEALTH_RATE,
      timeUnit: '1s',
      duration: DURATION,
      preAllocatedVUs: Math.max(2, HEALTH_RATE),
      maxVUs: Math.max(4, HEALTH_RATE * 2),
    },
    admin_signalr: {
      executor: 'shared-iterations',
      exec: 'adminSignalR',
      vus: 1,
      iterations: webOrigins.length,
      maxDuration: '20s',
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    checks: ['rate==1'],
    http_req_failed: ['rate==0'],
    server_5xx: ['rate==0'],
    unexpected_status: ['rate==0'],
    invalid_admin_response: ['rate==0'],
    admin_matrix_failure: ['rate==0'],
    admin_matrix_samples: [`count==${readOriginMatrix.length}`],
    admin_429: ['count==0'],
    health_failure: ['rate==0'],
    signalr_failure: ['rate==0'],
    dropped_iterations: ['count==0'],
    admin_read_ms: [`p(95)<${MAX_ADMIN_P95_MS}`],
    admin_matrix_ms: [`p(95)<${MAX_ADMIN_P95_MS}`],
    health_ms: [`p(95)<${MAX_HEALTH_P95_MS}`],
    signalr_handshake_ms: [`p(95)<${MAX_ADMIN_P95_MS}`],
  },
};

function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer`);
  }
  return parsed;
}

function positiveNumber(value, label) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed <= 0) throw new Error(`${label} must be positive`);
  return parsed;
}

function origin(value, label) {
  const text = String(value || '').trim().replace(/\/+$/, '');
  if (!/^https?:\/\/[^/?#]+$/i.test(text) || /[\s@]/.test(text.replace(/^https?:\/\//i, ''))) {
    throw new Error(`${label} must be a credential-free HTTP(S) origin without a path`);
  }
  return text;
}

function isLoopbackOrigin(value) {
  return /^https?:\/\/(?:127\.0\.0\.1|localhost|\[::1\])(?::\d+)?$/i.test(value);
}

function targetList(raw, label) {
  const text = String(raw || '').trim();
  if (!text) return [];
  let values;
  if (text.startsWith('[')) {
    values = JSON.parse(text);
    if (!Array.isArray(values)) throw new Error(`${label} JSON must be an array`);
  } else {
    values = text.split(',');
  }
  return values.map((value, index) => origin(value, `${label}[${index}]`));
}

function parseContext(raw) {
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new Error(`ADMIN_CONTEXT must be valid JSON: ${error.message}`);
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('ADMIN_CONTEXT must be a JSON object');
  }
  return parsed;
}

function unique(values) {
  return [...new Set(values)];
}

function requestHeaders() {
  return {
    Authorization: `Bearer ${ADMIN_TOKEN}`,
    'X-Real-IP': `31.${__VU % 250}.${__ITER % 250}.${(__VU * 17 + __ITER) % 250 + 1}`,
  };
}

function matrixRequestHeaders(index) {
  return {
    Authorization: `Bearer ${ADMIN_TOKEN}`,
    'X-Real-IP': `32.${Math.floor(index / 250) % 250}.${index % 250}.1`,
  };
}

function responseBody(response) {
  try {
    return response.json();
  } catch (_) {
    return undefined;
  }
}

function performAdminRead({ operation, path, selectedOrigin }, headers, phase) {
  const response = http.get(`${selectedOrigin}${path}`, {
    headers,
    tags: {
      operation: operation.id,
      phase,
      surface: operation.surface,
      target: selectedOrigin,
    },
  });

  const expected = operation.expectedStatuses.includes(response.status);
  const valid = validateAdminResponse(operation.id, {
    status: response.status,
    body: responseBody(response),
    headers: response.headers,
  });
  server5xx.add(response.status >= 500);
  unexpectedStatus.add(!expected);
  invalidAdminResponse.add(!valid);
  admin429.add(response.status === 429 ? 1 : 0);
  if (phase === 'matrix') {
    adminMatrixFailure.add(!expected || !valid, { operation: operation.id, target: selectedOrigin });
    adminMatrixSamples.add(1, { operation: operation.id, target: selectedOrigin });
    adminMatrixMs.add(response.timings.duration, { operation: operation.id, target: selectedOrigin });
  } else {
    adminReadMs.add(response.timings.duration, { operation: operation.id });
    endpointTrends[operation.id].add(response.timings.duration, { target: selectedOrigin });
  }

  check(response, {
    'admin read returned its expected status': () => expected,
    'admin read matched its response contract': () => valid,
  });
}

export function setup() {
  for (const [index, selected] of readOriginMatrix.entries()) {
    performAdminRead(selected, matrixRequestHeaders(index), 'matrix');
  }
}

export function adminReads() {
  const sequence = exec.scenario.iterationInTest;
  const selected = eligibleReads[sequence % eligibleReads.length];
  const origins = selected.operation.surface === 'control' ? controlOrigins : webOrigins;
  const selectedOrigin = origins[Math.floor(sequence / eligibleReads.length) % origins.length];
  performAdminRead(
    { ...selected, selectedOrigin },
    requestHeaders(),
    'fixed-rate',
  );
}

export function platformHealth() {
  const requests = [];
  for (const selectedOrigin of healthOrigins) {
    const params = { tags: { kind: 'health', target: selectedOrigin } };
    requests.push(['GET', `${selectedOrigin}/livez`, null, params]);
    requests.push(['GET', `${selectedOrigin}/healthz`, null, params]);
  }
  const responses = http.batch(requests);
  const healthy = responses.every((response) => response.status === 200);
  const duration = responses.reduce((maximum, response) => Math.max(maximum, response.timings.duration), 0);
  healthMs.add(duration);
  healthFailure.add(!healthy);
  for (const response of responses) server5xx.add(response.status >= 500);

  check(responses, {
    'all public, direct-replica, and control health endpoints remain valid': () => healthy,
  });
}

export function adminSignalR() {
  const selectedOrigin = webOrigins[exec.scenario.iterationInTest % webOrigins.length];
  const socketOrigin = selectedOrigin.replace(/^http/i, 'ws');
  const started = Date.now();
  let handshaken = false;
  let socketError = false;
  const response = ws.connect(
    `${socketOrigin}/hub/admin?access_token=${encodeURIComponent(ADMIN_TOKEN)}`,
    { tags: { kind: 'admin-signalr', target: selectedOrigin } },
    (socket) => {
      socket.on('open', () => {
        socket.send('{"protocol":"json","version":1}\u001e');
      });
      socket.on('message', (message) => {
        if (String(message).split('\u001e').some((frame) => frame.trim() === '{}')) {
          handshaken = true;
          signalrHandshakeMs.add(Date.now() - started, { target: selectedOrigin });
          socket.close();
        }
      });
      socket.on('error', () => {
        socketError = true;
      });
      socket.setTimeout(() => {
        socketError = true;
        socket.close();
      }, 5000);
    },
  );
  const valid = response?.status === 101 && handshaken && !socketError;
  signalrFailure.add(!valid, { target: selectedOrigin });
  check(response, {
    'admin SignalR upgraded and completed its JSON handshake': () => valid,
  });
}
