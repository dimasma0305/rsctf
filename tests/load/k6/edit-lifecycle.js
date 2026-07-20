import http from 'k6/http';
import { check } from 'k6';
import { Counter, Trend } from 'k6/metrics';

const target = (__ENV.TARGET || 'http://127.0.0.1:8080').replace(/\/$/, '');
const adminToken = __ENV.ADMIN_TOKEN || '';
const managerToken = __ENV.MANAGER_TOKEN || adminToken;
const context = JSON.parse(__ENV.EDIT_CONTEXT || '{}');
const rate = Number(__ENV.RATE || 4);
const duration = __ENV.DURATION || '20s';
const preAllocatedVUs = Number(__ENV.VUS || 12);
const maxVUs = Number(__ENV.MAX_VUS || 30);

const server5xx = new Counter('server_5xx');
const contractFailures = new Counter('edit_contract_failures');
const organizerReadMs = new Trend('organizer_read_ms', true);

export const options = {
  scenarios: {
    organizer_reads: {
      executor: 'constant-arrival-rate',
      rate,
      timeUnit: '1s',
      duration,
      preAllocatedVUs,
      maxVUs,
    },
  },
  thresholds: {
    server_5xx: ['count==0'],
    edit_contract_failures: ['count==0'],
    http_req_failed: ['rate==0'],
    organizer_read_ms: ['p(95)<2000'],
    dropped_iterations: ['count==0'],
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

function integer(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) throw new Error(`invalid ${label}`);
  return parsed;
}

const gameId = integer(context.gameId, 'future game id');
const challengeId = integer(context.challengeId, 'challenge id');
const adGameId = integer(context.adGameId, 'A&D game id');
const adServiceId = integer(context.adServiceId, 'A&D service id');
const kothGameId = integer(context.kothGameId, 'KotH game id');
const kothChallengeId = integer(context.kothChallengeId, 'KotH challenge id');

const endpoints = [
  { name: 'game', path: `/api/edit/games/${gameId}`, token: managerToken, shape: 'object' },
  { name: 'challenges', path: `/api/edit/games/${gameId}/challenges`, token: managerToken, shape: 'array' },
  { name: 'challenge', path: `/api/edit/games/${gameId}/challenges/${challengeId}`, token: managerToken, shape: 'object' },
  { name: 'reviews', path: `/api/edit/games/${gameId}/reviews?count=100&skip=0`, token: managerToken, shape: 'page' },
  { name: 'pending', path: `/api/edit/games/${gameId}/pendingchallenges`, token: managerToken, shape: 'array' },
  { name: 'notices', path: `/api/edit/games/${gameId}/notices`, token: managerToken, shape: 'array' },
  { name: 'divisions', path: `/api/edit/games/${gameId}/divisions`, token: managerToken, shape: 'array' },
  { name: 'ad_state', path: `/api/edit/games/${adGameId}/ad/State`, token: adminToken, shape: 'ad-state' },
  {
    name: 'ad_file',
    path: `/api/edit/games/${adGameId}/ad/Services/${adServiceId}/File?path=%2Fetc%2Fhostname`,
    token: adminToken,
    shape: 'object',
  },
  {
    name: 'ad_changes',
    path: `/api/edit/games/${adGameId}/ad/Services/${adServiceId}/Snapshot/Changes`,
    token: adminToken,
    shape: 'changes',
  },
  { name: 'koth_state', path: `/api/edit/games/${kothGameId}/ad/koth/state`, token: adminToken, shape: 'koth-state' },
  {
    name: 'koth_receipts',
    path: `/api/edit/games/${kothGameId}/ad/koth/${kothChallengeId}/receipts`,
    token: adminToken,
    shape: 'receipts',
  },
];

function unwrap(value) {
  if (
    value &&
    typeof value === 'object' &&
    !Array.isArray(value) &&
    Object.prototype.hasOwnProperty.call(value, 'data') &&
    !(Object.prototype.hasOwnProperty.call(value, 'total') && Object.prototype.hasOwnProperty.call(value, 'length'))
  ) return value.data;
  return value;
}

function validShape(shape, value) {
  if (shape === 'array') return Array.isArray(value);
  if (shape === 'object') return value !== null && typeof value === 'object' && !Array.isArray(value);
  if (shape === 'page') return value && Array.isArray(value.data) && Number.isInteger(value.total);
  if (shape === 'ad-state') return value && Array.isArray(value.challenges) && Array.isArray(value.teams);
  if (shape === 'changes') return value && typeof value.snapshotAvailable === 'boolean' && Array.isArray(value.changes);
  if (shape === 'koth-state') return value && Array.isArray(value.hills) && Array.isArray(value.teams);
  if (shape === 'receipts') return value && Array.isArray(value.receipts);
  return false;
}

export default function () {
  const endpoint = endpoints[__ITER % endpoints.length];
  const response = http.get(`${target}${endpoint.path}`, {
    headers: {
      Authorization: `Bearer ${endpoint.token}`,
      'X-Real-IP': `10.248.${__VU % 240}.${(__ITER % 240) + 1}`,
    },
    tags: { name: `edit_${endpoint.name}` },
    timeout: '15s',
  });
  organizerReadMs.add(response.timings.duration, { endpoint: endpoint.name });
  if (response.status >= 500) server5xx.add(1, { endpoint: endpoint.name, status: String(response.status) });

  let model;
  try {
    model = unwrap(response.json());
  } catch (_) {
    model = undefined;
  }
  const valid = response.status === 200 && validShape(endpoint.shape, model);
  if (!valid) contractFailures.add(1, { endpoint: endpoint.name, status: String(response.status) });
  check(response, {
    [`${endpoint.name}: status 200`]: (result) => result.status === 200,
    [`${endpoint.name}: exact response shape`]: () => valid,
  });
}
