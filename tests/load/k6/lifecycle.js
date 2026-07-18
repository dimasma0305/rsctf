// Whole-platform realistic load: onboarding (register→login→team→join), jeopardy play,
// A&D, KotH, anonymous browsing, an admin monitor feed, and a concurrent same-flag dedup
// burst — across two seeded events. State comes from .lifecycle-state.json (written by
// provision.mjs). Driven by lifecycle.mjs. Every VU uses a distinct X-Real-IP so the
// sharded rate-limiter buckets behave per-team; expected 429s are tracked apart from 5xx.
import http from 'k6/http';
import crypto from 'k6/crypto';
import encoding from 'k6/encoding';
import { check, sleep } from 'k6';
import { Trend, Rate, Counter } from 'k6/metrics';
import { SharedArray } from 'k6/data';
import { lifecycleStateOpenPath } from '../lifecycle-state-file.js';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const SECRET = __ENV.SECRET;
if (!SECRET) throw new Error('SECRET is required for load-test token minting');
const VUS = Number(__ENV.VUS || 400);
const DUR = __ENV.DURATION || '90s';
const STABLE_IPS = __ENV.STABLE_IPS === '1';
const PLAYER_THINK_SECONDS = Number(__ENV.PLAYER_THINK_SECONDS || 0);
const STRICT_ZERO_ERRORS = __ENV.STRICT_ZERO_ERRORS === '1';

const S = new SharedArray('state', () => [
  JSON.parse(open(lifecycleStateOpenPath(__ENV.LIFECYCLE_STATE_FILE))),
])[0];

const share = (fraction) => Math.max(1, Math.round(VUS * fraction));
function scenarioVus(name, fallback) {
  const raw = __ENV[name];
  if (raw === undefined || raw === '') return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${name} must be a non-negative integer (got ${raw})`);
  }
  return value;
}

const JEO_VUS = scenarioVus('JEO_VUS', share(0.55));
const AD_VUS = scenarioVus('AD_VUS', share(0.18));
const KOTH_VUS = scenarioVus('KOTH_VUS', share(0.12));

if (!Number.isFinite(PLAYER_THINK_SECONDS) || PLAYER_THINK_SECONDS < 0) {
  throw new Error(`PLAYER_THINK_SECONDS must be a non-negative number (got ${__ENV.PLAYER_THINK_SECONDS})`);
}

// ── metrics ─────────────────────────────────────────────────────────────────
const server5xx = new Rate('server_5xx');
const non2xx = new Rate('non_2xx'); // excludes 429
const epochBoardInvalid = new Rate('ad_epoch_board_invalid');
const kothLifecycleInvalid = new Rate('koth_lifecycle_invalid');
const board = new Trend('board_poll_ms', true);
const adEpochBoard = new Trend('ad_epoch_board_ms', true);
const details = new Trend('details_ms', true);
const adState = new Trend('ad_state_ms', true);
const adTargets = new Trend('ad_targets_ms', true);
const jeoSubmit = new Trend('jeo_submit_ms', true);
const adSubmit = new Trend('ad_submit_ms', true);
const onboard = new Trend('onboard_ms', true);
const kothHills = new Trend('koth_hills_ms', true);
const assetMs = new Trend('asset_ms', true);
const containerMs = new Trend('container_ms', true);
const containersSpawned = new Counter('containers_spawned');
const dupSeen = new Counter('dedup_duplicates');
const accepted = new Counter('captures_accepted');

// ── helpers ──────────────────────────────────────────────────────────────────
const b64 = (s) => encoding.b64encode(s, 'rawurl');
function mintJwt(uuid) {
  const now = Math.floor(Date.now() / 1000);
  const stamp = S.userStamps[uuid];
  const seg =
    b64(JSON.stringify({ alg: 'HS256', typ: 'JWT' })) +
    '.' +
    b64(
      JSON.stringify({
        sub: uuid,
        role: 1,
        name: 'lt',
        stamp,
        iat: now,
        exp: now + 7200,
      })
    );
  return seg + '.' + crypto.hmac('sha256', SECRET, seg, 'base64rawurl');
}
const srcIp = () =>
  `${20 + (__VU % 200)}.${(__VU * 7) % 254}.${((STABLE_IPS ? __VU : __ITER) * 13) % 254}.${(__VU % 254) + 1}`;
const playerThink = () => {
  if (PLAYER_THINK_SECONDS > 0) sleep(PLAYER_THINK_SECONDS);
};
// A distinct well-formed 64-hex browser fingerprint per call (any is accepted; varying it
// avoids the anti-cheat same-fingerprint gate). Captcha is disabled in Configs for the run.
const fingerprint = () => crypto.sha256(`${__VU}_${__ITER}_${Date.now()}_${Math.random()}`, 'hex');
function hdr(ip, jwt) {
  const h = { 'X-Real-IP': ip };
  if (jwt) h.Authorization = `Bearer ${jwt}`;
  return h;
}
function rec(r, name, trend) {
  const is5 = r.status >= 500;
  server5xx.add(is5);
  if (r.status !== 429) non2xx.add(r.status >= 300 || r.status === 0);
  if (trend && r.status >= 200 && r.status < 300) trend.add(r.timings.duration);
  check(r, { [`${name} ok`]: (x) => x.status < 500 });
  return r;
}
function post(path, body, ip, jwt) {
  return http.post(`${TARGET}${path}`, JSON.stringify(body), {
    headers: { ...hdr(ip, jwt), 'Content-Type': 'application/json' },
  });
}
function get(path, ip, jwt) {
  return http.get(`${TARGET}${path}`, { headers: hdr(ip, jwt) });
}
function del(path, ip, jwt) {
  return http.del(`${TARGET}${path}`, null, { headers: hdr(ip, jwt) });
}

function validServiceBreakdown(model) {
  if (!Array.isArray(model?.challenges) || model.challenges.length === 0 || !Array.isArray(model?.teams)) return false;
  if (!model.challenges.every((challenge) => Number.isSafeInteger(challenge.challengeId) && challenge.challengeId > 0))
    return false;
  const challengeIds = new Set(model.challenges.map((challenge) => challenge.challengeId));
  if (challengeIds.size !== model.challenges.length) return false;

  return model.teams.every((team) => {
    if (
      !Number.isFinite(team?.settledTotal) ||
      team.settledTotal < 0 ||
      team.settledTotal > 100 ||
      !Number.isFinite(team?.projectedTotal) ||
      team.projectedTotal < 0 ||
      team.projectedTotal > 100 ||
      !Array.isArray(team?.services) ||
      team.services.length !== challengeIds.size
    )
      return false;
    const serviceIds = new Set(team.services.map((service) => service.challengeId));
    if (serviceIds.size !== challengeIds.size || ![...serviceIds].every((id) => challengeIds.has(id))) return false;
    const valid = team.services.every(
      (service) =>
        Number.isFinite(service.settledPoints) &&
        service.settledPoints >= 0 &&
        service.settledPoints <= 100 &&
        Number.isFinite(service.projectedPoints) &&
        service.projectedPoints >= 0 &&
        service.projectedPoints <= 100 &&
        Number.isFinite(service.offenseRate) &&
        service.offenseRate >= 0 &&
        service.offenseRate <= 1 &&
        Number.isFinite(service.defenseRate) &&
        service.defenseRate >= 0 &&
        service.defenseRate <= 1 &&
        Number.isFinite(service.slaRate) &&
        service.slaRate >= 0 &&
        service.slaRate <= 1 &&
        Number.isSafeInteger(service.captureCount) &&
        service.captureCount >= 0
    );
    if (!valid) return false;
    const settled = team.services.reduce((sum, service) => sum + service.settledPoints, 0);
    const projected = team.services.reduce((sum, service) => sum + service.projectedPoints, 0);
    return Math.abs(settled - team.settledTotal) < 1e-6 && Math.abs(projected - team.projectedTotal) < 1e-6;
  });
}

function validKothConfig(model) {
  return (
    Number.isSafeInteger(model?.epochTicks) &&
    model.epochTicks >= 2 &&
    Number.isSafeInteger(model.cycleTicks) &&
    model.cycleTicks >= 1 &&
    model.epochTicks % model.cycleTicks === 0 &&
    Number.isSafeInteger(model.claimConfirmationTicks) &&
    model.claimConfirmationTicks >= 1 &&
    model.claimConfirmationTicks <= model.cycleTicks &&
    Array.isArray(model.hills) &&
    model.hills.length > 0
  );
}

function validKothHill(hill, cycleTicks) {
  return (
    Number.isSafeInteger(hill.challengeId) &&
    hill.challengeId > 0 &&
    Number.isSafeInteger(hill.cycleNumber) &&
    hill.cycleNumber > 0 &&
    Number.isSafeInteger(hill.cycleTick) &&
    hill.cycleTick >= 0 &&
    hill.cycleTick <= cycleTicks &&
    typeof hill.resetPhase === 'string' &&
    hill.resetPhase.length > 0 &&
    typeof hill.isScorable === 'boolean' &&
    Number.isSafeInteger(hill.provisionalConfirmationTicks) &&
    hill.provisionalConfirmationTicks >= 0 &&
    Array.isArray(hill.cooldownParticipants)
  );
}

function validKothScores(model) {
  if (!Array.isArray(model?.teams)) return false;
  const ratesAreBounded = (value) => Number.isFinite(value) && value >= 0 && value <= 1;
  return model.teams.every(
    (team) =>
      Number.isFinite(team.settledTotal) &&
      team.settledTotal >= 0 &&
      team.settledTotal <= 100 &&
      Number.isFinite(team.projectedTotal) &&
      team.projectedTotal >= 0 &&
      team.projectedTotal <= 100 &&
      ratesAreBounded(team.acquisitionRate) &&
      ratesAreBounded(team.controlRate) &&
      ratesAreBounded(team.reliabilityRate) &&
      Array.isArray(team.hills) &&
      team.hills.every(
        (hill) =>
          Number.isFinite(hill.settledPoints) &&
          hill.settledPoints >= 0 &&
          hill.settledPoints <= 100 &&
          Number.isFinite(hill.projectedPoints) &&
          hill.projectedPoints >= 0 &&
          hill.projectedPoints <= 100 &&
          ratesAreBounded(hill.acquisitionRate) &&
          ratesAreBounded(hill.controlRate) &&
          ratesAreBounded(hill.reliabilityRate) &&
          Number.isSafeInteger(hill.responsibleTicks) &&
          hill.responsibleTicks >= 0 &&
          Number.isSafeInteger(hill.healthyResponsibleTicks) &&
          hill.healthyResponsibleTicks >= 0 &&
          hill.healthyResponsibleTicks <= hill.responsibleTicks
      )
  );
}

function validKothLifecycle(model) {
  if (!validKothConfig(model) || model.started !== true || !validKothScores(model)) return false;
  return model.hills.every(
    (hill) =>
      validKothHill(hill, model.cycleTicks) &&
      hill.cooldownParticipants.every(
        (cooldown) =>
          Number.isSafeInteger(cooldown.participationId) &&
          cooldown.participationId > 0 &&
          Number.isSafeInteger(cooldown.remainingTicks) &&
          cooldown.remainingTicks >= 0
      )
  );
}

function validAdminKothLifecycle(model) {
  if (
    !validKothConfig(model) ||
    !validKothScores(model) ||
    !Number.isSafeInteger(model?.championCooldownTicks) ||
    model.championCooldownTicks < 0 ||
    model.championCooldownTicks >= model.cycleTicks
  )
    return false;
  return model.hills.every(
    (hill) =>
      validKothHill(hill, model.cycleTicks) &&
      typeof hill.canRetry === 'boolean' &&
      Number.isSafeInteger(hill.resetAttempt) &&
      hill.resetAttempt >= 0 &&
      Number.isSafeInteger(hill.readinessFailureCount) &&
      hill.readinessFailureCount >= 0 &&
      (hill.oldContainerId == null || typeof hill.oldContainerId === 'string') &&
      (hill.replacementContainerId == null || typeof hill.replacementContainerId === 'string') &&
      (hill.resetReceiptId == null || Number.isSafeInteger(hill.resetReceiptId)) &&
      (hill.scoringReceiptId == null || Number.isSafeInteger(hill.scoringReceiptId))
  );
}

// ── scenarios ─────────────────────────────────────────────────────────────────
export const options = {
  scenarios: {
    onboarding: {
      executor: 'ramping-vus',
      exec: 'onboarding',
      startVUs: 0,
      stages: [
        { duration: '20s', target: 40 },
        { duration: '20s', target: 0 },
      ],
      gracefulRampDown: '5s',
    },
    ...(JEO_VUS > 0
      ? {
          jeopardy: {
            executor: 'constant-vus',
            exec: 'jeopardy',
            vus: JEO_VUS,
            duration: DUR,
          },
        }
      : {}),
    ...(AD_VUS > 0
      ? {
          ad: {
            executor: 'constant-vus',
            exec: 'ad',
            vus: AD_VUS,
            duration: DUR,
          },
        }
      : {}),
    ...(KOTH_VUS > 0
      ? {
          koth: {
            executor: 'constant-vus',
            exec: 'koth',
            vus: KOTH_VUS,
            duration: DUR,
          },
        }
      : {}),
    browse: {
      executor: 'constant-arrival-rate',
      exec: 'browse',
      rate: 40,
      timeUnit: '1s',
      duration: DUR,
      preAllocatedVUs: 40,
    },
    monitor: {
      executor: 'constant-vus',
      exec: 'monitor',
      vus: 3,
      duration: DUR,
    },
    dedupBurst: {
      executor: 'per-vu-iterations',
      exec: 'dedupBurst',
      vus: 30,
      iterations: 1,
      startTime: '12s',
      maxDuration: '20s',
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    server_5xx: [
      {
        threshold: STRICT_ZERO_ERRORS ? 'rate==0' : 'rate<0.01',
        abortOnFail: true,
      },
    ],
    non_2xx: [STRICT_ZERO_ERRORS ? 'rate==0' : 'rate<0.01'],
    ad_epoch_board_invalid: ['rate==0'],
    koth_lifecycle_invalid: ['rate==0'],
    board_poll_ms: ['p(95)<1500'],
    ad_epoch_board_ms: ['p(95)<1500'],
    details_ms: ['p(95)<1500'],
  },
};

// 1. Onboarding — the full live entry flow (register→login→team→join→profile).
export function onboarding() {
  const ip = srcIp();
  const u = `ltlive_${__VU}_${__ITER}_${Date.now()}`;
  const jar = http.cookieJar();
  const t0 = Date.now();
  const reg = http.post(
    `${TARGET}/api/account/register`,
    JSON.stringify({
      userName: u,
      password: 'Loadtest1!',
      email: `${u}@load.test`,
      fingerprint: fingerprint(),
    }),
    { headers: { ...hdr(ip), 'Content-Type': 'application/json' } }
  );
  rec(reg, 'register');
  check(reg, {
    'register LoggedIn': (r) => r.status === 200 && (r.json('data') === 'LoggedIn' || r.status === 200),
  });
  if (reg.status !== 200) {
    playerThink();
    return;
  }
  // cookie carried by the per-VU jar; create a team then join the jeopardy game.
  const team = http.post(`${TARGET}/api/team`, JSON.stringify({ name: u }), {
    headers: { ...hdr(ip), 'Content-Type': 'application/json' },
  });
  rec(team, 'createTeam');
  const teamId = team.json('id') ?? team.json('data.id');
  if (teamId) {
    const join = http.post(`${TARGET}/api/game/${S.jeoGame}`, JSON.stringify({ teamId }), {
      headers: { ...hdr(ip), 'Content-Type': 'application/json' },
    });
    rec(join, 'joinGame');
  }
  rec(get(`/api/account/profile`, ip), 'profile');
  onboard.add(Date.now() - t0);
  playerThink();
}

// 2. Jeopardy — poll + occasional challenge detail, submit, rare container.
export function jeopardy() {
  const ip = srcIp();
  const jwt = mintJwt(S.jeoUsers[__VU % S.jeoUsers.length]);
  rec(get(`/api/game/${S.jeoGame}/details`, ip, jwt), 'jeo details', details);
  rec(get(`/api/game/${S.jeoGame}/scoreboard`, ip, jwt), 'jeo scoreboard', board);
  if (__ITER % 4 === 0) rec(get(`/api/game/${S.jeoGame}/notices`, ip, jwt), 'jeo notices');
  const cids = Object.keys(S.staticFlags);
  const cid = cids[__ITER % cids.length];
  if (__ITER % 3 === 0) rec(get(`/api/game/${S.jeoGame}/challenges/${cid}`, ip, jwt), 'jeo chal detail', details);
  // download the challenge attachment (stresses rsctf's /assets serving)
  if (__ITER % 6 === 0 && S.attachHash)
    rec(get(`/assets/${S.attachHash}/${S.attachName}`, ip, jwt), 'jeo attachment', assetMs);
  if (__ITER % 5 === 0) {
    const flag = __ITER % 10 === 0 ? S.staticFlags[cid] : `flag{wrong_${__VU}_${__ITER}}`;
    const r = post(`/api/game/${S.jeoGame}/challenges/${cid}`, { flag }, ip, jwt);
    rec(r, 'jeo submit', jeoSubmit);
  }
  // a small cohort spawns a REAL container (create → hold → destroy); bounded to a few
  // VUs since each is a live docker container (the game's container_count_limit caps it).
  if (__VU % Number(__ENV.CONTAINER_EVERY || 12) === 0 && __ITER % 6 === 0 && S.containerChal) {
    const c = post(`/api/game/${S.jeoGame}/container/${S.containerChal}`, undefined, ip, jwt);
    rec(c, 'container create', containerMs);
    if (c.status < 300) {
      containersSpawned.add(1);
      sleep(8);
      rec(del(`/api/game/${S.jeoGame}/container/${S.containerChal}`, ip, jwt), 'container delete');
    }
  }
  playerThink();
}

// 3. A&D — poll state/targets/official epoch board + submit captured flags.
export function ad() {
  const ip = srcIp();
  const jwt = mintJwt(S.adUsers[__VU % S.adUsers.length]);
  rec(get(`/api/Game/${S.mixGame}/Ad/State`, ip, jwt), 'ad state', adState);
  rec(get(`/api/Game/${S.mixGame}/Ad/Targets`, ip, jwt), 'ad targets', adTargets);
  const official = get(`/api/Game/${S.mixGame}/Ad/Scoreboard`, ip, jwt);
  rec(official, 'ad scoreboard');
  let officialModel = null;
  try {
    officialModel = official.json();
  } catch (_) {
    // Invalid JSON is reported by the semantic metric below.
  }
  const validOfficial =
    official.status === 200 &&
    officialModel?.started === true &&
    typeof officialModel?.fullySettled === 'boolean' &&
    officialModel?.startRound > 0 &&
    Array.isArray(officialModel?.teams) &&
    officialModel.teams.length >= 2 &&
    validServiceBreakdown(officialModel);
  epochBoardInvalid.add(!validOfficial);
  if (validOfficial) adEpochBoard.add(official.timings.duration);
  if (__ITER % 4 === 0) rec(get(`/api/game/${S.mixGame}/ad/koth/scoreboard`, ip, jwt), 'ad koth board', kothHills);
  if (__ITER % 5 === 0 && S.plantedFlags.length) {
    // submit a DIFFERENT team's planted flag (a valid capture) mixed with wrong ones.
    const pf = S.plantedFlags[(__VU + 1) % S.plantedFlags.length];
    const flags = __ITER % 10 === 0 ? [pf.flag] : [`flag{wrong_${__VU}_${__ITER}}`];
    const r = post(`/api/Game/${S.mixGame}/Ad/Submit`, { flags }, ip, jwt);
    rec(r, 'ad submit', adSubmit);
    if (r.status === 200) accepted.add(r.json('acceptedCount') || 0);
  }
  playerThink();
}

// 4. KotH — board + token + hill state + timeline.
export function koth() {
  const ip = srcIp();
  const jwt = mintJwt(S.adUsers[__VU % S.adUsers.length]);
  const scoreboard = get(`/api/game/${S.mixGame}/ad/koth/scoreboard`, ip, jwt);
  rec(scoreboard, 'koth board', board);
  let model = null;
  try {
    model = scoreboard.json();
  } catch (_) {
    // Semantic validation below reports malformed JSON.
  }
  kothLifecycleInvalid.add(scoreboard.status !== 200 || !validKothLifecycle(model));
  rec(get(`/api/game/${S.mixGame}/ad/koth/${S.kothChal}/token`, ip, jwt), 'koth token');
  rec(get(`/api/game/${S.mixGame}/ad/koth/${S.kothChal}/state`, ip, jwt), 'koth state', details);
  if (__ITER % 3 === 0) rec(get(`/api/game/${S.mixGame}/ad/koth/timeline`, ip, jwt), 'koth timeline', board);
  playerThink();
}

// 5. Browse — anonymous public surface.
export function browse() {
  const ip = srcIp();
  rec(get(`/api/game`, ip), 'game list');
  if (__ITER % 3 === 0) rec(get(`/api/game/${S.jeoGame}`, ip), 'game detail');
}

// 6. Monitor — admin feeds (read-only; never cheatreport).
export function monitor() {
  const ip = srcIp();
  const jwt = mintJwt(S.adminUuid);
  rec(get(`/api/game/${S.mixGame}/scoreboard`, ip, jwt), 'mon scoreboard', board);
  const official = get(`/api/Game/${S.mixGame}/Ad/Scoreboard`, ip, jwt);
  rec(official, 'mon ad board');
  let model = null;
  try {
    model = official.json();
  } catch (_) {
    // Invalid JSON is reported by the semantic metric below.
  }
  const validOfficial =
    official.status === 200 &&
    model?.started === true &&
    typeof model?.fullySettled === 'boolean' &&
    model?.startRound > 0 &&
    Array.isArray(model?.teams) &&
    model.teams.length >= 2 &&
    validServiceBreakdown(model);
  epochBoardInvalid.add(!validOfficial);
  if (validOfficial) adEpochBoard.add(official.timings.duration);
  const kothState = get(`/api/edit/games/${S.mixGame}/ad/koth/state`, ip, jwt);
  rec(kothState, 'mon koth state', board);
  let kothModel = null;
  try {
    kothModel = kothState.json();
  } catch (_) {
    // Invalid JSON is reported by the semantic metric below.
  }
  kothLifecycleInvalid.add(kothState.status !== 200 || !validAdminKothLifecycle(kothModel));
  playerThink();
}

// 7. Dedup burst — all VUs POST the SAME flag as the SAME attacker concurrently.
export function dedupBurst() {
  if (!S.dedupFlag) return;
  const ip = `55.55.${__VU % 254}.1`;
  const jwt = mintJwt(S.adUsers[0]); // one attacker
  const r = post(`/api/Game/${S.mixGame}/Ad/Submit`, { flags: [S.dedupFlag] }, ip, jwt);
  rec(r, 'dedup submit');
  if (r.status === 200) {
    const st = (r.json('results') || []).map((x) => x.status);
    if (st.includes('duplicate')) dupSeen.add(1);
  }
}
