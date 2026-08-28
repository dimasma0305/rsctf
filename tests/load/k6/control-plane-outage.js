import http from 'k6/http';
import ws from 'k6/ws';
import { Rate, Trend } from 'k6/metrics';

import { validWorkerInventory } from '../control-plane-outage.js';
import { appendProxyResponse, createProxyResponseTracker } from '../worker-plane.js';

const TARGET = __ENV.TARGET;
const WORKER_ID = __ENV.WORKER_ID;
const ADMIN_TOKEN = __ENV.ADMIN_TOKEN;
const MODE = __ENV.MODE;
const RATE = Number(__ENV.RATE || 5);
const VUS = Number(__ENV.VUS || 10);
const PROXY_ENDPOINTS = __ENV.PROXY_ENDPOINTS_FILE ? JSON.parse(open(__ENV.PROXY_ENDPOINTS_FILE)) : [];
if (!TARGET || !WORKER_ID || !ADMIN_TOKEN || !['worker-offline', 'image-unavailable'].includes(MODE)) throw new Error('control-plane outage fixture is incomplete');

const invalid = new Rate('control_plane_invalid');
const server5xx = new Rate('server_5xx');
const latency = new Trend('control_plane_ms', true);
const proxyInvalid = new Rate('control_plane_proxy_invalid');

const scenarios = {
  controlPlane: { executor: 'constant-arrival-rate', rate: RATE, timeUnit: '1s', duration: __ENV.DURATION || '20s', preAllocatedVUs: VUS, maxVUs: VUS * 2 },
  health: { executor: 'constant-arrival-rate', exec: 'health', rate: 2, timeUnit: '1s', duration: __ENV.DURATION || '20s', preAllocatedVUs: 4, maxVUs: 8 },
};
const thresholds = { control_plane_invalid: ['rate==0'], server_5xx: ['rate==0'], dropped_iterations: ['count==0'], control_plane_ms: ['p(95)<1000'] };
if (PROXY_ENDPOINTS.length) {
  scenarios.proxy = { executor: 'constant-arrival-rate', exec: 'proxy', rate: Number(__ENV.PROXY_RATE || 2), timeUnit: '1s', duration: __ENV.DURATION || '20s', preAllocatedVUs: 4, maxVUs: 16 };
  thresholds.control_plane_proxy_invalid = ['rate==0'];
}

export const options = {
  scenarios,
  thresholds,
};

export default function () {
  const response = http.get(`${TARGET}/api/admin/workers`, {
    headers: { Authorization: `Bearer ${ADMIN_TOKEN}` }, responseType: 'text', tags: { outage: MODE },
  });
  latency.add(response.timings.duration);
  server5xx.add(response.status >= 500);
  let body;
  try { body = response.json(); } catch (_) { body = null; }
  const expectedOnline = MODE === 'image-unavailable';
  invalid.add(response.status !== 200 || !validWorkerInventory(body, WORKER_ID, expectedOnline));
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, { responseType: 'text' });
  server5xx.add(response.status >= 500);
  invalid.add(response.status !== 200 || response.body !== 'ok');
}

export function proxy() {
  const endpoint = PROXY_ENDPOINTS[(__VU + __ITER) % PROXY_ENDPOINTS.length];
  let received = false;
  let valid = false;
  const tracker = createProxyResponseTracker(endpoint.marker || '');
  const response = ws.connect(endpoint.url, {
    headers: { Authorization: `Bearer ${endpoint.token}` },
  }, (socket) => {
    const accept = (data) => {
      appendProxyResponse(tracker, data);
      received = tracker.sawPayload;
      valid = tracker.valid;
      socket.close();
    };
    socket.on('open', () => socket.send(endpoint.payload || '\n'));
    socket.on('message', accept);
    socket.on('binaryMessage', accept);
    socket.setTimeout(() => socket.close(), 3000);
  });
  server5xx.add(response?.status >= 500);
  proxyInvalid.add(response?.status !== 101 || !received || !valid);
}
