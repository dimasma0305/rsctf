// Exhaustive disposable acceptance for every registered `/api/edit` operation.
// The scenario creates future mutable, active A&D, and active KotH fixtures,
// checks each route's authorization boundary, validates exact wire contracts,
// runs fixed-rate read pressure, and then removes only its recorded identities.
import { randomUUID } from 'node:crypto';

import * as A from './applib.mjs';
import {
  EDIT_OPERATION_IDS,
  EDIT_OPERATIONS,
  assertEditAdminProbeScope,
  assertCompleteEditCoverage,
  resolveEditOperationPath,
  validateEditResponse,
} from './edit-lifecycle.js';
import {
  acquireAdminLifecycleDatabaseLock,
  deleteDisposableAdminGame,
  disposableAdminGameRuntimeIds,
  expectStatus,
  inspectUnchangedServerRuntimeIdentity,
  inspectUniformServerRuntimeIdentity,
  originalServerRuntimeLogTargets,
  persistRecovery,
  rawRequest,
  removeRecovery,
  shouldRetainLifecycleManifest,
  sqlLiteral,
} from './admin-fixtures.mjs';
import {
  assertDisposableEditStack,
  assertFailedGameImportRolledBack,
  assertPersistedGameSettings,
  assertRuntimeRoles,
  assertSemanticGameClone,
  assertSemanticGameImport,
  challengeArchive,
  discoverManagedKothHill,
  mutateContainerFilesystem,
  ownedBuildImageRefs,
  removeCheckerDirectory,
  removeRuntime,
  requireCondition,
  resolveRemoteGitRefCommit,
  transactionalFailureGameArchive,
  waitForSql,
} from './edit-lifecycle-fixtures.mjs';
import {
  assertSafeAdminTarget,
} from './admin-lifecycle.js';
import { docker, mintJwt, PG, RSCTF, runK6, sql, TARGET } from './lib.mjs';
import { countContainerFatalLogs } from './log-audit.mjs';
import {
  acquireExclusiveProcessLock,
  loadOrchestrationLockPath,
} from './process-control.mjs';

const runKey = `${Date.now().toString(36)}${process.pid.toString(36)}`;
const tags = Object.freeze({
  future: `adm${runKey}f`,
  ad: `adm${runKey}a`,
  koth: `adm${runKey}k`,
});
const titleFor = (tag) => `ADMIN-LIFECYCLE-${tag}`;
const transferCheckerTitle = `edit-transfer-ad-${runKey}`;
const AD_CREATION_SETTINGS = Object.freeze({
  adWarmupSeconds: 0,
  adSnapshotRetentionDays: 3,
  adTickSeconds: 30,
  adFlagLifetimeTicks: 5,
  adResetCooldownMinutes: 7,
  adAllowSnapshotDownload: true,
  adGetflagWindowFraction: 0.5,
  adMinGracePeriodSeconds: 2,
  adEpochTicks: 2,
});
const KOTH_CREATION_SETTINGS = Object.freeze({
  ...AD_CREATION_SETTINGS,
  kothEpochTicks: 2,
  kothCycleTicks: 1,
  kothChampionCooldownTicks: 0,
  kothClaimConfirmationTicks: 1,
});
const recoveryPath = `/tmp/rsctf-edit-lifecycle-${runKey}.json`;
const k6SummaryPath = process.env.EDIT_SUMMARY_JSON || `/tmp/rsctf-edit-lifecycle-${runKey}-k6.json`;
const githubRepository = process.env.EDIT_GITHUB_REPOSITORY ||
  'https://github.com/dimasma0305/rsctf-challenges.git';
const githubRef = String(process.env.EDIT_GITHUB_REF || 'main').trim();
const githubSubpath = process.env.EDIT_GITHUB_SUBPATH || 'Jeopardy/Misc/static-handout';
const githubExpectedCommit = String(process.env.EDIT_GITHUB_EXPECTED_COMMIT || '').trim().toLowerCase();
if (!/^[a-f0-9]{40}$/.test(githubExpectedCommit)) {
  throw new Error('EDIT_GITHUB_EXPECTED_COMMIT must be a full 40-character Git commit');
}
const reportableAcceptance = process.env.RSCTF_ACCEPTANCE_REPORTABLE === '1';
const skipEditLoad = process.env.SKIP_EDIT_K6 === '1';
if (reportableAcceptance && skipEditLoad) {
  throw new Error('RSCTF_ACCEPTANCE_REPORTABLE=1 rejects SKIP_EDIT_K6=1');
}
const rawWebTargets = String(process.env.WEB_TARGETS || '').trim();
const webTargets = (rawWebTargets.startsWith('[') ? JSON.parse(rawWebTargets) : rawWebTargets.split(','))
  .map((target) => target.trim().replace(/\/$/, ''))
  .filter(Boolean);
const controlTarget = String(process.env.CONTROL_TARGET || TARGET).replace(/\/$/, '');
const redisContainer = process.env.REDIS_CONTAINER || PG.replace(/-db-(\d+)$/, '-redis-$1');
const serverContainers = [...new Set([
  RSCTF,
  ...String(process.env.ADMIN_RSCTF_CONTAINERS || '').split(',').map((name) => name.trim()).filter(Boolean),
])];

const operationById = new Map(EDIT_OPERATIONS.map((operation) => [operation.id, operation]));
const covered = new Set();
const timings = [];
const blockers = [];
let requestIndex = 0;
let processLock;
let databaseLock;

const state = {
  schemaVersion: 2,
  runKey,
  target: TARGET,
  startedAt: Date.now(),
  completed: false,
  reportable: reportableAcceptance && !skipEditLoad,
  scenarioFailure: null,
  cleanupFailure: null,
  verificationFailure: null,
  leaseFailures: [],
  cacheKeys: [],
  gameIds: [],
  futureGameIds: [],
  containerIds: [],
  runtimeIds: [],
  postIds: [],
  ownedImageRefs: [],
};

const context = {
  gameId: null,
  challengeId: null,
  deletableChallengeId: null,
  pendingApproveId: null,
  pendingRejectId: null,
  archiveChallengeId: null,
  containerChallengeId: null,
  workerGameId: null,
  workerChallengeId: null,
  transferChallengeId: null,
  postId: null,
  managerUserId: null,
  flagId: null,
  noticeId: null,
  divisionId: null,
  adGameId: null,
  adChallengeId: null,
  serviceId: null,
  // Authorization probes only need a syntactically valid id; the completed
  // checker fixture replaces this before the positive override operation.
  checkId: 1,
  inspectorId: 'inspector-not-created',
  kothGameId: null,
  kothChallengeId: null,
};

let identities;
let primaryGameModel;
let cloneGameId;
let importedGameId;
let authorizationGameId;
let adRuntimeBeforeRestart;

const AD_CHECK_STATUS = Object.freeze({
  0: 'Ok',
  1: 'Mumble',
  2: 'Offline',
  3: 'InternalError',
});

function saveRecovery() {
  persistRecovery(recoveryPath, state);
}

function redisRaw(args, label) {
  const result = docker(['exec', redisContainer, 'redis-cli', '--raw', ...args]);
  requireCondition(result.status === 0, `${label}: ${result.stderr.trim()}`);
  return result.stdout.trim();
}

function responseBody(response, operation) {
  if (operation.responseKind === 'zip' || operation.responseKind === 'tar') return response.bytes;
  const json = response.json;
  if (
    json &&
    typeof json === 'object' &&
    !Array.isArray(json) &&
    Object.hasOwn(json, 'data') &&
    !(Object.hasOwn(json, 'total') && Object.hasOwn(json, 'length'))
  ) {
    return json.data;
  }
  return json;
}

function multipartBody({ filename, content, contentType = 'application/octet-stream', field = 'file' }) {
  const form = new FormData();
  form.append(field, new Blob([content], { type: contentType }), filename);
  return form;
}

async function call(id, {
  ctx = context,
  body,
  form,
  jwt = A.adminJwt(),
  headers = {},
  baseUrl = TARGET,
  label = id,
} = {}) {
  const operation = operationById.get(id);
  if (!operation) throw new Error(`unknown edit operation ${id}`);
  if (covered.has(id)) throw new Error(`${id} was already positively exercised`);
  if (operation.auth === 'manager') {
    requireCondition(
      identities && jwt === identities.managerJwt,
      `${id} positive coverage must use the delegated manager identity`,
    );
  }
  const path = resolveEditOperationPath(operation, ctx);
  const requestHeaders = { ...headers };
  let requestBody;
  if (form) requestBody = multipartBody(form);
  else if (body !== undefined) {
    requestHeaders['content-type'] = 'application/json';
    requestBody = JSON.stringify(body);
  }
  const started = performance.now();
  const response = await rawRequest(operation.method, path, {
    baseUrl,
    jwt,
    ip: `10.254.${Math.floor(requestIndex / 240) % 240}.${(requestIndex++ % 240) + 1}`,
    headers: requestHeaders,
    body: requestBody,
    timeoutMs: 180_000,
  });
  expectStatus(response, operation.expectedStatuses, label);
  const model = responseBody(response, operation);
  validateEditResponse(operation, {
    status: response.status,
    body: model,
    headers: response.headers,
  });
  covered.add(id);
  const elapsed = Math.round((performance.now() - started) * 100) / 100;
  timings.push({ id, ms: elapsed });
  console.log(`  ✓ ${id} (${response.status}, ${elapsed} ms)`);
  return { model, response, path };
}

async function uncatalogued(method, path, { body, jwt = A.adminJwt(), expected = 200 } = {}) {
  const response = await A.api(method, path, {
    body,
    jwt,
    ip: `10.255.1.${(requestIndex++ % 240) + 1}`,
    timeoutMs: 180_000,
  });
  return expectStatus(response, expected, `${method} ${path}`);
}

function fixtureFingerprint() {
  if (!context.gameId) return null;
  const value = sql(
    `SELECT json_build_object(` +
      `'title',(SELECT title FROM "Games" WHERE id=${context.gameId}),` +
      `'challenges',(SELECT count(*) FROM "GameChallenges" WHERE game_id=${context.gameId}),` +
      `'notices',(SELECT count(*) FROM "GameNotices" WHERE game_id=${context.gameId}),` +
      `'divisions',(SELECT count(*) FROM "Divisions" WHERE game_id=${context.gameId}),` +
      `'managers',(SELECT count(*) FROM "GameManagers" WHERE game_id=${context.gameId}),` +
      `'flags',(SELECT count(*) FROM "FlagContexts" flag JOIN "GameChallenges" challenge ` +
        `ON challenge.id=flag.challenge_id WHERE challenge.game_id=${context.gameId})` +
    `)::text`,
  );
  return JSON.parse(value);
}

function authorizationProbeRequest(operation) {
  if (operation.multipart) {
    const form = new FormData();
    // The public submission path is intentionally reachable by an ordinary
    // authenticated user. Give it a well-formed multipart request containing
    // an invalid archive so it reaches archive validation (400) without
    // creating data. Manager/admin-denial probes return before reading bytes.
    const content = operation.auth === 'user-submit'
      ? Buffer.from('not-a-zip')
      : Buffer.from([0]);
    form.append('file', new Blob([content], { type: 'application/octet-stream' }), 'auth-probe.bin');
    return { headers: {}, body: form };
  }

  const jsonBodies = {
    edit_game_update: primaryGameModel,
    edit_challenge_add: { title: 'authorization probe', category: 'Misc', type: 'StaticAttachment' },
    edit_challenge_update: { content: 'authorization probe' },
    edit_challenge_import_github: {
      repoUrl: 'https://github.com/dimasma0305/rsctf-challenges.git',
      subpath: 'Jeopardy/Misc/static-handout',
    },
    edit_challenge_reject: { note: 'authorization probe' },
    edit_challenge_attachment: {
      attachmentType: 'Remote',
      remoteUrl: 'https://example.invalid/authorization-probe.txt',
    },
    edit_flags_add: [{ flag: `flag{authorization_probe_${runKey}}` }],
    edit_notice_add: { content: 'authorization probe' },
    edit_notice_update: { content: 'authorization probe' },
    edit_division_add: {
      name: `authorization-probe-${runKey}`,
      inviteCode: `authorization-probe-${runKey}`,
      defaultPermissions: 15,
      challengeConfigs: [],
    },
    edit_division_update: { name: `authorization-probe-${runKey}` },
  };
  const body = jsonBodies[operation.id];
  if (body === undefined) return { headers: {}, body: undefined };
  return {
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  };
}

async function authorizationMatrix() {
  console.log('\n64-operation authorization matrix…');
  const before = fixtureFingerprint();
  let missingChecks = 0;
  let privilegeChecks = 0;
  for (const [index, operation] of EDIT_OPERATIONS.entries()) {
    const path = resolveEditOperationPath(operation, context);
    const missingProbe = authorizationProbeRequest(operation);
    const missing = await rawRequest(operation.method, path, {
      jwt: null,
      ip: `10.250.1.${(index % 240) + 1}`,
      ...missingProbe,
      timeoutMs: 30_000,
    });
    requireCondition(missing.status === 401, `${operation.id} accepted missing credentials (${missing.status})`);
    missingChecks += 1;

    let token;
    let expected;
    if (operation.auth === 'admin') {
      assertEditAdminProbeScope(operation, context, identities.managerGameIds);
      token = identities.managerJwt;
      expected = [403];
    } else if (operation.auth === 'manager') {
      token = identities.crossGameManagerJwt;
      expected = [403];
    } else if (operation.auth === 'managed-list') {
      token = identities.ordinaryJwt;
      expected = [403];
    } else {
      // User submissions intentionally allow every authenticated user. A
      // malformed archive must reach request validation (400/415), not auth 403.
      token = identities.ordinaryJwt;
      expected = [400, 415];
    }
    const privilegeProbe = authorizationProbeRequest(operation);
    const denied = await rawRequest(operation.method, path, {
      jwt: token,
      ip: `10.250.2.${(index % 240) + 1}`,
      ...privilegeProbe,
      timeoutMs: 30_000,
    });
    requireCondition(
      expected.includes(denied.status),
      `${operation.id} privilege probe returned ${denied.status}, expected ${expected.join('/')}`,
    );
    privilegeChecks += 1;
  }
  requireCondition(
    JSON.stringify(fixtureFingerprint()) === JSON.stringify(before),
    'authorization matrix mutated the future fixture despite rejected credentials',
  );
  console.log(`  ✓ ${missingChecks} missing-token and ${privilegeChecks} least-privilege checks`);
}

function ensureLocalImage(reference) {
  let inspected = docker(['image', 'inspect', reference, '--format', '{{.Id}}']);
  if (inspected.status !== 0) {
    const pulled = docker(['pull', reference]);
    requireCondition(pulled.status === 0, `pull ${reference}: ${pulled.stderr.trim()}`);
    inspected = docker(['image', 'inspect', reference, '--format', '{{.Id}}']);
  }
  const image = inspected.stdout.trim();
  requireCondition(/^sha256:[a-f0-9]{64}$/.test(image), `${reference} did not resolve to an immutable image id`);
  return image;
}

function futureGameBody() {
  const now = A.nowMs();
  return {
    title: titleFor(tags.future),
    hidden: true,
    summary: `edit acceptance ${runKey}`,
    content: 'Disposable future organizer fixture',
    acceptWithoutReview: true,
    allowUserSubmissions: true,
    writeupRequired: false,
    teamMemberCountLimit: 1,
    containerCountLimit: 3,
    practiceMode: false,
    start: now + 86_400_000,
    end: now + 90_000_000,
    writeupDeadline: now + 90_000_000,
    writeupNote: '',
    bloodBonus: 0,
    ...KOTH_CREATION_SETTINGS,
  };
}

async function prepareFutureFixture() {
  console.log('\nfuture mutable organizer fixture…');
  const created = await call('edit_game_add', { body: futureGameBody() });
  context.gameId = created.model.id;
  context.workerGameId = context.gameId;
  primaryGameModel = created.model;
  state.gameIds.push(context.gameId);
  state.futureGameIds.push(context.gameId);
  saveRecovery();

  const cohort = A.seedCohort(context.gameId, 3);
  const [managerUserId, ordinaryUserId, crossManagerUserId] = cohort.userIds;
  const stamp = (id) => sql(`SELECT security_stamp FROM "AspNetUsers" WHERE id=${sqlLiteral(id)}::uuid`);
  context.managerUserId = managerUserId;
  identities = {
    managerUserId,
    managerJwt: mintJwt(managerUserId, stamp(managerUserId), 1),
    ordinaryUserId,
    ordinaryJwt: mintJwt(ordinaryUserId, stamp(ordinaryUserId), 1),
    crossManagerUserId,
    crossGameManagerJwt: mintJwt(crossManagerUserId, stamp(crossManagerUserId), 1),
    managerGameIds: new Set(),
  };

  authorizationGameId = await A.createGame({
    ...futureGameBody(),
    title: `EDIT-AUTH-${runKey}`,
    allowUserSubmissions: false,
  });
  state.gameIds.push(authorizationGameId);
  state.futureGameIds.push(authorizationGameId);
  saveRecovery();
  await uncatalogued('POST', `/api/edit/games/${authorizationGameId}/admins/${crossManagerUserId}`);

  await call('edit_game_admin_add');
  identities.managerGameIds.add(context.gameId);
  const managerList = await call('edit_games_get', { jwt: identities.managerJwt });
  requireCondition(
    managerList.model.data.some((game) => game.id === context.gameId),
    'manager game list omitted its delegated game',
  );

  const post = await call('edit_post_add', {
    body: {
      title: `Edit acceptance ${runKey}`,
      summary: 'Disposable post',
      content: 'Organizer route contract probe',
      tags: ['load-test'],
      isPinned: false,
    },
  });
  context.postId = post.model;
  state.postIds.push(context.postId);

  const challenge = await call('edit_challenge_add', {
    jwt: identities.managerJwt,
    body: { title: `edit-static-${runKey}`, category: 'Misc', type: 'StaticAttachment' },
  });
  context.challengeId = challenge.model.id;
  await call('edit_challenge_update', {
    jwt: identities.managerJwt,
    body: {
      content: 'Updated by exhaustive edit acceptance',
      originalScore: 1000,
      minScoreRate: 0.25,
      difficulty: 2,
      submissionLimit: 10,
    },
  });

  context.deletableChallengeId = await A.createChallenge(context.gameId, {
    title: `edit-delete-${runKey}`, category: 'Misc', type: 'StaticAttachment',
  });

  const containerImage = ensureLocalImage(process.env.EDIT_CONTAINER_IMAGE || 'nginx:alpine');
  context.containerChallengeId = await A.createChallenge(context.gameId, {
    title: `edit-container-${runKey}`, category: 'Pwn', type: 'StaticContainer',
  });
  await A.setChallenge(context.gameId, context.containerChallengeId, {
    content: 'Disposable test-container route fixture',
    containerImage,
    memoryLimit: 64,
    cpuCount: 1,
    exposePort: 80,
    enableTrafficCapture: false,
  });
  await A.addFlags(context.gameId, context.containerChallengeId, [`flag{edit_container_${runKey}}`]);

  context.workerChallengeId = await A.createChallenge(context.gameId, {
    title: `edit-workload-${runKey}`, category: 'Web', type: 'StaticContainer',
  });
  const workloadSpec = {
    gameKind: 'jeopardy',
    platform: { operatingSystem: 'linux', architecture: 'amd64' },
    services: [{
      name: 'app',
      image: {
        type: 'registryDigest',
        repository: 'registry.example/rsctf/edit-probe',
        digest: `sha256:${'a'.repeat(64)}`,
      },
      resources: { cpuMillis: 100, memoryBytes: 64 * 1024 * 1024 },
      replicas: 2,
      stateless: true,
      environment: { RSCTF_EDIT_PROBE: runKey },
      ports: [{ name: 'http', containerPort: 8080, protocol: 'tcp' }],
    }],
    primaryEndpoint: { service: 'app', port: 'http' },
    flagTarget: { service: 'app', path: '/flag' },
  };
  await A.setChallenge(context.gameId, context.workerChallengeId, { workloadSpec });

  // Transfer archives must preserve portable A&D policy while clearing this
  // deliberately foreign deployment-local executable reference on import.
  context.transferChallengeId = await A.createChallenge(context.gameId, {
    title: transferCheckerTitle, category: 'Pwn', type: 'AttackDefense',
  });
  await A.setChallenge(context.gameId, context.transferChallengeId, {
    content: 'Portable A&D import semantics fixture',
    containerImage,
    memoryLimit: 96,
    cpuCount: 1,
    exposePort: 8080,
    originalScore: 100,
    minScoreRate: 0.25,
    difficulty: 1,
    submissionLimit: 0,
    adCheckerImage: `/data/files/checkers/load/foreign-${runKey}/checker`,
    adAllowEgress: true,
    adAllowSelfReset: true,
    adSshRequiresFlag: true,
    adScoringWeight: 1.1,
  });

  await call('edit_flags_add', {
    jwt: identities.managerJwt,
    body: [
      { flag: `flag{edit_primary_${runKey}}` },
      { flag: `flag{edit_remove_${runKey}}` },
    ],
  });
  context.flagId = Number(sql(
    `SELECT id FROM "FlagContexts" WHERE challenge_id=${context.challengeId} ` +
      `AND flag=${sqlLiteral(`flag{edit_remove_${runKey}}`)} ORDER BY id DESC LIMIT 1`,
  ));
  requireCondition(Number.isSafeInteger(context.flagId) && context.flagId > 0, 'flag fixture was not persisted');

  const notice = await call('edit_notice_add', {
    jwt: identities.managerJwt,
    body: { content: `edit notice ${runKey}` },
  });
  context.noticeId = notice.model.id;
  const division = await call('edit_division_add', {
    jwt: identities.managerJwt,
    body: {
      name: `edit-${runKey}`,
      inviteCode: `invite-${runKey}`,
      defaultPermissions: 15,
      challengeConfigs: [{ challengeId: context.challengeId, permissions: 15 }],
    },
  });
  context.divisionId = division.model.id;

  const pendingArchive = challengeArchive([
    { name: `Pending Approve ${runKey}`, flag: `flag{pending_approve_${runKey}}` },
    { name: `Pending Reject ${runKey}`, flag: `flag{pending_reject_${runKey}}` },
  ]);
  const submitted = await call('edit_challenge_submit', {
    jwt: identities.ordinaryJwt,
    form: { filename: `${runKey}-pending.zip`, content: pendingArchive, contentType: 'application/zip' },
  });
  requireCondition(submitted.model.imported === 2 && submitted.model.failed === 0, 'user submission did not create two pending challenges');
  context.pendingApproveId = Number(sql(
    `SELECT id FROM "GameChallenges" WHERE game_id=${context.gameId} ` +
      `AND title=${sqlLiteral(`Pending Approve ${runKey}`)} ORDER BY id DESC LIMIT 1`,
  ));
  context.pendingRejectId = Number(sql(
    `SELECT id FROM "GameChallenges" WHERE game_id=${context.gameId} ` +
      `AND title=${sqlLiteral(`Pending Reject ${runKey}`)} ORDER BY id DESC LIMIT 1`,
  ));

  const trustedArchive = challengeArchive([
    { name: `Archive Audit ${runKey}`, flag: `flag{archive_audit_${runKey}}` },
  ]);
  const imported = await call('edit_challenge_import', {
    jwt: identities.managerJwt,
    form: { filename: `${runKey}-trusted.zip`, content: trustedArchive, contentType: 'application/zip' },
  });
  requireCondition(imported.model.imported === 1 && imported.model.failed === 0, 'trusted challenge import failed');
  context.archiveChallengeId = Number(sql(
    `SELECT id FROM "GameChallenges" WHERE game_id=${context.gameId} ` +
      `AND title=${sqlLiteral(`Archive Audit ${runKey}`)} ORDER BY id DESC LIMIT 1`,
  ));

  const githubCommitBefore = resolveRemoteGitRefCommit(githubRepository, githubRef);
  state.githubImportFence = {
    repository: githubRepository,
    ref: githubRef,
    subpath: githubSubpath,
    expectedCommit: githubExpectedCommit,
    commitBefore: githubCommitBefore,
    commitAfter: null,
    limitation: 'branch/tag ls-remote fence; the import API does not expose its checked-out commit',
  };
  saveRecovery();
  requireCondition(
    githubCommitBefore === githubExpectedCommit,
    `GitHub import ref ${githubRef} resolved ${githubCommitBefore}, expected ${githubExpectedCommit}`,
  );
  let github;
  let githubImportFailure = null;
  try {
    github = await call('edit_challenge_import_github', {
      jwt: identities.managerJwt,
      body: {
        repoUrl: githubRepository,
        ref: githubRef,
        subpath: githubSubpath,
      },
    });
  } catch (error) {
    githubImportFailure = error;
  }
  let githubFenceFailure = null;
  try {
    const githubCommitAfter = resolveRemoteGitRefCommit(githubRepository, githubRef);
    state.githubImportFence.commitAfter = githubCommitAfter;
    requireCondition(
      githubCommitAfter === githubCommitBefore && githubCommitAfter === githubExpectedCommit,
      `GitHub import ref ${githubRef} moved during import: before=${githubCommitBefore}, ` +
        `after=${githubCommitAfter}, expected=${githubExpectedCommit}`,
    );
  } catch (error) {
    githubFenceFailure = error;
  }
  saveRecovery();
  if (githubImportFailure && githubFenceFailure) {
    throw new AggregateError(
      [githubImportFailure, githubFenceFailure],
      'GitHub import and its remote-ref fence both failed',
    );
  }
  if (githubImportFailure) throw githubImportFailure;
  if (githubFenceFailure) throw githubFenceFailure;
  requireCondition(github.model.imported >= 1 && github.model.failed === 0, `GitHub import was not successful: ${JSON.stringify(github.model)}`);

  const onePixelPng = Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=',
    'base64',
  );
  await call('edit_game_poster_update', {
    jwt: identities.managerJwt,
    form: { filename: `${runKey}.png`, content: onePixelPng, contentType: 'image/png' },
  });

  await call('edit_challenge_attachment', {
    jwt: identities.managerJwt,
    body: { attachmentType: 'Remote', remoteUrl: `https://example.invalid/${runKey}.txt` },
  });
  const rebuild = await call('edit_challenge_rebuild', {
    ctx: { ...context, gameId: context.gameId },
    jwt: identities.managerJwt,
  });
  requireCondition(/success/i.test(rebuild.model.buildStatus), `container rebuild did not succeed: ${JSON.stringify(rebuild.model)}`);
  const rollout = await call('edit_workload_rollout', { jwt: identities.managerJwt });
  requireCondition(
    rollout.model.matched === 0 && Object.entries(rollout.model).every(([key, value]) => key === 'matched' || value === 0),
    `zero-instance stateless rollout was not a no-op: ${JSON.stringify(rollout.model)}`,
  );

  const testContainer = await call('edit_test_container_create', { jwt: identities.managerJwt });
  requireCondition(typeof testContainer.model.id === 'string', 'test-container creation omitted its public id');
  state.containerIds.push(testContainer.model.id);
  const runtime = sql(
    `SELECT container.container_id FROM "GameChallenges" challenge ` +
      `JOIN "Containers" container ON container.id=challenge.test_container_id ` +
      `WHERE challenge.id=${context.containerChallengeId} AND challenge.game_id=${context.gameId}`,
  );
  requireCondition(runtime, 'test-container creation omitted its backend identity');
  state.runtimeIds.push(runtime);
  saveRecovery();
}

async function prepareAdFixture() {
  console.log('\nactive A&D organizer fixture…');
  const now = A.nowMs();
  context.adGameId = await A.createGame({
    title: titleFor(tags.ad),
    hidden: true,
    practiceMode: false,
    acceptWithoutReview: true,
    start: now - 60_000,
    end: now + 3_600_000,
    teamMemberCountLimit: 1,
    containerCountLimit: 1,
    allowUserSubmissions: false,
    ...AD_CREATION_SETTINGS,
  });
  state.gameIds.push(context.adGameId);
  saveRecovery();
  assertPersistedGameSettings(context.adGameId, AD_CREATION_SETTINGS, 'A&D fixture');
  await uncatalogued(
    'POST',
    `/api/edit/games/${context.adGameId}/admins/${identities.managerUserId}`,
  );
  identities.managerGameIds.add(context.adGameId);
  A.seedCohort(context.adGameId, 3);

  const paused = await call('edit_ad_scoring_pause', { jwt: identities.managerJwt });
  requireCondition(paused.model.scoringPaused === true, 'A&D scoring did not pause before fixture configuration');
  context.adChallengeId = await A.createChallenge(context.adGameId, {
    title: `edit-ad-${runKey}`, category: 'Pwn', type: 'AttackDefense',
  });
  const checker = A.prepareExactChecker(context.adGameId, context.adChallengeId);
  const image = ensureLocalImage(process.env.EDIT_AD_IMAGE || 'nginx:alpine');
  await A.setChallenge(context.adGameId, context.adChallengeId, {
    content: 'Disposable managed A&D service',
    containerImage: image,
    memoryLimit: 64,
    cpuCount: 1,
    exposePort: 80,
    adCheckerImage: checker,
    adAllowEgress: false,
    adAllowSelfReset: true,
    adSelfHosted: false,
    flagTemplate: `flag{edit_ad_[TEAM_HASH]_[GUID]}`,
  });
  await A.rebuildChallengeImage(context.adGameId, context.adChallengeId, image, 'edit A&D service');
  await A.addFlags(context.adGameId, context.adChallengeId, [`flag{edit_ad_placeholder_${runKey}}`]);
  await A.setChallenge(context.adGameId, context.adChallengeId, { isEnabled: true });

  await call('edit_ad_ensure_containers');
  context.serviceId = Number(await waitForSql(
    `SELECT id FROM "AdTeamServices" WHERE game_id=${context.adGameId} ` +
      `AND challenge_id=${context.adChallengeId} AND container_id IS NOT NULL ORDER BY id LIMIT 1`,
    (value) => Number(value) > 0,
    { label: 'platform A&D service provisioning' },
  ));
  const initiallyProvisioned = disposableAdminGameRuntimeIds(context.adGameId, tags.ad);
  state.runtimeIds.push(...initiallyProvisioned);
  saveRecovery();

  // Cover the organizer toggle's real teardown, then re-enable the challenge
  // before exercising its inspector eligibility. The UI exposes an inspector
  // for an offline service; stopping the exact owned backend simulates that
  // fault without disabling the challenge or deleting its service identity.
  const toggle = await call('edit_ad_challenge_toggle', { jwt: identities.managerJwt });
  requireCondition(toggle.model.isEnabled === false, 'A&D toggle did not disable the fixture challenge');
  await waitForSql(
    `SELECT count(*) FROM "AdTeamServices" WHERE game_id=${context.adGameId} ` +
      `AND challenge_id=${context.adChallengeId} AND container_id IS NOT NULL`,
    (value) => Number(value) === 0,
    { label: 'A&D service teardown before inspector' },
  );
  const reenabled = await uncatalogued(
    'POST',
    `/api/edit/games/${context.adGameId}/ad/Challenges/${context.adChallengeId}/Toggle`,
    { jwt: identities.managerJwt },
  );
  requireCondition(reenabled.json?.isEnabled === true, 'A&D fixture did not re-enable');
  await uncatalogued('POST', `/api/edit/games/${context.adGameId}/ad/EnsureContainers`);
  context.serviceId = Number(await waitForSql(
    `SELECT id FROM "AdTeamServices" WHERE game_id=${context.adGameId} ` +
      `AND challenge_id=${context.adChallengeId} AND container_id IS NOT NULL ORDER BY id LIMIT 1`,
    (value) => Number(value) > 0,
    { label: 'A&D service reprovisioning after toggle' },
  ));
  adRuntimeBeforeRestart = sql(
    `SELECT container_id FROM "AdTeamServices" WHERE id=${context.serviceId} AND game_id=${context.adGameId}`,
  );
  state.runtimeIds.push(...disposableAdminGameRuntimeIds(context.adGameId, tags.ad));
  saveRecovery();
  const stopped = docker(['stop', '--time', '2', adRuntimeBeforeRestart]);
  requireCondition(stopped.status === 0, `could not stop A&D service for inspector: ${stopped.stderr.trim()}`);
  await exerciseInspector();
  const started = docker(['start', adRuntimeBeforeRestart]);
  requireCondition(started.status === 0, `could not restore A&D service after inspector: ${started.stderr.trim()}`);
  mutateContainerFilesystem(adRuntimeBeforeRestart, runKey);
}

async function prepareKothFixture() {
  console.log('\nactive KotH organizer fixture…');
  const now = A.nowMs();
  context.kothGameId = await A.createGame({
    title: titleFor(tags.koth),
    hidden: true,
    practiceMode: false,
    acceptWithoutReview: true,
    start: now - 60_000,
    end: now + 3_600_000,
    teamMemberCountLimit: 1,
    containerCountLimit: 1,
    allowUserSubmissions: false,
    ...KOTH_CREATION_SETTINGS,
  });
  state.gameIds.push(context.kothGameId);
  saveRecovery();
  assertPersistedGameSettings(context.kothGameId, KOTH_CREATION_SETTINGS, 'KotH fixture');
  await uncatalogued(
    'POST',
    `/api/edit/games/${context.kothGameId}/admins/${identities.managerUserId}`,
  );
  identities.managerGameIds.add(context.kothGameId);
  A.seedCohort(context.kothGameId, 2);
  await A.setAdScoringPaused(context.kothGameId, true);
  context.kothChallengeId = await A.createChallenge(context.kothGameId, {
    title: `edit-koth-${runKey}`, category: 'Pwn', type: 'KingOfTheHill',
  });
  const checker = A.prepareKothChecker(context.kothGameId, context.kothChallengeId);
  const image = A.buildCompetitiveKothImage();
  await A.setChallenge(context.kothGameId, context.kothChallengeId, {
    content: 'Disposable KotH recovery fixture',
    containerImage: image,
    memoryLimit: 64,
    cpuCount: 1,
    exposePort: 8080,
    adCheckerImage: checker,
    adAllowEgress: false,
  });
  await A.rebuildChallengeImage(context.kothGameId, context.kothChallengeId, image, 'edit KotH hill');
  await A.addFlags(context.kothGameId, context.kothChallengeId, [`flag{edit_koth_placeholder_${runKey}}`]);
  await A.setChallenge(context.kothGameId, context.kothChallengeId, { isEnabled: true });
  await uncatalogued('POST', `/api/edit/games/${context.kothGameId}/ad/EnsureContainers`);
  const hill = discoverManagedKothHill(context.kothGameId, context.kothChallengeId);
  state.containerIds.push(hill.containerId);
  state.runtimeIds.push(hill.backendId);
  saveRecovery();
  await A.setAdScoringPaused(context.kothGameId, false);
  await A.waitForCrownReady(context.kothGameId, context.kothChallengeId, 2, 180);
}

async function exerciseInspector() {
  try {
    const created = await call('edit_ad_inspector_create', { jwt: identities.managerJwt });
    context.inspectorId = created.model.containerGuid;
    state.containerIds.push(context.inspectorId);
    const inspectorBackendId = sql(
      `SELECT container_id FROM "Containers" WHERE id=${sqlLiteral(context.inspectorId)}::uuid`,
    );
    requireCondition(
      /^[a-zA-Z0-9:._-]{12,256}$/.test(inspectorBackendId),
      'inspector public UUID did not resolve to a persisted backend identity',
    );
    const backendInspection = docker(['container', 'inspect', inspectorBackendId]);
    requireCondition(backendInspection.status === 0, 'inspector backend is not present after creation');
    const backendRecords = JSON.parse(backendInspection.stdout);
    requireCondition(backendRecords.length === 1, 'inspector backend inspection is ambiguous');
    const hostBindings = backendRecords[0]?.HostConfig?.PortBindings;
    const networkBindings = backendRecords[0]?.NetworkSettings?.Ports;
    requireCondition(
      !hostBindings || Object.keys(hostBindings).length === 0,
      'shell-only inspector unexpectedly publishes a host port',
    );
    requireCondition(
      !networkBindings || Object.values(networkBindings).every((bindings) => !bindings || bindings.length === 0),
      'shell-only inspector has a live network port binding',
    );
    state.runtimeIds.push(inspectorBackendId);
    saveRecovery();
    const destroyed = await call('edit_ad_inspector_delete', { jwt: identities.managerJwt });
    requireCondition(destroyed.response.status === 200, 'inspector destruction failed');
    requireCondition(
      Number(sql(`SELECT count(*) FROM "Containers" WHERE id=${sqlLiteral(context.inspectorId)}::uuid`)) === 0,
      'destroyed inspector public row is still present',
    );
    const inspection = docker(['container', 'inspect', inspectorBackendId]);
    requireCondition(inspection.status !== 0, 'destroyed inspector runtime is still present');
  } catch (error) {
    // Do not convert the present empty GUID/no-op pair into false coverage. Keep
    // running so the report identifies every other route in the same attempt.
    covered.delete('edit_ad_inspector_create');
    covered.delete('edit_ad_inspector_delete');
    const destroyPath = resolveEditOperationPath('edit_ad_inspector_delete', context);
    const destroyProbe = await rawRequest('DELETE', destroyPath, {
      jwt: identities.managerJwt,
      ip: '10.247.1.1',
      timeoutMs: 30_000,
    });
    blockers.push(
      `A&D inspector lifecycle: ${error.message}; paired DELETE returned ${destroyProbe.status} ` +
        'without an owned runtime identity whose absence could be proven',
    );
    console.error(`  ✗ A&D inspector lifecycle is not implemented truthfully: ${error.message}`);
  }
}

async function positiveReadAndMutationSurface() {
  console.log('\nremaining positive edit contracts…');
  await call('edit_post_update', {
    body: { title: `Edit acceptance updated ${runKey}`, isPinned: true },
  });
  const game = await call('edit_game_get', { jwt: identities.managerJwt });
  primaryGameModel = game.model;
  const updatedGameBody = {
    ...primaryGameModel,
    summary: `updated edit acceptance ${runKey}`,
  };
  await call('edit_game_update', { jwt: identities.managerJwt, body: updatedGameBody });
  const salt = await call('edit_game_hash_salt', { jwt: identities.managerJwt });
  requireCondition(/^[a-f0-9]{64}$/.test(salt.model), 'game hash salt is not a SHA-256 value');

  const clone = await call('edit_game_clone', {
    body: {
      title: `EDIT-CLONE-${runKey}`,
      startTimeUtc: primaryGameModel.start,
      endTimeUtc: primaryGameModel.end,
      includeChallenges: true,
    },
  });
  cloneGameId = clone.model;
  state.gameIds.push(cloneGameId);
  state.futureGameIds.push(cloneGameId);
  saveRecovery();
  const cloneShape = assertSemanticGameClone(
    context.gameId,
    cloneGameId,
    `EDIT-CLONE-${runKey}`,
  );
  console.log(`  ✓ clone preserved ${cloneShape.cloneChallenges} challenge template(s) without live ownership`);
  await call('edit_game_writeups_delete');
  await call('edit_scoreboard_flush', { jwt: identities.managerJwt });
  const admins = await call('edit_game_admins_get');
  requireCondition(admins.model.some((user) => user.userId === context.managerUserId), 'game manager list omitted delegated user');

  const reviews = await call('edit_reviews_get', { jwt: identities.managerJwt });
  requireCondition(Array.isArray(reviews.model.data), 'review list is not paged');
  const analytics = await call('edit_reviews_analytics_get', { jwt: identities.managerJwt });
  requireCondition(
    analytics.model.total === analytics.model.likes + analytics.model.dislikes,
    'review analytics totals are internally inconsistent',
  );
  const pending = await call('edit_pending_challenges_get', { jwt: identities.managerJwt });
  requireCondition(
    pending.model.some((challenge) => challenge.id === context.pendingApproveId) &&
      pending.model.some((challenge) => challenge.id === context.pendingRejectId),
    'pending challenge queue omitted submitted fixtures',
  );
  const challenges = await call('edit_challenges_get', { jwt: identities.managerJwt });
  requireCondition(challenges.model.some((challenge) => challenge.id === context.challengeId), 'challenge list omitted primary fixture');
  const challenge = await call('edit_challenge_get', { jwt: identities.managerJwt });
  requireCondition(challenge.model.id === context.challengeId, 'challenge detail returned the wrong fixture');
  const audit = await call('edit_challenge_audit_meta', { jwt: identities.managerJwt });
  requireCondition(audit.model.archiveAvailable === true && audit.model.files.length > 0, 'archive audit metadata is not materialized');

  await call('edit_notices_get', { jwt: identities.managerJwt });
  const notice = await call('edit_notice_update', {
    jwt: identities.managerJwt,
    body: { content: `edit notice updated ${runKey}` },
  });
  requireCondition(notice.model.values[0].includes('updated'), 'notice update did not persist');
  await call('edit_divisions_get', { jwt: identities.managerJwt });
  const division = await call('edit_division_update', {
    jwt: identities.managerJwt,
    body: { name: `edit-updated-${runKey}`, defaultPermissions: 7 },
  });
  requireCondition(division.model.name === `edit-updated-${runKey}`, 'division update did not persist');

  const adState = await call('edit_ad_state', { jwt: identities.managerJwt });
  requireCondition(adState.model.challenges.some((challenge) => challenge.challengeId === context.adChallengeId), 'A&D state omitted fixture challenge');
  const serviceFile = await call('edit_ad_service_file', { jwt: identities.managerJwt });
  requireCondition(serviceFile.model.containerRunning === true, 'A&D service file did not inspect a live container');
  const changes = await call('edit_ad_snapshot_changes', { jwt: identities.managerJwt });
  requireCondition(changes.model.changes.some((change) => change.path.includes('/tmp/rsctf-edit')), 'snapshot changes omitted real filesystem drift');
  const diff = await call('edit_ad_snapshot_diff', { jwt: identities.managerJwt });
  requireCondition(diff.model.added.some((path) => path.includes('/tmp/rsctf-edit')), 'snapshot diff omitted real added path');
  const points = await call('edit_ad_snapshots_get', { jwt: identities.managerJwt });
  requireCondition(points.model.length > 0, 'snapshot history did not expose live drift point');
  const snapshot = await call('edit_ad_snapshot_download', { jwt: identities.managerJwt });
  requireCondition(snapshot.model.length >= 512, 'snapshot TAR is empty');
  await call('edit_ad_service_restart', { jwt: identities.managerJwt });
  const restartedRuntime = await waitForSql(
    `SELECT container_id FROM "AdTeamServices" WHERE id=${context.serviceId} AND game_id=${context.adGameId}`,
    (value) => value && value !== adRuntimeBeforeRestart,
    { label: 'A&D service replacement identity' },
  );
  state.runtimeIds.push(restartedRuntime);
  saveRecovery();
  const oldRuntime = docker(['container', 'inspect', adRuntimeBeforeRestart]);
  requireCondition(oldRuntime.status !== 0, 'A&D restart left the retired runtime present');

  await A.setAdScoringPaused(context.adGameId, false);
  context.checkId = Number(await waitForSql(
    `SELECT result.id FROM "AdCheckResults" result JOIN "AdRounds" round ON round.id=result.round_id ` +
      `WHERE round.game_id=${context.adGameId} AND result.sla_credit IS NOT NULL ` +
      `ORDER BY round.number DESC,result.id DESC LIMIT 1`,
    (value) => Number(value) > 0,
    { timeoutMs: 180_000, label: 'completed A&D checker result' },
  ));
  let checkerTiming;
  await waitForSql(
    `WITH latest AS (` +
      `SELECT round.id,round.flags_published_at,game.ad_tick_seconds,` +
        `game.ad_getflag_window_fraction,game.ad_min_grace_period_seconds ` +
      `FROM "AdRounds" round JOIN "Games" game ON game.id=round.game_id ` +
      `WHERE round.game_id=${context.adGameId} AND round.flags_published_at IS NOT NULL ` +
      `ORDER BY round.number DESC LIMIT 1` +
    `), evidence AS (` +
      `SELECT result.checked_at,delivery.completed_at AS delivered_at ` +
      `FROM latest JOIN "AdFlagDeliveryResults" delivery ON delivery.round_id=latest.id ` +
      `JOIN "AdCheckResults" result ON result.round_id=delivery.round_id ` +
        `AND result.team_service_id=delivery.team_service_id ` +
      `WHERE delivery.delivered=TRUE AND result.sla_credit IS NOT NULL` +
    `) SELECT json_build_object(` +
      `'roundId',latest.id,` +
      `'expected',(SELECT count(*) FROM "AdFlagDeliveryResults" delivery ` +
        `WHERE delivery.round_id=latest.id AND delivery.delivered=TRUE),` +
      `'observed',(SELECT count(*) FROM evidence),` +
      `'minimumMs',(SELECT min(extract(epoch FROM (checked_at-delivered_at))*1000) FROM evidence),` +
      `'maximumMs',(SELECT max(extract(epoch FROM (checked_at-delivered_at))*1000) FROM evidence),` +
      `'distinctMilliseconds',(SELECT count(DISTINCT date_trunc('milliseconds',checked_at)) FROM evidence),` +
      `'tickSeconds',COALESCE(latest.ad_tick_seconds,60),` +
      `'windowFraction',COALESCE(latest.ad_getflag_window_fraction,0.5),` +
      `'graceSeconds',COALESCE(latest.ad_min_grace_period_seconds,3)` +
    `)::text FROM latest`,
    (value) => {
      try {
        checkerTiming = JSON.parse(value);
        return Number(checkerTiming.expected) >= 3 && checkerTiming.observed === checkerTiming.expected;
      } catch {
        return false;
      }
    },
    { timeoutMs: 180_000, label: 'complete independently scheduled A&D checker field' },
  );
  requireCondition(
    Number(checkerTiming.minimumMs) >= (Number(checkerTiming.graceSeconds) * 1000) - 50,
    `checker fired before its configured grace: ${JSON.stringify(checkerTiming)}`,
  );
  const checkerUpperBoundMs = (
    Number(checkerTiming.graceSeconds) +
    (Number(checkerTiming.tickSeconds) * Number(checkerTiming.windowFraction)) +
    32
  ) * 1000;
  requireCondition(
    Number(checkerTiming.maximumMs) <= checkerUpperBoundMs,
    `checker exceeded its jitter+execution bound: ${JSON.stringify(checkerTiming)}`,
  );
  requireCondition(
    Number(checkerTiming.distinctMilliseconds) >= 2,
    `checker field collapsed onto one predictable timestamp: ${JSON.stringify(checkerTiming)}`,
  );
  state.checkerTiming = checkerTiming;
  saveRecovery();
  const liveGameBefore = await uncatalogued(
    'GET',
    `/api/edit/games/${context.adGameId}`,
    { jwt: identities.managerJwt },
  );
  const originalLiveStart = liveGameBefore.json.start;
  await uncatalogued(
    'PUT',
    `/api/edit/games/${context.adGameId}`,
    {
      jwt: identities.managerJwt,
      body: { ...liveGameBefore.json, start: originalLiveStart + 60_000 },
      expected: 400,
    },
  );
  const liveGameAfter = await uncatalogued(
    'GET',
    `/api/edit/games/${context.adGameId}`,
    { jwt: identities.managerJwt },
  );
  requireCondition(
    liveGameAfter.json.start === originalLiveStart,
    'rejected live-event start mutation changed the persisted scheduler boundary',
  );

  // Override evidence from an already-materialized epoch so the endpoint must
  // invalidate a real durable rollup suffix, not merely execute a no-op delete
  // against the still-live epoch. This may wait for the configured flag-lifetime
  // horizon; the fixture remains the only state in scope.
  let overrideBefore;
  await waitForSql(
    `SELECT json_build_object(` +
      `'id',result.id,'status',result.status,'message',result.message,` +
      `'slaCredit',result.sla_credit,'roundNumber',round.number,'epoch',rollup.epoch,` +
      `'rollupSuffixCount',(` +
        `SELECT count(*) FROM "AdEpochRollups" suffix ` +
        `WHERE suffix.game_id=rollup.game_id AND suffix.epoch>=rollup.epoch` +
      `)` +
    `)::text ` +
    `FROM "AdEpochRollups" rollup ` +
    `JOIN "AdRounds" round ON round.game_id=rollup.game_id ` +
      `AND round.number BETWEEN rollup.start_round AND rollup.end_round ` +
    `JOIN "AdCheckResults" result ON result.round_id=round.id ` +
    `WHERE rollup.game_id=${context.adGameId} AND result.sla_credit IS NOT NULL ` +
    `ORDER BY rollup.epoch DESC,round.number DESC,result.id DESC LIMIT 1`,
    (value) => {
      try {
        overrideBefore = JSON.parse(value);
        return Number.isSafeInteger(overrideBefore.id) && overrideBefore.id > 0 &&
          Object.hasOwn(AD_CHECK_STATUS, overrideBefore.status) &&
          Number(overrideBefore.rollupSuffixCount) > 0;
      } catch {
        return false;
      }
    },
    { timeoutMs: 300_000, label: 'rolled-up A&D checker result for override' },
  );
  context.checkId = Number(overrideBefore.id);
  await A.setAdScoringPaused(context.adGameId, true);

  const overrideTarget = overrideBefore.status === 0
    ? { label: 'Offline', status: 2 }
    : { label: 'Ok', status: 0 };
  requireCondition(
    overrideTarget.status !== overrideBefore.status,
    'A&D override fixture did not select a different verdict',
  );
  const overrideNote = `edit acceptance override ${runKey}`;
  const expectedOverrideMessage =
    `[admin override: ${AD_CHECK_STATUS[overrideBefore.status]} -> ${overrideTarget.label}] ${overrideNote}`;
  const scoreboardKeys = [
    `_AdScoreBoard_${context.adGameId}`,
    `_AdScoreBoard_${context.adGameId}:stale`,
    `_AdScoreBoardFrozen_${context.adGameId}`,
    `_AdScoreBoardFrozen_${context.adGameId}:stale`,
  ];
  state.cacheKeys = [...new Set([...state.cacheKeys, ...scoreboardKeys])];
  saveRecovery();
  const cacheSentinel = `edit-lifecycle:${runKey}:${context.checkId}`;
  for (const key of scoreboardKeys) {
    requireCondition(
      redisRaw(['SET', key, cacheSentinel, 'EX', '300'], `seed A&D cache key ${key}`) === 'OK',
      `could not seed fixture-owned A&D cache key ${key}`,
    );
  }
  const gameRevisionBeforeOverride = sql(
    `SELECT xmin::text FROM "Games" WHERE id=${context.adGameId}`,
  );
  const rollupSuffixBefore = Number(sql(
    `SELECT count(*) FROM "AdEpochRollups" ` +
      `WHERE game_id=${context.adGameId} AND epoch>=${Number(overrideBefore.epoch)}`,
  ));
  requireCondition(
    rollupSuffixBefore >= Number(overrideBefore.rollupSuffixCount) && rollupSuffixBefore > 0,
    'A&D override rollup suffix changed before the mutation',
  );
  await call('edit_ad_check_override', {
    body: { newStatus: overrideTarget.label, note: overrideNote },
  });
  const overrideAfter = JSON.parse(sql(
    `SELECT json_build_object(` +
      `'status',status,'message',message,'slaCredit',sla_credit` +
    `)::text FROM "AdCheckResults" WHERE id=${context.checkId}`,
  ));
  requireCondition(
    overrideAfter.status === overrideTarget.status,
    `A&D override status did not persist: ${JSON.stringify({ overrideBefore, overrideAfter })}`,
  );
  requireCondition(
    overrideAfter.message === expectedOverrideMessage,
    `A&D override note did not persist exactly: ${JSON.stringify(overrideAfter)}`,
  );
  requireCondition(
    Number(overrideAfter.slaCredit) === 0,
    `A&D override did not reset SLA credit: ${JSON.stringify(overrideAfter)}`,
  );
  const rollupSuffixAfter = Number(sql(
    `SELECT count(*) FROM "AdEpochRollups" ` +
      `WHERE game_id=${context.adGameId} AND epoch>=${Number(overrideBefore.epoch)}`,
  ));
  requireCondition(rollupSuffixAfter === 0, 'A&D override left its affected rollup suffix materialized');
  const gameRevisionAfterOverride = sql(
    `SELECT xmin::text FROM "Games" WHERE id=${context.adGameId}`,
  );
  requireCondition(
    gameRevisionAfterOverride !== gameRevisionBeforeOverride,
    'A&D override did not advance the scoreboard cache publication fence',
  );
  for (const key of scoreboardKeys) {
    requireCondition(
      redisRaw(['EXISTS', key], `verify A&D cache eviction ${key}`) === '0',
      `A&D override left Redis scoreboard key ${key}`,
    );
  }
  state.adCheckOverride = {
    checkId: context.checkId,
    roundNumber: overrideBefore.roundNumber,
    epoch: overrideBefore.epoch,
    previousStatus: AD_CHECK_STATUS[overrideBefore.status],
    newStatus: overrideTarget.label,
    message: overrideAfter.message,
    slaCredit: Number(overrideAfter.slaCredit),
    invalidatedRollups: rollupSuffixBefore,
    cacheKeysEvicted: scoreboardKeys,
    gameRevisionBefore: gameRevisionBeforeOverride,
    gameRevisionAfter: gameRevisionAfterOverride,
  };
  saveRecovery();
  await call('edit_ad_advance_round', { jwt: identities.managerJwt });

  const kothState = await call('edit_koth_state', { jwt: identities.managerJwt });
  const activeHillView = kothState.model.hills.find(
    (hill) => hill.challengeId === context.kothChallengeId,
  );
  requireCondition(activeHillView, 'KotH state omitted live hill');
  requireCondition(
    activeHillView.durablePhase === 'Active' && activeHillView.resetPhase === 'Active',
    `KotH fixture was not active before recovery fault: ${JSON.stringify(activeHillView)}`,
  );
  const receipts = await call('edit_koth_receipts', { jwt: identities.managerJwt });
  requireCondition(receipts.model.challengeId === context.kothChallengeId, 'KotH receipt feed returned wrong hill');

  // Freeze the round coordinate, then emulate a crash-safe readiness fault on
  // this run's exact active cycle. The stopped replacement is a real backend
  // failure: recovery must durably reclaim it, advance the reset attempt, create
  // a new runtime, republish the target, mint capabilities, pass readiness, and
  // activate firewall state before the public endpoint can return Active.
  await A.setAdScoringPaused(context.kothGameId, true);
  const oldHill = discoverManagedKothHill(context.kothGameId, context.kothChallengeId);
  const cycleBefore = JSON.parse(sql(
    `SELECT json_build_object(` +
      `'id',cycle.id,'cycleNumber',cycle.cycle_number,'phase',cycle.phase,` +
      `'resetAttempt',cycle.reset_attempt,'readinessAttempt',cycle.readiness_attempt,` +
      `'readinessFailures',cycle.readiness_failures,` +
      `'replacementContainerId',cycle.replacement_container_id,` +
      `'targetContainerId',target.container_id` +
    `)::text FROM "KothCrownCycles" cycle ` +
    `JOIN "KothTargets" target ON target.game_id=cycle.game_id ` +
      `AND target.challenge_id=cycle.challenge_id ` +
    `WHERE cycle.game_id=${context.kothGameId} ` +
      `AND cycle.challenge_id=${context.kothChallengeId} ` +
    `ORDER BY cycle.cycle_number DESC LIMIT 1`,
  ));
  requireCondition(
    cycleBefore.phase === 'Active' &&
      cycleBefore.replacementContainerId === oldHill.backendId &&
      cycleBefore.targetContainerId === oldHill.backendId,
    `KotH durable/runtime identity diverged before fault: ${JSON.stringify(cycleBefore)}`,
  );
  const faultMarker = `edit-lifecycle-readiness-fault-${runKey}`;
  state.kothRecovery = {
    faultMarker,
    cycleId: cycleBefore.id,
    cycleNumber: cycleBefore.cycleNumber,
    oldRuntimeId: oldHill.backendId,
    resetAttemptBefore: cycleBefore.resetAttempt,
    preexistingReceiptIds: receipts.model.receipts.map((receipt) => receipt.id),
    phaseFaulted: false,
    runtimeStopped: false,
  };
  saveRecovery();
  const faultedCycleId = sql(
    `UPDATE "KothCrownCycles" cycle SET ` +
      `phase='ReadinessPending',readiness_error=${sqlLiteral(faultMarker)},` +
      `last_error=${sqlLiteral(faultMarker)},updated_at=clock_timestamp() ` +
    `FROM "KothTargets" target,"Games" game,"GameChallenges" challenge ` +
    `WHERE cycle.id=${Number(cycleBefore.id)} ` +
      `AND cycle.game_id=${context.kothGameId} ` +
      `AND cycle.challenge_id=${context.kothChallengeId} ` +
      `AND cycle.phase='Active' ` +
      `AND cycle.replacement_container_id=${sqlLiteral(oldHill.backendId)} ` +
      `AND target.game_id=cycle.game_id AND target.challenge_id=cycle.challenge_id ` +
      `AND target.container_id=cycle.replacement_container_id ` +
      `AND game.id=cycle.game_id AND game.title=${sqlLiteral(titleFor(tags.koth))} ` +
      `AND challenge.game_id=cycle.game_id AND challenge.id=cycle.challenge_id ` +
      `AND challenge.title=${sqlLiteral(`edit-koth-${runKey}`)} ` +
    `RETURNING cycle.id`,
  );
  requireCondition(
    Number(faultedCycleId) === Number(cycleBefore.id),
    'KotH readiness fault CAS did not affect the exact run-owned active cycle',
  );
  state.kothRecovery.phaseFaulted = true;
  saveRecovery();
  const stoppedHill = docker(['stop', '--time', '2', oldHill.backendId]);
  requireCondition(
    stoppedHill.status === 0,
    `could not stop exact KotH recovery runtime: ${stoppedHill.stderr.trim()}`,
  );
  state.kothRecovery.runtimeStopped = true;
  saveRecovery();

  const recovered = await call('edit_koth_recover', { jwt: identities.managerJwt });
  requireCondition(
    recovered.model.challengeId === context.kothChallengeId &&
      recovered.model.cycleNumber === cycleBefore.cycleNumber &&
      recovered.model.resetPhase === 'Active',
    `KotH recovery response did not converge the faulted cycle: ${JSON.stringify(recovered.model)}`,
  );
  const replacementHill = discoverManagedKothHill(context.kothGameId, context.kothChallengeId);
  requireCondition(
    replacementHill.backendId !== oldHill.backendId,
    'KotH recovery reused the stopped runtime instead of replacing it',
  );
  state.runtimeIds.push(replacementHill.backendId);
  saveRecovery();
  requireCondition(
    docker(['container', 'inspect', oldHill.backendId]).status !== 0,
    'KotH recovery left the stopped runtime present',
  );
  const cycleAfter = JSON.parse(sql(
    `SELECT json_build_object(` +
      `'id',cycle.id,'cycleNumber',cycle.cycle_number,'phase',cycle.phase,` +
      `'resetAttempt',cycle.reset_attempt,'readinessAttempt',cycle.readiness_attempt,` +
      `'readinessFailures',cycle.readiness_failures,` +
      `'readinessError',cycle.readiness_error,'lastError',cycle.last_error,` +
      `'replacementContainerId',cycle.replacement_container_id,` +
      `'targetContainerId',target.container_id,'targetHost',target.host,'targetPort',target.port` +
    `)::text FROM "KothCrownCycles" cycle ` +
    `JOIN "KothTargets" target ON target.game_id=cycle.game_id ` +
      `AND target.challenge_id=cycle.challenge_id ` +
    `WHERE cycle.id=${Number(cycleBefore.id)}`,
  ));
  requireCondition(
    cycleAfter.id === cycleBefore.id &&
      cycleAfter.cycleNumber === cycleBefore.cycleNumber &&
      cycleAfter.phase === 'Active' &&
      cycleAfter.resetAttempt === cycleBefore.resetAttempt + 1 &&
      cycleAfter.readinessAttempt >= cycleBefore.readinessAttempt + 1 &&
      cycleAfter.readinessFailures >= cycleBefore.readinessFailures + 1 &&
      cycleAfter.readinessError === null && cycleAfter.lastError === null &&
      cycleAfter.replacementContainerId === replacementHill.backendId &&
      cycleAfter.targetContainerId === replacementHill.backendId &&
      typeof cycleAfter.targetHost === 'string' && cycleAfter.targetHost.length > 0 &&
      Number(cycleAfter.targetPort) > 0,
    `KotH durable state did not converge after recovery: ${JSON.stringify({ cycleBefore, cycleAfter })}`,
  );

  const receiptResponse = await uncatalogued(
    'GET',
    `/api/edit/games/${context.kothGameId}/ad/koth/${context.kothChallengeId}/receipts`,
    { jwt: identities.managerJwt },
  );
  const receiptModel = responseBody(receiptResponse, operationById.get('edit_koth_receipts'));
  const expectedAttempt = cycleBefore.resetAttempt + 1;
  const recoveryReceipts = receiptModel.receipts.filter(
    (receipt) => receipt.attempt === expectedAttempt,
  );
  const recoveryReceiptPhases = new Set(recoveryReceipts.map((receipt) => receipt.phase));
  for (const phase of [
    'DestroyPending',
    'CreatePending',
    'PublishPending',
    'CapabilityPending',
    'ReadinessPending',
    'FirewallPending',
  ]) {
    requireCondition(
      recoveryReceiptPhases.has(phase),
      `KotH recovery omitted ${phase} receipt for attempt ${expectedAttempt}`,
    );
  }
  const preexistingReceiptIds = new Set(state.kothRecovery.preexistingReceiptIds);
  requireCondition(
    recoveryReceipts.every((receipt) => !preexistingReceiptIds.has(receipt.id)),
    'KotH recovery receipt chain reused a pre-fault receipt identity',
  );
  requireCondition(
    receiptModel.challengeId === context.kothChallengeId &&
      receiptModel.cycleNumber === cycleBefore.cycleNumber,
    'KotH recovery receipt feed switched cycles unexpectedly',
  );

  const stateResponse = await uncatalogued(
    'GET',
    `/api/edit/games/${context.kothGameId}/ad/koth/state`,
    { jwt: identities.managerJwt },
  );
  const recoveredState = responseBody(stateResponse, operationById.get('edit_koth_state'));
  const recoveredHillView = recoveredState.hills.find(
    (hill) => hill.challengeId === context.kothChallengeId,
  );
  requireCondition(
    recoveredHillView?.durablePhase === 'Active' &&
      recoveredHillView.resetPhase === 'Active' &&
      recoveredHillView.cycleNumber === cycleBefore.cycleNumber &&
      recoveredHillView.resetAttempt === expectedAttempt &&
      recoveredHillView.containerGuid === replacementHill.backendId &&
      recoveredHillView.replacementContainerId === replacementHill.backendId,
    `KotH operator state did not publish recovered ownership: ${JSON.stringify(recoveredHillView)}`,
  );
  state.kothRecovery = {
    ...state.kothRecovery,
    completed: true,
    newRuntimeId: replacementHill.backendId,
    resetAttemptAfter: cycleAfter.resetAttempt,
    phaseAfter: cycleAfter.phase,
    recoveryReceiptIds: recoveryReceipts.map((receipt) => receipt.id),
    recoveryReceiptPhases: [...recoveryReceiptPhases].sort(),
  };
  saveRecovery();

}

async function runReadSimulation() {
  if (skipEditLoad) {
    state.reportable = false;
    state.load = { skipped: true, reportable: false, reason: 'SKIP_EDIT_K6=1' };
    saveRecovery();
    return;
  }
  console.log('\nfixed-rate organizer read simulation…');
  const status = runK6('edit-lifecycle.js', {
    TARGET,
    ADMIN_TOKEN: A.adminJwt(),
    MANAGER_TOKEN: identities.managerJwt,
    EDIT_CONTEXT: JSON.stringify({
      gameId: context.gameId,
      challengeId: context.challengeId,
      adGameId: context.adGameId,
      adServiceId: context.serviceId,
      kothGameId: context.kothGameId,
      kothChallengeId: context.kothChallengeId,
    }),
    RATE: process.env.EDIT_RATE || 4,
    VUS: process.env.EDIT_VUS || 12,
    MAX_VUS: process.env.EDIT_MAX_VUS || 30,
    DURATION: process.env.EDIT_DURATION || '20s',
    SUMMARY_JSON: k6SummaryPath,
  });
  requireCondition(status === 0, `k6 edit lifecycle failed with exit code ${status}`);
  state.load = {
    skipped: false,
    reportable: state.reportable,
    summaryPath: k6SummaryPath,
  };
  saveRecovery();
  console.log(`  ✓ fixed-rate summary: ${k6SummaryPath}`);
}

async function destructivePositiveSurface() {
  console.log('\npositive delete/review contracts…');
  await call('edit_flag_delete', { jwt: identities.managerJwt });
  await call('edit_test_container_delete', { jwt: identities.managerJwt });
  await call('edit_challenge_approve', { jwt: identities.managerJwt });
  await call('edit_challenge_reject', {
    jwt: identities.managerJwt,
    body: { note: `rejected by edit acceptance ${runKey}` },
  });
  await call('edit_challenge_delete', { jwt: identities.managerJwt });
  await call('edit_notice_delete', { jwt: identities.managerJwt });
  await call('edit_division_delete', { jwt: identities.managerJwt });

  const exported = await call('edit_game_export', { jwt: identities.managerJwt });
  const imported = await call('edit_game_import', {
    form: {
      filename: `${runKey}-game.zip`,
      content: exported.response.bytes,
      contentType: 'application/zip',
    },
  });
  importedGameId = imported.model;
  state.gameIds.push(importedGameId);
  state.futureGameIds.push(importedGameId);
  saveRecovery();

  const importedGameRead = await uncatalogued('GET', `/api/edit/games/${importedGameId}`);
  const importedGameModel = importedGameRead.json?.data ?? importedGameRead.json;
  requireCondition(importedGameModel?.id === importedGameId, 'imported game could not be read back');
  requireCondition(importedGameModel.poster == null, 'imported game exposed a dangling source poster');
  const importedChallengeRead = await uncatalogued(
    'GET',
    `/api/edit/games/${importedGameId}/challenges`,
  );
  const importedChallenges = importedChallengeRead.json?.data ?? importedChallengeRead.json;
  requireCondition(Array.isArray(importedChallenges), 'imported challenge list could not be read back');
  const importedCheckerSummary = importedChallenges.find(({ title }) => title === transferCheckerTitle);
  requireCondition(importedCheckerSummary?.id > 0, 'imported challenge list omitted the checker sentinel');
  const importedCheckerRead = await uncatalogued(
    'GET',
    `/api/edit/games/${importedGameId}/challenges/${importedCheckerSummary.id}`,
  );
  const importedChecker = importedCheckerRead.json?.data ?? importedCheckerRead.json;
  requireCondition(importedChecker?.adCheckerImage == null, 'imported checker sentinel retained executable state');
  requireCondition(
    importedChecker.adAllowEgress === true && importedChecker.adAllowSelfReset === true &&
      importedChecker.adSshRequiresFlag === true && importedChecker.adScoringWeight === 1.1,
    'imported checker sentinel lost portable A&D policy',
  );
  const importShape = assertSemanticGameImport(
    context.gameId,
    importedGameId,
    transferCheckerTitle,
  );
  state.importSemantics = {
    sourceGameId: context.gameId,
    importedGameId,
    ...importShape,
    posterCleared: true,
    checkerCleared: true,
  };
  saveRecovery();
  console.log(
    `  ✓ import preserved ${importShape.importedChallenges} portable challenge template(s) ` +
      'and cleared poster/checker ownership',
  );

  const rollbackProbe = transactionalFailureGameArchive(runKey, A.nowMs());
  state.importRollbackProbe = {
    title: rollbackProbe.title,
    challengeTitle: rollbackProbe.challengeTitle,
    divisionName: rollbackProbe.divisionName,
    flag: rollbackProbe.flag,
    hash: rollbackProbe.hash,
  };
  saveRecovery();
  assertFailedGameImportRolledBack(rollbackProbe, serverContainers);
  const rollbackResponse = await rawRequest('POST', '/api/edit/games/import', {
    jwt: A.adminJwt(),
    ip: `10.253.1.${(requestIndex++ % 240) + 1}`,
    body: multipartBody({
      filename: `${runKey}-transaction-rollback.zip`,
      content: rollbackProbe.bytes,
      contentType: 'application/zip',
    }),
    timeoutMs: 180_000,
  });
  expectStatus(rollbackResponse, 500, 'late-invalid transactional game import');
  const rollbackCounts = assertFailedGameImportRolledBack(rollbackProbe, serverContainers);
  state.importRollback = { status: rollbackResponse.status, counts: rollbackCounts };
  saveRecovery();
  console.log('  ✓ late-invalid import rolled back game, challenge, attachment, file, flag, and division state');

  await call('edit_game_admin_delete');
  await call('edit_post_delete');
  state.postIds = state.postIds.filter((id) => id !== context.postId);
  state.ownedImageRefs = ownedBuildImageRefs(state.gameIds);
  saveRecovery();
  const deleted = await call('edit_game_delete');
  requireCondition(deleted.model.id === context.gameId, 'game delete returned the wrong model');
  state.gameIds = state.gameIds.filter((id) => id !== context.gameId);
  state.futureGameIds = state.futureGameIds.filter((id) => id !== context.gameId);
  saveRecovery();
}

async function deleteFutureGame(gameId) {
  if (!gameId || Number(sql(`SELECT count(*) FROM "Games" WHERE id=${Number(gameId)}`)) === 0) return;
  const response = await A.deleteGame(gameId);
  expectStatus(response, 200, `cleanup future game ${gameId}`);
  requireCondition(Number(sql(`SELECT count(*) FROM "Games" WHERE id=${Number(gameId)}`)) === 0, `future game ${gameId} survived cleanup`);
}

async function removeOwnedImages() {
  for (const imageRef of state.ownedImageRefs) {
    const response = await rawRequest(
      'DELETE',
      `/api/admin/builds/images?tag=${encodeURIComponent(imageRef)}&force=false`,
      { jwt: A.adminJwt(), ip: '10.249.1.1', timeoutMs: 180_000 },
    );
    expectStatus(response, 200, `cleanup owned build image ${imageRef}`);
    requireCondition(
      response.json?.removed === 1,
      `cleanup could not remove exact owned build image ${imageRef}: ${response.text}`,
    );
    requireCondition(
      docker(['image', 'inspect', imageRef]).status !== 0,
      `owned build image ${imageRef} survived public cleanup`,
    );
  }
}

function dockerObjectPresent(kind, identity, label) {
  const inspected = docker([kind, 'inspect', String(identity)]);
  if (inspected.status === 0) return 1;
  if (/no such (?:container|image|object)/i.test(String(inspected.stderr || inspected.stdout || ''))) return 0;
  throw new Error(`could not audit ${label} ${identity}: ${String(inspected.stderr || inspected.stdout || '').trim()}`);
}

function checkerDirectoryPresent(container, gameId) {
  const path = `/data/files/checkers/load/${gameId}`;
  const absent = docker(['exec', container, 'test', '!', '-e', path]);
  if (absent.status === 0) return 0;
  const present = docker(['exec', container, 'test', '-e', path]);
  if (present.status === 0) return 1;
  throw new Error(`could not audit checker directory ${path} in ${container}`);
}

function editResidualSnapshot() {
  const gameIds = [...new Set([
    ...state.gameIds,
    context.gameId,
    authorizationGameId,
    context.adGameId,
    context.kothGameId,
    cloneGameId,
    importedGameId,
  ].map(Number).filter((id) => Number.isSafeInteger(id) && id > 0))];
  const containerIds = [...new Set(state.containerIds
    .map((id) => String(id || '').trim())
    .filter((id) => /^[a-f0-9-]{36}$/i.test(id)))];
  const postIds = [...new Set([...state.postIds, context.postId].filter(Boolean).map(String))];
  const rollbackCounts = state.importRollbackProbe
    ? assertFailedGameImportRolledBack(state.importRollbackProbe, serverContainers)
    : {};
  return Object.freeze({
    games: gameIds.length
      ? Number(sql(`SELECT count(*) FROM "Games" WHERE id IN (${gameIds.join(',')})`))
      : 0,
    gameNamespace: Number(sql(`SELECT count(*) FROM "Games" WHERE strpos(title, ${sqlLiteral(runKey)}) > 0`)),
    containers: containerIds.length
      ? Number(sql(
          `SELECT count(*) FROM "Containers" WHERE id IN (` +
            `${containerIds.map((id) => `${sqlLiteral(id)}::uuid`).join(',')})`,
        ))
      : 0,
    posts: postIds.length
      ? Number(sql(`SELECT count(*) FROM "Posts" WHERE id IN (${postIds.map(sqlLiteral).join(',')})`))
      : 0,
    runtimeContainers: [...new Set(state.runtimeIds.filter(Boolean))]
      .reduce((count, id) => count + dockerObjectPresent('container', id, 'runtime container'), 0),
    checkerDirectories: gameIds.reduce(
      (count, gameId) => count + serverContainers.reduce(
        (serverCount, container) => serverCount + checkerDirectoryPresent(container, gameId),
        0,
      ),
      0,
    ),
    cacheKeys: [...new Set(state.cacheKeys || [])]
      .reduce((count, key) => count + Number(redisRaw(['EXISTS', key], `audit cache key ${key}`)), 0),
    ownedImages: [...new Set(state.ownedImageRefs || [])]
      .reduce((count, image) => count + dockerObjectPresent('image', image, 'owned image'), 0),
    failedImportResidue: Object.values(rollbackCounts).reduce((sum, value) => sum + Number(value), 0),
  });
}

async function assertStableExactEditCleanup() {
  const delayMs = Number(process.env.EDIT_CLEANUP_STABILITY_MS || 2_000);
  requireCondition(
    Number.isSafeInteger(delayMs) && delayMs >= 1_000 && delayMs <= 10_000,
    'EDIT_CLEANUP_STABILITY_MS must be an integer from 1000 through 10000',
  );
  const passes = [];
  for (let pass = 0; pass < 2; pass += 1) {
    await new Promise((resolve) => setTimeout(resolve, delayMs));
    const snapshot = editResidualSnapshot();
    for (const [resource, count] of Object.entries(snapshot)) {
      requireCondition(count === 0, `${resource} cleanup residue is ${count}, expected zero`);
    }
    passes.push(snapshot);
  }
  requireCondition(
    JSON.stringify(passes[0]) === JSON.stringify(passes[1]),
    'edit cleanup residual snapshots changed between delayed reads',
  );
  state.cleanupAudit = { delayMs, passes };
  saveRecovery();
}

async function cleanup() {
  const errors = [];
  const capture = async (label, action) => {
    try { await action(); } catch (error) { errors.push(`${label}: ${error.message}`); }
  };

  await capture('owned image inventory', async () => {
    state.ownedImageRefs = [...new Set([
      ...state.ownedImageRefs,
      ...ownedBuildImageRefs(state.gameIds),
    ])];
    saveRecovery();
  });

  await capture('pause A&D scoring', async () => {
    if (context.adGameId && Number(sql(`SELECT count(*) FROM "Games" WHERE id=${context.adGameId}`))) {
      await A.setAdScoringPaused(context.adGameId, true);
    }
  });
  await capture('pause KotH scoring', async () => {
    if (context.kothGameId && Number(sql(`SELECT count(*) FROM "Games" WHERE id=${context.kothGameId}`))) {
      await A.setAdScoringPaused(context.kothGameId, true);
    }
  });
  await capture('fixture cache keys', async () => {
    for (const key of [...new Set(state.cacheKeys || [])]) {
      redisRaw(['DEL', key], `cleanup cache key ${key}`);
      requireCondition(
        redisRaw(['EXISTS', key], `verify cleanup cache key ${key}`) === '0',
        `cache key ${key} survived cleanup`,
      );
    }
  });

  const runtimeIds = [...new Set([
    ...state.runtimeIds,
    ...(context.adGameId ? disposableAdminGameRuntimeIds(context.adGameId, tags.ad) : []),
    ...(context.kothGameId ? disposableAdminGameRuntimeIds(context.kothGameId, tags.koth) : []),
    ...(context.kothGameId ? A.kothContainerIdsForGames([context.kothGameId]) : []),
  ].filter(Boolean))];
  state.runtimeIds = runtimeIds;
  saveRecovery();
  for (const runtimeId of runtimeIds) {
    await capture(`runtime ${runtimeId}`, async () => removeRuntime(runtimeId));
  }

  for (const gameId of [context.adGameId, context.kothGameId].filter(Boolean)) {
    await capture(`checker ${gameId}`, async () => removeCheckerDirectory(gameId));
  }
  await capture('exact A&D graph', async () => {
    if (context.adGameId) deleteDisposableAdminGame(context.adGameId, tags.ad, { runtimeIds });
  });
  await capture('exact KotH graph', async () => {
    if (context.kothGameId) deleteDisposableAdminGame(context.kothGameId, tags.koth, { runtimeIds });
  });

  for (const gameId of [...new Set(state.futureGameIds)]) {
    await capture(`future game ${gameId}`, async () => deleteFutureGame(gameId));
  }
  await capture('run-tagged game reconciliation', async () => {
    // A create/clone endpoint can fail after inserting but before returning its
    // id. Reconcile by this run's unguessable title tag so such partial writes
    // cannot escape an otherwise "successful" cleanup audit.
    const tagged = String(sql(
      `SELECT id FROM "Games" WHERE strpos(title, ${sqlLiteral(runKey)}) > 0 ORDER BY id`,
    ) || '')
      .split('\n')
      .map(Number)
      .filter((id) => Number.isSafeInteger(id) && id > 0);
    for (const gameId of tagged) await deleteFutureGame(gameId);
    requireCondition(
      Number(sql(`SELECT count(*) FROM "Games" WHERE strpos(title, ${sqlLiteral(runKey)}) > 0`)) === 0,
      `run-tagged game survived cleanup for ${runKey}`,
    );
  });
  await capture('failed import rollback residue', async () => {
    if (state.importRollbackProbe) {
      assertFailedGameImportRolledBack(state.importRollbackProbe, serverContainers);
    }
  });
  for (const postId of state.postIds) {
    await capture(`post ${postId}`, async () => {
      const response = await rawRequest('DELETE', `/api/edit/posts/${encodeURIComponent(postId)}`, { jwt: A.adminJwt() });
      if (response.status !== 404) expectStatus(response, 200, `cleanup post ${postId}`);
    });
  }
  await capture('owned build images', removeOwnedImages);
  await capture('stable exact residual audit', assertStableExactEditCleanup);

  if (errors.length) {
    throw new Error(`edit lifecycle cleanup failed:\n- ${errors.join('\n- ')}\nrecovery: ${recoveryPath}`);
  }
}

async function main() {
  // These must remain the first interactions with anything backing TARGET.
  assertSafeAdminTarget(process.env);
  assertDisposableEditStack({ webTargets, controlTarget, serverContainers });
  state.runtimeIdentity = {
    before: inspectUniformServerRuntimeIdentity(serverContainers),
    after: null,
  };
  processLock = await acquireExclusiveProcessLock(loadOrchestrationLockPath, {
    label: 'edit lifecycle',
    metadata: { target: TARGET },
  });
  databaseLock = await acquireAdminLifecycleDatabaseLock();
  saveRecovery();

  let failure;
  try {
    await assertRuntimeRoles({ webTargets, controlTarget });
    await A.preflight();
    await prepareFutureFixture();
    await prepareAdFixture();
    await prepareKothFixture();
    await authorizationMatrix();
    await positiveReadAndMutationSurface();
    await runReadSimulation();
    await destructivePositiveSurface();

    if (blockers.length) {
      throw new Error(`organizer acceptance blockers:\n- ${blockers.join('\n- ')}`);
    }
    assertCompleteEditCoverage(covered);
    state.scenarioCompleted = true;
    state.coveredOperations = [...covered].sort();
    state.timing = timings;
    state.loadSummaryPath = skipEditLoad ? null : k6SummaryPath;
    saveRecovery();
    const total = timings.reduce((sum, timing) => sum + timing.ms, 0);
    console.log(`\n✓ ${covered.size}/${EDIT_OPERATION_IDS.length} edit operations accepted (${Math.round(total)} ms HTTP time)`);
  } catch (error) {
    failure = error;
    state.scenarioFailure = String(error?.stack || error?.message || error);
    saveRecovery();
  }

  let cleanupVerified = false;
  try {
    await cleanup();
    cleanupVerified = true;
  } catch (cleanupError) {
    state.cleanupFailure = String(cleanupError?.stack || cleanupError?.message || cleanupError);
    saveRecovery();
    failure = failure
      ? new AggregateError([failure, cleanupError], 'edit lifecycle and cleanup both failed')
      : cleanupError;
  }
  try {
    const startingRuntimeIdentity = state.runtimeIdentity.before;
    state.runtimeIdentity.after = inspectUnchangedServerRuntimeIdentity(
      startingRuntimeIdentity,
      serverContainers,
    );
    saveRecovery();
    const fatalLogsByContainer = Object.fromEntries(
      originalServerRuntimeLogTargets(startingRuntimeIdentity).map(({ name, containerId }) => [
        name,
        {
          containerId,
          fatalLineCount: countContainerFatalLogs(containerId, state.startedAt),
        },
      ]),
    );
    state.fatalLogAudit = fatalLogsByContainer;
    const fatalLogs = Object.values(fatalLogsByContainer)
      .reduce((sum, value) => sum + value.fatalLineCount, 0);
    requireCondition(fatalLogs === 0, `runtime log audit found ${fatalLogs} panic/fatal records`);
    state.runtimeIdentity.after = inspectUnchangedServerRuntimeIdentity(
      startingRuntimeIdentity,
      serverContainers,
    );
  } catch (verificationError) {
    state.verificationFailure = String(verificationError?.stack || verificationError?.message || verificationError);
    saveRecovery();
    failure = failure
      ? new AggregateError([failure, verificationError], 'edit lifecycle and verification both failed')
      : verificationError;
  }
  for (const [label, lease] of [['database', databaseLock], ['process', processLock]]) {
    if (!lease) continue;
    try {
      await lease.release();
    } catch (releaseError) {
      const wrapped = new Error(`${label} lifecycle lease release failed: ${releaseError.message}`);
      state.leaseFailures.push(String(wrapped.stack || wrapped.message));
      saveRecovery();
      failure = failure
        ? new AggregateError([failure, wrapped], 'edit lifecycle and lease release both failed')
        : wrapped;
    }
  }
  state.cleanupVerified = cleanupVerified;
  state.completed = !failure && Boolean(state.scenarioCompleted);
  state.completedAt = Date.now();
  state.failure = failure ? String(failure.stack || failure.message || failure) : null;
  saveRecovery();
  if (failure) throw failure;

  console.log(`  recovery/audit manifest: ${recoveryPath}`);
  if (!shouldRetainLifecycleManifest({
    completed: state.completed,
    cleanupVerified: state.cleanupVerified,
    keep: process.env.KEEP_EDIT_MANIFEST,
  })) removeRecovery(recoveryPath);
}

main().catch((error) => {
  console.error(error?.stack || error);
  process.exitCode = 1;
});
