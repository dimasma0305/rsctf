// Pure contracts shared by the organizer-hub E2E orchestrator, its k6 load
// phase, and unit tests. Keep secrets and Docker/SQL side effects out of this
// module so safety checks can run before the first authenticated request.

export const SIGNALR_RECORD_SEPARATOR = '\u001e';

export const PRIVILEGED_HUB_SURFACES = Object.freeze([
  Object.freeze({ id: 'admin_upgrade', method: 'GET', path: '/hub/admin' }),
  Object.freeze({ id: 'admin_negotiate', method: 'POST', path: '/hub/admin/negotiate' }),
  Object.freeze({ id: 'container_exec_upgrade', method: 'GET', path: '/hub/containerExec' }),
  Object.freeze({ id: 'container_exec_negotiate', method: 'POST', path: '/hub/containerExec/negotiate' }),
  Object.freeze({ id: 'scoped_container_exec_upgrade', method: 'GET', path: '/hub/containerExec/games/{game_id}' }),
  Object.freeze({
    id: 'scoped_container_exec_negotiate',
    method: 'POST',
    path: '/hub/containerExec/games/{game_id}/negotiate',
  }),
]);

export function assertPrivilegedHubCoverage(routes) {
  const key = ({ method, path }) => `${method} ${path}`;
  const expected = new Set(PRIVILEGED_HUB_SURFACES.map(key));
  const actualEntries = (routes || []).map(key);
  const actual = new Set(actualEntries);
  const missing = [...expected].filter((entry) => !actual.has(entry));
  const extra = [...actual].filter((entry) => !expected.has(entry));
  if (missing.length || extra.length || actual.size !== actualEntries.length) {
    throw new Error(
      `privileged hub catalog drift (missing: ${missing.join(', ') || 'none'}; ` +
        `uncovered: ${extra.join(', ') || 'none'}; duplicates: ${actualEntries.length - actual.size})`,
    );
  }
  return actual.size;
}

export function assertPrivilegedHubRuntimeCoverage(covered) {
  const expected = new Set(PRIVILEGED_HUB_SURFACES.map(({ id }) => id));
  const entries = [...(covered || [])].map(String);
  const actual = new Set(entries);
  const missing = [...expected].filter((id) => !actual.has(id));
  const extra = [...actual].filter((id) => !expected.has(id));
  if (missing.length || extra.length || actual.size !== entries.length) {
    throw new Error(
      `privileged hub runtime coverage incomplete (missing: ${missing.join(', ') || 'none'}; ` +
        `unknown: ${extra.join(', ') || 'none'}; duplicates: ${entries.length - actual.size})`,
    );
  }
  return actual.size;
}

export function privilegedHubSurfaceId(method, path) {
  const actualMethod = String(method || '').toUpperCase();
  const actualPath = String(path || '')
    .split('?')[0]
    .replace(/\/games\/[1-9][0-9]*(?=\/|$)/, '/games/{game_id}');
  const matches = PRIVILEGED_HUB_SURFACES.filter(
    (surface) => surface.method === actualMethod && surface.path === actualPath,
  );
  if (matches.length !== 1) {
    throw new Error(`unknown privileged hub runtime surface ${actualMethod} ${actualPath}`);
  }
  return matches[0].id;
}

export function organizerByocMode(environment) {
  const env = environment || {};
  if (env.SKIP_ORGANIZER_HUB_BYOC !== undefined) {
    throw new Error(
      'SKIP_ORGANIZER_HUB_BYOC is no longer accepted; use ORGANIZER_HUB_DIAGNOSTIC_SKIP_BYOC=1',
    );
  }
  if (env.ORGANIZER_HUB_REQUIRE_BYOC === '0') {
    throw new Error(
      'ORGANIZER_HUB_REQUIRE_BYOC=0 is ambiguous; use ORGANIZER_HUB_DIAGNOSTIC_SKIP_BYOC=1',
    );
  }
  const explicitlyRequired = env.ORGANIZER_HUB_REQUIRE_BYOC === '1';
  const skipped = env.ORGANIZER_HUB_DIAGNOSTIC_SKIP_BYOC === '1';
  if (explicitlyRequired && skipped) {
    throw new Error(
      'ORGANIZER_HUB_REQUIRE_BYOC=1 conflicts with ORGANIZER_HUB_DIAGNOSTIC_SKIP_BYOC=1',
    );
  }
  return Object.freeze({ required: !skipped, skipped });
}

export function normalizeOrigin(value, label = 'origin') {
  const text = String(value || '').trim().replace(/\/+$/, '');
  if (!/^https?:\/\/[^/?#]+$/i.test(text) || /[\s@]/.test(text.replace(/^https?:\/\//i, ''))) {
    throw new Error(`${label} must be a credential-free HTTP(S) origin without a path`);
  }
  return text;
}

function privateIpv4(host) {
  const octets = host.split('.').map(Number);
  if (octets.length !== 4 || octets.some((part) => !Number.isInteger(part) || part < 0 || part > 255)) {
    return false;
  }
  return (
    octets[0] === 10 ||
    octets[0] === 127 ||
    (octets[0] === 172 && octets[1] >= 16 && octets[1] <= 31) ||
    (octets[0] === 192 && octets[1] === 168)
  );
}

export function isDisposableOrigin(value) {
  const origin = normalizeOrigin(value);
  const host = new URL(origin).hostname.replace(/^\[|\]$/g, '').toLowerCase();
  return host === 'localhost' || host === '::1' || privateIpv4(host);
}

export function organizerWebTargets(value, fallback) {
  const raw = String(value || '').trim();
  let entries;
  if (!raw) {
    entries = [fallback];
  } else if (raw.startsWith('[')) {
    try {
      entries = JSON.parse(raw);
    } catch (error) {
      throw new Error(`ORGANIZER_HUB_WEB_TARGETS is not valid JSON: ${error.message}`);
    }
  } else {
    entries = raw.split(',');
  }
  if (!Array.isArray(entries) || entries.length < 2) {
    throw new Error('ORGANIZER_HUB_WEB_TARGETS must contain at least two direct web origins');
  }
  const targets = entries.map((entry, index) => normalizeOrigin(entry, `web target[${index}]`));
  if (new Set(targets).size !== targets.length) {
    throw new Error('ORGANIZER_HUB_WEB_TARGETS must contain distinct direct web origins');
  }
  const expectedAdmin = normalizeOrigin(fallback, 'admin target');
  if (!targets.includes(expectedAdmin)) {
    throw new Error('ORGANIZER_HUB_WEB_TARGETS must include ORGANIZER_HUB_ADMIN_TARGET');
  }
  for (const target of targets) {
    if (!isDisposableOrigin(target)) {
      throw new Error(`web target ${target} must be loopback or RFC1918`);
    }
  }
  return Object.freeze(targets);
}

export function assertOrganizerHubAcknowledgements(environment, topology) {
  const env = environment || {};
  const expected = {
    CONFIRM_ORGANIZER_HUB_ADMIN_TARGET: normalizeOrigin(topology.adminTarget, 'admin target'),
    CONFIRM_ORGANIZER_HUB_EXEC_TARGET: normalizeOrigin(topology.execTarget, 'exec target'),
    CONFIRM_ORGANIZER_HUB_WEB_TARGETS: (topology.webTargets || [])
      .map((target, index) => normalizeOrigin(target, `web target[${index}]`))
      .join(','),
    CONFIRM_ORGANIZER_HUB_ADMIN_CONTAINER: String(topology.adminContainer || '').trim(),
    CONFIRM_ORGANIZER_HUB_EXEC_CONTAINER: String(topology.execContainer || '').trim(),
    CONFIRM_ORGANIZER_HUB_WEB_CONTAINERS: (topology.webContainers || [])
      .map((container) => String(container).trim())
      .join(','),
    CONFIRM_ORGANIZER_HUB_PG_CONTAINER: String(topology.pgContainer || '').trim(),
    CONFIRM_ORGANIZER_HUB_REDIS_CONTAINER: String(topology.redisContainer || '').trim(),
    CONFIRM_ORGANIZER_HUB_COMPOSE_PROJECT: String(topology.composeProject || '').trim(),
    CONFIRM_ORGANIZER_HUB_NETWORK: String(topology.network || '').trim(),
    CONFIRM_ORGANIZER_HUB_AD_NETWORK: String(topology.adNetwork || '').trim(),
  };
  if (env.ORGANIZER_HUBS_DISPOSABLE !== '1') {
    throw new Error('set ORGANIZER_HUBS_DISPOSABLE=1 for the destructive organizer-hub scenario');
  }
  for (const [key, value] of Object.entries(expected)) {
    if (!value || String(env[key] || '').trim() !== value) {
      throw new Error(`${key} must exactly acknowledge ${value || '<missing topology value>'}`);
    }
  }
  if (!isDisposableOrigin(expected.CONFIRM_ORGANIZER_HUB_ADMIN_TARGET)) {
    throw new Error('organizer admin hub target must be loopback or RFC1918');
  }
  if (!isDisposableOrigin(expected.CONFIRM_ORGANIZER_HUB_EXEC_TARGET)) {
    throw new Error('organizer exec hub target must be loopback or RFC1918');
  }
  return expected;
}

function environmentMap(entries) {
  return new Map((entries || []).map((entry) => {
    const text = String(entry);
    const separator = text.indexOf('=');
    return separator < 0 ? [text, ''] : [text.slice(0, separator), text.slice(separator + 1)];
  }));
}

function targetMatchesContainer(target, container) {
  const url = new URL(normalizeOrigin(target));
  const host = url.hostname.replace(/^\[|\]$/g, '');
  if ((container.addresses || []).includes(host)) return true;
  if (!['127.0.0.1', 'localhost', '::1'].includes(host.toLowerCase())) return false;
  const port = Number(url.port || (url.protocol === 'https:' ? 443 : 80));
  return (container.hostPorts || []).includes(port);
}

export function assertDisposableOrganizerTopology({
  adminTarget,
  execTarget,
  composeProject,
  marker,
  admin,
  exec,
  webReplicas,
  postgres,
  redis,
}) {
  const expectedMarker = String(marker || '').trim();
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]{7,127}$/.test(expectedMarker)) {
    throw new Error('ADMIN_LIFECYCLE_STACK_MARKER must name the dedicated server-side disposable marker');
  }
  if (!admin || !exec || !postgres || !redis) {
    throw new Error('admin, exec, PostgreSQL, and Redis inspections are required');
  }
  for (const [label, container] of [
    ['admin', admin],
    ['exec', exec],
    ['PostgreSQL', postgres],
    ['Redis', redis],
  ]) {
    if (container.project !== composeProject) {
      throw new Error(`${label} container ${container.name} belongs to ${container.project || '<none>'}, not ${composeProject}`);
    }
    const markers = (container.environment || []).filter((entry) =>
      String(entry).startsWith('RSCTF_ADMIN_LIFECYCLE_MARKER='));
    if (
      markers.length !== 1 ||
      markers[0] !== `RSCTF_ADMIN_LIFECYCLE_MARKER=${expectedMarker}`
    ) {
      throw new Error(`${label} container ${container.name} does not carry the one exact disposable marker`);
    }
  }
  const adminEnvironment = environmentMap(admin.environment);
  const execEnvironment = environmentMap(exec.environment);
  const adminRole = adminEnvironment.get('RSCTF_ROLE') || adminEnvironment.get('RSCTF_RUNTIME_ROLE') || 'all';
  const execRole = execEnvironment.get('RSCTF_ROLE') || execEnvironment.get('RSCTF_RUNTIME_ROLE') || 'all';
  if (adminRole !== 'web') {
    throw new Error(`admin hub container role must be web, got ${adminRole}`);
  }
  if (admin.service !== 'rsctf') {
    throw new Error(`admin hub container must be the rsctf Compose service, got ${admin.service || '<none>'}`);
  }
  if (!['control', 'network'].includes(execRole)) {
    throw new Error(`containerExec owner role must be control or network, got ${execRole}`);
  }
  if (!['rsctf-control', 'rsctf-network'].includes(exec.service)) {
    throw new Error(
      `containerExec owner must be an rsctf-control/rsctf-network Compose service, got ${exec.service || '<none>'}`,
    );
  }
  if (!targetMatchesContainer(adminTarget, admin)) {
    throw new Error(`admin target is not bound directly to declared container ${admin.name}`);
  }
  if (!targetMatchesContainer(execTarget, exec)) {
    throw new Error(`exec target is not bound directly to declared container ${exec.name}`);
  }
  if (!Array.isArray(webReplicas) || webReplicas.length < 2) {
    throw new Error('at least two direct web replica inspections are required');
  }
  if (
    new Set(webReplicas.map(({ target }) => normalizeOrigin(target))).size !== webReplicas.length ||
    new Set(webReplicas.map(({ container }) => container?.name)).size !== webReplicas.length
  ) {
    throw new Error('direct web replica targets and containers must map one-to-one');
  }
  for (const [index, { target, container }] of webReplicas.entries()) {
    if (!container || container.project !== composeProject) {
      throw new Error(`web replica[${index}] is not in Compose project ${composeProject}`);
    }
    const replicaEnvironment = environmentMap(container.environment);
    const role = replicaEnvironment.get('RSCTF_ROLE') ||
      replicaEnvironment.get('RSCTF_RUNTIME_ROLE') || 'all';
    if (role !== 'web') throw new Error(`web replica[${index}] role must be web, got ${role}`);
    if (container.service !== 'rsctf') {
      throw new Error(`web replica[${index}] must be the rsctf Compose service`);
    }
    const markers = (container.environment || []).filter((entry) =>
      String(entry).startsWith('RSCTF_ADMIN_LIFECYCLE_MARKER='));
    if (markers.length !== 1 || markers[0] !== `RSCTF_ADMIN_LIFECYCLE_MARKER=${expectedMarker}`) {
      throw new Error(`web replica[${index}] does not carry the one exact disposable marker`);
    }
    if (!targetMatchesContainer(target, container)) {
      throw new Error(`web target[${index}] is not bound directly to declared container ${container.name}`);
    }
  }
  const socket = (exec.mounts || []).find((mount) => mount.destination === '/var/run/docker.sock');
  if (!socket || socket.source !== '/var/run/docker.sock' || socket.rw !== true) {
    throw new Error(`exec container ${exec.name} needs the exact writable /var/run/docker.sock mount`);
  }
  if (postgres.service !== 'db' && postgres.service !== 'postgres') {
    throw new Error(`declared PostgreSQL container has unexpected compose service ${postgres.service || '<none>'}`);
  }
  if (redis.service !== 'redis') {
    throw new Error(`declared Redis container has unexpected compose service ${redis.service || '<none>'}`);
  }
  return { adminRole, execRole, composeProject };
}

export function hubFrame(value) {
  return `${JSON.stringify(value)}${SIGNALR_RECORD_SEPARATOR}`;
}

export function consumeHubFrames(state, chunk) {
  const source = `${state?.remainder || ''}${String(chunk)}`;
  const parts = source.split(SIGNALR_RECORD_SEPARATOR);
  const remainder = parts.pop() || '';
  const frames = parts
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => JSON.parse(part));
  return { frames, remainder };
}

export function assertNegotiateContract(response, label = 'SignalR negotiate') {
  if (response?.status !== 200) throw new Error(`${label} returned ${response?.status}`);
  const model = response.json;
  if (
    model?.negotiateVersion !== 1 ||
    typeof model.connectionId !== 'string' ||
    model.connectionId.length === 0 ||
    model.connectionToken !== model.connectionId ||
    model.availableTransports?.length !== 1 ||
    model.availableTransports[0]?.transport !== 'WebSockets' ||
    !model.availableTransports[0]?.transferFormats?.includes('Text')
  ) {
    throw new Error(`${label} returned an invalid WebSocket transport contract`);
  }
  return model.connectionToken;
}

export function assertReceivedLog(frame, { message, userName }) {
  const payload = frame?.type === 1 && frame.target === 'ReceivedLog' ? frame.arguments?.[0] : null;
  if (!payload || payload.msg !== message || payload.name !== userName) {
    throw new Error('ReceivedLog payload did not match the exact semantic audit action');
  }
  if (
    payload.level !== 'Information' ||
    payload.status !== 'Success' ||
    !Number.isFinite(payload.time) ||
    payload.time <= 0
  ) {
    throw new Error('ReceivedLog payload is missing its level, status, or millisecond timestamp');
  }
  return payload;
}

export function decodeReceive(frame, sessionId) {
  if (frame?.type !== 1 || frame.target !== 'Receive' || frame.arguments?.[0] !== sessionId) return null;
  try {
    return Buffer.from(String(frame.arguments[1] || ''), 'base64').toString('utf8');
  } catch {
    return null;
  }
}

export function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) throw new Error(`${label} must be a positive integer`);
  return parsed;
}

export function scopedContainerExecPath(gameId) {
  return `/hub/containerExec/games/${positiveInteger(gameId, 'scoped exec game id')}`;
}
