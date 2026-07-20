// Fixed-rate organizer transport load. The Node orchestrator owns the exact
// disposable Admin identity and Containers row; this phase only negotiates,
// performs real SignalR handshakes, and opens short-lived exec sessions.
import { check } from 'k6';
import encoding from 'k6/encoding';
import http from 'k6/http';
import { Rate, Trend } from 'k6/metrics';
import ws from 'k6/ws';

const RS = '\u001e';
const ADMIN_TARGET = origin(__ENV.ADMIN_TARGET, 'ADMIN_TARGET');
const EXEC_TARGET = origin(__ENV.EXEC_TARGET, 'EXEC_TARGET');
const ADMIN_TOKEN = __ENV.ADMIN_TOKEN || '';
const CONTAINER_GUID = __ENV.CONTAINER_GUID || '';
const MANAGER_TOKEN = __ENV.MANAGER_TOKEN || '';
const SCOPED_GAME_ID = __ENV.SCOPED_GAME_ID || '';
const SCOPED_TARGET = __ENV.SCOPED_TARGET || '';
const SCOPED_VALUES = [MANAGER_TOKEN, SCOPED_GAME_ID, SCOPED_TARGET];
const HAS_SCOPED_EXEC = SCOPED_VALUES.every(Boolean);
const SCOPED_PATH = HAS_SCOPED_EXEC
  ? `/hub/containerExec/games/${positiveInteger(SCOPED_GAME_ID, 'SCOPED_GAME_ID')}`
  : null;
const RATE = positiveInteger(__ENV.RATE || 1, 'RATE');
const VUS = positiveInteger(__ENV.VUS || 4, 'VUS');
const DURATION = __ENV.DURATION || '20s';
const MAX_P95_MS = positiveInteger(__ENV.MAX_ORGANIZER_HUB_P95_MS || 2000, 'MAX_ORGANIZER_HUB_P95_MS');

if (__ENV.ORGANIZER_HUBS_DISPOSABLE !== '1') {
  throw new Error('ORGANIZER_HUBS_DISPOSABLE=1 is required');
}
if (__ENV.CONFIRM_ORGANIZER_HUB_ADMIN_TARGET !== ADMIN_TARGET) {
  throw new Error('CONFIRM_ORGANIZER_HUB_ADMIN_TARGET must exactly match ADMIN_TARGET');
}
if (__ENV.CONFIRM_ORGANIZER_HUB_EXEC_TARGET !== EXEC_TARGET) {
  throw new Error('CONFIRM_ORGANIZER_HUB_EXEC_TARGET must exactly match EXEC_TARGET');
}
if (!disposableOrigin(ADMIN_TARGET) || !disposableOrigin(EXEC_TARGET)) {
  throw new Error('organizer-hub load targets must be loopback or RFC1918');
}
if (!ADMIN_TOKEN) throw new Error('ADMIN_TOKEN is required');
if (!/^[0-9a-f-]{36}$/i.test(CONTAINER_GUID)) throw new Error('CONTAINER_GUID must be a UUID');
if (SCOPED_VALUES.some(Boolean) && !HAS_SCOPED_EXEC) {
  throw new Error('MANAGER_TOKEN, SCOPED_GAME_ID, and SCOPED_TARGET must be supplied together');
}
if (
  HAS_SCOPED_EXEC &&
  !/^byoc:[1-9]\d*:[1-9]\d*$/.test(SCOPED_TARGET) &&
  !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(SCOPED_TARGET)
) {
  throw new Error(
    'SCOPED_TARGET must be a canonical byoc:<participation>:<challenge> id or public container UUID',
  );
}
if (RATE > 1) {
  throw new Error('RATE must stay at 1 iteration per 2 seconds (negotiates and upgrades share the Admin quota)');
}

const organizerFailure = new Rate('organizer_hub_failure');
const negotiateFailure = new Rate('organizer_negotiate_failure');
const adminSessionMs = new Trend('admin_hub_session_ms', true);
const execSessionMs = new Trend('container_exec_session_ms', true);
const scopedExecSessionMs = new Trend('scoped_container_exec_session_ms', true);
const iterationMs = new Trend('organizer_hub_iteration_ms', true);

const thresholds = {
  checks: ['rate==1'],
  http_req_failed: ['rate==0'],
  organizer_hub_failure: ['rate==0'],
  organizer_negotiate_failure: ['rate==0'],
  dropped_iterations: ['count==0'],
  admin_hub_session_ms: [`p(95)<${MAX_P95_MS}`],
  container_exec_session_ms: [`p(95)<${MAX_P95_MS}`],
};
if (HAS_SCOPED_EXEC) {
  thresholds.scoped_container_exec_session_ms = [`p(95)<${MAX_P95_MS}`];
}

export const options = {
  scenarios: {
    organizer_hubs: {
      executor: 'constant-arrival-rate',
      rate: RATE,
      timeUnit: '2s',
      duration: DURATION,
      preAllocatedVUs: VUS,
      maxVUs: Math.max(VUS, RATE * 4),
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds,
};

function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) throw new Error(`${label} must be a positive integer`);
  return parsed;
}

function origin(value, label) {
  const text = String(value || '').trim().replace(/\/+$/, '');
  if (!/^https?:\/\/[^/?#]+$/i.test(text) || /[\s@]/.test(text.replace(/^https?:\/\//i, ''))) {
    throw new Error(`${label} must be a credential-free HTTP(S) origin without a path`);
  }
  return text;
}

function disposableOrigin(value) {
  const host = value.replace(/^https?:\/\//i, '').replace(/:\d+$/, '').replace(/^\[|\]$/g, '').toLowerCase();
  if (host === 'localhost' || host === '::1') return true;
  const octets = host.split('.').map(Number);
  return octets.length === 4 && octets.every((part) => Number.isInteger(part) && part >= 0 && part <= 255) && (
    octets[0] === 10 || octets[0] === 127 ||
    (octets[0] === 172 && octets[1] >= 16 && octets[1] <= 31) ||
    (octets[0] === 192 && octets[1] === 168)
  );
}

function websocketUrl(baseUrl, path, token, connectionToken) {
  const scheme = baseUrl.startsWith('https:') ? 'wss:' : 'ws:';
  const authority = baseUrl.replace(/^https?:/i, '');
  return `${scheme}${authority}${path}?id=${encodeURIComponent(connectionToken)}` +
    `&access_token=${encodeURIComponent(token)}`;
}

function frame(value) {
  return `${JSON.stringify(value)}${RS}`;
}

function frames(value) {
  return String(value)
    .split(RS)
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => JSON.parse(part));
}

function negotiate(baseUrl, path, token, label) {
  const response = http.post(`${baseUrl}${path}/negotiate?negotiateVersion=1`, '', {
    headers: {
      authorization: `Bearer ${token}`,
      'content-type': 'text/plain;charset=UTF-8',
    },
    tags: { name: `${label}_negotiate` },
  });
  let body;
  try {
    body = response.json();
  } catch {
    body = null;
  }
  const valid = check(response, {
    [`${label} negotiate preserves SignalR WebSocket contract`]: () =>
      response.status === 200 &&
      body?.negotiateVersion === 1 &&
      typeof body?.connectionToken === 'string' &&
      body.connectionToken === body.connectionId &&
      body.availableTransports?.[0]?.transport === 'WebSockets',
  });
  negotiateFailure.add(!valid, { hub: label });
  return valid ? body.connectionToken : null;
}

function adminHubSession(connectionToken) {
  const started = Date.now();
  let passed = false;
  const response = ws.connect(
    websocketUrl(ADMIN_TARGET, '/hub/admin', ADMIN_TOKEN, connectionToken),
    { tags: { name: 'admin_hub_ws' } },
    (socket) => {
      socket.on('open', () => socket.send(frame({ protocol: 'json', version: 1 })));
      socket.on('message', (message) => {
        for (const item of frames(message)) {
          if (item && typeof item === 'object' && Object.keys(item).length === 0) {
            passed = true;
            socket.close();
          }
        }
      });
      socket.setTimeout(() => socket.close(), 5_000);
    },
  );
  passed = check(response, {
    'AdminHub upgraded and acknowledged a real SignalR handshake': () => response?.status === 101 && passed,
  }) && passed;
  organizerFailure.add(!passed, { hub: 'admin' });
  adminSessionMs.add(Date.now() - started);
  return passed;
}

function execHubSession(connectionToken, { path, token, target, label, trend }) {
  const started = Date.now();
  let handshaken = false;
  let sessionId = null;
  let welcome = false;
  let resizeDone = false;
  let inputDone = false;
  let closeSent = false;
  let passed = false;
  let parseFailed = false;
  const response = ws.connect(
    websocketUrl(EXEC_TARGET, path, token, connectionToken),
    { tags: { name: `${label}_ws` } },
    (socket) => {
      const sendInvocation = (id, target, args) => socket.send(frame({
        type: 1,
        invocationId: String(id),
        target,
        arguments: args,
      }));
      socket.on('open', () => socket.send(frame({ protocol: 'json', version: 1 })));
      socket.on('message', (message) => {
        let decoded;
        try {
          decoded = frames(message);
        } catch {
          parseFailed = true;
          socket.close();
          return;
        }
        for (const item of decoded) {
          if (!handshaken && item && typeof item === 'object' && Object.keys(item).length === 0) {
            handshaken = true;
            sendInvocation(1, 'Open', [target, 'sh']);
          } else if (item?.type === 3 && item.invocationId === '1') {
            sessionId = item.result;
            sendInvocation(2, 'Resize', [sessionId, 80, 24]);
            sendInvocation(3, 'Input', [sessionId, encoding.b64encode('true\n')]);
          } else if (item?.type === 1 && item.target === 'Receive' && item.arguments?.[0] === sessionId) {
            const output = encoding.b64decode(item.arguments[1], 'std', 's');
            if (String(output).includes('[rsctf] connected to')) welcome = true;
          } else if (item?.type === 3 && item.invocationId === '2') {
            resizeDone = !item.error;
          } else if (item?.type === 3 && item.invocationId === '3') {
            inputDone = !item.error;
          } else if (item?.type === 3 && item.invocationId === '4') {
            passed = !item.error && handshaken && welcome && resizeDone && inputDone;
            socket.close();
          }
          if (sessionId && resizeDone && inputDone && !closeSent) {
            closeSent = true;
            sendInvocation(4, 'Close', [sessionId]);
          }
        }
      });
      socket.setTimeout(() => socket.close(), 8_000);
    },
  );
  passed = check(response, {
    [`${label} completed handshake/Open/Input/Resize/Close`]: () =>
      response?.status === 101 && passed && !parseFailed,
  }) && passed;
  organizerFailure.add(!passed, { hub: label });
  trend.add(Date.now() - started);
  return passed;
}

export default function () {
  const started = Date.now();
  const adminConnection = negotiate(ADMIN_TARGET, '/hub/admin', ADMIN_TOKEN, 'admin_hub');
  const execConnection = negotiate(
    EXEC_TARGET,
    '/hub/containerExec',
    ADMIN_TOKEN,
    'container_exec',
  );
  const scopedConnection = HAS_SCOPED_EXEC
    ? negotiate(EXEC_TARGET, SCOPED_PATH, MANAGER_TOKEN, 'scoped_container_exec')
    : null;
  const adminValid = adminConnection ? adminHubSession(adminConnection) : false;
  const execValid = execConnection
    ? execHubSession(execConnection, {
      path: '/hub/containerExec',
      token: ADMIN_TOKEN,
      target: CONTAINER_GUID,
      label: 'ContainerExecHub',
      trend: execSessionMs,
    })
    : false;
  const scopedValid = !HAS_SCOPED_EXEC || (scopedConnection
    ? execHubSession(scopedConnection, {
      path: SCOPED_PATH,
      token: MANAGER_TOKEN,
      target: SCOPED_TARGET,
      label: 'ScopedContainerExecHub',
      trend: scopedExecSessionMs,
    })
    : false);
  const valid = Boolean(adminValid && execValid && scopedValid);
  organizerFailure.add(!valid, { hub: 'iteration' });
  iterationMs.add(Date.now() - started);
}
