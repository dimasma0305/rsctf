// One fixed-rate request per iteration across every newly bounded monitor read.
import http from 'k6/http';
import exec from 'k6/execution';
import { Rate, Trend } from 'k6/metrics';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const GAME = __ENV.GAME || '';
const FIXTURE = JSON.parse(open(__ENV.FIXTURE_FILE || ''));
const RATE = Number(__ENV.RATE || 4);
const VUS = Number(__ENV.VUS || 16);
const FLOW = FIXTURE.flow || {};
if (
  !/^\d+$/.test(GAME) ||
  !Array.isArray(FIXTURE.tokens) ||
  FIXTURE.tokens.length < 4 ||
  !Number.isSafeInteger(FLOW.challengeId) ||
  FLOW.challengeId <= 0 ||
  !Number.isSafeInteger(FLOW.participationId) ||
  FLOW.participationId <= 0 ||
  typeof FLOW.filename !== 'string' ||
  !/^[^/\\\r\n]+\.pcap$/i.test(FLOW.filename) ||
  typeof FLOW.snapshotVersion !== 'string' ||
  !/^[a-f\d]{32}$/i.test(FLOW.snapshotVersion) ||
  typeof FLOW.flowId !== 'string' ||
  FLOW.flowId.length > 76 ||
  FLOW.flowId.length % 2 !== 0 ||
  !/^[a-f\d]+$/i.test(FLOW.flowId) ||
  !Number.isSafeInteger(FLOW.connectionPort) ||
  FLOW.connectionPort < 1 ||
  FLOW.connectionPort > 65_535 ||
  typeof FLOW.peerIp !== 'string' ||
  FLOW.peerIp.length < 1 ||
  FLOW.peerIp.length > 64 ||
  !Number.isSafeInteger(FLOW.firstSeenUtc) ||
  FLOW.firstSeenUtc < 0 ||
  !Number.isSafeInteger(FLOW.lastSeenUtc) ||
  FLOW.lastSeenUtc < FLOW.firstSeenUtc ||
  !['ContainerToTeam', 'TeamToContainer'].includes(FLOW.direction)
) {
  throw new Error(
    'GAME and a bounded monitor evidence/inventory fixture with four tokens and one seeded PCAP flow are required',
  );
}

const invalidResponse = new Rate('monitor_inventory_invalid');
const oversizedBody = new Rate('monitor_inventory_oversized');
const server5xx = new Rate('monitor_inventory_5xx');
const rateLimited = new Rate('monitor_inventory_429');
const busyWithoutRetry = new Rate('monitor_inventory_busy_without_retry');
const newestFlowMismatch = new Rate('monitor_inventory_flow_newest_mismatch');
const monitorDuration = new Trend('monitor_inventory_ms', true);
const healthFailure = new Rate('monitor_inventory_health_failure');
const healthDuration = new Trend('monitor_inventory_health_ms', true);

export const options = {
  scenarios: {
    monitorReads: {
      executor: 'constant-arrival-rate',
      exec: 'monitorRead',
      rate: RATE,
      timeUnit: '1s',
      duration: __ENV.DURATION || '30s',
      preAllocatedVUs: VUS,
      maxVUs: VUS * 2,
    },
    exactHealth: {
      executor: 'constant-arrival-rate',
      exec: 'health',
      rate: 1,
      timeUnit: '1s',
      duration: __ENV.DURATION || '30s',
      preAllocatedVUs: 1,
      maxVUs: 2,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    monitor_inventory_invalid: ['rate==0'],
    monitor_inventory_oversized: ['rate==0'],
    monitor_inventory_5xx: ['rate==0'],
    monitor_inventory_429: ['rate==0'],
    monitor_inventory_busy_without_retry: ['rate==0'],
    monitor_inventory_flow_newest_mismatch: ['rate==0'],
    monitor_inventory_health_failure: ['rate==0'],
    dropped_iterations: ['count==0'],
    monitor_inventory_ms: ['p(95)<1000'],
    monitor_inventory_health_ms: ['p(95)<500'],
  },
};

let reportEtag = null;

const flowBase =
  `/api/game/captures/${FLOW.challengeId}/${FLOW.participationId}/` + `${encodeURIComponent(FLOW.filename)}`;
const flowPage = (kind, query, extra = {}) => ({
  kind,
  contract: 'flowPage',
  path: `${flowBase}/flows?${query}`,
  pageSize: 20,
  ...extra,
});
const rapidFlowFilters = [
  flowPage('flow-rapid-regex', 'regexPattern=.&page=1&pageSize=20'),
  flowPage('flow-rapid-direction', `direction=${FLOW.direction}&page=1&pageSize=20`),
  // This is deliberately last: its response must satisfy the newest filter,
  // even when the three cached scans finish out of order.
  flowPage('flow-rapid-newest-peer', `peerIpContains=${encodeURIComponent(FLOW.peerIp)}&page=1&pageSize=20`, {
    requireSeedPeer: true,
  }),
];

const endpoints = [
  { kind: 'incident', path: `/api/game/${GAME}/cheatinfo/page?limit=100` },
  {
    kind: 'delta',
    path: `/api/game/${GAME}/cheatinfo/page?limit=100&afterId=0`,
  },
  { kind: 'report', path: `/api/game/${GAME}/cheatreport` },
  {
    kind: 'evidence',
    path: `/api/game/${GAME}/cheatreport/events/${FIXTURE.eventId}`,
  },
  {
    kind: 'compare',
    path: `/api/game/${GAME}/cheatreport/compare?participationA=${FIXTURE.pair[0]}&participationB=${FIXTURE.pair[1]}`,
  },
  {
    kind: 'challenge',
    path: `/api/game/games/${GAME}/captures/page?count=10000`,
  },
  {
    kind: 'team',
    path: `/api/game/captures/${FIXTURE.challengeId}/page?count=10000`,
  },
  {
    kind: 'file',
    path: `/api/game/captures/${FIXTURE.challengeId}/${FIXTURE.participationId}/page?count=10000`,
  },
  flowPage('flow-summary', 'page=1&pageSize=20'),
  flowPage('flow-regex', 'regexPattern=.&page=1&pageSize=20'),
  flowPage('flow-peer', `peerIpContains=${encodeURIComponent(FLOW.peerIp)}&page=1&pageSize=20`, {
    requireSeedPeer: true,
  }),
  flowPage('flow-time', `startUtc=${FLOW.firstSeenUtc}&endUtc=${FLOW.lastSeenUtc}&page=1&pageSize=20`),
  flowPage('flow-direction', `direction=${FLOW.direction}&page=1&pageSize=20`),
  flowPage('flow-flags', 'flagsOnly=true&page=1&pageSize=20'),
  {
    kind: 'flow-invalid-regex',
    contract: 'invalidRegex',
    path: `${flowBase}/flows?regexPattern=${encodeURIComponent('(')}&page=1&pageSize=20`,
  },
  {
    kind: 'flow-detail',
    contract: 'flowDetail',
    path: `${flowBase}/flow/${FLOW.connectionPort}?snapshotVersion=${FLOW.snapshotVersion}` + `&flowId=${FLOW.flowId}`,
  },
  { kind: 'flow-rapid', contract: 'flowRapid' },
];

function token(index) {
  return FIXTURE.tokens[index % FIXTURE.tokens.length];
}

function header(response, name) {
  const target = name.toLowerCase();
  for (const [key, value] of Object.entries(response.headers || {})) {
    if (key.toLowerCase() === target) return value;
  }
  return null;
}

function parse(response) {
  try {
    return response.json();
  } catch (_) {
    return null;
  }
}

function validPage(body, itemsKey = 'items') {
  const rows = body && Array.isArray(body[itemsKey]) ? body[itemsKey] : null;
  return rows !== null && rows.length <= 100 && (body.nextCursor === null || typeof body.nextCursor === 'string');
}

const flowSummaryFields = [
  'flowId',
  'connectionPort',
  'firstSeenUtc',
  'lastSeenUtc',
  'peerIp',
  'packetsIn',
  'packetsOut',
  'bytesIn',
  'bytesOut',
  'flagHits',
  'payloadTruncated',
];
const exactFields = (value, fields) =>
  value &&
  typeof value === 'object' &&
  Object.keys(value).length === fields.length &&
  fields.every((field) => Object.hasOwn(value, field));

function validFlowSummary(row, requireExactFields = true) {
  if (requireExactFields && !exactFields(row, flowSummaryFields)) return false;
  return (
    row &&
    typeof row.flowId === 'string' &&
    row.flowId.length <= 76 &&
    row.flowId.length % 2 === 0 &&
    /^[a-f\d]+$/i.test(row.flowId) &&
    Number.isSafeInteger(row.connectionPort) &&
    row.connectionPort >= 1 &&
    row.connectionPort <= 65_535 &&
    Number.isSafeInteger(row.firstSeenUtc) &&
    row.firstSeenUtc >= 0 &&
    Number.isSafeInteger(row.lastSeenUtc) &&
    row.lastSeenUtc >= row.firstSeenUtc &&
    typeof row.peerIp === 'string' &&
    row.peerIp.length >= 1 &&
    row.peerIp.length <= 64 &&
    Number.isSafeInteger(row.packetsIn) &&
    row.packetsIn >= 0 &&
    Number.isSafeInteger(row.packetsOut) &&
    row.packetsOut >= 0 &&
    Number.isSafeInteger(row.bytesIn) &&
    row.bytesIn >= 0 &&
    Number.isSafeInteger(row.bytesOut) &&
    row.bytesOut >= 0 &&
    Number.isSafeInteger(row.flagHits) &&
    row.flagHits >= 0 &&
    typeof row.payloadTruncated === 'boolean'
  );
}

function validFlowPage(body, endpoint) {
  const fields = [
    'items',
    'page',
    'pageSize',
    'totalItems',
    'totalPages',
    'snapshotVersion',
    'indexedPayloadBytes',
    'payloadTruncated',
  ];
  if (!exactFields(body, fields) || !Array.isArray(body.items) || body.items.length > endpoint.pageSize) return false;
  if (
    body.page !== 1 ||
    body.pageSize !== endpoint.pageSize ||
    !Number.isSafeInteger(body.totalItems) ||
    body.totalItems < body.items.length ||
    !Number.isSafeInteger(body.totalPages) ||
    body.totalPages !== Math.ceil(body.totalItems / body.pageSize) ||
    body.snapshotVersion !== FLOW.snapshotVersion ||
    !Number.isSafeInteger(body.indexedPayloadBytes) ||
    body.indexedPayloadBytes < 0 ||
    typeof body.payloadTruncated !== 'boolean' ||
    !body.items.every((row) => validFlowSummary(row))
  )
    return false;
  if (endpoint.requireSeedPeer) {
    const peer = FLOW.peerIp.toLowerCase();
    return body.items.length > 0 && body.items.every((row) => row.peerIp.toLowerCase().includes(peer));
  }
  return true;
}

function validFlowDetail(body) {
  const fields = [...flowSummaryFields, 'snapshotVersion', 'chunks'];
  if (!exactFields(body, fields) || !validFlowSummary(body, false)) return false;
  if (
    body.flowId !== FLOW.flowId ||
    body.connectionPort !== FLOW.connectionPort ||
    body.snapshotVersion !== FLOW.snapshotVersion ||
    !Array.isArray(body.chunks) ||
    body.chunks.length > 1_024
  )
    return false;
  let encodedPayloadBytes = 0;
  for (const chunk of body.chunks) {
    if (
      !exactFields(chunk, ['direction', 'timestampUtc', 'payloadBase64', 'flagOffsets']) ||
      !['ContainerToTeam', 'TeamToContainer'].includes(chunk.direction) ||
      !Number.isSafeInteger(chunk.timestampUtc) ||
      chunk.timestampUtc < 0 ||
      typeof chunk.payloadBase64 !== 'string' ||
      !/^(?:[A-Za-z\d+/]{4})*(?:[A-Za-z\d+/]{2}==|[A-Za-z\d+/]{3}=)?$/.test(chunk.payloadBase64) ||
      !Array.isArray(chunk.flagOffsets) ||
      chunk.flagOffsets.length > 256 ||
      !chunk.flagOffsets.every((offset) => Number.isSafeInteger(offset) && offset >= 0 && offset < 256 * 1024)
    )
      return false;
    encodedPayloadBytes += chunk.payloadBase64.length;
    if (encodedPayloadBytes > 384 * 1024) return false;
  }
  return true;
}

function validIncident(body, delta) {
  if (!body || !Array.isArray(body.data) || body.data.length > 100 || !Number.isSafeInteger(body.checkpointId))
    return false;
  const ids = new Set();
  let previous = -1;
  for (const row of body.data) {
    if (!row || !Number.isSafeInteger(row.id) || !Number.isFinite(row.observedAt) || ids.has(row.id)) return false;
    if (delta && row.id <= previous) return false;
    previous = row.id;
    ids.add(row.id);
  }
  return (
    typeof body.hasMore === 'boolean' &&
    (body.nextBefore === null ||
      (Number.isFinite(body.nextBefore.observedAt) && Number.isSafeInteger(body.nextBefore.id)))
  );
}

function semantic(endpoint, response) {
  if (endpoint.contract === 'invalidRegex') return response.status === 400;
  if (endpoint.kind === 'report' && response.status === 304) return String(response.body || '').length === 0;
  if (response.status !== 200) return false;
  const body = parse(response);
  if (endpoint.contract === 'flowPage') return validFlowPage(body, endpoint);
  if (endpoint.contract === 'flowDetail') return validFlowDetail(body);
  if (endpoint.kind === 'incident' || endpoint.kind === 'delta') return validIncident(body, endpoint.kind === 'delta');
  if (endpoint.kind === 'report') {
    return (
      body &&
      Number.isFinite(body.generatedAt) &&
      Array.isArray(body.suspicionList) &&
      Array.isArray(body.collusionGroups)
    );
  }
  if (endpoint.kind === 'evidence') return body && Number.isSafeInteger(body.eventId) && Array.isArray(body.sources);
  if (endpoint.kind === 'compare')
    return body && Number.isFinite(body.rsi) && Array.isArray(body.details) && body.details.length <= 50;
  return validPage(body);
}

function responseByteLimit(endpoint) {
  if (endpoint.kind === 'report') return 4 * 1024 * 1024;
  if (endpoint.contract === 'flowDetail') return 768 * 1024;
  if (endpoint.contract === 'invalidRegex') return 16 * 1024;
  return 512 * 1024;
}

function observe(endpoint, response) {
  monitorDuration.add(response.timings.duration);
  const busy = response.status === 503;
  const validRetryAfter = /^\d+$/.test(String(header(response, 'retry-after') || ''));
  server5xx.add(response.status >= 500 && !busy);
  rateLimited.add(response.status === 429);
  busyWithoutRetry.add(busy && !validRetryAfter);
  oversizedBody.add(String(response.body || '').length > responseByteLimit(endpoint));
  invalidResponse.add(busy ? !validRetryAfter : !semantic(endpoint, response));
}

function rapidFlowRead(headers) {
  const responses = http.batch(
    rapidFlowFilters.map((endpoint) => ({
      method: 'GET',
      url: `${TARGET}${endpoint.path}`,
      params: {
        headers,
        responseType: 'text',
        tags: { endpoint: endpoint.kind },
      },
    })),
  );
  responses.forEach((response, index) => observe(rapidFlowFilters[index], response));

  const newestIndex = rapidFlowFilters.length - 1;
  const newestResponse = responses[newestIndex];
  newestFlowMismatch.add(
    newestResponse.status === 200 && !validFlowPage(parse(newestResponse), rapidFlowFilters[newestIndex]),
  );
}

export function monitorRead() {
  const sequence = exec.scenario.iterationInTest;
  const endpoint = endpoints[sequence % endpoints.length];
  const headers = {
    Authorization: `Bearer ${token(sequence)}`,
    'X-Real-IP': `31.${1 + (sequence % 240)}.${1 + (Math.floor(sequence / 240) % 250)}.${1 + (sequence % 250)}`,
  };
  if (endpoint.contract === 'flowRapid') {
    rapidFlowRead(headers);
    return;
  }
  if (endpoint.kind === 'report' && reportEtag) headers['If-None-Match'] = reportEtag;
  const response = http.get(`${TARGET}${endpoint.path}`, {
    headers,
    responseType: 'text',
    tags: { endpoint: endpoint.kind },
  });
  observe(endpoint, response);

  if (endpoint.kind === 'report' && response.status === 200) reportEtag = header(response, 'etag');
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, {
    responseType: 'text',
    tags: { endpoint: 'healthz' },
  });
  healthDuration.add(response.timings.duration);
  healthFailure.add(response.status !== 200 || response.body !== 'ok');
}
