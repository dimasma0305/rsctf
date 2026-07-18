// One real event bot per container. The container owns one WireGuard identity and
// receives only its team's short-lived runtime configuration in /config/bot.json.
import http from 'k6/http';
import { check, sleep } from 'k6';
import exec from 'k6/execution';
import { Counter, Rate, Trend } from 'k6/metrics';
import {
  attackPlan,
  boundedPlatformRetryDelay,
  buildRoundActionBudget,
  canSpendActionCredit,
  classifyPlatformFirstAttemptFailure,
  classifyKothPendingTransition,
  defenseLevelAt,
  isProfileActive,
  isKothTerminalWindow,
  isRetryablePlatformRequest,
  isRetryablePlatformStatus,
  isReplacementKothInstance,
  jeopardyIntent,
  keyedUnit,
  kothCapabilityMatchesState,
  kothControllerParticipationId,
  kothHealthyHoldStatusMatches,
  kothIntent,
  kothPatchIntent,
  kothPatchRepairReady,
  kothTargetMatchesState,
  parseKothServiceStatus,
  playerThinkDelay,
  publicAdNetworkTargets,
  recordAttackOutcome,
  spendActionCredit,
} from '../player-model.js';
import {
  MANDATORY_TEAM_EVIDENCE_COUNTERS,
  TEAM_EVIDENCE_SCHEMA_VERSION,
} from '../team-evidence.js';

function readConfig() {
  let raw;
  try {
    raw = JSON.parse(open('/config/bot.json'));
  } catch (_) {
    throw new Error('bot.json must contain valid JSON');
  }
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new Error('bot.json must contain one configuration object');
  }

  const integer = (value, label, minimum = 1) => {
    const parsed = Number(value);
    if (!Number.isSafeInteger(parsed) || parsed < minimum) {
      throw new Error(`${label} must be an integer >= ${minimum}`);
    }
    return parsed;
  };
  const text = (value, label) => {
    if (typeof value !== 'string' || value.trim() === '') {
      throw new Error(`${label} must be a non-empty string`);
    }
    return value.trim();
  };

  const teamCount = integer(raw.teamCount, 'teamCount', 2);
  const teamIndex = integer(raw.teamIndex, 'teamIndex', 0);
  if (teamIndex >= teamCount) throw new Error('teamIndex must be smaller than teamCount');

  if (Object.prototype.hasOwnProperty.call(raw, 'listeners')) {
    throw new Error('bot config must not disclose opponent service listeners');
  }
  const ownListener = text(raw.ownListener, 'ownListener');
  const ownListenerMatch = ownListener.match(/^([A-Za-z0-9._-]+):(\d{1,5})$/);
  const ownListenerPort = ownListenerMatch ? Number(ownListenerMatch[2]) : 0;
  if (!ownListenerMatch || ownListenerPort < 1 || ownListenerPort > 65535) {
    throw new Error('ownListener must use host:port form');
  }

  if (!Array.isArray(raw.teamIds) || raw.teamIds.length !== teamCount) {
    throw new Error('teamIds must contain exactly one entry per team');
  }
  const teamIds = raw.teamIds.map((value, index) => integer(value, `teamIds[${index}]`));
  if (new Set(teamIds).size !== teamCount) throw new Error('teamIds must be distinct');

  const target = text(raw.target ?? 'https://tcp.1pc.tf', 'target').replace(/\/+$/, '');
  if (!/^https?:\/\/[A-Za-z0-9.-]+(?::\d{1,5})?$/.test(target)) {
    throw new Error('target must be an HTTP(S) origin without credentials or a path');
  }

  const duration = text(raw.duration ?? '1h', 'duration');
  const durationMatch = duration.match(/^(\d+(?:\.\d+)?)(ms|s|m|h)$/);
  if (!durationMatch || Number(durationMatch[1]) <= 0) {
    throw new Error('duration must be a positive k6 duration using ms, s, m, or h');
  }

  const thinkSeconds = Number(raw.thinkSeconds ?? 5);
  if (!Number.isFinite(thinkSeconds) || thinkSeconds < 1) {
    throw new Error('thinkSeconds must be a finite number >= 1');
  }

  const evidenceFile = text(raw.evidenceFile ?? `team-${teamIndex}.json`, 'evidenceFile');
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*\.json$/.test(evidenceFile) || evidenceFile.includes('..')) {
    throw new Error('evidenceFile must be a safe JSON filename');
  }

  const hasJeopardyGame = raw.jeoGame !== undefined && raw.jeoGame !== null;
  const hasJeopardyJwt = typeof raw.jeoJwt === 'string' && raw.jeoJwt.trim() !== '';
  if (hasJeopardyGame !== hasJeopardyJwt) {
    throw new Error('jeoGame and jeoJwt must either both be configured or both be absent');
  }
  const realisticCompetition = raw.realisticCompetition === true;
  let profile = null;
  let defenseKey = null;
  let competitionRunId = null;
  let eventCreatedAtMs = null;
  let competitionSeed = null;
  if (realisticCompetition) {
    competitionRunId = text(raw.competitionRunId, 'competitionRunId');
    if (!/^[A-Za-z0-9][A-Za-z0-9_-]{14,126}[A-Za-z0-9]$/.test(competitionRunId)) {
      throw new Error('competitionRunId must satisfy the evidence run-id contract');
    }
    eventCreatedAtMs = integer(raw.eventCreatedAtMs, 'eventCreatedAtMs');
    competitionSeed = text(raw.competitionSeed, 'competitionSeed');
    if (competitionSeed.length > 64) throw new Error('competitionSeed cannot exceed 64 characters');
    if (!isObject(raw.profile) || raw.competitionModelVersion !== 2) {
      throw new Error('competitive mode requires one model-v2 player profile');
    }
    profile = raw.profile;
    if (profile.version !== 2 || profile.index !== teamIndex) {
      throw new Error('competitive profile identity does not match teamIndex');
    }
    if (
      typeof profile.seed !== 'string' ||
      typeof profile.engagementTier !== 'string' ||
      typeof profile.specialty !== 'string' ||
      !Number.isFinite(profile.activity) ||
      !Number.isFinite(profile.offense) ||
      !Number.isFinite(profile.defense) ||
      !Number.isFinite(profile.koth) ||
      !Number.isFinite(profile.jeopardy) ||
      !Number.isFinite(profile.risk) ||
      !Number.isFinite(profile.persistence) ||
      !Number.isFinite(profile.exploration) ||
      !Number.isSafeInteger(profile.actionCreditsPerRound) ||
      profile.actionCreditsPerRound < 1 ||
      profile.actionCreditsPerRound > 6 ||
      !Number.isSafeInteger(profile.maxAttacks) ||
      profile.maxAttacks < 1 ||
      profile.maxAttacks > 3 ||
      profile.seed !== `${competitionSeed}:${teamIndex}`
    ) {
      throw new Error('competitive player profile is malformed');
    }
    defenseKey = text(raw.defenseKey, 'defenseKey');
  }

  const jeoChallenges = Array.isArray(raw.jeoChallenges)
    ? raw.jeoChallenges.map((entry, index) => ({
        challengeId: integer(entry?.challengeId, `jeoChallenges[${index}].challengeId`),
        flag: text(entry?.flag, `jeoChallenges[${index}].flag`),
        kind: text(entry?.kind ?? 'static', `jeoChallenges[${index}].kind`),
        category: realisticCompetition
          ? text(entry?.category, `jeoChallenges[${index}].category`)
          : 'Misc',
        difficulty: realisticCompetition ? Number(entry?.difficulty) : 1,
        attachmentPath:
          realisticCompetition && entry?.attachmentPath != null
            ? text(entry?.attachmentPath, `jeoChallenges[${index}].attachmentPath`)
            : null,
      }))
    : [];
  if (realisticCompetition && hasJeopardyGame && jeoChallenges.length === 0) {
    throw new Error('competitive Jeopardy clients require a public challenge catalog');
  }
  if (
    realisticCompetition &&
    (new Set(jeoChallenges.map((challenge) => challenge.challengeId)).size !== jeoChallenges.length ||
      jeoChallenges.some(
      (challenge) =>
        !['static', 'attachment', 'container'].includes(challenge.kind) ||
        !Number.isFinite(challenge.difficulty) ||
        challenge.difficulty < 1 ||
        challenge.difficulty > 10 ||
        (challenge.kind === 'attachment' && challenge.attachmentPath === null)
      ))
  ) {
    throw new Error('Jeopardy challenge catalog is malformed');
  }
  if (realisticCompetition && Number(raw.participationId) !== teamIds[teamIndex]) {
    throw new Error('competitive participationId must match the public roster entry');
  }
  if (realisticCompetition && evidenceFile !== 'summary.json') {
    throw new Error('competitive evidence must use the isolated summary.json path');
  }

  return Object.freeze({
    teamIndex,
    teamCount,
    jwt: text(raw.jwt, 'jwt'),
    adToken: text(raw.adToken, 'adToken'),
    gameId: integer(raw.gameId, 'gameId'),
    adChallengeId: integer(raw.adChallengeId, 'adChallengeId'),
    kothChallengeId: integer(raw.kothChallengeId, 'kothChallengeId'),
    ownListener,
    teamIds,
    participationId: realisticCompetition
      ? integer(raw.participationId, 'participationId')
      : teamIds[teamIndex],
    epochStartRound: integer(raw.epochStartRound, 'epochStartRound'),
    jeoGame: hasJeopardyGame ? integer(raw.jeoGame, 'jeoGame') : null,
    jeoJwt: hasJeopardyJwt ? raw.jeoJwt.trim() : null,
    jeoChallenges,
    target,
    duration,
    thinkSeconds,
    realisticCompetition,
    competitionModelVersion: realisticCompetition ? 2 : 1,
    competitionRunId,
    eventCreatedAtMs,
    competitionSeed,
    profile,
    defenseKey,
    evidenceFile,
  });
}

const CONFIG = readConfig();
if (!/^ad_[A-Za-z0-9_-]{43}$/.test(CONFIG.adToken)) {
  throw new Error('adToken must be an official A&D automation token');
}

// At the default five-second think time, even a Jeopardy-enabled bot stays below
// 150 requests/minute: 9 steady polls/attacks per iteration, a timeline every
// third iteration, and normally one submit per 30-second scoring round.
export const options = {
  scenarios: {
    team: {
      executor: 'constant-vus',
      vus: 1,
      duration: CONFIG.duration,
      gracefulStop: '15s',
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    server_5xx: ['rate==0'],
    platform_first_attempt_server_errors: ['count==0'],
    unexpected_non_2xx: ['rate==0'],
    platform_api_failure: ['rate==0'],
    request_timeout: ['rate==0'],
    rate_limited: ['rate==0'],
    semantic_invalid: ['rate==0'],
    vpn_attack_failure: ['rate==0'],
    iteration_runtime_errors: ['count==0'],
  },
};

const server5xx = new Rate('server_5xx');
const unexpectedNon2xx = new Rate('unexpected_non_2xx');
const platformApiFailure = new Rate('platform_api_failure');
const requestTimeout = new Rate('request_timeout');
const rateLimited = new Rate('rate_limited');
const platformFirstAttemptFailures = new Counter('platform_first_attempt_failures');
const platformFirstAttemptTimeouts = new Counter('platform_first_attempt_timeouts');
const platformFirstAttemptRateLimits = new Counter('platform_first_attempt_rate_limits');
const platformFirstAttemptServerErrors = new Counter('platform_first_attempt_server_errors');
const platformRetryAttempts = new Counter('platform_retry_attempts');
const platformRetryRecoveries = new Counter('platform_retry_recoveries');
const platformRetryExhaustions = new Counter('platform_retry_exhaustions');
const semanticInvalid = new Rate('semantic_invalid');
const vpnFirstAttemptFailure = new Rate('vpn_first_attempt_failure');
const vpnAttackFailure = new Rate('vpn_attack_failure');
const vpnRetryAttempts = new Counter('vpn_retry_attempts');
const acceptedCaptures = new Counter('accepted_captures');
const duplicateCaptures = new Counter('duplicate_captures');
const priorRoundCaptures = new Counter('prior_round_captures');
const captureAttempts = new Counter('capture_attempts');
const captureSubmissionReplays = new Counter('capture_submission_replays');
const terminalCaptureVerdicts = new Counter('terminal_capture_verdicts');
const roundsSeen = new Counter('rounds_seen');
const flagSyncWaits = new Counter('flag_sync_waits');
const flagDeliveryFailures = new Counter('flag_delivery_failures');
const iterationsCompleted = new Counter('iterations_completed');
const activeIterations = new Counter('active_iterations');
const idleIterations = new Counter('idle_iterations');
const iterationRuntimeErrors = new Counter('iteration_runtime_errors');
const exploitAttempts = new Counter('exploit_attempts');
const exploitPatched = new Counter('exploit_patched');
const exploitCaptures = new Counter('exploit_captures');
const defenseUpdates = new Counter('defense_updates');
const defenseIncidents = new Counter('defense_incidents');
const defenseRepairs = new Counter('defense_repairs');
const exploitUnavailable = new Counter('exploit_unavailable');
const actionCreditsSpent = new Counter('action_credits_spent');
const actionCreditDenials = new Counter('action_credit_denials');
const jeopardySubmissions = new Counter('jeopardy_submissions');
const jeopardyDetailsViewed = new Counter('jeopardy_details_viewed');
const jeopardyAttachmentDownloads = new Counter('jeopardy_attachment_downloads');
const jeopardyWrongGuesses = new Counter('jeopardy_wrong_guesses');
const jeopardyContainerCreates = new Counter('jeopardy_container_creates');
const jeopardyContainerDeletes = new Counter('jeopardy_container_deletes');
const jeopardyContainerFailures = new Counter('jeopardy_container_failures');
const kothCaptureAttempts = new Counter('koth_capture_attempts');
const kothCaptureSuccesses = new Counter('koth_capture_successes');
const kothOpeningClaims = new Counter('koth_opening_claims');
const kothTakeoverClaims = new Counter('koth_takeover_claims');
const kothResetRaces = new Counter('koth_reset_races');
const kothCaptureWindowClosed = new Counter('koth_capture_window_closed');
const kothCaptureIneligibleTransitions = new Counter('koth_capture_ineligible_transitions');
const kothCaptureStateUnavailable = new Counter('koth_capture_state_unavailable');
const kothCaptureAttemptFailures = new Counter('koth_capture_attempt_failures');
const kothCaptureRetryRecoveries = new Counter('koth_capture_retry_recoveries');
const kothCapturePendingStarts = new Counter('koth_capture_pending_starts');
const kothCaptureBurstExhaustions = new Counter('koth_capture_burst_exhaustions');
const kothCaptureTerminalWindows = new Counter('koth_capture_terminal_windows');
const kothCapturePendingInvariantFailures = new Counter('koth_capture_pending_invariant_failures');
const kothCaptureNetworkErrors = new Counter('koth_capture_network_errors');
const kothCaptureHttp4xx = new Counter('koth_capture_http_4xx');
const kothCaptureHttp5xx = new Counter('koth_capture_http_5xx');
const kothCaptureOtherStatusFailures = new Counter('koth_capture_other_status_failures');
const kothCaptureTargetUnavailable = new Counter('koth_capture_target_unavailable');
const kothTargetIdentityMismatches = new Counter('koth_target_identity_mismatches');
const kothPatchAttempts = new Counter('koth_patch_attempts');
const kothPatchSuccesses = new Counter('koth_patch_successes');
const kothPatchFailures = new Counter('koth_patch_failures');
const kothPatchHealthy = new Counter('koth_patch_healthy');
const kothPatchMumble = new Counter('koth_patch_mumble');
const kothPatchOffline = new Counter('koth_patch_offline');
const kothPatchRepairAttempts = new Counter('koth_patch_repair_attempts');
const kothPatchRepairs = new Counter('koth_patch_repairs');
const kothPatchRepairFailures = new Counter('koth_patch_repair_failures');
const kothPatchBlockedTakeovers = new Counter('koth_patch_blocked_takeovers');
const kothPatchBypassedTakeovers = new Counter('koth_patch_bypassed_takeovers');
const kothPatchHealthyHolds = new Counter('koth_patch_healthy_holds');
const kothPatchHoldChecks = new Counter('koth_patch_hold_checks');
const kothPatchHoldCheckFailures = new Counter('koth_patch_hold_check_failures');
const kothPatchHoldInterruptions = new Counter('koth_patch_hold_interruptions');
const kothPatchResetChecks = new Counter('koth_patch_reset_checks');
const kothPatchResetLosses = new Counter('koth_patch_reset_losses');
const kothPatchResetRetentions = new Counter('koth_patch_reset_retentions');
const kothPatchResetCheckFailures = new Counter('koth_patch_reset_check_failures');

const CUSTOM_EVIDENCE_COUNTERS = Object.freeze({
  platform_first_attempt_failures: platformFirstAttemptFailures,
  platform_first_attempt_timeouts: platformFirstAttemptTimeouts,
  platform_first_attempt_rate_limits: platformFirstAttemptRateLimits,
  platform_first_attempt_server_errors: platformFirstAttemptServerErrors,
  platform_retry_attempts: platformRetryAttempts,
  platform_retry_recoveries: platformRetryRecoveries,
  platform_retry_exhaustions: platformRetryExhaustions,
  vpn_retry_attempts: vpnRetryAttempts,
  accepted_captures: acceptedCaptures,
  duplicate_captures: duplicateCaptures,
  prior_round_captures: priorRoundCaptures,
  capture_attempts: captureAttempts,
  capture_submission_replays: captureSubmissionReplays,
  terminal_capture_verdicts: terminalCaptureVerdicts,
  rounds_seen: roundsSeen,
  flag_sync_waits: flagSyncWaits,
  flag_delivery_failures: flagDeliveryFailures,
  iterations_completed: iterationsCompleted,
  active_iterations: activeIterations,
  idle_iterations: idleIterations,
  iteration_runtime_errors: iterationRuntimeErrors,
  exploit_attempts: exploitAttempts,
  exploit_patched: exploitPatched,
  exploit_captures: exploitCaptures,
  defense_updates: defenseUpdates,
  defense_incidents: defenseIncidents,
  defense_repairs: defenseRepairs,
  exploit_unavailable: exploitUnavailable,
  action_credits_spent: actionCreditsSpent,
  action_credit_denials: actionCreditDenials,
  jeopardy_submissions: jeopardySubmissions,
  jeopardy_details_viewed: jeopardyDetailsViewed,
  jeopardy_attachment_downloads: jeopardyAttachmentDownloads,
  jeopardy_wrong_guesses: jeopardyWrongGuesses,
  jeopardy_container_creates: jeopardyContainerCreates,
  jeopardy_container_deletes: jeopardyContainerDeletes,
  jeopardy_container_failures: jeopardyContainerFailures,
  koth_capture_attempts: kothCaptureAttempts,
  koth_capture_successes: kothCaptureSuccesses,
  koth_opening_claims: kothOpeningClaims,
  koth_takeover_claims: kothTakeoverClaims,
  koth_reset_races: kothResetRaces,
  koth_capture_window_closed: kothCaptureWindowClosed,
  koth_capture_ineligible_transitions: kothCaptureIneligibleTransitions,
  koth_capture_state_unavailable: kothCaptureStateUnavailable,
  koth_capture_attempt_failures: kothCaptureAttemptFailures,
  koth_capture_retry_recoveries: kothCaptureRetryRecoveries,
  koth_capture_pending_starts: kothCapturePendingStarts,
  koth_capture_burst_exhaustions: kothCaptureBurstExhaustions,
  koth_capture_terminal_windows: kothCaptureTerminalWindows,
  koth_capture_pending_invariant_failures: kothCapturePendingInvariantFailures,
  koth_capture_network_errors: kothCaptureNetworkErrors,
  koth_capture_http_4xx: kothCaptureHttp4xx,
  koth_capture_http_5xx: kothCaptureHttp5xx,
  koth_capture_other_status_failures: kothCaptureOtherStatusFailures,
  koth_capture_target_unavailable: kothCaptureTargetUnavailable,
  koth_target_identity_mismatches: kothTargetIdentityMismatches,
  koth_patch_attempts: kothPatchAttempts,
  koth_patch_successes: kothPatchSuccesses,
  koth_patch_failures: kothPatchFailures,
  koth_patch_healthy: kothPatchHealthy,
  koth_patch_mumble: kothPatchMumble,
  koth_patch_offline: kothPatchOffline,
  koth_patch_repair_attempts: kothPatchRepairAttempts,
  koth_patch_repairs: kothPatchRepairs,
  koth_patch_repair_failures: kothPatchRepairFailures,
  koth_patch_blocked_takeovers: kothPatchBlockedTakeovers,
  koth_patch_bypassed_takeovers: kothPatchBypassedTakeovers,
  koth_patch_healthy_holds: kothPatchHealthyHolds,
  koth_patch_hold_checks: kothPatchHoldChecks,
  koth_patch_hold_check_failures: kothPatchHoldCheckFailures,
  koth_patch_hold_interruptions: kothPatchHoldInterruptions,
  koth_patch_reset_checks: kothPatchResetChecks,
  koth_patch_reset_losses: kothPatchResetLosses,
  koth_patch_reset_retentions: kothPatchResetRetentions,
  koth_patch_reset_check_failures: kothPatchResetCheckFailures,
});
const missingEvidenceCounters = MANDATORY_TEAM_EVIDENCE_COUNTERS.filter(
  (name) => name !== 'http_reqs' && !Object.prototype.hasOwnProperty.call(CUSTOM_EVIDENCE_COUNTERS, name)
);
if (missingEvidenceCounters.length > 0) {
  throw new Error(`mandatory evidence counters are not initialized: ${missingEvidenceCounters.join(', ')}`);
}

const adStateMs = new Trend('ad_state_ms', true);
const adTargetsMs = new Trend('ad_targets_ms', true);
const adScoreboardMs = new Trend('ad_scoreboard_ms', true);
const kothScoreboardMs = new Trend('koth_scoreboard_ms', true);
const kothTokenMs = new Trend('koth_token_ms', true);
const kothStateMs = new Trend('koth_state_ms', true);
const kothTimelineMs = new Trend('koth_timeline_ms', true);
const jeopardyGameMs = new Trend('jeopardy_game_ms', true);
const jeopardyDetailsMs = new Trend('jeopardy_details_ms', true);
const jeopardySubmitMs = new Trend('jeopardy_submit_ms', true);
const jeopardyContainerMs = new Trend('jeopardy_container_ms', true);
const jeopardyAssetMs = new Trend('jeopardy_asset_ms', true);
const vpnAttackMs = new Trend('vpn_attack_ms', true);
const adSubmitMs = new Trend('ad_submit_ms', true);
const kothCaptureMs = new Trend('koth_capture_ms', true);

const settledCaptures = new Set();
const observedRounds = new Set();
const attackedVictimsByRound = new Map();
const solvedJeopardyChallenges = new Set();
const viewedJeopardyChallenges = new Set();
const downloadedJeopardyChallenges = new Set();
const createdJeopardyContainers = new Map();
const jeopardyAttemptMemory = {};
const FLAG_PATTERN = /^flag\{[^\r\n]{1,240}\}$/;
const VPN_RETRY_DELAY_SECONDS = 0.1;
const RUN_STARTED_AT_MS = Date.now();
const DURATION_MATCH = CONFIG.duration.match(/^(\d+(?:\.\d+)?)(ms|s|m|h)$/);
const DURATION_MS = Number(DURATION_MATCH[1]) * { ms: 1, s: 1000, m: 60_000, h: 3_600_000 }[DURATION_MATCH[2]];
const JEOPARDY_CATALOG = CONFIG.realisticCompetition ? CONFIG.jeoChallenges : [];
let currentDefenseLevel = null;
let pendingDefenseRepair = null;
let currentActionBudget = null;
let opponentMemory = {};
let pendingAdCapture = null;
let pendingKothCapture = null;
let observedKothCycle = 0;
let observedKothTick = 0;
let observationsInKothTick = 0;
let currentKothPatch = null;
const attemptedKothWindows = new Set();
let lastJeopardyActionRound = 0;
let evidenceCountersInitialized = false;

const authParams = (jwt, kind) => ({
  headers: { Authorization: `Bearer ${jwt}` },
  tags: { kind },
  timeout: '5s',
  redirects: 0,
});

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function isRawObject(value) {
  return isObject(value) && !Object.prototype.hasOwnProperty.call(value, 'data');
}

function isSafeCount(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

function boundedRate(value) {
  return Number.isFinite(value) && value >= 0 && value <= 1;
}

function successfulStatus(response) {
  return response.status >= 200 && response.status < 300;
}

function platformRetryDelaySeconds(response, label, maximumRetryAfter = 2) {
  const retryAfter = response.headers?.['Retry-After'] ?? response.headers?.['retry-after'];
  const seed = CONFIG.profile?.seed ?? `${CONFIG.gameId}:${CONFIG.teamIndex}`;
  const fallback = 0.2 + keyedUnit(seed, 'platform-retry', label, __ITER) * 0.3;
  return boundedPlatformRetryDelay(retryAfter, fallback, maximumRetryAfter);
}

function issuePlatformRequest(method, url, body, params) {
  return http.request(method, url, body, params);
}

function recordPlatformFirstAttemptFailure(status) {
  const classification = classifyPlatformFirstAttemptFailure(status);
  if (classification === null) {
    throw new Error(`retryable platform status ${status} has no evidence classification`);
  }
  platformFirstAttemptFailures.add(1);
  if (classification === 'timeout') platformFirstAttemptTimeouts.add(1);
  if (classification === 'rateLimit') platformFirstAttemptRateLimits.add(1);
  if (classification === 'serverError') platformFirstAttemptServerErrors.add(1);
}

function settlePlatformRequest(
  response,
  method,
  url,
  body,
  params,
  label,
  retryable = isRetryablePlatformRequest(method, response.status),
  beforeRetry = null,
) {
  if (!retryable) return response;
  recordPlatformFirstAttemptFailure(response.status);
  platformRetryAttempts.add(1);
  // The container bucket returns ten seconds and the global window can return
  // up to sixty. Mutation retries honor that header; GET retry timing retains
  // the existing short bound.
  const maximumRetryAfter = method === 'GET' ? 2 : 60;
  sleep(platformRetryDelaySeconds(response, label, maximumRetryAfter));
  if (beforeRetry !== null) beforeRetry();
  const retry = issuePlatformRequest(method, url, body, {
    ...params,
    tags: { ...(params?.tags || {}), retry: '1' },
  });
  if (successfulStatus(retry)) platformRetryRecoveries.add(1);
  else platformRetryExhaustions.add(1);
  return retry;
}

function platformRequest(method, url, body, params, label) {
  return settlePlatformRequest(
    issuePlatformRequest(method, url, body, params),
    method,
    url,
    body,
    params,
    label,
  );
}

function platformGet(url, params, label) {
  return platformRequest('GET', url, null, params, label);
}

function platformPost(url, body, params, label) {
  return platformRequest('POST', url, body, params, label);
}

function platformDelete(url, body, params, label) {
  return platformRequest('DELETE', url, body, params, label);
}

function platformBatchGet(requests, labels) {
  const responses = http.batch(requests);
  return responses.map((response, index) => {
    const [method, url, body, params] = requests[index];
    if (method !== 'GET') throw new Error('platformBatchGet accepts only GET requests');
    return settlePlatformRequest(response, method, url, body, params, labels[index]);
  });
}

function recordHttp(response, label, trend, scope = 'platform') {
  const successful = successfulStatus(response);
  // Keep platform availability gates independent from the team-owned service
  // path. VPN quality has its own first-attempt and post-retry metrics below.
  if (scope === 'platform') {
    server5xx.add(response.status >= 500 && response.status < 600);
    unexpectedNon2xx.add(!successful);
    platformApiFailure.add(!successful);
    requestTimeout.add(response.status === 0);
    rateLimited.add(response.status === 429);
  }
  if (successful && trend) trend.add(response.timings.duration);
  check(response, { [`${label} returned 2xx`]: () => successful });
  return successful;
}

function readVpnFlag(victimUrl, nonSelfVictim, attempt) {
  const response = http.get(victimUrl, {
    tags: { kind: 'vpn_attack', attempt },
    timeout: '5s',
    redirects: 0,
  });
  const httpOk = recordHttp(response, `VPN victim flag read (${attempt})`, vpnAttackMs, 'vpn');
  const flag = httpOk && typeof response.body === 'string' ? response.body.trim() : '';
  return {
    response,
    httpOk,
    flag,
    valid: nonSelfVictim && FLAG_PATTERN.test(flag),
  };
}

function recordJson(response, label, trend, validator) {
  const successful = recordHttp(response, label, trend);
  let model = null;
  let valid = false;
  if (successful) {
    try {
      model = response.json();
      valid = validator(model);
    } catch (_) {
      valid = false;
    }
  }
  semanticInvalid.add(successful && !valid);
  check(response, {
    [`${label} returned a valid raw model`]: () => !successful || valid,
  });
  return valid ? model : null;
}

function validAdState(model) {
  return (
    isRawObject(model) &&
    isSafeCount(model.currentRound) &&
    typeof model.flagsReady === 'boolean' &&
    isSafeCount(model.flagDeliveryFailures) &&
    Array.isArray(model.services) &&
    model.services.every(
      (service) => isObject(service) && Number.isSafeInteger(service.challengeId) && service.challengeId > 0
    )
  );
}

function validAdTargets(model) {
  if (
    !isRawObject(model) ||
    !isSafeCount(model.currentRound) ||
    !Array.isArray(model.challenges) ||
    model.challenges.length === 0
  ) {
    return false;
  }
  const validChallenges = model.challenges.every((challenge) => {
      if (
        !isObject(challenge) ||
        !Number.isSafeInteger(challenge.challengeId) ||
        challenge.challengeId <= 0 ||
        !Array.isArray(challenge.teams)
      ) {
        return false;
      }
      // The combined Targets response includes both A&D services and the shared
      // KotH hill. A hill intentionally has no per-team targets; treating that
      // empty list as a malformed A&D roster made every team summary fail.
      if (isObject(challenge.hill)) {
        return (
          challenge.teams.length === 0 &&
          isSafeCount(challenge.hill.lastRefreshRound) &&
          isSafeCount(challenge.hill.cycleNumber) &&
          !Object.prototype.hasOwnProperty.call(challenge.hill, 'containerId') &&
          (challenge.hill.ip === null || typeof challenge.hill.ip === 'string') &&
          (challenge.hill.port === null ||
            (Number.isSafeInteger(challenge.hill.port) && challenge.hill.port > 0 && challenge.hill.port <= 65535))
        );
      }
      return (
        challenge.hill === null &&
        challenge.teams.length === CONFIG.teamCount - 1 &&
        challenge.teams.every((team) => isObject(team))
      );
    });
  if (!validChallenges) return false;
  return (
    publicAdNetworkTargets(
      model,
      CONFIG.adChallengeId,
      CONFIG.teamIds,
      CONFIG.participationId,
    ).length === CONFIG.teamCount - 1
  );
}

function validAdScoreboard(model) {
  if (
    !isRawObject(model) ||
    model.started !== true ||
    model.startRound !== CONFIG.epochStartRound ||
    !isSafeCount(model.latestRound) ||
    typeof model.fullySettled !== 'boolean' ||
    !Array.isArray(model.challenges) ||
    model.challenges.length === 0 ||
    !Array.isArray(model.teams) ||
    model.teams.length !== CONFIG.teamCount
  )
    return false;
  const participationIds = model.teams.map((team) => team?.participationId);
  if (new Set(participationIds).size !== CONFIG.teamCount) return false;
  return model.teams.every(
    (team) =>
      isObject(team) &&
      Number.isSafeInteger(team.rank) &&
      team.rank > 0 &&
      Number.isSafeInteger(team.participationId) &&
      CONFIG.teamIds.includes(team.participationId) &&
      Number.isFinite(team.settledTotal) &&
      team.settledTotal >= 0 &&
      Number.isFinite(team.projectedTotal) &&
      team.projectedTotal >= 0 &&
      boundedRate(team.offenseRate) &&
      boundedRate(team.defenseRate) &&
      boundedRate(team.slaRate) &&
      Array.isArray(team.services) &&
      team.services.every(
        (service) =>
          isObject(service) &&
          Number.isFinite(service.settledPoints) &&
          service.settledPoints >= 0 &&
          Number.isFinite(service.projectedPoints) &&
          service.projectedPoints >= 0 &&
          boundedRate(service.offenseRate) &&
          boundedRate(service.defenseRate) &&
          boundedRate(service.slaRate) &&
          isSafeCount(service.captureCount)
      )
  );
}

function publicAdOpponents(board, networkTargets) {
  if (!board) return [];
  const routable = new Set(networkTargets.map((target) => target.index));
  return board.teams
    .map((team) => ({
      index: CONFIG.teamIds.indexOf(team.participationId),
      rank: team.rank,
      settledTotal: team.settledTotal,
      projectedTotal: team.projectedTotal,
      defenseRate: team.defenseRate,
      slaRate: team.slaRate,
    }))
    .filter(
      (team) =>
        team.index >= 0 &&
        team.index !== CONFIG.teamIndex &&
        routable.has(team.index)
    );
}

function validKothScoreboard(model) {
  if (
    !isRawObject(model) ||
    model.started !== true ||
    typeof model.fullySettled !== 'boolean' ||
    !Number.isSafeInteger(model.epochTicks) ||
    model.epochTicks < 2 ||
    !Number.isSafeInteger(model.cycleTicks) ||
    model.cycleTicks < 1 ||
    model.epochTicks % model.cycleTicks !== 0 ||
    !Array.isArray(model.hills) ||
    !model.hills.some((hill) => hill?.challengeId === CONFIG.kothChallengeId) ||
    !Array.isArray(model.teams) ||
    model.teams.length !== CONFIG.teamCount
  )
    return false;
  const participationIds = model.teams.map((team) => team?.participationId);
  if (new Set(participationIds).size !== CONFIG.teamCount) return false;
  return model.teams.every(
    (team) =>
      isObject(team) &&
      Number.isSafeInteger(team.rank) &&
      team.rank > 0 &&
      Number.isSafeInteger(team.participationId) &&
      CONFIG.teamIds.includes(team.participationId) &&
      Number.isFinite(team.settledTotal) &&
      team.settledTotal >= 0 &&
      Number.isFinite(team.projectedTotal) &&
      team.projectedTotal >= 0 &&
      boundedRate(team.acquisitionRate) &&
      boundedRate(team.controlRate) &&
      boundedRate(team.reliabilityRate) &&
      Array.isArray(team.hills)
  );
}

function validKothToken(model) {
  const statuses = ['warmup', 'no-cycle-token', 'ready'];
  return (
    isRawObject(model) &&
    isSafeCount(model.round) &&
    statuses.includes(model.status) &&
    (model.token === null || typeof model.token === 'string') &&
    (model.status !== 'ready' || (typeof model.token === 'string' && model.token.length > 0))
  );
}

function validKothState(model) {
  return (
    isRawObject(model) &&
    isSafeCount(model.round) &&
    isSafeCount(model.cycleNumber) &&
    isSafeCount(model.cycleTick) &&
    Number.isSafeInteger(model.cycleTicks) &&
    model.cycleTicks > 0 &&
    typeof model.resetPhase === 'string' &&
    typeof model.isScorable === 'boolean' &&
    typeof model.eligibleNow === 'boolean' &&
    !Object.prototype.hasOwnProperty.call(model, 'containerId') &&
    (model.holderParticipationId === null ||
      (Number.isSafeInteger(model.holderParticipationId) && model.holderParticipationId > 0)) &&
    (model.provisionalClaimantParticipationId === null ||
      (Number.isSafeInteger(model.provisionalClaimantParticipationId) &&
        model.provisionalClaimantParticipationId > 0)) &&
    Array.isArray(model.cooldownParticipants)
  );
}

function validKothTimeline(model) {
  return (
    isRawObject(model) &&
    isSafeCount(model.latestRound) &&
    Array.isArray(model.teams) &&
    model.teams.length <= CONFIG.teamCount &&
    model.teams.every((team) => Array.isArray(team?.items))
  );
}

function validJeopardyGame(model) {
  return (
    isRawObject(model) &&
    model.id === CONFIG.jeoGame &&
    typeof model.title === 'string' &&
    Number.isFinite(model.start) &&
    Number.isFinite(model.end)
  );
}

function validJeopardyDetails(model) {
  return isRawObject(model) && isObject(model.challenges) && isSafeCount(model.challengeCount);
}

function victimForRound(round, networkTargets) {
  if (networkTargets.length === 0) return null;
  const elapsed = Math.max(0, round - CONFIG.epochStartRound);
  const offset = (elapsed % (CONFIG.teamCount - 1)) + 1;
  const preferredIndex = (CONFIG.teamIndex + offset) % CONFIG.teamCount;
  return (
    networkTargets.find((target) => target.index === preferredIndex) ??
    networkTargets[elapsed % networkTargets.length]
  );
}

function elapsedState() {
  const elapsedMs = Math.max(0, Date.now() - RUN_STARTED_AT_MS);
  return {
    elapsedSeconds: elapsedMs / 1000,
    progress: Math.min(1, elapsedMs / DURATION_MS),
  };
}

function initializeEvidenceCounters() {
  if (!CONFIG.realisticCompetition || evidenceCountersInitialized) return;
  // k6 omits untouched custom metrics from handleSummary. Emit a genuine zero
  // observation for every schema-v9 counter during the first VU iteration so
  // absent/renamed/truncated metrics remain distinguishable from real zeroes.
  for (const counter of Object.values(CUSTOM_EVIDENCE_COUNTERS)) counter.add(0);
  evidenceCountersInitialized = true;
}

function ensureActionBudget(round) {
  if (!CONFIG.realisticCompetition) return;
  if (currentActionBudget?.round !== round) {
    currentActionBudget = buildRoundActionBudget(CONFIG.profile, round);
  }
}

function useActionCredit(action) {
  if (!CONFIG.realisticCompetition) return true;
  if (!currentActionBudget || !canSpendActionCredit(currentActionBudget, action)) {
    actionCreditDenials.add(1);
    return false;
  }
  const before = currentActionBudget.remaining;
  currentActionBudget = spendActionCredit(currentActionBudget, action);
  actionCreditsSpent.add(before - currentActionBudget.remaining);
  return true;
}

function thinkDelay(multiplier = 1) {
  if (!CONFIG.realisticCompetition) return CONFIG.thinkSeconds * multiplier;
  return playerThinkDelay(CONFIG.profile, __ITER, multiplier);
}

function patchIncident(desired, round) {
  if (desired <= currentDefenseLevel) return 'healthy';
  const roll = keyedUnit(CONFIG.profile.seed, 'patch-incident', desired, round);
  const offlineChance = 0.02 + (1 - CONFIG.profile.defense) * 0.12 + CONFIG.profile.risk * 0.03;
  const mumbleChance = 0.05 + (1 - CONFIG.profile.defense) * 0.15;
  if (roll < offlineChance) return 'offline';
  if (roll < offlineChance + mumbleChance) return 'mumble';
  return 'healthy';
}

function performDefenseRequest(endpoint, label) {
  let successful = false;
  for (const attempt of ['first', 'retry']) {
    const response = http.get(endpoint, {
      headers: { 'X-Defense-Key': CONFIG.defenseKey },
      tags: { kind: 'vpn_defense', attempt },
      timeout: '5s',
      redirects: 0,
    });
    successful = recordHttp(response, `${label} (${attempt})`, null, 'vpn');
    if (attempt === 'first') vpnFirstAttemptFailure.add(!successful);
    if (successful) break;
    if (attempt === 'first') {
      vpnRetryAttempts.add(1);
      sleep(VPN_RETRY_DELAY_SECONDS);
    }
  }
  vpnAttackFailure.add(!successful);
  return successful;
}

function settlePendingDefenseRepair(countCompetitiveRepair = true) {
  if (pendingDefenseRepair === null) return;
  const endpoint = `http://${CONFIG.ownListener}/defense?repair=1`;
  if (performDefenseRequest(endpoint, 'own service repair')) {
    pendingDefenseRepair = null;
    if (countCompetitiveRepair) defenseRepairs.add(1);
  }
}

function updateOwnDefense(progress, currentRound) {
  if (!CONFIG.realisticCompetition) return;
  if (currentDefenseLevel === null) currentDefenseLevel = 0;
  if (pendingDefenseRepair && currentRound >= pendingDefenseRepair.repairRound) {
    if (!useActionCredit('patch')) return;
    settlePendingDefenseRepair();
    return;
  }
  const desired = defenseLevelAt(CONFIG.profile, progress);
  if (desired === currentDefenseLevel) return;
  if (!useActionCredit('patch')) return;
  const incident = patchIncident(desired, currentRound);
  const endpoint =
    `http://${CONFIG.ownListener}/defense?level=${desired}` +
    `&incident=${incident}`;
  if (performDefenseRequest(endpoint, 'own service patch update')) {
    currentDefenseLevel = desired;
    defenseUpdates.add(1);
    if (incident !== 'healthy') {
      defenseIncidents.add(1);
      const repairDelay = 1 + Math.floor(keyedUnit(CONFIG.profile.seed, 'repair-delay', desired) * 2);
      pendingDefenseRepair = { incident, repairRound: currentRound + repairDelay };
    }
  }
}

function readVpnExploit(target, technique, attempt) {
  const victimUrl =
    `http://${target.host}:${target.port}/exploit?team=${encodeURIComponent(target.participationId)}` +
    `&technique=${technique}`;
  exploitAttempts.add(1);
  const response = http.get(victimUrl, {
    tags: { kind: 'vpn_exploit', technique: String(technique), attempt },
    timeout: '5s',
    redirects: 0,
  });
  if (response.status > 0) vpnAttackMs.add(response.timings.duration);
  const patched = response.status === 403;
  const mumble = response.status === 200 && typeof response.body === 'string' && response.body.trim() === 'service-mumble';
  const unavailable = response.status === 503 || mumble;
  const httpOk = response.status >= 200 && response.status < 300;
  const flag = httpOk && typeof response.body === 'string' ? response.body.trim() : '';
  const valid = FLAG_PATTERN.test(flag);
  if (patched) exploitPatched.add(1);
  if (unavailable) exploitUnavailable.add(1);
  if (valid) exploitCaptures.add(1);
  return { response, patched, unavailable, httpOk, flag, valid };
}

function deleteJeopardyContainer(challengeId) {
  const response = platformDelete(
    `${CONFIG.target}/api/game/${CONFIG.jeoGame}/container/${challengeId}`,
    null,
    authParams(CONFIG.jeoJwt, 'jeopardy_container_delete'),
    'jeopardy_container_delete',
  );
  const successful = recordHttp(response, 'Jeopardy container delete');
  if (successful) {
    createdJeopardyContainers.delete(challengeId);
    jeopardyContainerDeletes.add(1);
  } else {
    jeopardyContainerFailures.add(1);
  }
  return successful;
}

function cleanupJeopardyContainers() {
  for (const challengeId of createdJeopardyContainers.keys()) {
    deleteJeopardyContainer(challengeId);
  }
}

function attemptJeopardySolve(progress, currentRound) {
  if (!CONFIG.realisticCompetition || CONFIG.jeoGame === null) return;
  if (progress >= 0.97) {
    if (createdJeopardyContainers.size > 0) cleanupJeopardyContainers();
    return;
  }
  const nowMs = Date.now();
  const liveMemory = Object.fromEntries(
    JEOPARDY_CATALOG.map((challenge) => {
      const prior = jeopardyAttemptMemory[challenge.challengeId] ?? {
        attempts: 0,
        lastAttemptRound: 0,
      };
      const container = createdJeopardyContainers.get(challenge.challengeId);
      return [
        challenge.challengeId,
        {
          ...prior,
          solved: solvedJeopardyChallenges.has(challenge.challengeId),
          viewed: viewedJeopardyChallenges.has(challenge.challengeId),
          downloaded: downloadedJeopardyChallenges.has(challenge.challengeId),
          containerCreated: container !== undefined,
          containerReady:
            container !== undefined &&
            nowMs - container.createdAtMs >= container.holdSeconds * 1000,
        },
      ];
    })
  );
  const intent = jeopardyIntent(CONFIG.profile, JEOPARDY_CATALOG, liveMemory, {
    round: currentRound,
    progress,
    availableCredits: currentActionBudget?.remaining ?? 0,
    actedThisRound: lastJeopardyActionRound === currentRound,
  });
  if (intent.action === 'wait' || intent.challengeId === null) return;
  const challenge = JEOPARDY_CATALOG.find(
    (entry) => entry.challengeId === intent.challengeId
  );
  if (!challenge) {
    semanticInvalid.add(true);
    return;
  }
  lastJeopardyActionRound = currentRound;

  if (intent.action === 'view') {
    const detail = platformGet(
      `${CONFIG.target}/api/game/${CONFIG.jeoGame}/challenges/${challenge.challengeId}`,
      authParams(CONFIG.jeoJwt, 'jeopardy_challenge_detail'),
      'jeopardy_challenge_detail',
    );
    if (recordHttp(detail, 'Jeopardy challenge detail', jeopardyDetailsMs)) {
      viewedJeopardyChallenges.add(challenge.challengeId);
      jeopardyDetailsViewed.add(1);
    }
    return;
  }

  if (intent.action === 'download') {
    if (!useActionCredit('attachmentSolve')) return;
    const asset = platformGet(
      `${CONFIG.target}${challenge.attachmentPath}`,
      authParams(CONFIG.jeoJwt, 'jeopardy_attachment'),
      'jeopardy_attachment',
    );
    if (recordHttp(asset, 'Jeopardy attachment download', jeopardyAssetMs)) {
      downloadedJeopardyChallenges.add(challenge.challengeId);
      jeopardyAttachmentDownloads.add(1);
    }
    return;
  }

  if (intent.action === 'createContainer') {
    if (!useActionCredit('containerSolve')) return;
    const response = platformPost(
      `${CONFIG.target}/api/game/${CONFIG.jeoGame}/container/${challenge.challengeId}`,
      null,
      authParams(CONFIG.jeoJwt, 'jeopardy_container_create'),
      'jeopardy_container_create',
    );
    if (recordHttp(response, 'Jeopardy container create', jeopardyContainerMs)) {
      createdJeopardyContainers.set(challenge.challengeId, {
        createdAtMs: Date.now(),
        holdSeconds:
          8 +
          Math.floor(
            keyedUnit(
              CONFIG.profile.seed,
              'jeopardy-container-hold',
              challenge.challengeId,
              currentRound,
            ) * 23
          ),
      });
      jeopardyContainerCreates.add(1);
    } else {
      jeopardyContainerFailures.add(1);
    }
    return;
  }

  if (!['research', 'submitWrong', 'submitCorrect'].includes(intent.action)) {
    semanticInvalid.add(true);
    return;
  }
  if (!useActionCredit('staticSolve')) return;
  const previous = jeopardyAttemptMemory[challenge.challengeId] ?? {
    attempts: 0,
    lastAttemptRound: 0,
  };
  jeopardyAttemptMemory[challenge.challengeId] = {
    attempts: previous.attempts + 1,
    lastAttemptRound: currentRound,
  };
  if (intent.action === 'research') return;

  const submitFlag =
    intent.action === 'submitCorrect'
      ? challenge.flag
      : `flag{ordinary_wrong_${CONFIG.teamIndex}_${challenge.challengeId}_${previous.attempts}}`;
  const response = http.post(
    `${CONFIG.target}/api/game/${CONFIG.jeoGame}/challenges/${challenge.challengeId}`,
    JSON.stringify({ flag: submitFlag }),
    {
      ...authParams(CONFIG.jeoJwt, 'jeopardy_submit'),
      headers: {
        Authorization: `Bearer ${CONFIG.jeoJwt}`,
        'Content-Type': 'application/json',
      },
    }
  );
  if (recordHttp(response, 'Jeopardy flag submission', jeopardySubmitMs)) {
    if (intent.action === 'submitWrong') {
      jeopardyWrongGuesses.add(1);
    } else {
      solvedJeopardyChallenges.add(challenge.challengeId);
      jeopardySubmissions.add(1);
      if (challenge.kind === 'container') deleteJeopardyContainer(challenge.challengeId);
    }
  }
}

function hillFromTargets(targetsModel) {
  const hill = targetsModel?.challenges?.find(
    (challenge) => challenge?.challengeId === CONFIG.kothChallengeId
  )?.hill;
  return hill && typeof hill.ip === 'string' && Number.isSafeInteger(hill.port) ? hill : null;
}

function refreshKothCaptureState() {
  return recordJson(
    platformGet(
      `${CONFIG.target}/api/game/${CONFIG.gameId}/ad/koth/${CONFIG.kothChallengeId}/state`,
      authParams(CONFIG.jwt, 'koth_race_check'),
      'koth_race_check',
    ),
    'KotH reset-race state',
    kothStateMs,
    validKothState
  );
}

function refreshOwnedKothPatchState(tokenModel, expectedState) {
  const latestState = refreshKothCaptureState();
  if (
    !latestState ||
    latestState.round !== expectedState.round ||
    latestState.cycleNumber !== expectedState.cycleNumber ||
    latestState.cycleTick !== expectedState.cycleTick ||
    latestState.cycleTicks !== expectedState.cycleTicks ||
    latestState.resetPhase !== 'Active' ||
    latestState.isScorable !== true ||
    latestState.eligibleNow !== true ||
    !kothCapabilityMatchesState(tokenModel, latestState) ||
    kothControllerParticipationId(latestState) !== CONFIG.participationId
  ) {
    return null;
  }
  return latestState;
}

function observeKothState(stateModel) {
  if (!stateModel) return;
  if (
    stateModel.cycleNumber !== observedKothCycle ||
    stateModel.cycleTick !== observedKothTick
  ) {
    observedKothCycle = stateModel.cycleNumber;
    observedKothTick = stateModel.cycleTick;
    observationsInKothTick = 1;
  } else {
    observationsInKothTick++;
  }
}

function recordKothCaptureAttemptFailure(response) {
  kothCaptureAttemptFailures.add(1);
  if (response.status === 0) kothCaptureNetworkErrors.add(1);
  else if (response.status >= 400 && response.status < 500) kothCaptureHttp4xx.add(1);
  else if (response.status >= 500 && response.status < 600) kothCaptureHttp5xx.add(1);
  else kothCaptureOtherStatusFailures.add(1);
}

function clearPendingKothCapture(outcome) {
  if (outcome === 'resetRace') kothResetRaces.add(1);
  if (outcome === 'windowClosed') {
    kothCaptureWindowClosed.add(1);
    if (isKothTerminalWindow(pendingKothCapture, outcome)) {
      kothCaptureTerminalWindows.add(1);
    }
  }
  if (outcome === 'ineligibleTransition') kothCaptureIneligibleTransitions.add(1);
  pendingKothCapture = null;
}

function resolvePendingKothCapture(stateModel) {
  if (pendingKothCapture === null) return null;
  const outcome = classifyKothPendingTransition(pendingKothCapture, stateModel);
  if (outcome === 'stateUnavailable') {
    // A failed state read is not authoritative. Keep the logical capture open
    // so a later state observation or successful write can settle it.
    kothCaptureStateUnavailable.add(1);
  } else if (outcome !== 'pending') {
    clearPendingKothCapture(outcome);
  }
  return outcome;
}

function startPendingKothCapture(writeKey, phase, stateModel, technique) {
  if (pendingKothCapture === null) {
    pendingKothCapture = {
      writeKey,
      phase,
      technique,
      cycleNumber: stateModel.cycleNumber,
      cycleTick: stateModel.cycleTick,
      attempts: 0,
      burstExhausted: false,
    };
    kothCapturePendingStarts.add(1);
    return;
  }
  if (pendingKothCapture.writeKey !== writeKey) {
    // A valid authoritative transition is resolved before this point. Seeing
    // a second key here means the harness would otherwise overwrite evidence.
    kothCapturePendingInvariantFailures.add(1);
    pendingKothCapture = null;
    pendingKothCapture = {
      writeKey,
      phase,
      technique,
      cycleNumber: stateModel.cycleNumber,
      cycleTick: stateModel.cycleTick,
      attempts: 0,
      burstExhausted: false,
    };
    kothCapturePendingStarts.add(1);
  }
}

function kothDefenseHeader(response) {
  return String(
    response.headers?.['X-Koth-Defense'] ?? response.headers?.['x-koth-defense'] ?? ''
  ).toLowerCase();
}

function kothInstanceHeader(response) {
  return String(
    response.headers?.['X-Koth-Instance'] ?? response.headers?.['x-koth-instance'] ?? ''
  );
}

function kothServiceStatus(response) {
  return parseKothServiceStatus(
    typeof response.body === 'string' ? response.body : '',
    kothInstanceHeader(response),
  );
}

function verifyKothPatchReset(hill, stateModel) {
  if (
    currentKothPatch === null ||
    currentKothPatch.resetChecked ||
    currentKothPatch.cycleNumber === stateModel.cycleNumber ||
    stateModel.resetPhase !== 'Active' ||
    !stateModel.eligibleNow
  ) {
    return;
  }
  currentKothPatch.resetChecked = true;
  kothPatchResetChecks.add(1);
  const response = http.get(`http://${hill.ip}:${hill.port}/status`, {
    tags: { kind: 'vpn_koth_patch_reset_check' },
    timeout: '5s',
    redirects: 0,
  });
  const successful = recordHttp(response, 'KotH patch reset replacement check', null, 'vpn');
  const status = successful ? kothServiceStatus(response) : null;
  if (!status) kothPatchResetCheckFailures.add(1);
  else if (status.instance === currentKothPatch.instance) kothPatchResetRetentions.add(1);
  else if (isReplacementKothInstance(currentKothPatch.instance, status)) {
    kothPatchResetLosses.add(1);
  } else {
    kothPatchResetCheckFailures.add(1);
  }
}

function attemptKothPatchLifecycle(hill, tokenModel, stateModel, active, progress) {
  if (
    !CONFIG.realisticCompetition ||
    !active ||
    progress >= 0.97 ||
    !stateModel
  ) {
    return;
  }

  const patchInCurrentCycle =
    currentKothPatch?.cycleNumber === stateModel.cycleNumber;
  const controllerParticipationId = kothControllerParticipationId(stateModel);
  const ownsCurrentPatch =
    patchInCurrentCycle &&
    stateModel.resetPhase === 'Active' &&
    stateModel.isScorable === true &&
    stateModel.eligibleNow === true &&
    controllerParticipationId === CONFIG.participationId;
  const skippedAuthoritativeRound =
    patchInCurrentCycle &&
    Number.isSafeInteger(currentKothPatch.lastObservedControlRound) &&
    stateModel.round > currentKothPatch.lastObservedControlRound + 1;
  if (
    patchInCurrentCycle &&
    stateModel.resetPhase === 'Active' &&
    stateModel.isScorable === true &&
    (!ownsCurrentPatch || skippedAuthoritativeRound) &&
    !currentKothPatch.controlInterrupted
  ) {
    currentKothPatch.controlInterrupted = true;
    kothPatchHoldInterruptions.add(1);
  }
  if (ownsCurrentPatch && !currentKothPatch.controlInterrupted) {
    currentKothPatch.lastObservedControlRound = Math.max(
      currentKothPatch.lastObservedControlRound,
      stateModel.round,
    );
  }

  if (!hill || !kothCapabilityMatchesState(tokenModel, stateModel)) return;
  verifyKothPatchReset(hill, stateModel);

  if (ownsCurrentPatch && currentKothPatch.incident !== 'healthy') {
    if (!kothPatchRepairReady(currentKothPatch.patchedAtRound, stateModel.round)) return;
    const latestState = refreshOwnedKothPatchState(tokenModel, stateModel);
    if (
      !latestState ||
      !kothPatchRepairReady(currentKothPatch.patchedAtRound, latestState.round)
    ) {
      return;
    }
    if (!useActionCredit('patch')) return;
    kothPatchRepairAttempts.add(1);
    const response = http.get(`http://${hill.ip}:${hill.port}/defense?repair=1`, {
      headers: { 'X-Koth-Token': tokenModel.token },
      tags: { kind: 'vpn_koth_patch_repair' },
      timeout: '5s',
      redirects: 0,
    });
    if (recordHttp(response, 'KotH patch repair', null, 'vpn')) {
      currentKothPatch.incident = 'healthy';
      currentKothPatch.healthySinceRound = latestState.round;
      currentKothPatch.holdCheckAttempted = false;
      kothPatchRepairs.add(1);
    } else {
      kothPatchRepairFailures.add(1);
    }
    return;
  }
  if (
    ownsCurrentPatch &&
    currentKothPatch.incident === 'healthy' &&
    !currentKothPatch.holdRecorded &&
    !currentKothPatch.holdCheckAttempted &&
    !currentKothPatch.controlInterrupted &&
    stateModel.round > currentKothPatch.healthySinceRound
  ) {
    currentKothPatch.holdCheckAttempted = true;
    kothPatchHoldChecks.add(1);
    const response = http.get(`http://${hill.ip}:${hill.port}/status`, {
      tags: { kind: 'vpn_koth_patch_hold_check' },
      timeout: '5s',
      redirects: 0,
    });
    const successful = recordHttp(response, 'KotH healthy hold check', null, 'vpn');
    const status = successful ? kothServiceStatus(response) : null;
    if (kothHealthyHoldStatusMatches(currentKothPatch, status)) {
      currentKothPatch.holdRecorded = true;
      kothPatchHealthyHolds.add(1);
    } else {
      kothPatchHoldCheckFailures.add(1);
    }
  }

  const intent = kothPatchIntent(CONFIG.profile, stateModel, {
    active,
    ownParticipationId: CONFIG.participationId,
    availableCredits: currentActionBudget?.remaining ?? 0,
    patchedCycleNumber: currentKothPatch?.cycleNumber ?? 0,
  });
  if (!intent.attempt) return;
  const latestState = refreshOwnedKothPatchState(tokenModel, stateModel);
  if (!latestState) return;
  const latestIntent = kothPatchIntent(CONFIG.profile, latestState, {
    active,
    ownParticipationId: CONFIG.participationId,
    availableCredits: currentActionBudget?.remaining ?? 0,
    patchedCycleNumber: currentKothPatch?.cycleNumber ?? 0,
  });
  if (!latestIntent.attempt || !useActionCredit('patch')) return;
  kothPatchAttempts.add(1);
  const response = http.get(
    `http://${hill.ip}:${hill.port}/defense?level=${latestIntent.level}&incident=${latestIntent.incident}`,
    {
      headers: { 'X-Koth-Token': tokenModel.token },
      tags: { kind: 'vpn_koth_patch' },
      timeout: '5s',
      redirects: 0,
    }
  );
  if (!recordHttp(response, 'KotH holder patch', null, 'vpn')) {
    kothPatchFailures.add(1);
    return;
  }
  const instance = kothInstanceHeader(response);
  if (!/^[a-f0-9]{16}$/.test(instance)) {
    kothPatchFailures.add(1);
    return;
  }
  kothPatchSuccesses.add(1);
  if (latestIntent.incident === 'mumble') kothPatchMumble.add(1);
  else if (latestIntent.incident === 'offline') kothPatchOffline.add(1);
  else kothPatchHealthy.add(1);
  currentKothPatch = {
    cycleNumber: latestState.cycleNumber,
    instance,
    level: latestIntent.level,
    incident: latestIntent.incident,
    patchedAtRound: latestState.round,
    healthySinceRound: latestState.round,
    holdRecorded: false,
    holdCheckAttempted: false,
    controlInterrupted: false,
    lastObservedControlRound: latestState.round,
    resetChecked: false,
  };
}

function attemptKothCapture(
  targetsModel,
  tokenModel,
  stateModel,
  scoreboardModel,
  active,
  progress
) {
  if (
    !CONFIG.realisticCompetition ||
    !active ||
    progress >= 0.97 ||
    !targetsModel ||
    !kothCapabilityMatchesState(tokenModel, stateModel) ||
    !stateModel?.isScorable ||
    !stateModel.eligibleNow ||
    stateModel.resetPhase !== 'Active' ||
    stateModel.cycleNumber < 1 ||
    stateModel.cycleTick < 1
  ) {
    return;
  }
  if (!kothTargetMatchesState(targetsModel, stateModel, CONFIG.kothChallengeId)) {
    kothTargetIdentityMismatches.add(1);
    return;
  }
  const hill = hillFromTargets(targetsModel);
  if (!hill) {
    if (pendingKothCapture !== null) kothCaptureTargetUnavailable.add(1);
    return;
  }

  const windowKey = `${stateModel.cycleNumber}:${stateModel.cycleTick}`;
  const writeKey = `${windowKey}:${CONFIG.teamIndex}`;
  const maxAttempts = 2;
  let phase;
  let technique;
  if (pendingKothCapture !== null) {
    if (
      pendingKothCapture.writeKey !== writeKey ||
      pendingKothCapture.attempts >= maxAttempts
    ) {
      return;
    }
    phase = pendingKothCapture.phase;
    technique = pendingKothCapture.technique;
  } else {
    const ownScore = scoreboardModel?.teams?.find(
      (team) => team?.participationId === CONFIG.participationId
    );
    const intent = kothIntent(CONFIG.profile, stateModel, {
      competitionSeed: CONFIG.competitionSeed,
      active,
      attempted: attemptedKothWindows.has(windowKey),
      availableCredits: currentActionBudget?.remaining ?? 0,
      observationsInTick: Math.max(1, observationsInKothTick),
      ownParticipationId: CONFIG.participationId,
      teamCount: CONFIG.teamCount,
      scoreboardRank: ownScore?.rank ?? CONFIG.teamCount,
    });
    if (!intent.attempt || !useActionCredit('kothClaim')) return;
    attemptedKothWindows.add(windowKey);
    phase = intent.phase;
    technique = intent.technique;
  }

  // One write per observation models a player retrying after seeing the next
  // current target. It avoids an artificial same-millisecond retry burst while
  // retaining one bounded logical claim and its exact evidence accounting.
  kothCaptureAttempts.add(1);
  const response = http.get(`http://${hill.ip}:${hill.port}/capture?technique=${technique}`, {
    headers: { 'X-Koth-Token': tokenModel.token },
    tags: { kind: 'vpn_koth_capture' },
    timeout: '5s',
    redirects: 0,
  });
  const successful = recordHttp(response, 'KotH network capture', kothCaptureMs, 'vpn');
  if (successful) {
    if (pendingKothCapture !== null) {
      if (pendingKothCapture.writeKey === writeKey) kothCaptureRetryRecoveries.add(1);
      else kothCapturePendingInvariantFailures.add(1);
      pendingKothCapture = null;
    }
    kothCaptureSuccesses.add(1);
    if (phase === 'takeover') {
      kothTakeoverClaims.add(1);
      if (kothDefenseHeader(response) === 'bypassed') kothPatchBypassedTakeovers.add(1);
    }
    else kothOpeningClaims.add(1);
    return;
  }

  recordKothCaptureAttemptFailure(response);
  const defense = kothDefenseHeader(response);
  if (defense === 'blocked') kothPatchBlockedTakeovers.add(1);
  if (['blocked', 'mumble', 'offline'].includes(defense)) return;
  startPendingKothCapture(writeKey, phase, stateModel, technique);
  pendingKothCapture.attempts++;
  if (pendingKothCapture.attempts >= maxAttempts) {
    kothCaptureBurstExhaustions.add(1);
    pendingKothCapture.burstExhausted = true;
  }
}

const TERMINAL_SUBMIT_STATUSES = new Set([
  'wrong',
  'expired',
  'self_attack',
  'not_started',
  'ended',
  'paused',
  'rejected',
]);

function validSubmitOutcome(model, flag) {
  if (
    !isRawObject(model) ||
    !isSafeCount(model.acceptedCount) ||
    !Array.isArray(model.results) ||
    model.results.length !== 1
  )
    return null;
  const result = model.results[0];
  if (!isObject(result) || result.flag !== flag || typeof result.status !== 'string') return null;
  const plantedRound = result.flagPlantedAtRound;
  const validPlantedRound = Number.isSafeInteger(plantedRound) && plantedRound >= 1;
  if (result.status === 'accepted' && model.acceptedCount === 1 && validPlantedRound) {
    return { status: 'accepted', plantedRound };
  }
  if (result.status === 'duplicate' && model.acceptedCount === 0 && validPlantedRound) {
    return { status: 'duplicate', plantedRound };
  }
  if (
    TERMINAL_SUBMIT_STATUSES.has(result.status) &&
    model.acceptedCount === 0 &&
    (plantedRound == null || validPlantedRound)
  ) {
    return {
      status: result.status,
      plantedRound: validPlantedRound ? plantedRound : null,
    };
  }
  return null;
}

function submitPendingAdCapture(replay) {
  if (pendingAdCapture === null) return true;
  const capture = pendingAdCapture;
  if (replay) captureSubmissionReplays.add(1);
  const url = `${CONFIG.target}/api/Game/${CONFIG.gameId}/Ad/Submit`;
  const body = JSON.stringify({ flags: [capture.flag] });
  const params = {
    headers: {
      Authorization: `Bearer ${CONFIG.adToken}`,
      'Content-Type': 'application/json',
    },
    tags: { kind: 'ad_token_submit', replay: replay ? '1' : '0' },
    timeout: '5s',
    redirects: 0,
  };
  const firstResponse = issuePlatformRequest('POST', url, body, params);
  const submitResponse = settlePlatformRequest(
    firstResponse,
    'POST',
    url,
    body,
    params,
    'ad_token_submit',
    isRetryablePlatformStatus(firstResponse.status),
    () => captureSubmissionReplays.add(1),
  );
  const submitHttpOk = recordHttp(submitResponse, 'A&D exact flag submit', adSubmitMs);
  let submitModel = null;
  if (submitHttpOk) {
    try {
      submitModel = submitResponse.json();
    } catch (_) {
      submitModel = null;
    }
  }
  const outcome = submitHttpOk ? validSubmitOutcome(submitModel, capture.flag) : null;
  semanticInvalid.add(submitHttpOk && outcome === null);
  check(submitResponse, {
    'A&D captured flag received a valid verdict': () => outcome !== null,
  });
  if (outcome === null) return false;

  if (outcome.status === 'accepted') acceptedCaptures.add(1);
  else if (outcome.status === 'duplicate') duplicateCaptures.add(1);
  else terminalCaptureVerdicts.add(1);
  if (
    Number.isSafeInteger(outcome.plantedRound) &&
    outcome.plantedRound < capture.observedRound
  ) {
    priorRoundCaptures.add(1);
  }
  settledCaptures.add(capture.key);
  pendingAdCapture = null;
  return true;
}

function startAdCapture(captureKey, flag, observedRound) {
  if (pendingAdCapture !== null || settledCaptures.has(captureKey)) return false;
  captureAttempts.add(1);
  pendingAdCapture = Object.freeze({
    key: captureKey,
    flag,
    observedRound,
  });
  submitPendingAdCapture(false);
  return true;
}

function drainExistingWork() {
  if (pendingAdCapture !== null) submitPendingAdCapture(true);
  if (pendingKothCapture !== null) {
    resolvePendingKothCapture(refreshKothCaptureState());
  }
  if (pendingDefenseRepair !== null) {
    const hasRetainedCredit =
      currentActionBudget !== null && canSpendActionCredit(currentActionBudget, 'patch');
    if (hasRetainedCredit) {
      useActionCredit('patch');
      settlePendingDefenseRepair(true);
    }
  }
  cleanupJeopardyContainers();
  iterationsCompleted.add(1);
  sleep(thinkDelay(2));
}

function runTeamIteration() {
  const base = CONFIG.target;
  const { elapsedSeconds, progress } = elapsedState();
  initializeEvidenceCounters();
  if (CONFIG.realisticCompetition && progress >= 0.97) {
    // The closing window only settles work already in flight. It must not wake
    // idle players or create a synchronized final burst of attacks and claims.
    idleIterations.add(1);
    drainExistingWork();
    return;
  }
  const active = !CONFIG.realisticCompetition || isProfileActive(CONFIG.profile, elapsedSeconds);
  if (!active) {
    // A player who has stepped away does not discover new opportunities. An
    // already-started capture still gets a cheap authoritative poll so its
    // evidence cannot remain unresolved merely because this session went idle.
    // Otherwise, a low-frequency background refresh models a browser tab left
    // open and guarantees that a completely idle team still has HTTP evidence.
    idleIterations.add(1);
    if (pendingAdCapture !== null) submitPendingAdCapture(true);
    if (pendingKothCapture !== null) {
      resolvePendingKothCapture(refreshKothCaptureState());
    } else if (__ITER % 3 === 0) {
      recordJson(
        platformGet(
          `${base}/api/Game/${CONFIG.gameId}/Ad/State`,
          authParams(CONFIG.jwt, 'ad_idle_poll'),
          'ad_idle_poll',
        ),
        'A&D idle browser refresh',
        adStateMs,
        validAdState,
      );
    }
    iterationsCompleted.add(1);
    sleep(thinkDelay(2));
    return;
  }
  activeIterations.add(1);
  if (pendingAdCapture !== null && !submitPendingAdCapture(true)) {
    iterationsCompleted.add(1);
    sleep(thinkDelay());
    return;
  }
  const adRequests = [
    ['GET', `${base}/api/Game/${CONFIG.gameId}/Ad/State`, null, authParams(CONFIG.jwt, 'ad_poll')],
    ['GET', `${base}/api/Game/${CONFIG.gameId}/Ad/Targets`, null, authParams(CONFIG.adToken, 'ad_token_poll')],
    ['GET', `${base}/api/Game/${CONFIG.gameId}/Ad/Scoreboard`, null, authParams(CONFIG.jwt, 'ad_poll')],
  ];
  const adResponses = platformBatchGet(adRequests, ['ad_state', 'ad_targets', 'ad_scoreboard']);
  const state = recordJson(adResponses[0], 'A&D state', adStateMs, validAdState);
  const targetsModel = recordJson(adResponses[1], 'A&D targets', adTargetsMs, validAdTargets);
  const adScoreboard = recordJson(adResponses[2], 'A&D scoreboard', adScoreboardMs, validAdScoreboard);

  const kothParams = authParams(CONFIG.jwt, 'koth_poll');
  const kothRequests = [
    ['GET', `${base}/api/game/${CONFIG.gameId}/ad/koth/scoreboard`, null, kothParams],
    ['GET', `${base}/api/game/${CONFIG.gameId}/ad/koth/${CONFIG.kothChallengeId}/token`, null, kothParams],
    ['GET', `${base}/api/game/${CONFIG.gameId}/ad/koth/${CONFIG.kothChallengeId}/state`, null, kothParams],
  ];
  const kothResponses = platformBatchGet(
    kothRequests,
    ['koth_scoreboard', 'koth_token', 'koth_state'],
  );
  const kothScoreboard = recordJson(
    kothResponses[0],
    'KotH scoreboard',
    kothScoreboardMs,
    validKothScoreboard
  );
  const kothTokenModel = recordJson(kothResponses[1], 'KotH token', kothTokenMs, validKothToken);
  const kothStateModel = recordJson(kothResponses[2], 'KotH state', kothStateMs, validKothState);
  observeKothState(kothStateModel);
  resolvePendingKothCapture(kothStateModel);

  if (__ITER % 3 === 0) {
    const timeline = platformGet(
      `${base}/api/game/${CONFIG.gameId}/ad/koth/timeline`,
      authParams(CONFIG.jwt, 'koth_timeline'),
      'koth_timeline',
    );
    recordJson(timeline, 'KotH timeline', kothTimelineMs, validKothTimeline);
  }

  if (CONFIG.jeoGame !== null) {
    const jeoParams = authParams(CONFIG.jeoJwt, 'jeopardy_poll');
    const jeopardyRequests = [
      ['GET', `${base}/api/game/${CONFIG.jeoGame}`, null, jeoParams],
      ['GET', `${base}/api/game/${CONFIG.jeoGame}/details`, null, jeoParams],
    ];
    const jeopardy = platformBatchGet(
      jeopardyRequests,
      ['jeopardy_game', 'jeopardy_details'],
    );
    recordJson(jeopardy[0], 'Jeopardy game', jeopardyGameMs, validJeopardyGame);
    recordJson(jeopardy[1], 'Jeopardy details', jeopardyDetailsMs, validJeopardyDetails);
  }

  const currentRound = state?.currentRound;
  if (!Number.isSafeInteger(currentRound) || currentRound < CONFIG.epochStartRound) {
    vpnAttackFailure.add(true);
    semanticInvalid.add(true);
    check(null, {
      'current scoring round is available for VPN attack': () => false,
    });
    iterationsCompleted.add(1);
    sleep(thinkDelay());
    return;
  }

  ensureActionBudget(currentRound);
  updateOwnDefense(progress, currentRound);
  const exactKothHill = kothTargetMatchesState(
    targetsModel,
    kothStateModel,
    CONFIG.kothChallengeId,
  )
    ? hillFromTargets(targetsModel)
    : null;
  attemptKothPatchLifecycle(
    exactKothHill,
    kothTokenModel,
    kothStateModel,
    active,
    progress,
  );
  attemptKothCapture(
    targetsModel,
    kothTokenModel,
    kothStateModel,
    kothScoreboard,
    active,
    progress
  );
  attemptJeopardySolve(progress, currentRound);

  if (!observedRounds.has(currentRound)) {
    observedRounds.add(currentRound);
    roundsSeen.add(1);
    for (const round of attackedVictimsByRound.keys()) {
      if (round < currentRound - 1) attackedVictimsByRound.delete(round);
    }
  }
  if (!state.flagsReady) {
    // Advancing the authoritative round and publishing its flags are separate,
    // observable phases. Waiting here avoids attacking a prior-round flag and
    // keeps expected synchronization time out of the VPN failure rate.
    flagSyncWaits.add(1);
    iterationsCompleted.add(1);
    sleep(thinkDelay());
    return;
  }
  if (state.flagDeliveryFailures > 0) {
    flagDeliveryFailures.add(state.flagDeliveryFailures);
    semanticInvalid.add(true);
    check(null, {
      'current round has zero flag delivery failures': () => false,
    });
    iterationsCompleted.add(1);
    sleep(thinkDelay());
    return;
  }
  const networkTargets = publicAdNetworkTargets(
    targetsModel,
    CONFIG.adChallengeId,
    CONFIG.teamIds,
    CONFIG.participationId,
  );
  let victimTarget;
  let technique = 1;
  if (CONFIG.realisticCompetition) {
    const plan = attackPlan(
      CONFIG.profile,
      publicAdOpponents(adScoreboard, networkTargets),
      opponentMemory,
      currentRound,
      CONFIG.epochStartRound,
      progress,
      {
        maxTargets: Math.min(
          3,
          CONFIG.profile.maxAttacks,
          currentActionBudget?.remaining ?? 0
        ),
      }
    );
    const attacked = attackedVictimsByRound.get(currentRound) ?? new Set();
    attackedVictimsByRound.set(currentRound, attacked);
    const victimIndex = plan.targets.find((index) => !attacked.has(index));
    victimTarget = networkTargets.find((target) => target.index === victimIndex);
    technique = plan.technique;
    if (!victimTarget) {
      iterationsCompleted.add(1);
      sleep(thinkDelay());
      return;
    }
    if (!useActionCredit('attack')) {
      iterationsCompleted.add(1);
      sleep(thinkDelay());
      return;
    }
    attacked.add(victimTarget.index);
  } else {
    victimTarget = victimForRound(currentRound, networkTargets);
    if (!victimTarget) {
      iterationsCompleted.add(1);
      sleep(thinkDelay());
      return;
    }
  }
  const victimIndex = victimTarget.index;
  const nonSelfVictim = victimIndex !== CONFIG.teamIndex;
  let attack;
  if (CONFIG.realisticCompetition) {
    attack = readVpnExploit(victimTarget, technique, 'first');
    const firstAttemptSucceeded = attack.patched || attack.unavailable || attack.valid;
    vpnFirstAttemptFailure.add(!firstAttemptSucceeded);
    if (!firstAttemptSucceeded) {
      vpnRetryAttempts.add(1);
      sleep(VPN_RETRY_DELAY_SECONDS);
      attack = readVpnExploit(victimTarget, technique, 'retry');
    }
    vpnAttackFailure.add(!attack.patched && !attack.unavailable && !attack.valid);
  } else {
    const victimUrl =
      `http://${victimTarget.host}:${victimTarget.port}/flag?team=` +
      encodeURIComponent(victimTarget.participationId);
    attack = readVpnFlag(victimUrl, nonSelfVictim, 'first');
    vpnFirstAttemptFailure.add(!attack.valid);
    if (nonSelfVictim && !attack.valid) {
      vpnRetryAttempts.add(1);
      sleep(VPN_RETRY_DELAY_SECONDS);
      attack = readVpnFlag(victimUrl, nonSelfVictim, 'retry');
    }
  }

  const { response: attackResponse, httpOk: attackHttpOk, flag, valid: attackValid } = attack;
  const expectedPatch = CONFIG.realisticCompetition && attack.patched;
  const expectedUnavailable = CONFIG.realisticCompetition && attack.unavailable;
  if (!CONFIG.realisticCompetition) vpnAttackFailure.add(!attackValid);
  semanticInvalid.add(attackHttpOk && !attackValid && !expectedPatch && !expectedUnavailable);
  check(attackResponse, {
    'VPN victim is another team': () => nonSelfVictim,
    'VPN victim returned an exact flag or an expected defensive result': () =>
      attackValid || expectedPatch || expectedUnavailable,
  });
  if (CONFIG.realisticCompetition) {
    const outcome = attackValid
      ? 'captured'
      : expectedPatch
        ? 'patched'
        : expectedUnavailable
          ? 'unavailable'
          : 'transportFailure';
    opponentMemory = recordAttackOutcome(opponentMemory, {
      victimIndex,
      technique,
      outcome,
      round: currentRound,
    });
  }

  const captureKey = `${currentRound}:${victimIndex}`;
  if (attackValid && !settledCaptures.has(captureKey)) {
    startAdCapture(captureKey, flag, currentRound);
  }

  iterationsCompleted.add(1);
  sleep(thinkDelay());
}

export default function () {
  try {
    runTeamIteration();
  } catch (error) {
    iterationRuntimeErrors.add(1);
    const message = (error instanceof Error ? `${error.name}: ${error.message}` : String(error))
      .replace(/[\r\n\t]+/g, ' ')
      .slice(0, 512);
    console.error(`team ${CONFIG.teamIndex} iteration runtime error: ${message}`);
    exec.test.abort(`team ${CONFIG.teamIndex} iteration runtime error`);
  }
}

const EVIDENCE_METRICS = [...new Set([
  ...MANDATORY_TEAM_EVIDENCE_COUNTERS,
  'http_req_duration',
  'checks',
  'server_5xx',
  'unexpected_non_2xx',
  'platform_api_failure',
  'request_timeout',
  'rate_limited',
  'semantic_invalid',
  'vpn_first_attempt_failure',
  'vpn_attack_failure',
  'ad_state_ms',
  'ad_targets_ms',
  'ad_scoreboard_ms',
  'koth_scoreboard_ms',
  'koth_token_ms',
  'koth_state_ms',
  'koth_timeline_ms',
  'jeopardy_game_ms',
  'jeopardy_details_ms',
  'jeopardy_submit_ms',
  'jeopardy_container_ms',
  'jeopardy_asset_ms',
  'vpn_attack_ms',
  'ad_submit_ms',
  'koth_capture_ms',
])];

function sanitizedMetric(metric) {
  if (!metric) return null;
  const values = {};
  for (const [name, value] of Object.entries(metric.values || {})) {
    if (typeof value === 'number' && Number.isFinite(value)) values[name] = value;
    if (typeof value === 'boolean') values[name] = value;
  }
  const thresholds = {};
  for (const [name, result] of Object.entries(metric.thresholds || {})) {
    thresholds[name] = result?.ok === true;
  }
  return Object.keys(thresholds).length > 0 ? { values, thresholds } : { values };
}

export function handleSummary(data) {
  const metrics = {};
  for (const name of EVIDENCE_METRICS) {
    const metric = sanitizedMetric(data.metrics?.[name]);
    if (metric) metrics[name] = metric;
  }
  const thresholdResults = Object.values(metrics).flatMap((metric) => Object.values(metric.thresholds || {}));
  const evidence = {
    schemaVersion: CONFIG.realisticCompetition ? TEAM_EVIDENCE_SCHEMA_VERSION : 1,
    generatedAt: new Date().toISOString(),
    team: {
      index: CONFIG.teamIndex,
      count: CONFIG.teamCount,
      ...(CONFIG.realisticCompetition ? { participationId: CONFIG.participationId } : {}),
    },
    event: {
      gameId: CONFIG.gameId,
      kothChallengeId: CONFIG.kothChallengeId,
      jeopardyEnabled: CONFIG.jeoGame !== null,
      ...(CONFIG.realisticCompetition
        ? {
            runId: CONFIG.competitionRunId,
            eventCreatedAtMs: CONFIG.eventCreatedAtMs,
            jeopardyGameId: CONFIG.jeoGame,
            epochStartRound: CONFIG.epochStartRound,
          }
        : {}),
    },
    workload: {
      duration: CONFIG.duration,
      thinkSeconds: CONFIG.thinkSeconds,
      mode: CONFIG.realisticCompetition ? 'competitive' : 'capacity',
      seed: CONFIG.competitionSeed,
      modelVersion: CONFIG.competitionModelVersion,
    },
    profile: CONFIG.realisticCompetition ? CONFIG.profile : null,
    authentication: { adAutomationTokenExercised: true },
    thresholdsPassed: thresholdResults.length > 0 && thresholdResults.every(Boolean),
    metrics,
  };
  return {
    [`/evidence/${CONFIG.evidenceFile}`]: `${JSON.stringify(evidence, null, 2)}\n`,
    stdout: `team ${CONFIG.teamIndex}: sanitized evidence written to ${CONFIG.evidenceFile}\n`,
  };
}
