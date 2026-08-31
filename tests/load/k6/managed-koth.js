import http from 'k6/http';
import crypto from 'k6/crypto';
import exec from 'k6/execution';
import { check } from 'k6';
import { Counter, Rate, Trend } from 'k6/metrics';
import { SharedArray } from 'k6/data';

const PLATFORM = __ENV.TARGET || 'http://127.0.0.1:8080';
const ARENA = __ENV.MANAGED_KOTH_ARENA;
const GAME = Number(__ENV.MANAGED_KOTH_GAME);
const CHALLENGE = Number(__ENV.MANAGED_KOTH_CHALLENGE);
const ADMIN_TOKEN = __ENV.MANAGED_KOTH_ADMIN_TOKEN;
const RATE = Number(__ENV.RATE || 100);
const VUS = Number(__ENV.VUS || 128);
const DURATION_SECONDS = Number(__ENV.MANAGED_KOTH_DURATION_SECONDS || 20);
const ACTIVE_FLEET = Number(__ENV.MANAGED_KOTH_ACTIVE_FLEET || 64);
const PHASE = __ENV.MANAGED_KOTH_PHASE || 'valid';
const TOKENS_FILE = __ENV.MANAGED_KOTH_TOKENS_FILE;

if (
  !ARENA ||
  !ADMIN_TOKEN ||
  !TOKENS_FILE ||
  !Number.isSafeInteger(GAME) ||
  GAME <= 0 ||
  !Number.isSafeInteger(CHALLENGE) ||
  CHALLENGE <= 0 ||
  !Number.isSafeInteger(RATE) ||
  RATE <= 0 ||
  !Number.isSafeInteger(VUS) ||
  VUS <= 0 ||
  !Number.isSafeInteger(DURATION_SECONDS) ||
  DURATION_SECONDS <= 0 ||
  !Number.isSafeInteger(ACTIVE_FLEET) ||
  ACTIVE_FLEET < 2 ||
  ACTIVE_FLEET > 128 ||
  !['valid', 'abuse'].includes(PHASE)
) {
  throw new Error('managed KotH k6 scope and bounded load settings are required');
}

const TOKENS = new SharedArray('managed-koth-capabilities', () => {
  const parsed = JSON.parse(open(TOKENS_FILE));
  if (
    !Array.isArray(parsed) ||
    parsed.length !== 2_000 ||
    new Set(parsed).size !== parsed.length ||
    parsed.some((token) => typeof token !== 'string' || !token.startsWith('koth_'))
  ) {
    throw new Error('managed KotH requires exactly 2,000 unique capabilities');
  }
  return parsed;
});

const server5xx = new Rate('server_5xx');
const validPlayInvalid = new Rate('valid_play_invalid');
const invalidCapabilityAccepted = new Rate('invalid_capability_accepted');
const adminReadInvalid = new Rate('admin_read_invalid');
const healthInvalid = new Rate('health_invalid');
const validCapabilities = new Counter('valid_capabilities_exercised');
const validPlayHttp401 = new Counter('valid_play_http_401');
const validPlayHttp409 = new Counter('valid_play_http_409');
const validPlayHttp429 = new Counter('valid_play_http_429');
const validPlayHttp503 = new Counter('valid_play_http_503');
const validPlayHttpOther = new Counter('valid_play_http_other');
const validPlayModelMismatch = new Counter('valid_play_model_mismatch');
const rejectedCapabilities = new Counter('invalid_capabilities_rejected');
const rateLimitedCapabilities = new Counter('invalid_capabilities_rate_limited');
const invalidRetryAfter = new Rate('invalid_retry_after');
const adminReadHttp400 = new Counter('admin_read_http_400');
const adminReadHttp401 = new Counter('admin_read_http_401');
const adminReadHttp403 = new Counter('admin_read_http_403');
const adminReadHttp404 = new Counter('admin_read_http_404');
const adminReadHttp429 = new Counter('admin_read_http_429');
const adminReadHttpOther = new Counter('admin_read_http_other');
const adminReadModelMismatch = new Counter('admin_read_model_mismatch');
const playLatency = new Trend('managed_koth_play_ms', true);
const boardLatency = new Trend('managed_koth_admin_read_ms', true);
const healthLatency = new Trend('managed_koth_health_ms', true);

const duration = `${DURATION_SECONDS}s`;
const primaryScenario = PHASE === 'valid'
  ? {
      executor: 'constant-arrival-rate',
      exec: 'play',
      rate: RATE,
      timeUnit: '1s',
      duration,
      preAllocatedVUs: Math.min(VUS, Math.max(16, Math.ceil(RATE / 2))),
      maxVUs: VUS,
      gracefulStop: '10s',
    }
  : {
      executor: 'constant-arrival-rate',
      exec: 'abuse',
      rate: RATE,
      timeUnit: '1s',
      duration,
      preAllocatedVUs: Math.min(VUS, Math.max(32, Math.ceil(RATE / 2))),
      maxVUs: VUS,
      gracefulStop: '10s',
    };
export const options = {
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  scenarios: {
    primary: primaryScenario,
    ...(PHASE === 'valid' ? { admin: {
      executor: 'constant-arrival-rate',
      exec: 'adminRead',
      rate: 2,
      timeUnit: '1s',
      duration,
      preAllocatedVUs: 2,
      maxVUs: 4,
      gracefulStop: '5s',
    } } : {}),
    health: {
      executor: 'constant-arrival-rate',
      exec: 'health',
      rate: 1,
      timeUnit: '1s',
      duration,
      preAllocatedVUs: 1,
      maxVUs: 2,
      gracefulStop: '5s',
    },
  },
  thresholds: {
    server_5xx: ['rate==0'],
    ...(PHASE === 'abuse' ? {
      invalid_capability_accepted: ['rate==0'],
      invalid_capabilities_rejected: ['count>0'],
      invalid_capabilities_rate_limited: ['count>0'],
      invalid_retry_after: ['rate==0'],
    } : {
      valid_play_invalid: ['rate==0'],
      valid_capabilities_exercised: ['count==2000'],
      admin_read_invalid: ['rate==0'],
      managed_koth_play_ms: ['p(95)<1500'],
      managed_koth_admin_read_ms: ['p(95)<1000'],
    }),
    health_invalid: ['rate==0'],
    dropped_iterations: ['count==0'],
    managed_koth_health_ms: ['p(95)<500'],
  },
};

function record5xx(response) {
  server5xx.add(response.status >= 500 || response.status === 0);
}

function responseJson(response) {
  try {
    return response.json();
  } catch (_) {
    return null;
  }
}

export function play() {
  const iteration = exec.scenario.iterationInTest;
  const token = TOKENS[iteration % TOKENS.length];
  const expectedTeamId = crypto.sha256(token, 'hex');
  const expectedScoreable = iteration < ACTIVE_FLEET;
  const response = http.post(
    `${ARENA}/play`,
    JSON.stringify({ token, score: expectedScoreable ? 1_000 - iteration : 0 }),
    { headers: { 'Content-Type': 'application/json' } },
  );
  record5xx(response);
  const model = responseJson(response);
  const invalid =
    response.status !== 200 ||
    model?.accepted !== true ||
    model?.teamId !== expectedTeamId ||
    model?.scoreable !== expectedScoreable;
  validPlayInvalid.add(invalid);
  if (!invalid) {
    validCapabilities.add(1);
    playLatency.add(response.timings.duration);
  } else if (response.status === 401) {
    validPlayHttp401.add(1);
  } else if (response.status === 409) {
    validPlayHttp409.add(1);
  } else if (response.status === 429) {
    validPlayHttp429.add(1);
  } else if (response.status === 503) {
    validPlayHttp503.add(1);
  } else if (response.status !== 200) {
    validPlayHttpOther.add(1);
  } else {
    validPlayModelMismatch.add(1);
  }
  check(response, { 'valid capability authenticated by arena': () => !invalid });
}

export function abuse() {
  const iteration = exec.scenario.iterationInTest;
  const invalid = `koth_invalid_${iteration}_${__VU}`;
  const response = http.post(
    `${ARENA}/play`,
    JSON.stringify({ token: invalid, score: 0 }),
    { headers: { 'Content-Type': 'application/json' } },
  );
  record5xx(response);
  const expected = response.status === 401 || response.status === 429;
  invalidCapabilityAccepted.add(!expected);
  if (response.status === 401) rejectedCapabilities.add(1);
  if (response.status === 429) {
    rateLimitedCapabilities.add(1);
    const retryAfter = Number(response.headers['Retry-After']);
    invalidRetryAfter.add(!Number.isFinite(retryAfter) || retryAfter < 1);
  }
  check(response, { 'invalid capability is rejected or rate limited': () => expected });
}

export function adminRead() {
  const headers = { Authorization: `Bearer ${ADMIN_TOKEN}` };
  const responses = http.batch([
    ['GET', `${PLATFORM}/api/game/${GAME}/ad/koth/scoreboard`, null, { headers }],
    ['GET', `${PLATFORM}/api/edit/games/${GAME}/ad/koth/state`, null, { headers }],
  ]);
  let invalid = false;
  for (const response of responses) {
    record5xx(response);
    const model = responseJson(response);
    const responseInvalid = response.status !== 200 || model === null;
    invalid ||= responseInvalid;
    if (!responseInvalid) {
      boardLatency.add(response.timings.duration);
    } else if (response.status === 400) {
      adminReadHttp400.add(1);
    } else if (response.status === 401) {
      adminReadHttp401.add(1);
    } else if (response.status === 403) {
      adminReadHttp403.add(1);
    } else if (response.status === 404) {
      adminReadHttp404.add(1);
    } else if (response.status === 429) {
      adminReadHttp429.add(1);
    } else if (response.status !== 200) {
      adminReadHttpOther.add(1);
    } else {
      adminReadModelMismatch.add(1);
    }
  }
  adminReadInvalid.add(invalid);
  check(responses, { 'hidden event remains operable by admin': () => !invalid });
}

export function health() {
  const responses = http.batch([
    ['GET', `${PLATFORM}/healthz`],
    ['GET', `${ARENA}/healthz`],
  ]);
  let invalid = false;
  for (const response of responses) {
    record5xx(response);
    invalid ||= response.status !== 200 || response.body !== 'ok';
    if (response.status === 200) healthLatency.add(response.timings.duration);
  }
  healthInvalid.add(invalid);
  check(responses, { 'platform and managed arena health are exact': () => !invalid });
}
