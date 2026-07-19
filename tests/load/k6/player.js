// A&D + KotH player load — mimics real team behaviour under an intensive event.
//
// Every simulated player, each cycle:
//   1. polls the three LIVE boards (general + A&D + KotH scoreboards) — the dominant,
//      always-on load a real client generates,
//   2. periodically pulls the KotH timeline + its own A&D state/targets,
//   3. checks its KotH token + the hill's live holder,
//   4. occasionally submits captured flags (an attack attempt — mostly wrong).
//
// Each VU is a distinct logged-in player. Board requests carry the same session JWT
// as the rest of that player's traffic, so a production reverse proxy can replace
// client-supplied forwarding headers without collapsing every board poll into one
// anonymous source-IP bucket. Requests still include X-Real-IP for direct-to-app
// development runs where the load source is explicitly trusted.
//
//   TARGET=http://127.0.0.1:8080 GAME=10 VUS=200 DURATION=90s \
//   TOKENS=<jwt1,jwt2,...> KOTH_HILLS=67,65 AD_CHALS=56,58,66,68 \
//   k6 run k6/player.js
//
// Prefer `npm run player` from tests/load; player.mjs mints tokens and discovers IDs.
// `server_5xx` and board thresholds fail the run if the stack degrades.
import http from 'k6/http';
import { sleep } from 'k6';
import { Rate, Trend } from 'k6/metrics';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const GAME = __ENV.GAME || '10';
const TOKENS = (__ENV.TOKENS || '').split(',').filter(Boolean);
const HILLS = (__ENV.KOTH_HILLS || '').split(',').filter(Boolean);
const CHALS = (__ENV.AD_CHALS || '').split(',').filter(Boolean);
const VUS = Number(__ENV.VUS || 200);
const RATE = Number(__ENV.RATE || Math.max(1, Math.round(VUS / 2)));
const THINK_MIN_SECONDS = Number(__ENV.THINK_MIN_SECONDS || 3);
const THINK_MAX_SECONDS = Number(__ENV.THINK_MAX_SECONDS || 5);

if (
  !Number.isFinite(THINK_MIN_SECONDS) ||
  !Number.isFinite(THINK_MAX_SECONDS) ||
  THINK_MIN_SECONDS < 0 ||
  THINK_MAX_SECONDS < THINK_MIN_SECONDS
) {
  throw new Error('THINK_MIN_SECONDS and THINK_MAX_SECONDS must be finite, non-negative, and ordered');
}

const errors = new Rate('errors'); //         non-2xx on a board poll (should be ~0)
const server5xx = new Rate('server_5xx'); //  any 5xx anywhere (the real failure signal)
const epochBoardInvalid = new Rate('ad_epoch_board_invalid');
const board = new Trend('board_poll_ms', true);
const mainBoard = new Trend('main_board_ms', true);
const epochBoard = new Trend('ad_epoch_board_ms', true);
const kothBoard = new Trend('koth_board_ms', true);
const kothTimeline = new Trend('koth_timeline_ms', true);
const adState = new Trend('ad_state_ms', true);
const adTargets = new Trend('ad_targets_ms', true);
const kothToken = new Trend('koth_token_ms', true);
const kothState = new Trend('koth_state_ms', true);
const submit = new Trend('submit_ms', true);

export const options = {
  scenarios: {
    players: {
      executor: 'constant-arrival-rate',
      rate: RATE,
      timeUnit: '1s',
      duration: __ENV.DURATION || '90s',
      preAllocatedVUs: VUS,
      maxVUs: VUS * 2,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    server_5xx: ['rate<0.01'], //     <1% 5xx — a real defect if breached
    errors: ['rate<0.01'],
    ad_epoch_board_invalid: ['rate==0'],
    board_poll_ms: ['p(95)<800'], //  boards stay responsive under load
    ad_epoch_board_ms: ['p(95)<800'], // SQL-aggregated official board stays bounded
  },
};

// Distinct source IP per VU (the per-IP limiter keys on it), spread across /8s.
function srcIp() {
  const v = __VU;
  return `10.${10 + (v % 240)}.${1 + (Math.floor(v / 240) % 254)}.${1 + (v % 250)}`;
}

// Keep the deliberately unknown capture in the engine's exact 32-character
// payload shape. A short placeholder is rejected before PostgreSQL and would
// make this scenario stop exercising authoritative flag lookup under load.
function unknownFlag(vu, iteration) {
  const identity = `${vu.toString(36)}_${iteration.toString(36)}`;
  return `flag{${identity.padEnd(32, 'x').slice(0, 32)}}`;
}

function validServiceBreakdown(model) {
  if (!Array.isArray(model?.challenges) || model.challenges.length === 0 || !Array.isArray(model?.teams)) return false;
  if (!model.challenges.every((challenge) => Number.isSafeInteger(challenge.challengeId) && challenge.challengeId > 0)) return false;
  const challengeIds = new Set(model.challenges.map((challenge) => challenge.challengeId));
  if (challengeIds.size !== model.challenges.length) return false;

  return model.teams.every((team) => {
    if (!Number.isFinite(team?.settledTotal) || team.settledTotal < 0 || team.settledTotal > 100 ||
        !Number.isFinite(team?.projectedTotal) || team.projectedTotal < 0 || team.projectedTotal > 100 ||
        !Array.isArray(team?.services) || team.services.length !== challengeIds.size) return false;
    const serviceIds = new Set(team.services.map((service) => service.challengeId));
    if (serviceIds.size !== challengeIds.size || ![...serviceIds].every((id) => challengeIds.has(id))) return false;
    const valid = team.services.every((service) =>
      Number.isFinite(service.settledPoints) && service.settledPoints >= 0 && service.settledPoints <= 100 &&
      Number.isFinite(service.projectedPoints) && service.projectedPoints >= 0 && service.projectedPoints <= 100 &&
      Number.isFinite(service.offenseRate) && service.offenseRate >= 0 && service.offenseRate <= 1 &&
      Number.isFinite(service.defenseRate) && service.defenseRate >= 0 && service.defenseRate <= 1 &&
      Number.isFinite(service.slaRate) && service.slaRate >= 0 && service.slaRate <= 1 &&
      Number.isSafeInteger(service.captureCount) && service.captureCount >= 0
    );
    if (!valid) return false;
    const settled = team.services.reduce((sum, service) => sum + service.settledPoints, 0);
    const projected = team.services.reduce((sum, service) => sum + service.projectedPoints, 0);
    return Math.abs(settled - team.settledTotal) < 1e-6 &&
      Math.abs(projected - team.projectedTotal) < 1e-6;
  });
}

export default function () {
  const vu = __VU;
  const it = __ITER;
  const ip = srcIp();
  const tok = TOKENS.length ? TOKENS[vu % TOKENS.length] : '';
  const pubHeaders = { 'X-Real-IP': ip };
  if (tok) pubHeaders.Authorization = `Bearer ${tok}`;
  const pub = { headers: pubHeaders, tags: { kind: 'board' } };
  const auth = { headers: { 'X-Real-IP': ip, Authorization: `Bearer ${tok}` }, tags: { kind: 'auth' } };

  // 1. Live boards — every player, every cycle.
  const b = http.batch([
    ['GET', `${TARGET}/api/game/${GAME}/scoreboard`, null, pub],
    ['GET', `${TARGET}/api/Game/${GAME}/Ad/Scoreboard`, null, pub],
    ['GET', `${TARGET}/api/game/${GAME}/ad/koth/scoreboard`, null, pub],
  ]);
  for (const r of b) {
    if (r.status === 200) board.add(r.timings.duration);
    errors.add(r.status !== 200);
    server5xx.add(r.status >= 500);
  }
  if (b[0].status === 200) mainBoard.add(b[0].timings.duration);
  if (b[2].status === 200) kothBoard.add(b[2].timings.duration);
  let officialBoard = null;
  try {
    officialBoard = b[1].json();
  } catch (_) {
    // Invalid JSON is reported by the semantic metric below.
  }
  const validOfficialBoard =
    b[1].status === 200 &&
    officialBoard?.started === true &&
    typeof officialBoard?.fullySettled === 'boolean' &&
    officialBoard?.startRound > 0 &&
    Array.isArray(officialBoard?.teams) &&
    officialBoard.teams.length >= 2 &&
    validServiceBreakdown(officialBoard);
  epochBoardInvalid.add(!validOfficialBoard);
  if (validOfficialBoard) epochBoard.add(b[1].timings.duration);

  // 2. KotH timeline + this team's own A&D view (~every 3rd cycle).
  if (it % 3 === 0) {
    const r = http.batch([
      ['GET', `${TARGET}/api/game/${GAME}/ad/koth/timeline`, null, pub],
      ['GET', `${TARGET}/api/Game/${GAME}/Ad/State`, null, auth],
      ['GET', `${TARGET}/api/Game/${GAME}/Ad/Targets`, null, auth],
    ]);
    for (const x of r) server5xx.add(x.status >= 500);
    if (r[0].status === 200) kothTimeline.add(r[0].timings.duration);
    if (r[1].status === 200) adState.add(r[1].timings.duration);
    if (r[2].status === 200) adTargets.add(r[2].timings.duration);
  }

  // 3. KotH — a player checking its minted token + the hill's live holder.
  if (HILLS.length) {
    const hill = HILLS[it % HILLS.length];
    const r = http.batch([
      ['GET', `${TARGET}/api/game/${GAME}/ad/koth/${hill}/token`, null, auth],
      ['GET', `${TARGET}/api/game/${GAME}/ad/koth/${hill}/state`, null, auth],
    ]);
    for (const x of r) server5xx.add(x.status >= 500);
    if (r[0].status === 200) kothToken.add(r[0].timings.duration);
    if (r[1].status === 200) kothState.add(r[1].timings.duration);
  }

  // 4. Attack attempt — submit a captured flag (~every 5th cycle; mostly wrong, which
  //    is the realistic case + exercises the cheat-scan cache + flag-lookup path).
  if (it % 5 === 0 && CHALS.length) {
    const r = http.post(
      `${TARGET}/api/Game/${GAME}/Ad/Submit`,
      JSON.stringify({ flags: [unknownFlag(vu, it)] }),
      {
        headers: { 'X-Real-IP': ip, Authorization: `Bearer ${tok}`, 'Content-Type': 'application/json' },
        tags: { kind: 'submit' },
      }
    );
    submit.add(r.timings.duration);
    server5xx.add(r.status >= 500);
  }

  // Realistic think time between poll cycles.
  sleep(THINK_MIN_SECONDS + Math.random() * (THINK_MAX_SECONDS - THINK_MIN_SECONDS));
}
