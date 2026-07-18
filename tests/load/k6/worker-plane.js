// Trusted-worker data-path load. The Node orchestrator creates real Jeopardy
// workloads first, then passes their proxy entries and owning player JWTs here.
// Each iteration opens a fresh authenticated WebSocket, sends one TCP payload,
// and closes after the first response (or a bounded timeout).
import http from 'k6/http';
import ws from 'k6/ws';
import { check } from 'k6';
import { Counter, Rate, Trend } from 'k6/metrics';

import { appendProxyResponse, createProxyResponseTracker } from '../worker-plane.js';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const ENDPOINTS = JSON.parse(__ENV.WORKER_ENDPOINTS || '[]');
const EXPECTED_WORKERS = JSON.parse(__ENV.EXPECTED_WORKERS || '[]');
const ADMIN_TOKEN = __ENV.ADMIN_TOKEN;
const DURATION = __ENV.DURATION || '30s';
const RATE = Number(__ENV.RATE || 20);
const VUS = Number(__ENV.VUS || Math.max(10, RATE));
const MAX_VUS = Number(__ENV.MAX_VUS || Math.max(VUS, RATE * 2));
const HEALTH_POLL_RATE = Number(__ENV.HEALTH_POLL_RATE || __ENV.WORKER_POLL_RATE || 1);
const WORKER_INVENTORY_INTERVAL_SECONDS = Number(
  __ENV.WORKER_INVENTORY_INTERVAL_SECONDS || 10,
);
const STREAM_TIMEOUT_MS = Number(__ENV.STREAM_TIMEOUT_MS || 5000);
const EXPECT_PROXY_RESPONSE = __ENV.EXPECT_PROXY_RESPONSE !== '0';
const EXPECTED_RESPONSE_MARKER = __ENV.EXPECTED_RESPONSE_MARKER || '';
const PROBE_PAYLOAD =
  __ENV.PROBE_PAYLOAD || 'GET / HTTP/1.1\r\nHost: worker-load\r\nConnection: close\r\n\r\n';
const DEBUG_PROXY_ERRORS = __ENV.DEBUG_PROXY_ERRORS === '1';

if (!Array.isArray(ENDPOINTS) || ENDPOINTS.length === 0) {
  throw new Error('WORKER_ENDPOINTS must contain at least one prepared proxy endpoint');
}
if (!ADMIN_TOKEN) throw new Error('ADMIN_TOKEN is required for worker health polling');
if (![RATE, VUS, MAX_VUS, HEALTH_POLL_RATE, STREAM_TIMEOUT_MS].every(Number.isFinite)) {
  throw new Error('worker-plane numeric configuration is invalid');
}
if (
  !Number.isSafeInteger(WORKER_INVENTORY_INTERVAL_SECONDS) ||
  WORKER_INVENTORY_INTERVAL_SECONDS < 10 ||
  WORKER_INVENTORY_INTERVAL_SECONDS > 300
) {
  throw new Error('WORKER_INVENTORY_INTERVAL_SECONDS must be an integer from 10 to 300');
}

const server5xx = new Rate('server_5xx');
const healthFailure = new Rate('health_failure');
const workerListInvalid = new Rate('worker_list_invalid');
const workerList429 = new Counter('worker_list_429');
const proxyUpgrade429 = new Counter('proxy_upgrade_429');
const proxyHandshakeFailure = new Rate('proxy_handshake_failure');
const proxyStreamFailure = new Rate('proxy_stream_failure');
const proxyResponseMissing = new Rate('proxy_response_missing');
const proxyResponseInvalid = new Rate('proxy_response_invalid');
const proxyStreamMs = new Trend('worker_proxy_stream_ms', true);
const workerListMs = new Trend('worker_list_ms', true);
const healthMs = new Trend('health_ms', true);
const proxyResponses = new Counter('worker_proxy_responses');
let loggedProxyError = false;

const thresholds = {
  server_5xx: ['rate<0.000001'],
  health_failure: ['rate<0.000001'],
  worker_list_invalid: ['rate<0.000001'],
  worker_list_429: ['count==0'],
  proxy_upgrade_429: ['count==0'],
  proxy_handshake_failure: [`rate<${__ENV.MAX_PROXY_FAILURE_RATE || '0.001'}`],
  proxy_stream_failure: [
    `rate<${__ENV.MAX_PROXY_STREAM_FAILURE_RATE || __ENV.MAX_PROXY_FAILURE_RATE || '0.001'}`,
  ],
};
if (EXPECT_PROXY_RESPONSE) {
  thresholds.proxy_response_missing = [
    `rate<${__ENV.MAX_PROXY_RESPONSE_MISSING_RATE || '0.01'}`,
  ];
  thresholds.proxy_response_invalid = [
    `rate<${__ENV.MAX_PROXY_RESPONSE_INVALID_RATE || '0.01'}`,
  ];
}

export const options = {
  scenarios: {
    proxy_streams: {
      executor: 'constant-arrival-rate',
      exec: 'proxyStreams',
      rate: RATE,
      timeUnit: '1s',
      duration: DURATION,
      preAllocatedVUs: VUS,
      maxVUs: MAX_VUS,
    },
    worker_health: {
      executor: 'constant-arrival-rate',
      exec: 'workerHealth',
      rate: HEALTH_POLL_RATE,
      timeUnit: '1s',
      duration: DURATION,
      preAllocatedVUs: Math.max(2, HEALTH_POLL_RATE),
      maxVUs: Math.max(4, HEALTH_POLL_RATE * 2),
    },
    worker_inventory: {
      executor: 'constant-arrival-rate',
      exec: 'workerInventory',
      rate: 1,
      timeUnit: `${WORKER_INVENTORY_INTERVAL_SECONDS}s`,
      duration: DURATION,
      preAllocatedVUs: 1,
      maxVUs: 2,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds,
};

function headers(token, vu, iteration) {
  return {
    Authorization: `Bearer ${token}`,
    'X-Real-IP': `31.${vu % 250}.${iteration % 250}.${(vu * 17 + iteration) % 250 + 1}`,
  };
}

export function proxyStreams() {
  const endpoint = ENDPOINTS[(__VU * 17 + __ITER) % ENDPOINTS.length];
  let received = false;
  let streamErrored = false;
  const responseTracker = createProxyResponseTracker(EXPECTED_RESPONSE_MARKER);
  const started = Date.now();
  const response = ws.connect(
    endpoint.url,
    { headers: headers(endpoint.token, __VU, __ITER) },
    (socket) => {
      const acceptResponse = (data) => {
        appendProxyResponse(responseTracker, data);
        if (responseTracker.valid && !received) {
          proxyResponses.add(1);
          received = true;
          socket.close();
        }
      };
      socket.on('open', () => socket.send(PROBE_PAYLOAD));
      // The RSCTF proxy deliberately forwards TCP bytes as WebSocket binary
      // frames. Keep the text listener as a compatibility guard, but count the
      // binary event used by the real data path.
      socket.on('message', acceptResponse);
      socket.on('binaryMessage', acceptResponse);
      socket.on('error', (event) => {
        // k6 emits this sentinel after our own Socket.close(); its official
        // example treats it as normal close-handshake bookkeeping.
        const reason = typeof event?.error === 'function' ? event.error() : String(event || '');
        if (reason !== 'websocket: close sent') {
          streamErrored = true;
          if (DEBUG_PROXY_ERRORS && !loggedProxyError) {
            loggedProxyError = true;
            console.warn(`worker proxy WebSocket error: ${reason}`);
          }
        }
      });
      socket.setTimeout(() => socket.close(), STREAM_TIMEOUT_MS);
    },
  );
  const handshakeFailed = response?.status !== 101;
  server5xx.add(Number(response?.status || 0) >= 500);
  proxyHandshakeFailure.add(handshakeFailed);
  proxyUpgrade429.add(response?.status === 429 ? 1 : 0);
  if (!handshakeFailed) {
    proxyStreamFailure.add(streamErrored);
    proxyResponseMissing.add(EXPECT_PROXY_RESPONSE && !responseTracker.sawPayload);
    proxyResponseInvalid.add(
      EXPECT_PROXY_RESPONSE && responseTracker.sawPayload && !responseTracker.valid,
    );
  }
  proxyStreamMs.add(Date.now() - started);
  check(response, {
    'worker proxy upgraded without server error': (result) =>
      result?.status === 101 && result.status < 500,
  });
}

export function workerHealth() {
  const requestHeaders = headers(ADMIN_TOKEN, __VU, __ITER);
  const responses = http.batch([
    ['GET', `${TARGET}/livez`, null, { headers: requestHeaders }],
    ['GET', `${TARGET}/healthz`, null, { headers: requestHeaders }],
  ]);
  const [live, ready] = responses;
  healthMs.add(Math.max(live.timings.duration, ready.timings.duration));
  for (const response of responses) server5xx.add(response.status >= 500);
  const healthy = live.status === 200 && ready.status === 200;
  healthFailure.add(!healthy);

  check(responses, {
    'platform health endpoints remain valid': () => healthy,
  });
}

export function workerInventory() {
  const workers = http.get(`${TARGET}/api/admin/workers`, {
    headers: headers(ADMIN_TOKEN, __VU, __ITER),
  });
  workerListMs.add(workers.timings.duration);
  server5xx.add(workers.status >= 500);
  workerList429.add(workers.status === 429 ? 1 : 0);

  let validWorkers = false;
  try {
    const rows = workers.json();
    validWorkers =
      workers.status === 200 &&
      Array.isArray(rows) &&
      EXPECTED_WORKERS.every((id) => {
        const worker = rows.find((candidate) => candidate.id === id);
        return (
          worker?.online === true &&
          worker.administrativeState === 'Enabled' &&
          Number(worker.leaseExpiresAt || 0) > Date.now()
        );
      });
  } catch (_) {
    validWorkers = false;
  }
  workerListInvalid.add(!validWorkers);
  check(workers, {
    'worker inventory is not rate limited': (response) => response.status !== 429,
    'worker inventory remains valid': () => validWorkers,
  });
}
