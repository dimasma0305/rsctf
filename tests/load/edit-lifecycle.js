// Exact contracts for the organizer `/api/edit` acceptance gate. This module
// stays free of Node-only imports so its path/response helpers can also be used
// from k6 scenarios.

import { parseAxumRouterOperations } from './admin-lifecycle.js';

const operation = (id, method, path, options = {}) => Object.freeze({
  id,
  method,
  path,
  auth: options.auth || 'manager',
  responseKind: options.responseKind || 'object',
  expectedStatuses: Object.freeze([...(options.expectedStatuses || [200])]),
  params: Object.freeze({ ...(options.params || {}) }),
  query: options.query || '',
  multipart: options.multipart === true,
  mutation: options.mutation === true,
  runtime: options.runtime === true,
});

const game = { id: 'gameId' };
const challenge = { id: 'gameId', cId: 'challengeId' };
const service = { id: 'adGameId', adTeamServiceId: 'serviceId' };

export const EDIT_OPERATIONS = Object.freeze([
  operation('edit_post_add', 'POST', '/api/edit/posts', {
    auth: 'admin', mutation: true, responseKind: 'string',
  }),
  operation('edit_post_update', 'PUT', '/api/edit/posts/{id}', {
    auth: 'admin', mutation: true, responseKind: 'post', params: { id: 'postId' },
  }),
  operation('edit_post_delete', 'DELETE', '/api/edit/posts/{id}', {
    auth: 'admin', mutation: true, responseKind: 'message', params: { id: 'postId' },
  }),

  operation('edit_games_get', 'GET', '/api/edit/games', {
    auth: 'managed-list', responseKind: 'page', query: 'count=100&skip=0',
  }),
  operation('edit_game_add', 'POST', '/api/edit/games', {
    auth: 'admin', mutation: true, responseKind: 'game',
  }),
  operation('edit_game_import', 'POST', '/api/edit/games/import', {
    auth: 'admin', multipart: true, mutation: true, responseKind: 'number',
  }),
  operation('edit_game_get', 'GET', '/api/edit/games/{id}', {
    params: game, responseKind: 'game',
  }),
  operation('edit_game_update', 'PUT', '/api/edit/games/{id}', {
    params: game, mutation: true, responseKind: 'game',
  }),
  operation('edit_game_delete', 'DELETE', '/api/edit/games/{id}', {
    auth: 'admin', params: game, mutation: true, responseKind: 'game',
  }),
  operation('edit_game_hash_salt', 'GET', '/api/edit/games/{id}/HashSalt', {
    params: game, responseKind: 'private-string',
  }),
  operation('edit_game_clone', 'POST', '/api/edit/games/{id}/Clone', {
    auth: 'admin', params: game, mutation: true, responseKind: 'number',
  }),
  operation('edit_game_writeups_delete', 'DELETE', '/api/edit/games/{id}/writeups', {
    auth: 'admin', params: game, mutation: true, responseKind: 'game',
  }),
  operation('edit_game_poster_update', 'PUT', '/api/edit/games/{id}/poster', {
    params: game, multipart: true, mutation: true, responseKind: 'string',
  }),
  operation('edit_game_export', 'POST', '/api/edit/games/{id}/export', {
    params: game, responseKind: 'zip',
  }),
  operation('edit_scoreboard_flush', 'POST', '/api/edit/games/{id}/scoreboard/flush', {
    params: game, mutation: true, responseKind: 'message',
  }),

  operation('edit_game_admins_get', 'GET', '/api/edit/games/{id}/admins', {
    auth: 'admin', params: game, responseKind: 'array',
  }),
  operation('edit_game_admin_add', 'POST', '/api/edit/games/{id}/admins/{userId}', {
    auth: 'admin', params: { id: 'gameId', userId: 'managerUserId' }, mutation: true,
    responseKind: 'message',
  }),
  operation('edit_game_admin_delete', 'DELETE', '/api/edit/games/{id}/admins/{userId}', {
    auth: 'admin', params: { id: 'gameId', userId: 'managerUserId' }, mutation: true,
    responseKind: 'message',
  }),

  operation('edit_reviews_get', 'GET', '/api/edit/games/{id}/reviews', {
    params: game, responseKind: 'page', query: 'count=100&skip=0',
  }),
  operation('edit_reviews_analytics_get', 'GET', '/api/edit/games/{id}/reviews/analytics', {
    params: game, responseKind: 'review-analytics',
  }),
  operation('edit_pending_challenges_get', 'GET', '/api/edit/games/{id}/pendingchallenges', {
    params: game, responseKind: 'array',
  }),

  operation('edit_challenges_get', 'GET', '/api/edit/games/{id}/challenges', {
    params: game, responseKind: 'array',
  }),
  operation('edit_challenge_add', 'POST', '/api/edit/games/{id}/challenges', {
    params: game, mutation: true, responseKind: 'challenge',
  }),
  operation('edit_challenge_submit', 'POST', '/api/edit/games/{id}/challenges/submit', {
    auth: 'user-submit', params: game, multipart: true, mutation: true, responseKind: 'import',
  }),
  operation('edit_challenge_import', 'POST', '/api/edit/games/{id}/challenges/import', {
    params: game, multipart: true, mutation: true, responseKind: 'import',
  }),
  operation('edit_challenge_import_github', 'POST', '/api/edit/games/{id}/challenges/importfromgithub', {
    params: game, mutation: true, responseKind: 'import',
  }),
  operation('edit_challenge_get', 'GET', '/api/edit/games/{id}/challenges/{cId}', {
    params: challenge, responseKind: 'challenge',
  }),
  operation('edit_challenge_update', 'PUT', '/api/edit/games/{id}/challenges/{cId}', {
    params: challenge, mutation: true, responseKind: 'challenge',
  }),
  operation('edit_challenge_delete', 'DELETE', '/api/edit/games/{id}/challenges/{cId}', {
    params: { id: 'gameId', cId: 'deletableChallengeId' }, mutation: true,
    responseKind: 'message',
  }),
  operation('edit_challenge_approve', 'POST', '/api/edit/games/{id}/challenges/{cId}/approve', {
    params: { id: 'gameId', cId: 'pendingApproveId' }, mutation: true, responseKind: 'message',
  }),
  operation('edit_challenge_reject', 'POST', '/api/edit/games/{id}/challenges/{cId}/reject', {
    params: { id: 'gameId', cId: 'pendingRejectId' }, mutation: true, responseKind: 'message',
  }),
  operation('edit_challenge_attachment', 'POST', '/api/edit/games/{id}/challenges/{cId}/attachment', {
    params: challenge, mutation: true, responseKind: 'number',
  }),
  operation('edit_challenge_audit_meta', 'GET', '/api/edit/games/{id}/challenges/{cId}/auditmeta', {
    params: { id: 'gameId', cId: 'archiveChallengeId' }, responseKind: 'audit',
  }),
  operation('edit_challenge_rebuild', 'POST', '/api/edit/games/{id}/challenges/{cId}/rebuild', {
    params: { id: 'gameId', cId: 'containerChallengeId' }, mutation: true, runtime: true,
    responseKind: 'build',
  }),
  operation('edit_workload_rollout', 'POST', '/api/edit/games/{id}/challenges/{cId}/workload/rollout', {
    params: { id: 'workerGameId', cId: 'workerChallengeId' }, mutation: true, runtime: true,
    responseKind: 'rollout',
  }),
  operation('edit_test_container_create', 'POST', '/api/edit/games/{id}/challenges/{cId}/container', {
    params: { id: 'gameId', cId: 'containerChallengeId' }, mutation: true, runtime: true,
    responseKind: 'container',
  }),
  operation('edit_test_container_delete', 'DELETE', '/api/edit/games/{id}/challenges/{cId}/container', {
    params: { id: 'gameId', cId: 'containerChallengeId' }, mutation: true, runtime: true,
    responseKind: 'message',
  }),
  operation('edit_flags_add', 'POST', '/api/edit/games/{id}/challenges/{cId}/flags', {
    params: challenge, mutation: true, responseKind: 'message',
  }),
  operation('edit_flag_delete', 'DELETE', '/api/edit/games/{id}/challenges/{cId}/flags/{fId}', {
    params: { id: 'gameId', cId: 'challengeId', fId: 'flagId' }, mutation: true,
    responseKind: 'flag-status',
  }),

  operation('edit_notices_get', 'GET', '/api/edit/games/{id}/notices', {
    params: game, responseKind: 'array',
  }),
  operation('edit_notice_add', 'POST', '/api/edit/games/{id}/notices', {
    params: game, mutation: true, responseKind: 'notice',
  }),
  operation('edit_notice_update', 'PUT', '/api/edit/games/{id}/notices/{noticeId}', {
    params: { id: 'gameId', noticeId: 'noticeId' }, mutation: true, responseKind: 'notice',
  }),
  operation('edit_notice_delete', 'DELETE', '/api/edit/games/{id}/notices/{noticeId}', {
    params: { id: 'gameId', noticeId: 'noticeId' }, mutation: true, responseKind: 'message',
  }),

  operation('edit_divisions_get', 'GET', '/api/edit/games/{id}/divisions', {
    params: game, responseKind: 'array',
  }),
  operation('edit_division_add', 'POST', '/api/edit/games/{id}/divisions', {
    params: game, mutation: true, responseKind: 'division',
  }),
  operation('edit_division_update', 'PUT', '/api/edit/games/{id}/divisions/{divisionId}', {
    params: { id: 'gameId', divisionId: 'divisionId' }, mutation: true, responseKind: 'division',
  }),
  operation('edit_division_delete', 'DELETE', '/api/edit/games/{id}/divisions/{divisionId}', {
    params: { id: 'gameId', divisionId: 'divisionId' }, mutation: true, responseKind: 'message',
  }),

  operation('edit_ad_advance_round', 'POST', '/api/edit/games/{id}/ad/AdvanceRound', {
    params: { id: 'adGameId' }, mutation: true, expectedStatuses: [400], responseKind: 'error',
  }),
  operation('edit_ad_state', 'GET', '/api/edit/games/{id}/ad/State', {
    params: { id: 'adGameId' }, responseKind: 'ad-state',
  }),
  operation('edit_ad_ensure_containers', 'POST', '/api/edit/games/{id}/ad/EnsureContainers', {
    auth: 'admin', params: { id: 'adGameId' }, mutation: true, runtime: true,
    responseKind: 'message',
  }),
  operation('edit_ad_scoring_pause', 'POST', '/api/edit/games/{id}/ad/ScoringPause', {
    params: { id: 'adGameId' }, mutation: true, responseKind: 'scoring-pause',
  }),
  operation('edit_ad_challenge_toggle', 'POST', '/api/edit/games/{id}/ad/Challenges/{challengeId}/Toggle', {
    params: { id: 'adGameId', challengeId: 'adChallengeId' }, mutation: true, runtime: true,
    responseKind: 'toggle',
  }),
  operation('edit_ad_check_override', 'POST', '/api/edit/games/{id}/ad/Checks/{checkId}/Override', {
    auth: 'admin', params: { id: 'adGameId', checkId: 'checkId' }, mutation: true,
    responseKind: 'message',
  }),
  operation('edit_ad_service_file', 'GET', '/api/edit/games/{id}/ad/Services/{adTeamServiceId}/File', {
    params: service, query: 'path=%2Fetc%2Fhostname', responseKind: 'service-file',
  }),
  operation('edit_ad_inspector_create', 'POST', '/api/edit/games/{id}/ad/Services/{adTeamServiceId}/Inspector', {
    params: service, mutation: true, runtime: true, responseKind: 'inspector',
  }),
  operation('edit_ad_inspector_delete', 'DELETE', '/api/edit/games/{id}/ad/Services/{adTeamServiceId}/Inspector/{containerGuid}', {
    params: { id: 'adGameId', adTeamServiceId: 'serviceId', containerGuid: 'inspectorId' },
    mutation: true, runtime: true, responseKind: 'message',
  }),
  operation('edit_ad_service_restart', 'POST', '/api/edit/games/{id}/ad/Services/{adTeamServiceId}/Restart', {
    params: service, mutation: true, runtime: true, responseKind: 'message',
  }),
  operation('edit_ad_snapshot_download', 'GET', '/api/edit/games/{id}/ad/Services/{adTeamServiceId}/Snapshot', {
    params: service, runtime: true, responseKind: 'tar',
  }),
  operation('edit_ad_snapshot_changes', 'GET', '/api/edit/games/{id}/ad/Services/{adTeamServiceId}/Snapshot/Changes', {
    params: service, runtime: true, responseKind: 'snapshot-changes',
  }),
  operation('edit_ad_snapshot_diff', 'GET', '/api/edit/games/{id}/ad/Services/{adTeamServiceId}/SnapshotDiff', {
    params: service, runtime: true, responseKind: 'snapshot-diff',
  }),
  operation('edit_ad_snapshots_get', 'GET', '/api/edit/games/{id}/ad/Services/{adTeamServiceId}/Snapshots', {
    params: service, runtime: true, responseKind: 'array',
  }),

  operation('edit_koth_state', 'GET', '/api/edit/games/{id}/ad/koth/state', {
    params: { id: 'kothGameId' }, responseKind: 'koth-state',
  }),
  operation('edit_koth_receipts', 'GET', '/api/edit/games/{id}/ad/koth/{challengeId}/receipts', {
    params: { id: 'kothGameId', challengeId: 'kothChallengeId' }, responseKind: 'koth-receipts',
  }),
  operation('edit_koth_recover', 'POST', '/api/edit/games/{id}/ad/koth/{challengeId}/recover', {
    params: { id: 'kothGameId', challengeId: 'kothChallengeId' }, mutation: true, runtime: true,
    responseKind: 'koth-recovery',
  }),
]);

export const EDIT_OPERATION_IDS = Object.freeze(EDIT_OPERATIONS.map(({ id }) => id));
const operationById = new Map(EDIT_OPERATIONS.map((item) => [item.id, item]));

export function resolveEditOperationPath(operationOrId, context = {}) {
  const item = typeof operationOrId === 'string' ? operationById.get(operationOrId) : operationOrId;
  if (!item) throw new Error(`unknown edit operation ${operationOrId}`);
  let path = item.path;
  for (const [placeholder, contextKey] of Object.entries(item.params)) {
    const value = context[contextKey];
    if (value === undefined || value === null || String(value).length === 0) {
      throw new Error(`${item.id} requires edit context ${contextKey}`);
    }
    path = path.replace(`{${placeholder}}`, encodeURIComponent(String(value)));
  }
  if (/\{[^}]+\}/.test(path)) throw new Error(`${item.id} has an unresolved route parameter`);
  return `${path}${item.query ? `?${item.query}` : ''}`;
}

export function assertEditAdminProbeScope(operationOrId, context, managedGameIds) {
  const item = typeof operationOrId === 'string' ? operationById.get(operationOrId) : operationOrId;
  if (!item) throw new Error(`unknown edit operation ${operationOrId}`);
  if (item.auth !== 'admin') return null;

  const gameContextKey = item.params.id;
  const isGameContext = gameContextKey === 'gameId' || gameContextKey?.endsWith('GameId');
  if (!isGameContext) return null;
  const gameId = context?.[gameContextKey];
  if (gameId === undefined || gameId === null) {
    throw new Error(`${item.id} requires edit context ${gameContextKey} for its Admin probe`);
  }
  if (!(managedGameIds instanceof Set) || !managedGameIds.has(gameId)) {
    throw new Error(
      `${item.id} Admin probe is not scoped to game ${gameId} managed by the probe identity`,
    );
  }
  return gameId;
}

function routeKey({ method, path }) {
  return `${method} ${path}`;
}

export function parseEditRouterOperations(sources) {
  const chunks = typeof sources === 'string' ? [sources] : sources;
  if (!Array.isArray(chunks) || chunks.length === 0 || chunks.some((source) => typeof source !== 'string')) {
    throw new TypeError('edit router sources must contain production Rust source');
  }
  return Object.freeze(chunks
    // Other controller modules contain intentionally dynamic honeypot routes
    // (`.route(path, ...)`) which the literal-path catalog cannot parse. An edit
    // route must expose its public wire path as a literal; only parse files that
    // actually declare that namespace.
    .filter((source) => source.includes('/api/edit'))
    .flatMap((source) => parseAxumRouterOperations(source))
    .filter(({ path }) => path === '/api/edit' || path.startsWith('/api/edit/'))
    .map(Object.freeze));
}

export function assertEditRouterCoverage(sources) {
  const parsed = parseEditRouterOperations(sources);
  const expected = new Set(EDIT_OPERATIONS.map(routeKey));
  const actual = new Set(parsed.map(routeKey));
  const missing = [...expected].filter((key) => !actual.has(key));
  const extra = [...actual].filter((key) => !expected.has(key));
  if (missing.length || extra.length || actual.size !== parsed.length) {
    throw new Error(
      `edit router catalog drift (missing: ${missing.join(', ') || 'none'}; ` +
        `uncovered: ${extra.join(', ') || 'none'}; parsed=${parsed.length}, unique=${actual.size})`,
    );
  }
  return Object.freeze({ operations: actual.size });
}

function coverageIds(recorded) {
  if (recorded instanceof Set) return [...recorded];
  if (recorded instanceof Map) return [...recorded.entries()].filter(([, value]) => value).map(([id]) => id);
  if (Array.isArray(recorded)) return recorded;
  if (recorded && typeof recorded === 'object') {
    return Object.entries(recorded).filter(([, value]) => value).map(([id]) => id);
  }
  throw new TypeError('edit coverage must be an array, Set, Map, or object');
}

export function assertCompleteEditCoverage(recorded) {
  const covered = coverageIds(recorded);
  const coveredSet = new Set(covered);
  const requiredSet = new Set(EDIT_OPERATION_IDS);
  const duplicates = covered.filter((id, index) => covered.indexOf(id) !== index);
  const missing = EDIT_OPERATION_IDS.filter((id) => !coveredSet.has(id));
  const extra = covered.filter((id) => !requiredSet.has(id));
  if (duplicates.length || missing.length || extra.length) {
    throw new Error(
      `incomplete edit lifecycle coverage (` +
        `${duplicates.length ? `duplicate: ${[...new Set(duplicates)].join(', ')}; ` : ''}` +
        `${missing.length ? `missing: ${missing.join(', ')}; ` : ''}` +
        `${extra.length ? `unknown: ${[...new Set(extra)].join(', ')}` : ''})`,
    );
  }
  return Object.freeze({ covered: coveredSet.size, required: EDIT_OPERATION_IDS.length });
}

function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function headerValue(headers, name) {
  if (!headers) return '';
  if (typeof headers.get === 'function') return String(headers.get(name) || '');
  const wanted = name.toLowerCase();
  const entry = Object.entries(headers).find(([key]) => key.toLowerCase() === wanted);
  return entry ? String(entry[1]) : '';
}

export function validateEditResponse(operationOrId, response) {
  const item = typeof operationOrId === 'string' ? operationById.get(operationOrId) : operationOrId;
  if (!item) throw new Error(`unknown edit operation ${operationOrId}`);
  if (!item.expectedStatuses.includes(response?.status)) {
    throw new Error(`${item.id} returned ${response?.status}; expected ${item.expectedStatuses.join('/')}`);
  }
  if (response.status >= 300) {
    if (!isObject(response.body) || typeof response.body.title !== 'string' || response.body.status !== response.status) {
      throw new Error(`${item.id} returned an invalid error envelope`);
    }
    return true;
  }
  const body = response.body;
  const object = () => {
    if (!isObject(body)) throw new Error(`${item.id} expected an object`);
  };
  switch (item.responseKind) {
    case 'message':
      object();
      if (typeof body.title !== 'string' || body.status !== response.status) throw new Error(`${item.id} invalid message`);
      break;
    case 'string':
    case 'private-string':
      if (typeof body !== 'string' || body.length === 0) throw new Error(`${item.id} expected a nonempty string`);
      break;
    case 'number':
      if (!Number.isSafeInteger(body) || body <= 0) throw new Error(`${item.id} expected a positive integer`);
      break;
    case 'array':
      if (!Array.isArray(body)) throw new Error(`${item.id} expected an array`);
      break;
    case 'page':
      object();
      if (!Array.isArray(body.data) || !Number.isSafeInteger(body.total) || !Number.isSafeInteger(body.length)) {
        throw new Error(`${item.id} expected a page`);
      }
      break;
    case 'zip':
      if (!(body instanceof Uint8Array) || body[0] !== 0x50 || body[1] !== 0x4b) throw new Error(`${item.id} invalid ZIP`);
      break;
    case 'tar':
      if (!(body instanceof Uint8Array) || body.length < 512 ||
          !/application\/x-tar/i.test(headerValue(response.headers, 'content-type'))) {
        throw new Error(`${item.id} invalid TAR`);
      }
      break;
    case 'import':
      object();
      for (const key of ['imported', 'updated', 'failed']) {
        if (!Number.isSafeInteger(body[key]) || body[key] < 0) throw new Error(`${item.id} invalid import result`);
      }
      if (!Array.isArray(body.messages)) throw new Error(`${item.id} invalid import messages`);
      break;
    case 'flag-status':
      if (!['Success', 'NotFound'].includes(body)) throw new Error(`${item.id} invalid flag deletion status`);
      break;
    case 'inspector':
      object();
      if (typeof body.containerGuid !== 'string' || body.containerGuid.length < 12) {
        throw new Error(`${item.id} did not create a real inspector`);
      }
      break;
    case 'container':
      object();
      if (typeof body.id !== 'string' || body.id.length < 12) throw new Error(`${item.id} invalid container`);
      break;
    case 'rollout':
      object();
      for (const key of ['matched', 'updated', 'stale', 'incompatible', 'insufficientCapacity', 'failed']) {
        if (!Number.isSafeInteger(body[key]) || body[key] < 0) throw new Error(`${item.id} invalid rollout`);
      }
      break;
    case 'scoring-pause':
      object();
      if (typeof body.scoringPaused !== 'boolean') throw new Error(`${item.id} invalid pause state`);
      break;
    case 'toggle':
      object();
      if (typeof body.isEnabled !== 'boolean') throw new Error(`${item.id} invalid toggle state`);
      break;
    case 'snapshot-changes':
      object();
      if (typeof body.snapshotAvailable !== 'boolean' || !Array.isArray(body.changes)) {
        throw new Error(`${item.id} invalid snapshot changes`);
      }
      break;
    case 'snapshot-diff':
      object();
      if (!Array.isArray(body.added) || !Array.isArray(body.removed)) throw new Error(`${item.id} invalid snapshot diff`);
      break;
    case 'post':
      object();
      if (typeof body.id !== 'string' || typeof body.title !== 'string') throw new Error(`${item.id} invalid post`);
      break;
    case 'game':
      object();
      if (!Number.isSafeInteger(body.id) || typeof body.title !== 'string') throw new Error(`${item.id} invalid game`);
      break;
    case 'challenge':
      object();
      if (!Number.isSafeInteger(body.id) || typeof body.title !== 'string') throw new Error(`${item.id} invalid challenge`);
      break;
    case 'notice':
      object();
      if (!Number.isSafeInteger(body.id) || !Array.isArray(body.values) || !Number.isFinite(body.time)) {
        throw new Error(`${item.id} invalid notice`);
      }
      break;
    case 'division':
      object();
      if (!Number.isSafeInteger(body.id) || typeof body.name !== 'string') throw new Error(`${item.id} invalid division`);
      break;
    case 'audit':
      object();
      if (typeof body.archiveAvailable !== 'boolean' || !Array.isArray(body.files) || !isObject(body.previews)) {
        throw new Error(`${item.id} invalid audit metadata`);
      }
      break;
    case 'build':
      object();
      if (typeof body.buildStatus !== 'string' || typeof body.archiveAvailable !== 'boolean' ||
          !Array.isArray(body.files) || !isObject(body.previews)) {
        throw new Error(`${item.id} invalid build/audit result`);
      }
      break;
    case 'ad-state':
      object();
      if (!Array.isArray(body.challenges) || !Array.isArray(body.teams) || typeof body.scoringPaused !== 'boolean') {
        throw new Error(`${item.id} invalid A&D state`);
      }
      break;
    case 'service-file':
      object();
      if (typeof body.path !== 'string' || typeof body.containerRunning !== 'boolean') {
        throw new Error(`${item.id} invalid service file view`);
      }
      break;
    case 'review-analytics':
      object();
      for (const key of ['total', 'likes', 'dislikes']) {
        if (!Number.isSafeInteger(body[key]) || body[key] < 0) {
          throw new Error(`${item.id} invalid review analytics`);
        }
      }
      if (!Array.isArray(body.topLiked) || !Array.isArray(body.topDisliked)) {
        throw new Error(`${item.id} invalid review analytics`);
      }
      break;
    case 'koth-state':
      object();
      if (!Array.isArray(body.hills) || !Array.isArray(body.teams)) throw new Error(`${item.id} invalid KotH state`);
      break;
    case 'koth-receipts':
      object();
      if (!Number.isSafeInteger(body.challengeId) || !Array.isArray(body.receipts)) {
        throw new Error(`${item.id} invalid KotH receipts`);
      }
      break;
    case 'koth-recovery':
      object();
      if (!Number.isSafeInteger(body.challengeId) || !Number.isSafeInteger(body.cycleNumber) ||
          typeof body.resetPhase !== 'string') throw new Error(`${item.id} invalid KotH recovery`);
      break;
    case 'object':
      object();
      break;
    default:
      throw new Error(`missing response validator for ${item.responseKind}`);
  }
  return true;
}
