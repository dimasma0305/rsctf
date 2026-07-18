// Distributed load clients: one real WireGuard+k6 container per seeded team.
// Public HTTPS reaches Traefik from distinct bridge addresses while challenge
// traffic traverses each team's authenticated WireGuard peer.
import {
  chmodSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  realpathSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs';
import { execFileSync } from 'node:child_process';
import { basename, dirname, resolve } from 'node:path';
import { api } from './applib.mjs';
import { TARGET, docker, mintJwt, sleep, RSCTF } from './lib.mjs';
import {
  dockerLabelArgs,
  dockerTeamClientFilterArgs,
  normalizeTeamClientScope,
  ownsTeamClient,
  selectTeamClientOwnershipRecord,
  sameTeamClientScope,
  teamClientLabelKeys,
  teamClientLabels,
  teamClientOwner,
} from './fleet-ownership.js';
import { buildJeopardyCatalog, buildPlayerProfiles } from './player-model.js';
import { runInterruptibleStep } from './process-control.mjs';
import {
  MAX_TEAM_RUNNER_LOG_BYTES,
  TEAM_RUNNER_LOG_FILENAME,
} from './team-evidence.js';
import { ensureValidTeamToken } from './team-token-readiness.js';

const VPN_CLIENT_ROOT_PREFIX = 'rsctf-load-vpn-clients';
const DEFAULT_EVIDENCE_DIR = '/tmp/rsctf-team-event-evidence';
const TEAM_CLIENT_OWNERSHIP_SCHEMA = 1;
const DOCKER_STATUS_MAX_BUFFER_BYTES = 4 * 1024 * 1024;
const POSIX_FILE_LIMIT_BLOCK_BYTES = 512;
export const TEAM_RUNNER_FILE_LIMIT_BLOCKS =
  MAX_TEAM_RUNNER_LOG_BYTES / POSIX_FILE_LIMIT_BLOCK_BYTES;

if (!Number.isSafeInteger(TEAM_RUNNER_FILE_LIMIT_BLOCKS)) {
  throw new Error('team runner log limit must use an exact number of POSIX 512-byte blocks');
}

export function teamRunnerCommand(runnerLogFilename = TEAM_RUNNER_LOG_FILENAME) {
  if (
    typeof runnerLogFilename !== 'string' ||
    !/^[A-Za-z0-9][A-Za-z0-9._-]*\.log$/.test(runnerLogFilename) ||
    runnerLogFilename.includes('..')
  ) {
    throw new Error('team runner log must be a safe .log filename');
  }
  return (
    'ip link add wg0 type wireguard && wg setconf wg0 /config/wg.conf && ' +
    'ip address add "$(cat /config/address)"/32 dev wg0 && ip link set wg0 up && ' +
    'while read route; do ip route replace "$route" dev wg0; done < /config/routes && ' +
    'start_at="$(cat /config/start-at)" && while [ "$(date +%s)" -lt "$start_at" ]; ' +
    `do sleep 1; done && ulimit -c 0 && ulimit -f ${TEAM_RUNNER_FILE_LIMIT_BLOCKS} && ` +
    'exec /k6 run ' +
    `--log-output=file=/evidence/${runnerLogFilename} /team.js`
  );
}

function mustDocker(result, what) {
  if (result.status !== 0) {
    throw new Error(`${what}: ${(result.stderr || result.error?.message || 'docker command failed').trim()}`);
  }
  return result;
}

function distinctPositiveIntegers(values, label) {
  if (
    !Array.isArray(values) ||
    values.some((value) => !Number.isSafeInteger(Number(value)) || Number(value) <= 0) ||
    new Set(values.map(Number)).size !== values.length
  ) {
    throw new Error(`${label} must contain distinct positive integers`);
  }
  return values.map(Number);
}

function teamClientRunId(state) {
  if (typeof state?.competitionRunId === 'string' && state.competitionRunId) {
    return state.competitionRunId;
  }
  const createdAtMs = Number(state?.createdAtMs);
  if (!Number.isSafeInteger(createdAtMs) || createdAtMs <= 0) {
    throw new Error('team-client ownership requires a competition run id or event creation timestamp');
  }
  return `capacity-${createdAtMs}`;
}

function ownershipFromScope(scope, containerIds = []) {
  if (
    !Array.isArray(containerIds) ||
    new Set(containerIds).size !== containerIds.length ||
    containerIds.some((id) => typeof id !== 'string' || !/^[0-9a-f]{64}$/.test(id)) ||
    containerIds.length > scope.participationIds.length
  ) {
    throw new Error('team-client ownership contains invalid or duplicate container IDs');
  }
  return Object.freeze({
    schemaVersion: TEAM_CLIENT_OWNERSHIP_SCHEMA,
    owner: teamClientOwner,
    gameId: scope.gameId,
    runId: scope.runId,
    participationIds: Object.freeze([...scope.participationIds]),
    containerIds: Object.freeze([...containerIds]),
  });
}

function normalizeOwnership(value) {
  const record = selectTeamClientOwnershipRecord(value);
  if (record == null) return null;
  if (
    Number(record.schemaVersion) !== TEAM_CLIENT_OWNERSHIP_SCHEMA ||
    record.owner !== teamClientOwner
  ) {
    throw new Error('team-client cleanup requires an exact versioned ownership record');
  }
  const scope = normalizeTeamClientScope(record.gameId, record.runId, record.participationIds);
  return ownershipFromScope(scope, record.containerIds ?? []);
}

function scopeForOwnership(ownership) {
  return normalizeTeamClientScope(
    ownership.gameId,
    ownership.runId,
    ownership.participationIds,
  );
}

export function vpnTeamClientOwnership(state, count) {
  const teamCount = Number(count);
  if (!Number.isSafeInteger(teamCount) || teamCount < 2) {
    throw new Error(`team-client ownership requires at least two teams (got ${count})`);
  }
  const participationIds = distinctPositiveIntegers(
    state?.adPartIds?.slice(0, teamCount),
    'team-client participation ids',
  );
  if (participationIds.length !== teamCount) {
    throw new Error(`team-client ownership expected ${teamCount} participations`);
  }
  const scope = normalizeTeamClientScope(state.mixGame, teamClientRunId(state), participationIds);
  return ownershipFromScope(scope);
}

function teamClientRoot(scope) {
  const directory = resolve(`/tmp/${VPN_CLIENT_ROOT_PREFIX}-g${scope.gameId}-${scope.runId}`);
  if (
    dirname(directory) !== '/tmp' ||
    !new RegExp(`^${VPN_CLIENT_ROOT_PREFIX}-g\\d+-[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`).test(
      basename(directory),
    )
  ) {
    throw new Error(`invalid team-client configuration directory ${directory}`);
  }
  return directory;
}

function teamClientName(scope, index) {
  return `lcteam_${scope.runId}_${String(index).padStart(3, '0')}`;
}

function listOwnedTeamClientIds(scope) {
  const listed = mustDocker(
    docker(['ps', '--no-trunc', '-aq', ...dockerTeamClientFilterArgs(scope)]),
    'list run-owned VPN team clients',
  );
  const ids = listed.stdout.trim().split('\n').filter(Boolean);
  if (new Set(ids).size !== ids.length || ids.some((id) => !/^[0-9a-f]{64}$/.test(id))) {
    throw new Error('team-client discovery returned invalid or duplicate container identities');
  }
  return ids;
}

function inspectOwnedTeamClients(value, { requireComplete = false } = {}) {
  const ownership = normalizeOwnership(value);
  if (!ownership) throw new Error('team-client inspection requires an ownership record');
  const scope = scopeForOwnership(ownership);
  const format = [
    '{{.ID}}',
    '{{.Names}}',
    '{{.State}}',
    '{{.Status}}',
    `{{.Label "${teamClientLabelKeys.owner}"}}`,
    `{{.Label "${teamClientLabelKeys.game}"}}`,
    `{{.Label "${teamClientLabelKeys.run}"}}`,
    `{{.Label "${teamClientLabelKeys.role}"}}`,
    `{{.Label "${teamClientLabelKeys.participation}"}}`,
    `{{.Label "${teamClientLabelKeys.index}"}}`,
  ].join('|');
  const listed = mustDocker(
    docker([
      'ps',
      '--no-trunc',
      '-a',
      ...dockerTeamClientFilterArgs(scope),
      '--format',
      format,
    ], { maxBuffer: DOCKER_STATUS_MAX_BUFFER_BYTES }),
    'inspect run-owned VPN team clients',
  );
  const records = listed.stdout.trim().split('\n').filter(Boolean).map((line) => line.split('|'));
  if (records.some((fields) => fields.length !== 10)) {
    throw new Error('Docker returned malformed team-client status data');
  }
  const ids = records.map(([id]) => id);
  if (new Set(ids).size !== ids.length || ids.some((id) => !/^[0-9a-f]{64}$/.test(id))) {
    throw new Error('team-client inspection returned invalid or duplicate container identities');
  }
  const captured = new Set(ownership.containerIds);
  if (captured.size && ids.some((id) => !captured.has(id))) {
    throw new Error('team-client scope contains a container not present in the captured ownership set');
  }
  if (requireComplete && (captured.size !== scope.participationIds.length || ids.length !== captured.size)) {
    throw new Error(
      `team-client ownership is incomplete (${ids.length}/${scope.participationIds.length} discovered)`,
    );
  }
  if (!ids.length) return [];
  const indexes = new Set();
  const participations = new Set();
  return records.map(([
    id,
    name,
    state,
    status,
    owner,
    game,
    run,
    role,
    participation,
    rawIndex,
  ]) => {
    const labels = {
      [teamClientLabelKeys.owner]: owner,
      [teamClientLabelKeys.game]: game,
      [teamClientLabelKeys.run]: run,
      [teamClientLabelKeys.role]: role,
      [teamClientLabelKeys.participation]: participation,
      [teamClientLabelKeys.index]: rawIndex,
    };
    const index = Number(rawIndex);
    const participationId = Number(participation);
    if (
      !ownsTeamClient(labels, scope, participationId, index) ||
      name !== teamClientName(scope, index) ||
      indexes.has(index) ||
      participations.has(participationId)
    ) {
      throw new Error(`team-client ${String(id || 'unknown').slice(0, 12)} failed ownership validation`);
    }
    indexes.add(index);
    participations.add(participationId);
    return {
      id,
      index,
      participationId,
      state,
      exitCode: state === 'exited' ? Number(status.match(/^Exited \((\d+)\)/)?.[1]) : null,
    };
  });
}

function safeEvidenceDirectory(value) {
  if (typeof value !== 'string' || value === '') {
    throw new Error('TEAM_EVIDENCE_DIR must be a non-empty absolute path');
  }
  const directory = resolve(value);
  const name = basename(directory);
  if (
    dirname(directory) !== '/tmp' ||
    (name !== basename(DEFAULT_EVIDENCE_DIR) && !/^rsctf-team-event-evidence-[A-Za-z0-9][A-Za-z0-9._-]*$/.test(name))
  ) {
    throw new Error(`TEAM_EVIDENCE_DIR must be ${DEFAULT_EVIDENCE_DIR} or a suffixed sibling under /tmp`);
  }
  if (realpathSync('/tmp') !== '/tmp') {
    throw new Error('TEAM_EVIDENCE_DIR parent /tmp must not resolve through a symlink');
  }
  if (existsSync(directory)) validateEvidenceDirectory(directory);
  return directory;
}

function competitiveRunBinding(state, requestedSeed, teamCount) {
  if (
    Number(state.competitionModelVersion) !== 2 ||
    typeof state.competitionRunId !== 'string' ||
    !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(
      state.competitionRunId
    ) ||
    typeof state.competitionSeed !== 'string' ||
    state.competitionSeed !== requestedSeed ||
    !Number.isSafeInteger(Number(state.createdAtMs)) ||
    Number(state.createdAtMs) <= 0
  ) {
    throw new Error('competitive team clients require a model-v2 provision bound to the requested seed');
  }
  if (
    !Array.isArray(state.jeoUsers) ||
    state.jeoUsers.length !== teamCount ||
    !Array.isArray(state.jeoPartIds) ||
    state.jeoPartIds.length !== teamCount ||
    !Array.isArray(state.jeopardyCatalog) ||
    state.jeopardyCatalog.length < 2
  ) {
    throw new Error('competitive team clients require a complete Jeopardy roster and challenge catalog');
  }
  return Object.freeze({
    runId: state.competitionRunId,
    modelVersion: 2,
    seed: state.competitionSeed,
    eventCreatedAtMs: Number(state.createdAtMs),
  });
}

function prepareEvidenceDirectory(requestedDirectory, runBinding, teamCount) {
  const baseDirectory = safeEvidenceDirectory(requestedDirectory);
  const directory = runBinding
    ? safeEvidenceDirectory(
        baseDirectory.endsWith(`-${runBinding.runId}`)
          ? baseDirectory
          : `${baseDirectory}-${runBinding.runId}`
      )
    : baseDirectory;

  if (runBinding && existsSync(directory)) {
    throw new Error(`competitive evidence directory already exists; refusing to reuse ${directory}`);
  }
  mkdirSync(directory, { recursive: !runBinding, mode: 0o750 });
  validateEvidenceDirectory(directory);
  chmodSync(directory, 0o750);

  if (runBinding) {
    for (let index = 0; index < teamCount; index++) {
      const teamDirectory = `${directory}/team-${String(index).padStart(3, '0')}`;
      mkdirSync(teamDirectory, { mode: 0o750 });
      validateEvidenceDirectory(teamDirectory);
    }
  } else {
    for (const file of readdirSync(directory)) {
      if (/^team-\d{3}(?:\.json|\.runner\.log)$/.test(file)) {
        unlinkSync(`${directory}/${file}`);
      }
    }
  }
  return directory;
}

function competitiveJeopardyCatalog(state) {
  const catalogById = new Map(
    state.jeopardyCatalog.map((challenge) => [Number(challenge.challengeId), challenge])
  );
  if (
    catalogById.size !== state.jeopardyCatalog.length ||
    [...catalogById.keys()].some((id) => !Number.isSafeInteger(id) || id < 1)
  ) {
    throw new Error('competitive Jeopardy catalog contains duplicate or invalid challenge IDs');
  }
  return Object.freeze(
    buildJeopardyCatalog(state.jeopardyCatalog).map((publicChallenge) => {
      const challenge = catalogById.get(publicChallenge.challengeId);
      if (
        !challenge ||
        typeof challenge.flag !== 'string' ||
        challenge.flag === '' ||
        challenge.kind !== publicChallenge.kind
      ) {
        throw new Error(
          `competitive Jeopardy catalog contains an incomplete challenge ${publicChallenge.challengeId}`
        );
      }
      return Object.freeze({
        ...publicChallenge,
        flag: challenge.flag,
        attachmentPath: challenge.attachmentPath ?? null,
      });
    })
  );
}

function validateEvidenceDirectory(directory) {
  const metadata = lstatSync(directory);
  if (metadata.isSymbolicLink() || !metadata.isDirectory() || realpathSync(directory) !== directory) {
    throw new Error(`TEAM_EVIDENCE_DIR must be a real directory, not a symlink (${directory})`);
  }
  if (typeof process.getuid === 'function' && metadata.uid !== process.getuid()) {
    throw new Error(`TEAM_EVIDENCE_DIR must be owned by uid ${process.getuid()} (${directory})`);
  }
}

function configValue(config, name) {
  return config.match(new RegExp(`^${name}\\s*=\\s*(.+)$`, 'm'))?.[1].trim() || '';
}

function containerForComposeService(service) {
  const result = docker(['ps', '--format', '{{.Names}}|{{.Label "com.docker.compose.service"}}']);
  if (result.status !== 0) throw new Error(`discover ${service} container: ${result.stderr.trim()}`);
  return result.stdout
    .trim()
    .split('\n')
    .map((row) => row.split('|'))
    .find(([, label]) => label === service)?.[0];
}

function containerNetworkIp(container, network) {
  return mustDocker(
    docker(['inspect', container, '--format', `{{(index .NetworkSettings.Networks "${network}").IPAddress}}`]),
    `discover ${container} address on ${network}`
  ).stdout.trim();
}

function ipv4Number(value) {
  const octets = value.split('.').map(Number);
  if (octets.length !== 4 || octets.some((octet) => !Number.isInteger(octet) || octet < 0 || octet > 255)) {
    return null;
  }
  return octets.reduce((number, octet) => number * 256 + octet, 0) >>> 0;
}

function broadCidrContains(address, candidate) {
  const match = candidate.match(/^(\d{1,3}(?:\.\d{1,3}){3})\/(\d{1,2})$/);
  const addressNumber = ipv4Number(address);
  const networkNumber = match ? ipv4Number(match[1]) : null;
  const prefix = match ? Number(match[2]) : 33;
  if (addressNumber === null || networkNumber === null || prefix < 0 || prefix >= 32) return false;
  const mask = prefix === 0 ? 0 : (0xffffffff << (32 - prefix)) >>> 0;
  return (addressNumber & mask) === (networkNumber & mask);
}

async function downloadVpnConfig(gameId, userId, securityStamp, step) {
  const baseUrl = process.env.VPN_CONFIG_TARGET || 'http://127.0.0.1:8080';
  for (let attempt = 0; attempt < 20; attempt++) {
    const response = await step(() =>
      api('GET', `/api/Game/${gameId}/Ad/Vpn/Config`, {
        jwt: mintJwt(userId, securityStamp, 1),
        ip: '127.0.0.1',
        baseUrl,
        timeoutMs: 30_000,
      })
    );
    if (response.status === 200) return response.text;
    if (response.status !== 429) {
      throw new Error(`download VPN config for ${userId} → ${response.status} ${response.text.slice(0, 120)}`);
    }
    await step(() => sleep(5_000));
  }
  throw new Error(`download VPN config for ${userId} remained rate-limited`);
}

async function rotateAdToken(gameId, userId, securityStamp, step) {
  const baseUrl = process.env.VPN_CONFIG_TARGET || 'http://127.0.0.1:8080';
  for (let attempt = 0; attempt < 20; attempt++) {
    const response = await step(() =>
      api('POST', `/api/Game/${gameId}/Ad/Token`, {
        jwt: mintJwt(userId, securityStamp, 1),
        ip: '127.0.0.1',
        baseUrl,
        timeoutMs: 30_000,
      })
    );
    if (
      response.status === 200 &&
      typeof response.json?.token === 'string' &&
      /^ad_[A-Za-z0-9_-]{43}$/.test(response.json.token)
    ) {
      return response.json.token;
    }
    if (response.status !== 429) {
      throw new Error(`rotate A&D token for ${userId} → ${response.status} ${response.text.slice(0, 120)}`);
    }
    await step(() => sleep(5_000));
  }
  throw new Error(`rotate A&D token for ${userId} remained rate-limited`);
}

async function ensureAdToken(gameId, userId, securityStamp, token, step) {
  const baseUrl = process.env.VPN_CONFIG_TARGET || 'http://127.0.0.1:8080';
  return ensureValidTeamToken({
    token,
    probe: (candidate) =>
      step(() => api('GET', `/api/Game/${gameId}/Ad/Targets`, {
        jwt: candidate,
        ip: '127.0.0.1',
        baseUrl,
        timeoutMs: 30_000,
      })),
    rotate: () => rotateAdToken(gameId, userId, securityStamp, step),
    wait: (milliseconds) => step(() => sleep(milliseconds)),
  });
}

export async function startVpnTeamClients({
  state,
  ownership: requestedOwnership,
  listeners,
  count,
  duration,
  thinkSeconds = 5,
  evidenceDir,
  startDelaySeconds = 90,
  realisticCompetition = false,
  competitionSeed = 'rsctf-competitive-v2',
  defenseKeys = [],
  throwIfInterrupted = () => {},
}) {
  if (typeof throwIfInterrupted !== 'function') {
    throw new TypeError('throwIfInterrupted must be a function');
  }
  const step = (operation) => runInterruptibleStep(throwIfInterrupted, operation);
  throwIfInterrupted();
  const teamCount = Number(count);
  if (
    !Number.isSafeInteger(teamCount) ||
    teamCount < 2 ||
    teamCount !== state.adPartIds.length ||
    listeners.length !== teamCount ||
    state.adTeamIds.length !== teamCount ||
    state.adUsers.length !== teamCount
  ) {
    throw new Error(`invalid distributed team-client cohort size ${count}`);
  }
  distinctPositiveIntegers(state.adPartIds, 'A&D participation ids');
  distinctPositiveIntegers(state.adTeamIds, 'A&D team ids');
  if (new Set(state.adUsers).size !== state.adUsers.length) {
    throw new Error('A&D client users must be distinct');
  }
  const ownership = normalizeOwnership(
    requestedOwnership ?? vpnTeamClientOwnership(state, teamCount),
  );
  const scope = scopeForOwnership(ownership);
  const expectedScope = normalizeTeamClientScope(
    state.mixGame,
    teamClientRunId(state),
    state.adPartIds.map(Number),
  );
  if (!sameTeamClientScope(scope, expectedScope)) {
    throw new Error('team-client ownership does not match the requested event cohort');
  }
  const clientRoot = teamClientRoot(scope);
  if (!Number.isFinite(Number(thinkSeconds)) || Number(thinkSeconds) < 4) {
    throw new Error(`TEAM_THINK_SECONDS must be at least 4 (got ${thinkSeconds})`);
  }
  if (!Number.isSafeInteger(Number(startDelaySeconds)) || Number(startDelaySeconds) < 30) {
    throw new Error(`TEAM_START_DELAY_SECONDS must be an integer >= 30 (got ${startDelaySeconds})`);
  }
  if (
    realisticCompetition &&
    (defenseKeys.length !== teamCount || defenseKeys.some((key) => typeof key !== 'string' || !key))
  ) {
    throw new Error(`competitive team clients require ${teamCount} fixture defense capabilities`);
  }
  const runBinding = realisticCompetition
    ? competitiveRunBinding(state, competitionSeed, teamCount)
    : null;
  const profiles = realisticCompetition
    ? buildPlayerProfiles(teamCount, runBinding.seed)
    : null;
  const jeopardyCatalog = realisticCompetition
    ? competitiveJeopardyCatalog(state)
    : null;
  const outputDirectory = await step(() => prepareEvidenceDirectory(evidenceDir, runBinding, teamCount));
  const eventTarget = process.env.TARGET || TARGET;
  if (new URL(eventTarget).protocol !== 'https:') {
    throw new Error(`distributed team clients require an HTTPS TARGET (got ${eventTarget})`);
  }

  await step(() => teardownVpnTeamClients(ownership));
  await step(() => rmSync(clientRoot, { recursive: true, force: true }));
  await step(() => mkdirSync(clientRoot, { recursive: true, mode: 0o700 }));

  const traefik =
    process.env.TRAEFIK_CONTAINER || (await step(() => containerForComposeService('traefik')));
  if (!traefik) throw new Error('could not discover the Traefik container');
  const proxyIp = await step(() => containerNetworkIp(traefik, 'traefik'));
  const rsctfEnvironment = await step(() =>
    mustDocker(
      docker(['inspect', RSCTF, '--format', '{{range .Config.Env}}{{println .}}{{end}}']),
      'inspect rsctf trusted proxy configuration'
    ).stdout
  );
  const trustedProxyCidrs =
    rsctfEnvironment
      .split('\n')
      .find((line) => line.startsWith('RSCTF_TRUSTED_PROXY_CIDRS='))
      ?.slice('RSCTF_TRUSTED_PROXY_CIDRS='.length)
      .split(',')
      .map((value) => value.trim())
      .filter(Boolean) || [];
  if (!trustedProxyCidrs.includes(proxyIp) && !trustedProxyCidrs.includes(`${proxyIp}/32`)) {
    throw new Error(
      `distributed clients require the exact Traefik address ${proxyIp}/32 in ` +
        'RSCTF_TRUSTED_PROXY_CIDRS; broad or missing proxy trust invalidates per-team source evidence'
    );
  }
  const broadTrust = trustedProxyCidrs.find((candidate) => broadCidrContains(proxyIp, candidate));
  if (broadTrust) {
    throw new Error(
      `RSCTF_TRUSTED_PROXY_CIDRS entry ${broadTrust} broadly contains Traefik ${proxyIp}; ` +
        'distributed source-attribution evidence requires only the exact proxy /32 for that address'
    );
  }
  const vpnEndpointIp = await step(() => containerNetworkIp(RSCTF, 'traefik'));
  const image = await step(() =>
    mustDocker(
      docker(['inspect', RSCTF, '--format', '{{.Config.Image}}']),
      'discover rsctf image for VPN clients'
    ).stdout.trim()
  );
  const k6Binary = process.env.K6_BIN || '/usr/local/bin/k6';
  if (!existsSync(k6Binary)) throw new Error(`k6 binary does not exist at ${k6Binary}`);
  const script = new URL('./k6/team-event.js', import.meta.url).pathname;
  const playerModel = new URL('./player-model.js', import.meta.url).pathname;
  const teamEvidence = new URL('./team-evidence.js', import.meta.url).pathname;
  const configs = [];
  const bots = [];
  const peerPublicKeys = [];

  for (let index = 0; index < teamCount; index++) {
    throwIfInterrupted();
    const userId = state.adUsers[index];
    const jwt = mintJwt(userId, state.userStamps[userId], 1);
    const adToken = await rotateAdToken(state.mixGame, userId, state.userStamps[userId], step);
    const wireguard = await downloadVpnConfig(state.mixGame, userId, state.userStamps[userId], step);
    const privateKey = configValue(wireguard, 'PrivateKey');
    const address = configValue(wireguard, 'Address').split('/')[0];
    const publicKey = configValue(wireguard, 'PublicKey');
    const routes = configValue(wireguard, 'AllowedIPs')
      .split(',')
      .map((route) => route.trim())
      .filter(Boolean);
    if (
      !/^[A-Za-z0-9+/]{43}=$/.test(privateKey) ||
      !/^[A-Za-z0-9+/]{43}=$/.test(publicKey) ||
      !/^\d{1,3}(?:\.\d{1,3}){3}$/.test(address) ||
      routes.length < 2
    ) {
      throw new Error(`invalid WireGuard profile returned for team client ${index}`);
    }
    const peerPublicKey = await step(() =>
      execFileSync('wg', ['pubkey'], {
        input: `${privateKey}\n`,
        encoding: 'utf8',
      }).trim()
    );
    if (!/^[A-Za-z0-9+/]{43}=$/.test(peerPublicKey)) {
      throw new Error(`could not derive WireGuard public identity for team client ${index}`);
    }
    peerPublicKeys.push(peerPublicKey);
    const directory = `${clientRoot}/${index}`;
    await step(() => mkdirSync(directory, { recursive: true, mode: 0o700 }));
    const wgConfig = `[Interface]\nPrivateKey = ${privateKey}\n[Peer]\nPublicKey = ${publicKey}\nEndpoint = ${vpnEndpointIp}:51820\nAllowedIPs = ${routes.join(', ')}\nPersistentKeepalive = 5\n`;
    const bot = {
      teamIndex: index,
      teamCount,
      jwt,
      adToken,
      gameId: state.mixGame,
      adChallengeId: state.adChal,
      kothChallengeId: state.kothChal,
      // A client keeps only its own service endpoint for defensive actions.
      // Opponent routes must come from that client's public Ad/Targets poll.
      ownListener: listeners[index],
      // The checker contract currently keys fixture flags by participation id
      // (the value passed as RSCTF_TEAM_ID), not the Teams table id.
      teamIds: state.adPartIds.slice(0, teamCount),
      epochStartRound: state.epochStartRound,
      jeoGame: index < state.jeoUsers.length ? state.jeoGame : null,
      jeoJwt:
        index < state.jeoUsers.length
          ? mintJwt(state.jeoUsers[index], state.userStamps[state.jeoUsers[index]], 1)
          : null,
      jeoChallenges:
        jeopardyCatalog
          ? jeopardyCatalog
          : index < state.jeoUsers.length
          ? Object.entries(state.staticFlags || {}).map(([challengeId, flag]) => ({
              challengeId: Number(challengeId),
              flag,
              kind: 'static',
              unlockProgress: 0,
              downloadProgress: null,
              containerStartProgress: null,
              containerHoldSeconds: null,
              attachmentPath: null,
            }))
          : [],
      target: eventTarget,
      duration,
      thinkSeconds: profiles?.[index].thinkSeconds ?? Number(thinkSeconds),
      realisticCompetition,
      competitionModelVersion: runBinding?.modelVersion ?? 1,
      competitionRunId: runBinding?.runId ?? null,
      competitionSeed: runBinding?.seed ?? null,
      eventCreatedAtMs: runBinding?.eventCreatedAtMs ?? null,
      participationId: realisticCompetition ? state.adPartIds[index] : null,
      profile: profiles?.[index] ?? null,
      defenseKey: realisticCompetition ? defenseKeys[index] : null,
      evidenceFile: realisticCompetition ? 'summary.json' : `team-${String(index).padStart(3, '0')}.json`,
    };
    await step(() => writeFileSync(`${directory}/wg.conf`, wgConfig, { mode: 0o600 }));
    await step(() => writeFileSync(`${directory}/address`, `${address}\n`, { mode: 0o600 }));
    await step(() =>
      writeFileSync(`${directory}/routes`, `${routes.join('\n')}\n`, {
        mode: 0o600,
      })
    );
    for (const file of ['wg.conf', 'address', 'routes']) {
      await step(() => chmodSync(`${directory}/${file}`, 0o600));
    }
    configs.push(directory);
    bots.push(bot);
  }

  // A token can be rotated while the large cohort is being assembled. Probe
  // every final credential immediately before creating the clients and repair
  // only a rejected token, preventing a silent hour of 401 responses.
  for (let index = 0; index < teamCount; index++) {
    throwIfInterrupted();
    const userId = state.adUsers[index];
    bots[index].adToken = await ensureAdToken(
      state.mixGame,
      userId,
      state.userStamps[userId],
      bots[index].adToken,
      step,
    );
    await step(() =>
      writeFileSync(`${configs[index]}/bot.json`, `${JSON.stringify(bots[index])}\n`, {
        mode: 0o600,
      })
    );
    await step(() => chmodSync(`${configs[index]}/bot.json`, 0o600));
  }

  let createdAtSeconds = null;
  let startAtSeconds = null;
  const containerIds = [];
  try {
    for (let index = 0; index < teamCount; index++) {
      const name = teamClientName(scope, index);
      const runnerLogFilename = realisticCompetition
        ? TEAM_RUNNER_LOG_FILENAME
        : `team-${String(index).padStart(3, '0')}.runner.log`;
      const command = teamRunnerCommand(runnerLogFilename);
      const result = await step(() =>
        docker([
          'create',
          '--name',
          name,
          '--network',
          'traefik',
          '--cap-add',
          'NET_ADMIN',
          '--device',
          '/dev/net/tun',
          '--memory',
          process.env.TEAM_CLIENT_MEMORY || '128m',
          '--cpus',
          process.env.TEAM_CLIENT_CPUS || '0.25',
          '--pids-limit',
          '64',
          ...dockerLabelArgs(teamClientLabels(scope, scope.participationIds[index], index)),
          '--add-host',
          `${new URL(eventTarget).hostname}:${proxyIp}`,
          '-v',
          `${configs[index]}:/config:ro`,
          '-v',
          `${k6Binary}:/k6:ro`,
          '-v',
          `${script}:/team.js:ro`,
          '-v',
          `${playerModel}:/player-model.js:ro`,
          '-v',
          `${teamEvidence}:/team-evidence.js:ro`,
          '-v',
          `${
            realisticCompetition
              ? `${outputDirectory}/team-${String(index).padStart(3, '0')}`
              : outputDirectory
          }:/evidence`,
          '--entrypoint',
          'sh',
          image,
          '-lc',
          command,
        ])
      );
      if (result.status !== 0) throw new Error(`create VPN team client ${index}: ${result.stderr.trim()}`);
      const containerId = result.stdout.trim();
      if (!/^[0-9a-f]{64}$/.test(containerId)) {
        throw new Error(`create VPN team client ${index} returned an invalid container identity`);
      }
      containerIds.push(containerId);
    }
    // Start-delay accounting begins only once all clients exist. Creating a
    // 100-container cohort can otherwise consume the barrier on a loaded host,
    // causing early players to run while official scoring is still paused.
    createdAtSeconds = Math.floor(Date.now() / 1000);
    startAtSeconds = createdAtSeconds + Number(startDelaySeconds);
    for (const directory of configs) {
      await step(() => writeFileSync(`${directory}/start-at`, `${startAtSeconds}\n`, { mode: 0o600 }));
      await step(() => chmodSync(`${directory}/start-at`, 0o600));
    }
    const startedOwnership = ownershipFromScope(scope, containerIds);
    await step(() => inspectOwnedTeamClients(startedOwnership, { requireComplete: true }));
    await step(() =>
      mustDocker(docker(['start', ...containerIds]), 'start distributed VPN team clients')
    );
  } catch (error) {
    teardownVpnTeamClients(ownershipFromScope(scope, containerIds));
    throw error;
  }
  return {
    count: teamCount,
    createdAtSeconds,
    startAtSeconds,
    evidenceDir: outputDirectory,
    competitionRunId: runBinding?.runId ?? null,
    competitionModelVersion: runBinding?.modelVersion ?? 1,
    competitionSeed: runBinding?.seed ?? null,
    competitionDuration: runBinding ? String(duration) : null,
    eventCreatedAtMs: runBinding?.eventCreatedAtMs ?? null,
    profiles,
    jeopardyCatalog,
    peerPublicKeys,
    ownership: ownershipFromScope(scope, containerIds),
  };
}

export function vpnTeamClientStatus(value, expectedCount = null) {
  const ownership = normalizeOwnership(value);
  if (!ownership) throw new Error('team-client status requires an ownership record');
  const expected = expectedCount == null ? ownership.participationIds.length : Number(expectedCount);
  if (!Number.isSafeInteger(expected) || expected !== ownership.participationIds.length) {
    throw new Error(`team-client status expected count does not match ownership (${expectedCount})`);
  }
  const rows = inspectOwnedTeamClients(ownership);
  const running = rows.filter(({ state }) => state === 'running').length;
  const succeeded = rows.filter(({ state, exitCode }) => state === 'exited' && exitCode === 0).length;
  const failed = rows.filter(
    ({ state, exitCode }) => state === 'dead' || (state === 'exited' && exitCode !== 0),
  ).length;
  return {
    total: rows.length,
    running,
    succeeded,
    failed,
    missing: Math.max(0, expected - rows.length),
  };
}

export function vpnHandshakeCount(sinceEpochSeconds = 0, expectedPublicKeys = null) {
  const since = Number(sinceEpochSeconds);
  if (!Number.isSafeInteger(since) || since < 0) {
    throw new Error(`invalid WireGuard handshake lower bound ${sinceEpochSeconds}`);
  }
  const result = docker(['exec', RSCTF, 'wg', 'show', 'wg0', 'latest-handshakes']);
  if (result.status !== 0) return 0;
  const handshakes = new Map(
    result.stdout
    .trim()
    .split('\n')
    .filter(Boolean)
    .map((row) => {
      const [publicKey, timestamp] = row.trim().split(/\s+/);
      return [publicKey, Number(timestamp)];
    })
  );
  if (expectedPublicKeys !== null) {
    if (
      !Array.isArray(expectedPublicKeys) ||
      expectedPublicKeys.length === 0 ||
      new Set(expectedPublicKeys).size !== expectedPublicKeys.length ||
      expectedPublicKeys.some((key) => !/^[A-Za-z0-9+/]{43}=$/.test(key))
    ) {
      throw new Error('expected WireGuard peers must be a non-empty distinct public-key list');
    }
    return expectedPublicKeys.filter((key) => (handshakes.get(key) || 0) >= since).length;
  }
  return [...handshakes.values()].filter((timestamp) => timestamp >= since && timestamp > 0).length;
}

export function teardownVpnTeamClients(value) {
  const ownership = normalizeOwnership(value);
  if (!ownership) return { removed: 0 };
  const scope = scopeForOwnership(ownership);
  // The bounded `docker ps --format` projection validates the exact
  // participation, cohort index, and deterministic name before any removal.
  // This stays crash-recoverable even when captured IDs were not yet persisted.
  const ids = inspectOwnedTeamClients(ownership).map(({ id }) => id);
  if (ids.length) {
    mustDocker(docker(['rm', '-f', ...ids]), 'remove run-owned VPN team clients');
  }
  const remaining = listOwnedTeamClientIds(scope);
  if (remaining.length) {
    throw new Error(
      `distributed VPN team-client teardown left ${remaining.length} container(s): ` +
        remaining.join(',')
    );
  }
  rmSync(teamClientRoot(scope), { recursive: true, force: true });
  return { removed: ids.length };
}
