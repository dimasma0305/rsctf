import http from 'k6/http';
import ws from 'k6/ws';
import { check } from 'k6';
import { Counter, Rate, Trend } from 'k6/metrics';

import { validEndpointRows, validTrafficClose } from '../proxy-traffic-admission.js';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const ENDPOINTS = JSON.parse(__ENV.PROXY_TRAFFIC_ENDPOINTS || '[]');
const RATE = Number(__ENV.RATE || 2);
const DURATION = __ENV.DURATION || '30s';
const VUS = Number(__ENV.VUS || 20);
const MAX_VUS = Number(__ENV.MAX_VUS || 80);
const FRAME_BYTES = Number(__ENV.FRAME_BYTES || 65_536);
const FRAME_INTERVAL_MS = Number(__ENV.FRAME_INTERVAL_MS || 10);
const STREAM_MS = Number(__ENV.STREAM_MS || 10_000);

if (!validEndpointRows(ENDPOINTS)) throw new Error('PROXY_TRAFFIC_ENDPOINTS must contain bounded authenticated WSS endpoints');
if (![RATE, VUS, MAX_VUS, FRAME_BYTES, FRAME_INTERVAL_MS, STREAM_MS].every(Number.isFinite)) {
  throw new Error('proxy traffic numeric configuration is invalid');
}
if (FRAME_BYTES < 1 || FRAME_BYTES > 65_536 || FRAME_INTERVAL_MS < 1 || STREAM_MS < 100) {
  throw new Error('proxy traffic frame/interval/stream bounds are invalid');
}

const server5xx = new Rate('server_5xx');
const healthFailure = new Rate('health_failure');
const handshakeFailure = new Rate('proxy_handshake_failure');
const unexpectedClose = new Rate('proxy_unexpected_close');
const budgetCloses = new Counter('proxy_budget_closes');
const framesSent = new Counter('proxy_frames_sent');
const bytesSent = new Counter('proxy_bytes_sent');
const healthMs = new Trend('health_ms', true);

export const options = {
  scenarios: {
    streams: {
      executor: 'constant-arrival-rate', exec: 'streams', rate: RATE, timeUnit: '1s',
      duration: DURATION, preAllocatedVUs: VUS, maxVUs: MAX_VUS,
    },
    health: {
      executor: 'constant-arrival-rate', exec: 'health', rate: 1, timeUnit: '1s',
      duration: DURATION, preAllocatedVUs: 2, maxVUs: 4,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    server_5xx: ['rate==0'], health_failure: ['rate==0'],
    proxy_handshake_failure: ['rate<0.001'], proxy_unexpected_close: ['rate<0.001'],
    dropped_iterations: ['count==0'],
  },
};

export function streams() {
  const endpoint = ENDPOINTS[(__VU * 17 + __ITER) % ENDPOINTS.length];
  let expectedClose = false;
  const payload = 'x'.repeat(FRAME_BYTES);
  const response = ws.connect(endpoint.url, {
    headers: {
      Authorization: `Bearer ${endpoint.token}`,
      'X-Real-IP': `31.${__VU % 250}.${__ITER % 250}.${(__VU * 17 + __ITER) % 249 + 1}`,
    },
  }, (socket) => {
    socket.on('open', () => {
      socket.setInterval(() => {
        socket.send(payload);
        framesSent.add(1);
        bytesSent.add(FRAME_BYTES);
      }, FRAME_INTERVAL_MS);
    });
    socket.on('close', (code, reason) => {
      const budgetClose = validTrafficClose(code, reason);
      expectedClose = budgetClose || code === 1000;
      if (budgetClose) budgetCloses.add(1);
    });
    socket.setTimeout(() => socket.close(), STREAM_MS);
  });
  const failed = response?.status !== 101;
  handshakeFailure.add(failed);
  server5xx.add(Number(response?.status || 0) >= 500);
  unexpectedClose.add(!failed && !expectedClose);
  check(response, { 'proxy traffic upgraded without 5xx': (result) => result?.status === 101 });
}

export function health() {
  const started = Date.now();
  const response = http.get(`${TARGET}/healthz`);
  healthMs.add(Date.now() - started);
  server5xx.add(response.status >= 500);
  healthFailure.add(response.status !== 200 || response.body !== 'ok');
}
