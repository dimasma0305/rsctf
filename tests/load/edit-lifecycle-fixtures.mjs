// Disposable fixture mechanics for edit-lifecycle.mjs. Keep destructive scope
// checks and archive materialization separate from the 64-operation narrative.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import {
  assertDirectAdminOriginBindings,
  assertDisposableComposeTopology,
} from './admin-lifecycle.js';
import { rawRequest, sqlLiteral } from './admin-fixtures.mjs';
import { dockerScopeFromContainerEnv } from './docker-scope.js';
import { docker, PG, RSCTF, sleep, sql, TARGET } from './lib.mjs';

export function requireCondition(condition, message) {
  if (!condition) throw new Error(message);
}

/** Resolve one branch or tag to its exact remote commit without cloning it. */
export function resolveRemoteGitRefCommit(repository, gitRef, run = execFileSync) {
  const url = String(repository || '').trim();
  const ref = String(gitRef || '').trim();
  requireCondition(url.length > 0, 'GitHub import repository is required');
  requireCondition(ref.length > 0, 'GitHub import branch or tag is required');
  requireCondition(!/\s/.test(ref) && !ref.startsWith('-'), 'GitHub import branch or tag is invalid');
  const refs = ref.startsWith('refs/')
    ? [ref, `${ref}^{}`]
    : [`refs/heads/${ref}`, `refs/tags/${ref}`, `refs/tags/${ref}^{}`];
  let output;
  try {
    output = run('git', ['ls-remote', '--exit-code', url, ...refs], {
      encoding: 'utf8',
      timeout: 30_000,
      maxBuffer: 1024 * 1024,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
  } catch (error) {
    throw new Error(`cannot resolve GitHub import ref ${ref}: ${String(error?.stderr || error?.message || error).trim()}`);
  }
  const rows = String(output || '')
    .trim()
    .split(/\r?\n/)
    .filter(Boolean)
    .map((line) => {
      const match = line.match(/^([a-f0-9]{40})\s+(.+)$/i);
      requireCondition(match, `git ls-remote returned a malformed row for ${ref}`);
      return { commit: match[1].toLowerCase(), name: match[2] };
    });
  const peeled = new Map(
    rows.filter(({ name }) => name.endsWith('^{}'))
      .map(({ commit, name }) => [name.slice(0, -3), commit]),
  );
  const commits = new Set(rows.flatMap(({ commit, name }) => {
    if (name.endsWith('^{}')) return [commit];
    return peeled.has(name) ? [] : [commit];
  }));
  requireCondition(commits.size === 1, `GitHub import ref ${ref} resolved to ${commits.size} commits`);
  return [...commits][0];
}

function inspectComposeContainer(container, label) {
  const inspected = docker(['inspect', container]);
  requireCondition(inspected.status === 0, `cannot inspect declared disposable ${label} ${container}`);
  let records;
  try {
    records = JSON.parse(inspected.stdout);
  } catch (error) {
    throw new Error(`cannot parse ${label} ${container} inspection: ${error.message}`);
  }
  requireCondition(Array.isArray(records) && records.length === 1, `${label} ${container} inspection is ambiguous`);
  const record = records[0];
  return {
    name: container,
    environment: record?.Config?.Env,
    project: record?.Config?.Labels?.['com.docker.compose.project'],
    service: record?.Config?.Labels?.['com.docker.compose.service'],
    networkAddresses: Object.values(record?.NetworkSettings?.Networks || {}).flatMap((network) =>
      [network?.IPAddress, network?.GlobalIPv6Address].filter(Boolean),
    ),
  };
}

/**
 * Prove the complete backing stack is the marked disposable compose project
 * before the first SQL call or authenticated request in the orchestrator.
 */
export function assertDisposableEditStack({ webTargets, controlTarget, serverContainers }) {
  const redisContainer = process.env.REDIS_CONTAINER || PG.replace(/-db-(\d+)$/, '-redis-$1');
  const servers = serverContainers.map((container) => inspectComposeContainer(container, 'server'));
  assertDisposableComposeTopology({
    marker: process.env.ADMIN_LIFECYCLE_STACK_MARKER,
    servers,
    postgres: inspectComposeContainer(PG, 'PostgreSQL'),
    redis: inspectComposeContainer(redisContainer, 'Redis'),
  });
  assertDirectAdminOriginBindings({ webTargets, controlTarget, servers });
  for (const server of servers) {
    requireCondition(
      server.environment.includes('RSCTF_STORAGE_BACKEND=local'),
      `${server.name} must use the disposable local blob backend for exact leak auditing`,
    );
  }
}

export async function assertRuntimeRoles({ webTargets, controlTarget }) {
  const expected = [
    [TARGET, 'web'],
    ...webTargets.map((target) => [target, 'web']),
    [controlTarget, 'control'],
  ];
  for (const [endpoint, role] of expected) {
    const response = await rawRequest('GET', '/healthz', {
      baseUrl: endpoint,
      jwt: null,
      ip: null,
    });
    requireCondition(response.status === 200, `${endpoint} failed /healthz preflight`);
    requireCondition(
      response.headers.get('x-rsctf-role') === role,
      `${endpoint} reports ${response.headers.get('x-rsctf-role') || '<missing>'}, expected ${role}`,
    );
  }
}

function yamlString(value) {
  return JSON.stringify(String(value));
}

function staticManifest(name, flag) {
  return [
    `name: ${yamlString(name)}`,
    'author: "edit lifecycle"',
    'description: "Disposable organizer acceptance fixture."',
    'type: StaticAttachment',
    'category: Misc',
    'minScoreRate: 0.25',
    'difficulty: 1',
    'submissionLimit: 10',
    'flags:',
    `  - ${yamlString(flag)}`,
    '',
  ].join('\n');
}

/** Build a small challenge-repository ZIP. Returns bytes and always removes its temp tree. */
export function challengeArchive(entries) {
  const root = mkdtempSync(join(tmpdir(), 'rsctf-edit-archive-'));
  const archive = `${root}.zip`;
  try {
    entries.forEach((entry, index) => {
      const directory = join(root, `challenge-${index + 1}`);
      mkdirSync(directory, { recursive: true });
      writeFileSync(
        join(directory, 'challenge.yaml'),
        staticManifest(entry.name, entry.flag),
        { mode: 0o600 },
      );
    });
    execFileSync('zip', ['-q', '-r', archive, '.'], { cwd: root, stdio: 'pipe' });
    return readFileSync(archive);
  } finally {
    rmSync(root, { recursive: true, force: true });
    rmSync(archive, { force: true });
  }
}

/**
 * Build a valid game-transfer archive whose final duplicate division config
 * fails only after the game, challenge, attachment, blob metadata, and first
 * config have all been attempted inside the import transaction.
 */
export function transactionalFailureGameArchive(runKey, now = Date.now()) {
  const key = String(runKey || '');
  requireCondition(/^[a-z0-9]+$/i.test(key), 'transaction rollback probe requires an alphanumeric run key');
  const challengeId = 91_001;
  const title = `EDIT-ROLLBACK-${key}`;
  const challengeTitle = `edit-rollback-challenge-${key}`;
  const divisionName = `edit-rollback-division-${key}`;
  const flag = `flag{edit_rollback_${key}}`;
  const checker = `/data/files/checkers/load/other-${key}/checker`;
  const blob = Buffer.from(`rsctf transactional import probe ${key}`, 'utf8');
  const hash = createHash('sha256').update(blob).digest('hex');
  const game = {
    title,
    summary: `late import rollback probe ${key}`,
    content: 'This archive must never commit a partial game.',
    teamMemberCountLimit: 1,
    containerCountLimit: 1,
    startTimeUtc: new Date(now + 86_400_000).toISOString(),
    endTimeUtc: new Date(now + 90_000_000).toISOString(),
    writeupDeadline: new Date(now + 90_000_000).toISOString(),
    divisions: [{
      name: divisionName,
      defaultPermissions: 15,
      challengeConfigs: [
        { challengeId, permissions: 15 },
        { challengeId, permissions: 15 },
      ],
    }],
  };
  const challenge = {
    id: challengeId,
    title: challengeTitle,
    content: 'Late-failure transaction sentinel.',
    category: 'Misc',
    type: 'AttackDefense',
    submissionLimit: 0,
    originalScore: 100,
    minScoreRate: 0.25,
    difficulty: 1,
    adCheckerImage: checker,
    adScoringWeight: 1,
    attachmentType: 'Local',
    attachmentFileHash: hash,
    attachmentFileName: `${key}-rollback.bin`,
    flags: [{ flag }],
  };

  const root = mkdtempSync(join(tmpdir(), 'rsctf-edit-game-import-'));
  const archive = `${root}.zip`;
  try {
    mkdirSync(join(root, 'challenges'), { recursive: true });
    mkdirSync(join(root, 'files'), { recursive: true });
    writeFileSync(join(root, 'game.json'), JSON.stringify(game, null, 2), { mode: 0o600 });
    writeFileSync(
      join(root, 'challenges', `challenge-${challengeId}.json`),
      JSON.stringify(challenge, null, 2),
      { mode: 0o600 },
    );
    writeFileSync(join(root, 'files', hash), blob, { mode: 0o600 });
    execFileSync('zip', ['-q', '-r', archive, '.'], { cwd: root, stdio: 'pipe' });
    return {
      bytes: readFileSync(archive),
      title,
      challengeTitle,
      divisionName,
      flag,
      checker,
      hash,
    };
  } finally {
    rmSync(root, { recursive: true, force: true });
    rmSync(archive, { force: true });
  }
}

export async function waitForSql(query, predicate, {
  timeoutMs = 120_000,
  intervalMs = 1_000,
  label = 'database condition',
} = {}) {
  const deadline = Date.now() + timeoutMs;
  let value;
  do {
    value = sql(query);
    if (predicate(value)) return value;
    if (Date.now() < deadline) await sleep(intervalMs);
  } while (Date.now() <= deadline);
  throw new Error(`${label} did not converge within ${timeoutMs} ms (last value: ${value})`);
}

const GAME_SETTING_COLUMNS = Object.freeze({
  adWarmupSeconds: 'ad_warmup_seconds',
  adSnapshotRetentionDays: 'ad_snapshot_retention_days',
  adTickSeconds: 'ad_tick_seconds',
  adFlagLifetimeTicks: 'ad_flag_lifetime_ticks',
  adResetCooldownMinutes: 'ad_reset_cooldown_minutes',
  adAllowSnapshotDownload: 'ad_allow_snapshot_download',
  adGetflagWindowFraction: 'ad_getflag_window_fraction',
  adMinGracePeriodSeconds: 'ad_min_grace_period_seconds',
  adEpochTicks: 'ad_epoch_ticks',
  kothEpochTicks: 'koth_epoch_ticks',
  kothCycleTicks: 'koth_cycle_ticks',
  kothChampionCooldownTicks: 'koth_champion_cooldown_ticks',
  kothClaimConfirmationTicks: 'koth_claim_confirmation_ticks',
});

/** Prove that a create response did not silently discard any supplied engine knob. */
export function assertPersistedGameSettings(gameId, expected, label = 'game') {
  const id = Number(gameId);
  requireCondition(Number.isSafeInteger(id) && id > 0, `invalid ${label} id`);
  const entries = Object.entries(expected || {});
  requireCondition(entries.length > 0, `${label} has no expected creation settings`);
  for (const [field] of entries) {
    requireCondition(GAME_SETTING_COLUMNS[field], `${label} uses unknown creation setting ${field}`);
  }
  const projection = entries.flatMap(([field]) => [
    sqlLiteral(field),
    GAME_SETTING_COLUMNS[field],
  ]).join(',');
  const raw = sql(
    `SELECT json_build_object(${projection})::text FROM "Games" WHERE id=${id}`,
  );
  requireCondition(raw, `${label} ${id} disappeared before its settings could be verified`);
  const actual = JSON.parse(raw);
  for (const [field, value] of entries) {
    requireCondition(
      Object.is(actual[field], value),
      `${label} ${id} did not persist ${field}: expected ${JSON.stringify(value)}, ` +
        `observed ${JSON.stringify(actual[field])}`,
    );
  }
  return actual;
}

function installationDockerScope() {
  const inspected = docker(['inspect', RSCTF]);
  requireCondition(inspected.status === 0, `cannot inspect rsctf container ${RSCTF}`);
  const records = JSON.parse(inspected.stdout);
  requireCondition(records.length === 1, `rsctf container ${RSCTF} inspection is ambiguous`);
  return dockerScopeFromContainerEnv(records[0]?.Config?.Env);
}

/**
 * Resolve one public-API-provisioned KotH hill through exactly one durable
 * owner. The initial hill is challenge/shared-container owned; a crown-cycle
 * replacement is owned by its active KothCrownCycles row after the old public
 * container record has been retired.
 */
export function discoverManagedKothHill(gameId, challengeId) {
  const gid = Number(gameId);
  const cid = Number(challengeId);
  requireCondition(Number.isSafeInteger(gid) && gid > 0, `invalid KotH game ${gameId}`);
  requireCondition(Number.isSafeInteger(cid) && cid > 0, `invalid KotH challenge ${challengeId}`);
  const targetCount = Number(sql(
    `SELECT count(*) FROM "KothTargets" WHERE game_id=${gid} AND challenge_id=${cid}`,
  ));
  requireCondition(targetCount === 1, `KotH provisioning created ${targetCount} exact target rows`);
  const raw = sql(
    `WITH exact_target AS (` +
      `SELECT target.id,target.game_id,target.challenge_id,target.host,target.port,` +
        `target.container_id,challenge.shared_container_id ` +
      `FROM "KothTargets" target ` +
      `JOIN "GameChallenges" challenge ON challenge.id=target.challenge_id ` +
        `AND challenge.game_id=target.game_id AND challenge."Type"=5 ` +
      `WHERE target.game_id=${gid} AND target.challenge_id=${cid}` +
    `), owners AS (` +
      `SELECT 'shared'::text AS owner_kind,container.id::text AS container_id,` +
        `container.container_id AS owner_backend_id,container.ip AS container_ip,` +
        `container.port AS container_port,container.public_ip,container.public_port,` +
        `'container:'||container.id::text AS operation_owner ` +
      `FROM exact_target target ` +
      `JOIN "Containers" container ON container.id=target.shared_container_id ` +
        `AND container.container_id=target.container_id ` +
      `UNION ALL ` +
      `SELECT 'cycle'::text,NULL::text,cycle.replacement_container_id,` +
        `cycle.replacement_host,cycle.replacement_port,NULL::text,NULL::integer,` +
        `'koth-cycle:'||cycle.id::text||':attempt:'||cycle.reset_attempt::text ` +
      `FROM exact_target target ` +
      `JOIN "KothCrownCycles" cycle ON cycle.game_id=target.game_id ` +
        `AND cycle.challenge_id=target.challenge_id ` +
        `AND cycle.replacement_container_id=target.container_id ` +
        `AND cycle.phase='Active'` +
    `), resolved AS (` +
      `SELECT count(*) AS owner_count,min(owner_kind) AS owner_kind,` +
        `min(container_id) AS container_id,min(owner_backend_id) AS owner_backend_id,` +
        `min(container_ip) AS container_ip,min(container_port) AS container_port,` +
        `min(public_ip) AS public_ip,min(public_port) AS public_port,` +
        `min(operation_owner) AS operation_owner FROM owners` +
    `) SELECT json_build_object(` +
      `'targetId',target.id,'host',target.host,'port',target.port,` +
      `'backendId',target.container_id,'ownerCount',resolved.owner_count,` +
      `'ownerKind',resolved.owner_kind,'containerId',resolved.container_id,` +
      `'containerBackendId',resolved.owner_backend_id,` +
      `'containerIp',resolved.container_ip,'containerPort',resolved.container_port,` +
      `'publicIp',resolved.public_ip,'publicPort',resolved.public_port,` +
      `'operationOwner',resolved.operation_owner` +
    `)::text FROM exact_target target CROSS JOIN resolved`,
  );
  requireCondition(raw, 'KotH target is not linked to its challenge');
  const owner = JSON.parse(raw);
  requireCondition(
    owner.ownerCount === 1 && (owner.ownerKind === 'shared' || owner.ownerKind === 'cycle'),
    `KotH target has ${owner.ownerCount} durable owners`,
  );
  requireCondition(
    /^[a-f0-9]{64}$/.test(owner.backendId || ''),
    'KotH target omitted its immutable Docker backend identity',
  );
  requireCondition(owner.backendId === owner.containerBackendId, 'KotH target/backend ownership diverged');
  requireCondition(owner.host === (owner.publicIp || owner.containerIp), 'KotH target host was not published from its owned container');
  requireCondition(owner.port === (owner.publicPort || owner.containerPort), 'KotH target port was not published from its owned container');

  const inspected = docker(['container', 'inspect', owner.backendId]);
  requireCondition(inspected.status === 0, `KotH backend ${owner.backendId} is absent after provisioning`);
  const records = JSON.parse(inspected.stdout);
  requireCondition(records.length === 1, 'KotH backend inspection is ambiguous');
  const backend = records[0];
  const labels = backend?.Config?.Labels || {};
  const expectedScope = installationDockerScope();
  requireCondition(backend.Id === owner.backendId, 'KotH backend resolved to a different canonical identity');
  requireCondition(backend?.State?.Running === true, 'KotH backend is not running after provisioning');
  requireCondition(labels['rsctf.managed'] === expectedScope, 'KotH backend has the wrong managed-owner label');
  requireCondition(labels['rsctf.scope'] === expectedScope, 'KotH backend has the wrong installation-scope label');
  requireCondition(labels['rsctf.operation'] === owner.operationOwner, 'KotH backend has the wrong durable operation owner');
  requireCondition(/^[a-f0-9]{64}$/.test(labels['rsctf.launch-spec'] || ''), 'KotH backend has no immutable launch fingerprint');
  requireCondition(
    Object.keys(backend?.HostConfig?.PortBindings || {}).length === 0,
    'KotH backend unexpectedly published a host port',
  );
  return owner;
}

/** Assert the observable template semantics promised by the game Clone route. */
export function assertSemanticGameClone(sourceGameId, cloneGameId, expectedTitle) {
  const sourceId = Number(sourceGameId);
  const cloneId = Number(cloneGameId);
  requireCondition(Number.isSafeInteger(sourceId) && sourceId > 0, 'invalid clone source game id');
  requireCondition(Number.isSafeInteger(cloneId) && cloneId > 0 && cloneId !== sourceId, 'invalid clone game id');
  const title = String(expectedTitle || '');
  requireCondition(title.length > 0, 'clone title is required');

  const gameShape = sql(
    `SELECT (` +
      `clone.title=${sqlLiteral(title)} AND clone.hidden AND NOT clone.allow_user_submissions ` +
      `AND clone.writeup_deadline=to_timestamp(0) AND clone.ad_scoring_start_round IS NULL ` +
      `AND clone.koth_scoring_start_round IS NULL AND NOT clone.ad_scoring_paused ` +
      `AND clone.ad_allow_snapshot_download ` +
      `AND clone.public_key<>source.public_key AND clone.private_key<>source.private_key ` +
      `AND (clone.summary,clone.content,clone.practice_mode,clone.accept_without_review,` +
        `clone.writeup_required,clone.writeup_note,clone.team_member_count_limit,` +
        `clone.container_count_limit,clone.blood_bonus_value,clone.start_time_utc,clone.end_time_utc,` +
        `clone.ad_warmup_seconds,clone.ad_snapshot_retention_days,clone.ad_tick_seconds,` +
        `clone.ad_flag_lifetime_ticks,clone.ad_reset_cooldown_minutes,` +
        `clone.ad_getflag_window_fraction,clone.ad_min_grace_period_seconds,clone.ad_epoch_ticks,` +
        `clone.koth_epoch_ticks,clone.koth_cycle_ticks,clone.koth_champion_cooldown_ticks,` +
        `clone.koth_claim_confirmation_ticks) IS NOT DISTINCT FROM ` +
      `(source.summary,source.content,source.practice_mode,source.accept_without_review,` +
        `source.writeup_required,source.writeup_note,source.team_member_count_limit,` +
        `source.container_count_limit,source.blood_bonus_value,source.start_time_utc,source.end_time_utc,` +
        `source.ad_warmup_seconds,source.ad_snapshot_retention_days,source.ad_tick_seconds,` +
        `source.ad_flag_lifetime_ticks,source.ad_reset_cooldown_minutes,` +
        `source.ad_getflag_window_fraction,source.ad_min_grace_period_seconds,source.ad_epoch_ticks,` +
        `source.koth_epoch_ticks,source.koth_cycle_ticks,source.koth_champion_cooldown_ticks,` +
        `source.koth_claim_confirmation_ticks)` +
    `)::text FROM "Games" source JOIN "Games" clone ON clone.id=${cloneId} ` +
      `WHERE source.id=${sourceId}`,
  );
  requireCondition(gameShape === 'true', 'cloned game did not preserve its documented template settings');

  const sourceChallenges = Number(sql(`SELECT count(*) FROM "GameChallenges" WHERE game_id=${sourceId}`));
  const cloneChallenges = Number(sql(`SELECT count(*) FROM "GameChallenges" WHERE game_id=${cloneId}`));
  requireCondition(sourceChallenges > 0 && cloneChallenges === sourceChallenges, 'clone challenge cardinality diverged from its source');
  const copiedColumns = [
    'title', 'content', 'category', '"Type"', 'hints::text AS hints', 'flag_template', 'file_name',
    'container_image', 'memory_limit', 'storage_limit', 'cpu_count', 'expose_port',
    'workload_spec', 'enable_traffic_capture', 'disable_blood_bonus', 'original_score',
    'min_score_rate', 'difficulty', 'ad_scoring_weight', 'submission_limit',
  ].join(',');
  const challengeMismatch = Number(sql(
    `SELECT count(*) FROM (` +
      `(SELECT ${copiedColumns} FROM "GameChallenges" WHERE game_id=${sourceId} ` +
        `EXCEPT SELECT ${copiedColumns} FROM "GameChallenges" WHERE game_id=${cloneId}) ` +
      `UNION ALL ` +
      `(SELECT ${copiedColumns} FROM "GameChallenges" WHERE game_id=${cloneId} ` +
        `EXCEPT SELECT ${copiedColumns} FROM "GameChallenges" WHERE game_id=${sourceId})` +
    `) mismatch`,
  ));
  requireCondition(challengeMismatch === 0, `clone changed ${challengeMismatch} challenge template shapes`);
  const unsafeCloneRows = Number(sql(
    `SELECT count(*) FROM "GameChallenges" WHERE game_id=${cloneId} AND (` +
      `is_enabled OR accepted_count<>0 OR submission_count<>0 OR review_status<>0 OR build_status<>0 ` +
      `OR attachment_id IS NOT NULL OR test_container_id IS NOT NULL OR shared_container_id IS NOT NULL ` +
      `OR enable_shared_container OR ad_checker_image IS NOT NULL OR ad_allow_egress ` +
      `OR ad_allow_self_reset OR ad_ssh_requires_flag OR ad_self_hosted)`,
  ));
  requireCondition(unsafeCloneRows === 0, 'clone retained live or privileged challenge state');
  const flagMismatch = Number(sql(
    `SELECT count(*) FROM (` +
      `(SELECT challenge.title,flag.flag FROM "GameChallenges" challenge ` +
        `JOIN "FlagContexts" flag ON flag.challenge_id=challenge.id WHERE challenge.game_id=${sourceId} ` +
        `EXCEPT SELECT challenge.title,flag.flag FROM "GameChallenges" challenge ` +
        `JOIN "FlagContexts" flag ON flag.challenge_id=challenge.id WHERE challenge.game_id=${cloneId}) ` +
      `UNION ALL ` +
      `(SELECT challenge.title,flag.flag FROM "GameChallenges" challenge ` +
        `JOIN "FlagContexts" flag ON flag.challenge_id=challenge.id WHERE challenge.game_id=${cloneId} ` +
        `EXCEPT SELECT challenge.title,flag.flag FROM "GameChallenges" challenge ` +
        `JOIN "FlagContexts" flag ON flag.challenge_id=challenge.id WHERE challenge.game_id=${sourceId})` +
    `) mismatch`,
  ));
  requireCondition(flagMismatch === 0, `clone changed ${flagMismatch} static flag bindings`);
  return { sourceChallenges, cloneChallenges };
}

/** Assert the portable copy and deliberate sanitization contract of game import. */
export function assertSemanticGameImport(sourceGameId, importedGameId, checkerChallengeTitle) {
  const sourceId = Number(sourceGameId);
  const importedId = Number(importedGameId);
  requireCondition(Number.isSafeInteger(sourceId) && sourceId > 0, 'invalid import source game id');
  requireCondition(
    Number.isSafeInteger(importedId) && importedId > 0 && importedId !== sourceId,
    'invalid imported game id',
  );
  const checkerTitle = String(checkerChallengeTitle || '');
  requireCondition(checkerTitle.length > 0, 'checker challenge title is required');

  const gameShape = sql(
    `SELECT (` +
      `source.poster_hash IS NOT NULL AND imported.poster_hash IS NULL ` +
      `AND imported.title=source.title AND imported.hidden AND NOT imported.practice_mode ` +
      `AND imported.invite_code IS NULL AND imported.ad_scoring_start_round IS NULL ` +
      `AND imported.koth_scoring_start_round IS NULL AND NOT imported.ad_scoring_paused ` +
      `AND imported.public_key<>source.public_key AND imported.private_key<>source.private_key ` +
      `AND (imported.summary,imported.content,imported.accept_without_review,` +
        `imported.allow_user_submissions,imported.writeup_required,imported.team_member_count_limit,` +
        `imported.container_count_limit,imported.discord_webhook,imported.start_time_utc,` +
        `imported.end_time_utc,imported.writeup_deadline,imported.freeze_time_utc,` +
        `imported.writeup_note,imported.blood_bonus_value,imported.ad_warmup_seconds,` +
        `imported.ad_snapshot_retention_days,imported.ad_tick_seconds,` +
        `imported.ad_flag_lifetime_ticks,imported.ad_reset_cooldown_minutes,` +
        `imported.ad_allow_snapshot_download,imported.ad_getflag_window_fraction,` +
        `imported.ad_min_grace_period_seconds,imported.ad_epoch_ticks,imported.koth_epoch_ticks,` +
        `imported.koth_cycle_ticks,imported.koth_champion_cooldown_ticks,` +
        `imported.koth_claim_confirmation_ticks) IS NOT DISTINCT FROM ` +
      `(source.summary,source.content,source.accept_without_review,source.allow_user_submissions,` +
        `source.writeup_required,source.team_member_count_limit,source.container_count_limit,` +
        `source.discord_webhook,source.start_time_utc,source.end_time_utc,` +
        `source.writeup_deadline,source.freeze_time_utc,source.writeup_note,` +
        `source.blood_bonus_value,source.ad_warmup_seconds,source.ad_snapshot_retention_days,` +
        `source.ad_tick_seconds,source.ad_flag_lifetime_ticks,source.ad_reset_cooldown_minutes,` +
        `source.ad_allow_snapshot_download,source.ad_getflag_window_fraction,` +
        `source.ad_min_grace_period_seconds,source.ad_epoch_ticks,source.koth_epoch_ticks,` +
        `source.koth_cycle_ticks,source.koth_champion_cooldown_ticks,` +
        `source.koth_claim_confirmation_ticks)` +
    `)::text FROM "Games" source JOIN "Games" imported ON imported.id=${importedId} ` +
      `WHERE source.id=${sourceId}`,
  );
  requireCondition(gameShape === 'true', 'imported game changed portable settings or retained local state');

  const sourceChallenges = Number(sql(`SELECT count(*) FROM "GameChallenges" WHERE game_id=${sourceId}`));
  const importedChallenges = Number(sql(`SELECT count(*) FROM "GameChallenges" WHERE game_id=${importedId}`));
  requireCondition(
    sourceChallenges > 0 && importedChallenges === sourceChallenges,
    'imported challenge cardinality diverged from its source',
  );
  const copiedColumns = [
    'title', 'content', 'category', '"Type"', 'hints::text AS hints', 'flag_template', 'file_name',
    'container_image', 'memory_limit', 'storage_limit', 'cpu_count', 'expose_port',
    'workload_spec', 'deadline_utc', 'enable_traffic_capture', 'enable_shared_container',
    'disable_blood_bonus', 'original_score', 'min_score_rate', 'difficulty', 'score_curve',
    'submission_limit', 'ad_allow_egress', 'ad_allow_self_reset', 'ad_ssh_requires_flag',
    'ad_self_hosted', 'ad_scoring_weight',
  ].join(',');
  const challengeMismatch = Number(sql(
    `SELECT count(*) FROM (` +
      `(SELECT ${copiedColumns} FROM "GameChallenges" WHERE game_id=${sourceId} ` +
        `EXCEPT SELECT ${copiedColumns} FROM "GameChallenges" WHERE game_id=${importedId}) ` +
      `UNION ALL ` +
      `(SELECT ${copiedColumns} FROM "GameChallenges" WHERE game_id=${importedId} ` +
        `EXCEPT SELECT ${copiedColumns} FROM "GameChallenges" WHERE game_id=${sourceId})` +
    `) mismatch`,
  ));
  requireCondition(challengeMismatch === 0, `import changed ${challengeMismatch} portable challenge shapes`);
  const unsafeRows = Number(sql(
    `SELECT count(*) FROM "GameChallenges" WHERE game_id=${importedId} AND (` +
      `is_enabled OR accepted_count<>0 OR submission_count<>0 OR review_status<>0 OR build_status<>0 ` +
      `OR test_container_id IS NOT NULL OR shared_container_id IS NOT NULL ` +
      `OR ad_checker_image IS NOT NULL)`,
  ));
  requireCondition(unsafeRows === 0, 'import retained live challenge state or a deployment-local checker');
  const checkerShape = sql(
    `SELECT (` +
      `source.ad_checker_image IS NOT NULL AND imported.ad_checker_image IS NULL` +
    `)::text FROM "GameChallenges" source JOIN "GameChallenges" imported ` +
      `ON imported.game_id=${importedId} AND imported.title=source.title ` +
      `WHERE source.game_id=${sourceId} AND source.title=${sqlLiteral(checkerTitle)}`,
  );
  requireCondition(checkerShape === 'true', 'import did not clear the exact deployment-local checker');

  const flagMismatch = Number(sql(
    `SELECT count(*) FROM (` +
      `(SELECT challenge.title,flag.flag FROM "GameChallenges" challenge ` +
        `JOIN "FlagContexts" flag ON flag.challenge_id=challenge.id WHERE challenge.game_id=${sourceId} ` +
        `EXCEPT SELECT challenge.title,flag.flag FROM "GameChallenges" challenge ` +
        `JOIN "FlagContexts" flag ON flag.challenge_id=challenge.id WHERE challenge.game_id=${importedId}) ` +
      `UNION ALL ` +
      `(SELECT challenge.title,flag.flag FROM "GameChallenges" challenge ` +
        `JOIN "FlagContexts" flag ON flag.challenge_id=challenge.id WHERE challenge.game_id=${importedId} ` +
        `EXCEPT SELECT challenge.title,flag.flag FROM "GameChallenges" challenge ` +
        `JOIN "FlagContexts" flag ON flag.challenge_id=challenge.id WHERE challenge.game_id=${sourceId})` +
    `) mismatch`,
  ));
  requireCondition(flagMismatch === 0, `import changed ${flagMismatch} static flag bindings`);

  const remoteAttachmentMismatch = Number(sql(
    `SELECT count(*) FROM (` +
      `SELECT source.title,source_attachment."Type",source_attachment.remote_url ` +
      `FROM "GameChallenges" source JOIN "Attachments" source_attachment ` +
        `ON source_attachment.id=source.attachment_id WHERE source.game_id=${sourceId} ` +
        `AND source_attachment."Type"=2 ` +
      `EXCEPT SELECT imported.title,imported_attachment."Type",imported_attachment.remote_url ` +
      `FROM "GameChallenges" imported JOIN "Attachments" imported_attachment ` +
        `ON imported_attachment.id=imported.attachment_id WHERE imported.game_id=${importedId}` +
    `) mismatch`,
  ));
  requireCondition(remoteAttachmentMismatch === 0, 'import changed or dropped a portable remote attachment');
  return { sourceChallenges, importedChallenges };
}

/** Prove a deliberately late import failure left neither metadata nor physical content. */
export function assertFailedGameImportRolledBack(probe, serverContainers) {
  requireCondition(probe && /^[a-f0-9]{64}$/.test(probe.hash || ''), 'invalid import rollback probe');
  const raw = sql(
    `SELECT json_build_object(` +
      `'games',(SELECT count(*) FROM "Games" WHERE title=${sqlLiteral(probe.title)}),` +
      `'challenges',(SELECT count(*) FROM "GameChallenges" WHERE title=${sqlLiteral(probe.challengeTitle)}),` +
      `'files',(SELECT count(*) FROM "Files" WHERE hash=${sqlLiteral(probe.hash)}),` +
      `'attachments',(SELECT count(*) FROM "Attachments" attachment JOIN "Files" file ` +
        `ON file.id=attachment.local_file_id WHERE file.hash=${sqlLiteral(probe.hash)}),` +
      `'flags',(SELECT count(*) FROM "FlagContexts" WHERE flag=${sqlLiteral(probe.flag)}),` +
      `'divisions',(SELECT count(*) FROM "Divisions" WHERE name=${sqlLiteral(probe.divisionName)})` +
    `)::text`,
  );
  const counts = JSON.parse(raw);
  for (const [kind, count] of Object.entries(counts)) {
    requireCondition(count === 0, `failed game import left ${count} run-owned ${kind}`);
  }
  const path = `/data/files/${probe.hash.slice(0, 2)}/${probe.hash.slice(2, 4)}/${probe.hash}`;
  for (const container of serverContainers) {
    const absent = docker(['exec', container, 'test', '!', '-e', path]);
    requireCondition(absent.status === 0, `failed game import left physical blob ${path} in ${container}`);
  }
  return counts;
}

export function removeRuntime(containerId, label = 'fixture runtime') {
  if (!containerId) return;
  requireCondition(/^[a-zA-Z0-9:._-]{12,256}$/.test(String(containerId)), `invalid ${label} id`);
  const removed = docker(['rm', '-f', String(containerId)]);
  if (removed.status !== 0 && !/no such (?:container|object)/i.test(removed.stderr || '')) {
    throw new Error(`remove ${label} ${containerId}: ${removed.stderr.trim()}`);
  }
  const inspected = docker(['container', 'inspect', String(containerId)]);
  requireCondition(inspected.status !== 0, `${label} ${containerId} survived removal`);
}

export function removeCheckerDirectory(gameId) {
  const id = Number(gameId);
  requireCondition(Number.isSafeInteger(id) && id > 0, `invalid checker game ${gameId}`);
  const path = `/data/files/checkers/load/${id}`;
  const removed = docker(['exec', RSCTF, 'rm', '-rf', path]);
  requireCondition(removed.status === 0, `remove checker directory ${path}: ${removed.stderr.trim()}`);
  const absent = docker(['exec', RSCTF, 'test', '!', '-e', path]);
  requireCondition(absent.status === 0, `checker directory ${path} survived removal`);
}

export function mutateContainerFilesystem(containerId, marker) {
  requireCondition(/^[a-f0-9]{12,64}$/i.test(String(containerId)), 'invalid A&D container identity');
  const result = docker([
    'exec', String(containerId), 'sh', '-c',
    'mkdir -p /tmp/rsctf-edit && printf "%s" "$1" > /tmp/rsctf-edit/marker',
    'edit-marker', String(marker),
  ]);
  requireCondition(result.status === 0, `mutate A&D service filesystem: ${result.stderr.trim()}`);
}

function canonicalManagedRef(value) {
  let image = String(value || '').trim();
  if (!image) return null;
  if (image.includes('@')) return null;
  image = image.replace(/^index\.docker\.io\//i, 'docker.io/');
  const first = image.split('/')[0];
  if (!first.includes('.') && !first.includes(':') && first.toLowerCase() !== 'localhost') {
    image = `docker.io/${image}`;
  }
  const slash = image.lastIndexOf('/');
  if (!image.slice(slash + 1).includes(':')) image += ':latest';
  return image.startsWith('docker.io/rsctf/') ? image : null;
}

export function ownedBuildImageRefs(gameIds) {
  const ids = [...new Set(gameIds.map(Number))].filter((id) => Number.isSafeInteger(id) && id > 0);
  if (ids.length === 0) return [];
  const values = sql(
    `SELECT image_ref FROM (` +
      `SELECT game_id,container_image AS image_ref FROM "GameChallenges" ` +
      `UNION ALL SELECT game_id,ad_checker_image AS image_ref FROM "GameChallenges"` +
    `) images WHERE game_id IN (${ids.join(',')}) AND image_ref IS NOT NULL ` +
      `AND btrim(image_ref)<>'' ORDER BY image_ref`,
  );
  const ownedGames = new Set(ids.map(String));
  return [...new Set(String(values || '').split('\n').map(canonicalManagedRef).filter((imageRef) => {
    if (!imageRef) return false;
    const match = imageRef.match(/^docker\.io\/rsctf\/(\d+)\//);
    return match && ownedGames.has(match[1]);
  }))];
}
