import http from 'k6/http';
import ws from 'k6/ws';
import exec from 'k6/execution';
import { Rate, Trend } from 'k6/metrics';

import { signalrHandshake, unsupportedSignalrInvocation, validAbuseClose } from '../read-only-websocket-flood.js';

const TARGET = __ENV.TARGET;
const WS_TARGET = TARGET.replace(/^http/, 'ws');
const GAME = __ENV.GAME;
const RATE = Number(__ENV.RATE || 20);
const VUS = Number(__ENV.VUS || 40);
const MAX_VUS = Number(__ENV.MAX_VUS || 160);
const FRAME_BYTES = Number(__ENV.FRAME_BYTES || 65_536);
if (!TARGET || !/^\d+$/.test(GAME) || !Number.isSafeInteger(FRAME_BYTES) || FRAME_BYTES < 1024 || FRAME_BYTES > 65_536) {
  throw new Error('TARGET, GAME, and FRAME_BYTES between 1024 and 65536 are required');
}

const invalid = new Rate('readonly_ws_invalid');
const server5xx = new Rate('server_5xx');
const rateLimited = new Rate('rate_limited');
const closeMs = new Trend('readonly_ws_close_ms', true);

export const options = {
  scenarios: {
    inboundFlood: { executor: 'constant-arrival-rate', rate: RATE, timeUnit: '1s', duration: __ENV.DURATION || '30s', preAllocatedVUs: VUS, maxVUs: MAX_VUS },
    health: { executor: 'constant-arrival-rate', exec: 'health', rate: 2, timeUnit: '1s', duration: __ENV.DURATION || '30s', preAllocatedVUs: 4, maxVUs: 8 },
  },
  thresholds: { readonly_ws_invalid: ['rate==0'], server_5xx: ['rate==0'], rate_limited: ['rate==0'], dropped_iterations: ['count==0'], readonly_ws_close_ms: ['p(95)<1000'] },
};

function sourceIp(sequence) {
  return `39.${1 + (sequence % 200)}.${1 + (Math.floor(sequence / 200) % 200)}.${1 + (sequence % 250)}`;
}

export default function () {
  const sequence = exec.scenario.iterationInTest;
  const signalr = sequence % 2 === 1;
  const url = signalr ? `${WS_TARGET}/hub/attack?game=${GAME}` : `${WS_TARGET}/hub/attack/ws?game=${GAME}`;
  const started = Date.now();
  let greeting = false;
  let abuseSent = false;
  let closedByServer = false;
  let validClose = false;
  const response = ws.connect(url, { headers: { 'X-Real-IP': sourceIp(sequence) } }, (socket) => {
    socket.on('open', () => {
      if (signalr) socket.send(signalrHandshake());
    });
    socket.on('message', (message) => {
      if (signalr && !greeting && String(message).startsWith('{}')) {
        greeting = true;
        abuseSent = true;
        socket.send(unsupportedSignalrInvocation(sequence));
      } else if (!signalr && !greeting && String(message).includes('"kind":"hello"')) {
        greeting = true;
        abuseSent = true;
        socket.send('x'.repeat(FRAME_BYTES));
      }
    });
    socket.on('close', (code) => {
      closedByServer = true;
      validClose = validAbuseClose(code);
      closeMs.add(Date.now() - started);
    });
    socket.setTimeout(() => socket.close(), 2000);
  });
  server5xx.add(response?.status >= 500);
  rateLimited.add(response?.status === 429);
  invalid.add(response?.status !== 101 || !greeting || !abuseSent || !closedByServer || !validClose);
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, { responseType: 'text' });
  server5xx.add(response.status >= 500);
  invalid.add(response.status !== 200 || response.body !== 'ok');
}
