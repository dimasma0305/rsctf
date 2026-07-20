import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import test from 'node:test';

import {
  EDIT_OPERATION_IDS,
  EDIT_OPERATIONS,
  assertCompleteEditCoverage,
  assertEditRouterCoverage,
  parseEditRouterOperations,
  resolveEditOperationPath,
  validateEditResponse,
} from '../edit-lifecycle.js';

const REPOSITORY = fileURLToPath(new URL('../../..', import.meta.url));

function rustFiles(root) {
  const files = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) files.push(...rustFiles(path));
    else if (entry.isFile() && entry.name.endsWith('.rs') && !entry.name.endsWith('_tests.rs')) files.push(path);
  }
  return files.sort();
}

function controllerSources() {
  return rustFiles(join(REPOSITORY, 'src/controllers')).map((path) => readFileSync(path, 'utf8'));
}

const context = Object.freeze({
  gameId: 10,
  challengeId: 20,
  deletableChallengeId: 21,
  pendingApproveId: 22,
  pendingRejectId: 23,
  archiveChallengeId: 24,
  containerChallengeId: 25,
  workerGameId: 11,
  workerChallengeId: 26,
  postId: 'post-id',
  managerUserId: '018f3c6a-d79b-7cc0-8f68-8fdbad0f57bb',
  flagId: 30,
  noticeId: 31,
  divisionId: 32,
  adGameId: 12,
  adChallengeId: 33,
  serviceId: 34,
  checkId: 35,
  inspectorId: '0123456789abcdef',
  kothGameId: 13,
  kothChallengeId: 36,
});

function objectBody(kind) {
  switch (kind) {
    case 'message': return { title: '', status: 200 };
    case 'string':
    case 'private-string': return 'fixture';
    case 'number': return 1;
    case 'array': return [];
    case 'page': return { data: [], total: 0, length: 0 };
    case 'zip': return new Uint8Array([0x50, 0x4b, 0x03, 0x04]);
    case 'tar': return new Uint8Array(512);
    case 'import': return { imported: 1, updated: 0, failed: 0, messages: [] };
    case 'flag-status': return 'Success';
    case 'inspector': return { containerGuid: '0123456789abcdef' };
    case 'container': return { id: '018f3c6a-d79b-7cc0-8f68-8fdbad0f57bb' };
    case 'rollout':
      return { matched: 1, updated: 1, stale: 0, incompatible: 0, insufficientCapacity: 0, failed: 0 };
    case 'scoring-pause': return { scoringPaused: true };
    case 'toggle': return { isEnabled: false };
    case 'snapshot-changes': return { snapshotAvailable: true, changes: [] };
    case 'snapshot-diff': return { added: [], removed: [] };
    case 'post': return { id: 'post-id', title: 'Post' };
    case 'game': return { id: 1, title: 'Game' };
    case 'challenge': return { id: 1, title: 'Challenge' };
    case 'notice': return { id: 1, values: ['notice'], time: Date.now() };
    case 'division': return { id: 1, name: 'Open' };
    case 'audit': return { archiveAvailable: true, files: [], previews: {} };
    case 'build': return { status: 'Succeeded' };
    case 'ad-state': return { challenges: [], teams: [], scoringPaused: false };
    case 'service-file': return { path: '/etc/hostname', containerRunning: true };
    case 'koth-state': return { hills: [], teams: [] };
    case 'koth-receipts': return { challengeId: 1, cycleNumber: 1, receipts: [] };
    case 'koth-recovery': return { challengeId: 1, cycleNumber: 1, resetPhase: 'Active' };
    case 'object': return {};
    case 'error': return { title: 'Manual round advance is disabled', status: 400 };
    default: throw new Error(`missing sample body for ${kind}`);
  }
}

function sampleResponse(operation) {
  const status = operation.expectedStatuses[0];
  return {
    status,
    body: objectBody(operation.responseKind),
    headers: operation.responseKind === 'tar' ? { 'content-type': 'application/x-tar' } : {},
  };
}

test('catalog has exactly all 64 edit method/path operations', () => {
  assert.equal(EDIT_OPERATIONS.length, 64);
  assert.equal(new Set(EDIT_OPERATION_IDS).size, 64);
  assert.equal(new Set(EDIT_OPERATIONS.map(({ method, path }) => `${method} ${path}`)).size, 64);
  assert.deepEqual(
    EDIT_OPERATIONS.reduce((counts, operation) => {
      counts[operation.method] = (counts[operation.method] || 0) + 1;
      return counts;
    }, {}),
    { POST: 28, PUT: 6, DELETE: 10, GET: 20 },
  );
  assert.deepEqual(
    EDIT_OPERATIONS.reduce((counts, operation) => {
      counts[operation.auth] = (counts[operation.auth] || 0) + 1;
      return counts;
    }, {}),
    { admin: 13, 'managed-list': 1, manager: 49, 'user-submit': 1 },
  );
});

test('catalog and every production controller source have exact bidirectional coverage', () => {
  const sources = controllerSources();
  assert.deepEqual(assertEditRouterCoverage(sources), { operations: 64 });
  assert.equal(parseEditRouterOperations(sources).length, 64);
});

test('source drift catches routes added outside controllers/edit and removed catalog routes', () => {
  const sources = controllerSources();
  assert.throws(
    () => assertEditRouterCoverage([...sources, '.route("/api/edit/future", get(future))']),
    /uncovered: GET \/api\/edit\/future/,
  );
  const withoutPosts = sources.map((source) => source.replace(
    '.route("/api/edit/posts", post(add_post))',
    '',
  ));
  assert.throws(() => assertEditRouterCoverage(withoutPosts), /missing: POST \/api\/edit\/posts/);
});

test('every operation resolves all path parameters with the exact fixture context', () => {
  for (const operation of EDIT_OPERATIONS) {
    const path = resolveEditOperationPath(operation, context);
    assert.ok(path.startsWith('/api/edit/'), operation.id);
    assert.doesNotMatch(path, /\{[^}]+\}/, operation.id);
  }
  assert.throws(
    () => resolveEditOperationPath('edit_ad_inspector_delete', {}),
    /requires edit context/,
  );
});

test('every declared response contract has an accepting and rejecting unit sample', () => {
  for (const operation of EDIT_OPERATIONS) {
    assert.equal(validateEditResponse(operation, sampleResponse(operation)), true, operation.id);
    assert.throws(
      () => validateEditResponse(operation, { status: 599, body: {}, headers: {} }),
      /returned 599/,
      operation.id,
    );
  }
});

test('coverage accounting rejects missing, duplicate, and unknown operation ids', () => {
  assert.deepEqual(assertCompleteEditCoverage(EDIT_OPERATION_IDS), { covered: 64, required: 64 });
  assert.throws(() => assertCompleteEditCoverage(EDIT_OPERATION_IDS.slice(1)), /missing: edit_post_add/);
  assert.throws(() => assertCompleteEditCoverage([...EDIT_OPERATION_IDS, EDIT_OPERATION_IDS[0]]), /duplicate/);
  assert.throws(() => assertCompleteEditCoverage([...EDIT_OPERATION_IDS, 'future']), /unknown: future/);
});
