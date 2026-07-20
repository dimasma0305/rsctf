import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import test from 'node:test';

import {
  EDIT_OPERATION_IDS,
  EDIT_OPERATIONS,
  assertEditAdminProbeScope,
  assertCompleteEditCoverage,
  assertEditRouterCoverage,
  parseEditRouterOperations,
  resolveEditOperationPath,
  validateEditResponse,
} from '../edit-lifecycle.js';
import { resolveRemoteGitRefCommit } from '../edit-lifecycle-fixtures.mjs';

const REPOSITORY = fileURLToPath(new URL('../../..', import.meta.url));

test('GitHub import ref fencing resolves one stable branch/tag commit', () => {
  const commit = 'a'.repeat(40);
  const annotated = 'b'.repeat(40);
  const run = (_command, args) => {
    assert.deepEqual(args.slice(0, 3), [
      'ls-remote',
      '--exit-code',
      'https://github.com/dimasma0305/rsctf-challenges.git',
    ]);
    return `${annotated}\trefs/tags/release\n${commit}\trefs/tags/release^{}\n`;
  };
  assert.equal(
    resolveRemoteGitRefCommit(
      'https://github.com/dimasma0305/rsctf-challenges.git',
      'release',
      run,
    ),
    commit,
  );
  assert.throws(
    () => resolveRemoteGitRefCommit('https://github.com/example/repo.git', 'main', () =>
      `${'c'.repeat(40)}\trefs/heads/main\n${'d'.repeat(40)}\trefs/tags/main\n`),
    /resolved to 2 commits/,
  );
  assert.throws(
    () => resolveRemoteGitRefCommit('https://github.com/example/repo.git', 'main', () => ''),
    /resolved to 0 commits/,
  );
});

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

function positiveCallExpressions(source) {
  const calls = [];
  for (const match of source.matchAll(/\bcall\('([^']+)'/g)) {
    const open = source.indexOf('(', match.index);
    let depth = 0;
    let quote = null;
    let escaped = false;
    let lineComment = false;
    let blockComment = false;
    let end = -1;
    for (let index = open; index < source.length; index += 1) {
      const character = source[index];
      const next = source[index + 1];
      if (lineComment) {
        if (character === '\n') lineComment = false;
        continue;
      }
      if (blockComment) {
        if (character === '*' && next === '/') {
          blockComment = false;
          index += 1;
        }
        continue;
      }
      if (quote) {
        if (escaped) escaped = false;
        else if (character === '\\') escaped = true;
        else if (character === quote) quote = null;
        continue;
      }
      if (character === '/' && next === '/') {
        lineComment = true;
        index += 1;
        continue;
      }
      if (character === '/' && next === '*') {
        blockComment = true;
        index += 1;
        continue;
      }
      if (character === "'" || character === '"' || character === '`') {
        quote = character;
        continue;
      }
      if (character === '(') depth += 1;
      else if (character === ')') {
        depth -= 1;
        if (depth === 0) {
          end = index + 1;
          break;
        }
      }
    }
    assert.notEqual(end, -1, `unterminated positive call for ${match[1]}`);
    calls.push({ id: match[1], source: source.slice(match.index, end) });
  }
  return calls;
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
    case 'build': return { buildStatus: 'Success', archiveAvailable: false, files: [], previews: {} };
    case 'ad-state': return { challenges: [], teams: [], scoringPaused: false };
    case 'service-file': return { path: '/etc/hostname', containerRunning: true };
    case 'review-analytics': return { total: 0, likes: 0, dislikes: 0, topLiked: [], topDisliked: [] };
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

test('Admin probes for game-scoped operations require a manager of that exact game', () => {
  const managedGameIds = new Set([context.gameId, context.adGameId, context.kothGameId]);
  let scopedOperations = 0;
  for (const operation of EDIT_OPERATIONS.filter(({ auth }) => auth === 'admin')) {
    const gameContextKey = operation.params.id;
    const expectedGameId = gameContextKey === 'gameId' || gameContextKey?.endsWith('GameId')
      ? context[gameContextKey]
      : null;
    assert.equal(
      assertEditAdminProbeScope(operation, context, managedGameIds),
      expectedGameId,
      operation.id,
    );
    if (expectedGameId === null) continue;
    scopedOperations += 1;
    const otherGames = new Set([...managedGameIds].filter((gameId) => gameId !== expectedGameId));
    assert.throws(
      () => assertEditAdminProbeScope(operation, context, otherGames),
      /is not scoped to game .* managed by the probe identity/,
      operation.id,
    );
  }
  assert.ok(scopedOperations > 0, 'catalog unexpectedly has no game-scoped Admin operation');
  assert.throws(
    () => assertEditAdminProbeScope('edit_ad_ensure_containers', context, new Set([context.gameId])),
    /not scoped to game 12/,
  );
  assert.throws(
    () => assertEditAdminProbeScope('edit_ad_check_override', context, new Set([context.gameId])),
    /not scoped to game 12/,
  );
});

test('every declared response contract has an accepting and rejecting unit sample', () => {
  for (const operation of EDIT_OPERATIONS) {
    assert.equal(validateEditResponse(operation, sampleResponse(operation)), true, operation.id);
    assert.throws(
      () => validateEditResponse(operation, {
        ...sampleResponse(operation),
        body: null,
      }),
      undefined,
      operation.id,
    );
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

test('the disposable orchestrator has one explicit positive call for every catalog id', () => {
  const source = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  const invoked = positiveCallExpressions(source).map(({ id }) => id);
  assert.equal(invoked.length, 64);
  assert.deepEqual(new Set(invoked), new Set(EDIT_OPERATION_IDS));
  assert.equal(new Set(invoked).size, invoked.length, 'positive calls must not hide duplicate coverage');
});

test('every manager-class positive uses the delegated manager token', () => {
  const source = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  const calls = new Map(positiveCallExpressions(source).map((call) => [call.id, call.source]));
  for (const operation of EDIT_OPERATIONS.filter(({ auth }) => auth === 'manager')) {
    assert.match(
      calls.get(operation.id),
      /\bjwt:\s*identities\.managerJwt\b/,
      `${operation.id} silently fell back to the Admin token`,
    );
  }
  assert.match(source, /operation\.auth === 'manager'/);
  assert.match(source, /jwt === identities\.managerJwt/);
  for (const gameContextKey of ['adGameId', 'kothGameId']) {
    assert.match(
      source,
      new RegExp(
        `/api/edit/games/\\$\\{context\\.${gameContextKey}\\}/admins/\\$\\{identities\\.managerUserId\\}`,
      ),
      `${gameContextKey} manager delegation is missing`,
    );
  }
});

test('empty/no-op inspector responses can never satisfy acceptance', () => {
  assert.throws(
    () => validateEditResponse('edit_ad_inspector_create', {
      status: 200,
      body: { containerGuid: '' },
      headers: {},
    }),
    /did not create a real inspector/,
  );
});

test('organizer cleanup derives fixture-owned mutable tags without mutating the ownership ledger', () => {
  const fixtures = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle-fixtures.mjs'), 'utf8');
  const orchestrator = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  assert.match(fixtures, /container_image AS image_ref/);
  assert.match(fixtures, /ad_checker_image AS image_ref/);
  assert.ok(fixtures.includes('imageRef.match(/^docker\\.io\\/rsctf\\/'));
  assert.doesNotMatch(fixtures, /FROM\s+['"`]BuildImageOwnerships['"`]\s+WHERE\s+game_id/i);
  assert.match(orchestrator, /\/api\/admin\/builds\/images\?tag=/);
  assert.match(orchestrator, /response\.json\?\.removed === 1/);
});

test('failed edit acceptance retains its manifest after successful cleanup', () => {
  const source = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  const cleanup = source.slice(
    source.indexOf('async function cleanup()'),
    source.indexOf('async function main()'),
  );
  assert.doesNotMatch(cleanup, /removeRecovery\(/);
  assert.match(source, /shouldRetainLifecycleManifest\(/);
  assert.match(source, /KEEP_EDIT_MANIFEST/);
  assert.match(source, /state\.cleanupVerified = cleanupVerified/);
  assert.match(source, /state\.completed = !failure/);
  assert.match(source, /state\.scenarioFailure = String\(error\?\.stack/);
  assert.match(source, /state\.cleanupFailure = String\(cleanupError\?\.stack/);
  assert.match(source, /state\.leaseFailures\.push/);
  assert.match(source, /state\.scenarioFailure[^]*?saveRecovery\(\)/);
  assert.match(source, /state\.cleanupFailure[^]*?saveRecovery\(\)/);
  assert.match(source, /state\.verificationFailure/);
  assert.match(source, /inspectUniformServerRuntimeIdentity\(serverContainers\)/);
  assert.match(source, /runtimeIdentity\.after = inspectUnchangedServerRuntimeIdentity/);
  assert.match(source, /originalServerRuntimeLogTargets\(startingRuntimeIdentity\)/);
  assert.match(source, /countContainerFatalLogs\(containerId, state\.startedAt\)/);
  assert.match(source, /RSCTF_ACCEPTANCE_REPORTABLE=1 rejects SKIP_EDIT_K6=1/);
});

test('edit cleanup is stable and GitHub import is fenced before and after', () => {
  const source = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  const cleanup = source.slice(
    source.indexOf('function editResidualSnapshot()'),
    source.indexOf('async function main()'),
  );
  assert.match(cleanup, /EDIT_CLEANUP_STABILITY_MS/);
  assert.match(cleanup, /for \(let pass = 0; pass < 2; pass \+= 1\)/);
  const stabilityGate = cleanup.slice(cleanup.indexOf('async function assertStableExactEditCleanup()'));
  assert.ok(
    stabilityGate.indexOf('setTimeout(resolve, delayMs)') < stabilityGate.indexOf('editResidualSnapshot()'),
    'each edit cleanup sample must wait before reading residue',
  );
  assert.match(cleanup, /JSON\.stringify\(passes\[0\]\) === JSON\.stringify\(passes\[1\]\)/);
  assert.match(cleanup, /stable exact residual audit/);
  assert.match(source, /EDIT_GITHUB_EXPECTED_COMMIT must be a full 40-character Git commit/);
  const before = source.indexOf('const githubCommitBefore = resolveRemoteGitRefCommit');
  const request = source.indexOf("await call('edit_challenge_import_github'", before);
  const after = source.indexOf('const githubCommitAfter = resolveRemoteGitRefCommit', request);
  assert.ok(before >= 0 && before < request && request < after, 'GitHub ref fence must bracket the import');
  assert.match(source, /branch\/tag ls-remote fence; the import API does not expose its checked-out commit/);
});

test('cleanup reconciles run-tagged games whose failing create never returned an id', () => {
  const source = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  const cleanup = source.slice(
    source.indexOf('async function cleanup()'),
    source.indexOf('async function main()'),
  );
  assert.match(cleanup, /run-tagged game reconciliation/);
  assert.match(cleanup, /strpos\(title, \$\{sqlLiteral\(runKey\)\}\) > 0/);
  assert.match(cleanup, /for \(const gameId of tagged\) await deleteFutureGame\(gameId\)/);
  assert.match(cleanup, /run-tagged game survived cleanup/);
});

test('edit acceptance awaits the shared orchestration lease with the canonical path', () => {
  const source = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  assert.match(
    source,
    /await acquireExclusiveProcessLock\(loadOrchestrationLockPath,\s*\{/,
  );
  assert.doesNotMatch(source, /loadOrchestrationLockPath\(\)/);
  assert.match(source, /await lease\.release\(\)/);
});

test('GitHub import defaults to the challenge repository rather than its parent gitlink', () => {
  const source = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  assert.match(source, /https:\/\/github\.com\/dimasma0305\/rsctf-challenges\.git/);
  assert.match(source, /'Jeopardy\/Misc\/static-handout'/);
  assert.doesNotMatch(source, /examples\/challenge-repository\/Jeopardy/);
});

test('inspector acceptance keeps challenge eligibility while simulating an offline service', () => {
  const source = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  const prepare = source.slice(
    source.indexOf('async function prepareAdFixture()'),
    source.indexOf('async function prepareKothFixture()'),
  );
  assert.match(prepare, /isEnabled === true/);
  assert.match(prepare, /docker\(\['stop', '--time', '2', adRuntimeBeforeRestart\]\)/);
  assert.match(prepare, /await exerciseInspector\(\)/);
  assert.match(prepare, /docker\(\['start', adRuntimeBeforeRestart\]\)/);
  assert.ok(prepare.indexOf('isEnabled === true') < prepare.indexOf('await exerciseInspector()'));
});

test('A&D and KotH create fixtures immediately prove every supplied engine setting persisted', () => {
  const source = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  for (const field of [
    'adWarmupSeconds',
    'adSnapshotRetentionDays',
    'adTickSeconds',
    'adFlagLifetimeTicks',
    'adResetCooldownMinutes',
    'adAllowSnapshotDownload',
    'adGetflagWindowFraction',
    'adMinGracePeriodSeconds',
    'adEpochTicks',
    'kothEpochTicks',
    'kothCycleTicks',
    'kothChampionCooldownTicks',
    'kothClaimConfirmationTicks',
  ]) {
    assert.match(source, new RegExp(`\\b${field}:`), field);
  }
  const adPrepare = source.slice(
    source.indexOf('async function prepareAdFixture()'),
    source.indexOf('async function prepareKothFixture()'),
  );
  const kothPrepare = source.slice(
    source.indexOf('async function prepareKothFixture()'),
    source.indexOf('async function exerciseInspector()'),
  );
  assert.match(adPrepare, /assertPersistedGameSettings\(context\.adGameId, AD_CREATION_SETTINGS/);
  assert.match(kothPrepare, /assertPersistedGameSettings\(context\.kothGameId, KOTH_CREATION_SETTINGS/);
  assert.ok(adPrepare.indexOf('saveRecovery()') < adPrepare.indexOf('assertPersistedGameSettings('));
  assert.ok(kothPrepare.indexOf('saveRecovery()') < kothPrepare.indexOf('assertPersistedGameSettings('));
});

test('game import acceptance reads back portable semantics and proves late-failure rollback', () => {
  const orchestrator = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  const fixtures = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle-fixtures.mjs'), 'utf8');
  const destructive = orchestrator.slice(
    orchestrator.indexOf('async function destructivePositiveSurface()'),
    orchestrator.indexOf('async function deleteFutureGame('),
  );
  assert.match(destructive, /assertSemanticGameImport\(/);
  assert.match(destructive, /importedGameModel\.poster == null/);
  assert.match(destructive, /importedChecker\?\.adCheckerImage == null/);
  assert.match(destructive, /transactionalFailureGameArchive\(/);
  assert.match(destructive, /expectStatus\(rollbackResponse, 500/);
  assert.equal((destructive.match(/assertFailedGameImportRolledBack\(/g) || []).length, 2);
  assert.ok(
    destructive.indexOf("call('edit_game_export'") < destructive.indexOf("call('edit_game_admin_delete'"),
    'manager delegation must remain live through the manager-authorized export',
  );
  assert.match(fixtures, /challengeConfigs:\s*\[\s*\{ challengeId, permissions: 15 \},\s*\{ challengeId, permissions: 15 \}/);
  assert.match(fixtures, /source\.poster_hash IS NOT NULL AND imported\.poster_hash IS NULL/);
  assert.match(fixtures, /source\.ad_checker_image IS NOT NULL AND imported\.ad_checker_image IS NULL/);
  assert.match(fixtures, /failed game import left physical blob/);
});

test('late-failure archive materializer uses unique content-addressed ownership markers', () => {
  const fixtures = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle-fixtures.mjs'), 'utf8');
  const builder = fixtures.slice(
    fixtures.indexOf('export function transactionalFailureGameArchive('),
    fixtures.indexOf('export async function waitForSql('),
  );
  assert.match(builder, /createHash\('sha256'\)\.update\(blob\)\.digest\('hex'\)/);
  assert.match(builder, /writeFileSync\(join\(root, 'files', hash\), blob/);
  assert.match(builder, /execFileSync\('zip', \['-q', '-r', archive, '\.'\]/);
  assert.match(builder, /attachmentType: 'Local'/);
  assert.match(builder, /attachmentFileHash: hash/);
  assert.match(builder, /adCheckerImage: checker/);
});

test('KotH fixture uses public provisioning and proves exact durable and Docker ownership', () => {
  const orchestrator = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  const fixtures = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle-fixtures.mjs'), 'utf8');
  const prepare = orchestrator.slice(
    orchestrator.indexOf('async function prepareKothFixture()'),
    orchestrator.indexOf('async function exerciseInspector()'),
  );
  const cleanup = orchestrator.slice(
    orchestrator.indexOf('async function cleanup()'),
    orchestrator.indexOf('async function main()'),
  );
  assert.match(prepare, /\/api\/edit\/games\/\$\{context\.kothGameId\}\/ad\/EnsureContainers/);
  assert.match(prepare, /discoverManagedKothHill\(context\.kothGameId, context\.kothChallengeId\)/);
  assert.match(prepare, /state\.runtimeIds\.push\(hill\.backendId\)/);
  assert.doesNotMatch(prepare, /seedKothTarget|startHill/);
  assert.doesNotMatch(cleanup, /teardownHill|lckoth_hill/);
  assert.match(fixtures, /JOIN "GameChallenges" challenge ON challenge\.id=target\.challenge_id/);
  assert.match(fixtures, /JOIN "Containers" container ON container\.id=target\.shared_container_id/);
  assert.match(fixtures, /JOIN "KothCrownCycles" cycle ON cycle\.game_id=target\.game_id/);
  assert.match(fixtures, /cycle\.replacement_container_id=target\.container_id/);
  assert.match(fixtures, /cycle\.phase='Active'/);
  assert.match(fixtures, /owner\.ownerCount === 1/);
  assert.match(fixtures, /labels\['rsctf\.managed'\] === expectedScope/);
  assert.match(fixtures, /labels\['rsctf\.scope'\] === expectedScope/);
  assert.match(fixtures, /labels\['rsctf\.operation'\] === owner\.operationOwner/);
  assert.match(fixtures, /PortBindings/);
});

test('clone acceptance verifies template semantics and rejects inherited live ownership', () => {
  const orchestrator = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  const fixtures = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle-fixtures.mjs'), 'utf8');
  const positives = orchestrator.slice(
    orchestrator.indexOf('async function positiveReadAndMutationSurface()'),
    orchestrator.indexOf('async function runReadSimulation()'),
  );
  assert.match(positives, /assertSemanticGameClone\(/);
  assert.ok(positives.indexOf('saveRecovery()') < positives.indexOf('assertSemanticGameClone('));
  assert.match(fixtures, /clone\.public_key<>source\.public_key/);
  assert.match(fixtures, /clone\.private_key<>source\.private_key/);
  assert.match(fixtures, /cloneChallenges === sourceChallenges/);
  assert.match(fixtures, /EXCEPT SELECT \$\{copiedColumns\}/);
  assert.match(fixtures, /test_container_id IS NOT NULL OR shared_container_id IS NOT NULL/);
  assert.match(fixtures, /clone changed \$\{flagMismatch\} static flag bindings/);
});

test('A&D acceptance proves configured checker grace and independent timing', () => {
  const source = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  assert.match(source, /round\.flags_published_at/);
  assert.match(source, /delivery\.completed_at AS delivered_at/);
  assert.match(source, /checked_at-delivered_at/);
  assert.match(source, /minimumMs.*graceSeconds/s);
  assert.match(source, /distinctMilliseconds\) >= 2/);
  assert.match(source, /state\.checkerTiming = checkerTiming/);
  assert.match(source, /start: originalLiveStart \+ 60_000/);
  assert.match(source, /expected: 400/);
  assert.match(source, /liveGameAfter\.json\.start === originalLiveStart/);
});

test('A&D override acceptance changes authoritative evidence and proves both invalidation layers', () => {
  const source = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  const positives = source.slice(
    source.indexOf('async function positiveReadAndMutationSurface()'),
    source.indexOf('async function runReadSimulation()'),
  );
  assert.match(positives, /FROM \"AdEpochRollups\" rollup/);
  assert.match(positives, /result\.sla_credit IS NOT NULL/);
  assert.match(positives, /overrideTarget\.status !== overrideBefore\.status/);
  assert.match(positives, /newStatus: overrideTarget\.label, note: overrideNote/);
  assert.match(positives, /FROM \"AdCheckResults\" WHERE id=\$\{context\.checkId\}/);
  assert.match(positives, /overrideAfter\.status === overrideTarget\.status/);
  assert.match(positives, /overrideAfter\.message === expectedOverrideMessage/);
  assert.match(positives, /Number\(overrideAfter\.slaCredit\) === 0/);
  assert.match(positives, /rollupSuffixBefore > 0/);
  assert.match(positives, /rollupSuffixAfter === 0/);
  assert.match(positives, /gameRevisionAfterOverride !== gameRevisionBeforeOverride/);
  for (const key of [
    '_AdScoreBoard_',
    '_AdScoreBoard_${context.adGameId}:stale',
    '_AdScoreBoardFrozen_',
    '_AdScoreBoardFrozen_${context.adGameId}:stale',
  ]) assert.ok(positives.includes(key), `missing cache invalidation evidence for ${key}`);
  assert.match(positives, /redisRaw\(\['SET', key, cacheSentinel, 'EX', '300'\]/);
  assert.match(positives, /redisRaw\(\['EXISTS', key\]/);
  assert.ok(
    positives.indexOf("call('edit_ad_check_override'") < positives.indexOf('const overrideAfter'),
    'authoritative readback must happen after the public override',
  );
  assert.match(positives, /state\.adCheckOverride = \{/);
});

test('KotH recovery acceptance faults a scoped durable phase and proves receipt/runtime convergence', () => {
  const source = readFileSync(join(REPOSITORY, 'tests/load/edit-lifecycle.mjs'), 'utf8');
  const positives = source.slice(
    source.indexOf('async function positiveReadAndMutationSurface()'),
    source.indexOf('async function runReadSimulation()'),
  );
  assert.match(positives, /setAdScoringPaused\(context\.kothGameId, true\)/);
  assert.match(positives, /phase='ReadinessPending'/);
  assert.match(positives, /cycle\.phase='Active'/);
  assert.match(positives, /game\.title=\$\{sqlLiteral\(titleFor\(tags\.koth\)\)\}/);
  assert.match(positives, /challenge\.title=\$\{sqlLiteral\(`edit-koth-\$\{runKey\}`\)\}/);
  assert.match(positives, /docker\(\['stop', '--time', '2', oldHill\.backendId\]\)/);
  assert.ok(
    positives.indexOf("phase='ReadinessPending'") <
      positives.indexOf("call('edit_koth_recover'") &&
      positives.indexOf("docker(['stop', '--time', '2', oldHill.backendId])") <
      positives.indexOf("call('edit_koth_recover'"),
    'the durable fault and stopped runtime must precede public recovery',
  );
  assert.match(positives, /recovered\.model\.resetPhase === 'Active'/);
  assert.match(positives, /replacementHill\.backendId !== oldHill\.backendId/);
  assert.match(positives, /docker\(\['container', 'inspect', oldHill\.backendId\]\)\.status !== 0/);
  assert.match(positives, /cycleAfter\.phase === 'Active'/);
  assert.match(positives, /cycleAfter\.resetAttempt === cycleBefore\.resetAttempt \+ 1/);
  assert.match(positives, /cycleAfter\.replacementContainerId === replacementHill\.backendId/);
  assert.match(positives, /cycleAfter\.targetContainerId === replacementHill\.backendId/);
  for (const phase of [
    'DestroyPending',
    'CreatePending',
    'PublishPending',
    'CapabilityPending',
    'ReadinessPending',
    'FirewallPending',
  ]) assert.ok(positives.includes(`'${phase}'`), `missing ${phase} recovery receipt assertion`);
  assert.match(positives, /recoveryReceipts\.every\(\(receipt\) => !preexistingReceiptIds\.has\(receipt\.id\)\)/);
  assert.match(positives, /recoveredHillView\?\.durablePhase === 'Active'/);
  assert.match(positives, /recoveredHillView\.containerGuid === replacementHill\.backendId/);
  assert.match(positives, /state\.kothRecovery = \{/);
});
