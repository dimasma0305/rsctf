// Focused helpers for the destructive, disposable admin lifecycle harness.
// Runtime orchestration stays in admin-lifecycle.mjs; this module owns exact
// HTTP/SQL fixture mechanics so route assertions remain readable.
import { execFileSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { adminJwt, api } from './applib.mjs';
import { docker, PG, PG_DATABASE, PG_USER, RSCTF, sql, TARGET } from './lib.mjs';

const ADMIN_LIFECYCLE_DATABASE_LOCK_KEY = createHash('sha256')
  .update('rsctf:load:admin-lifecycle')
  .digest()
  .readBigInt64BE(0)
  .toString();

function waitForChildExit(child, timeoutMs, label) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      child.kill('SIGTERM');
      reject(new Error(`${label} did not exit within ${timeoutMs} ms`));
    }, timeoutMs);
    child.once('error', (error) => {
      clearTimeout(timeout);
      reject(error);
    });
    child.once('exit', (code, signal) => {
      clearTimeout(timeout);
      if (code === 0) resolve();
      else reject(new Error(`${label} exited with ${code ?? signal ?? 'unknown status'}`));
    });
  });
}

/**
 * Hold one PostgreSQL session advisory lock for the complete destructive run.
 * The host-local process lock prevents accidental overlap on one runner; this
 * lease also excludes a lifecycle started from another host against the same
 * disposable database. Keeping psql's stdin open keeps the database session,
 * and therefore the lock, alive without a polling table or stale lease row.
 */
export async function acquireAdminLifecycleDatabaseLock({ timeoutMs = 10_000 } = {}) {
  const child = spawn(
    'docker',
    ['exec', '-i', PG, 'psql', '-XqAt', '-v', 'ON_ERROR_STOP=1', '-U', PG_USER, '-d', PG_DATABASE],
    { stdio: ['pipe', 'pipe', 'pipe'] },
  );
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr += chunk; });

  const acquired = await new Promise((resolve, reject) => {
    let stdout = '';
    const timeout = setTimeout(() => {
      child.stdin.end('\\q\n');
      reject(new Error(`PostgreSQL admin lifecycle lock timed out after ${timeoutMs} ms`));
    }, timeoutMs);
    const finish = (action, value) => {
      clearTimeout(timeout);
      child.stdout.off('data', onData);
      child.off('error', onError);
      child.off('exit', onExit);
      action(value);
    };
    const onData = (chunk) => {
      stdout += chunk;
      const line = stdout.split(/\r?\n/).map((value) => value.trim()).find(Boolean);
      if (line === 't') finish(resolve, true);
      else if (line === 'f') finish(resolve, false);
    };
    const onError = (error) => finish(reject, error);
    const onExit = (code, signal) => finish(
      reject,
      new Error(
        `PostgreSQL admin lifecycle lock session exited before acquisition ` +
          `(${code ?? signal ?? 'unknown status'}): ${stderr.trim()}`,
      ),
    );
    child.stdout.on('data', onData);
    child.once('error', onError);
    child.once('exit', onExit);
    child.stdin.write(`SELECT pg_try_advisory_lock(${ADMIN_LIFECYCLE_DATABASE_LOCK_KEY});\n`);
  });

  if (!acquired) {
    const exit = waitForChildExit(child, timeoutMs, 'busy PostgreSQL admin lifecycle lock session');
    child.stdin.end('\\q\n');
    await exit;
    const error = new Error('another admin lifecycle holds the disposable database advisory lock');
    error.code = 'ELOCKED';
    throw error;
  }

  let released = false;
  return Object.freeze({
    key: ADMIN_LIFECYCLE_DATABASE_LOCK_KEY,
    async release() {
      if (released) return false;
      released = true;
      const exit = waitForChildExit(child, timeoutMs, 'PostgreSQL admin lifecycle lock session');
      child.stdin.end(
        `SELECT pg_advisory_unlock(${ADMIN_LIFECYCLE_DATABASE_LOCK_KEY});\n\\q\n`,
      );
      await exit;
      return true;
    },
  });
}

export const unwrap = (response) =>
  response?.json && Object.hasOwn(response.json, 'data') ? response.json.data : response?.json;

export function sqlLiteral(value) {
  if (value === null || value === undefined) return 'NULL';
  return `'${String(value).replaceAll("'", "''")}'`;
}

export function positiveId(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer (got ${value})`);
  }
  return parsed;
}

export function expectStatus(response, expected, label) {
  const accepted = Array.isArray(expected) ? expected : [expected];
  if (!accepted.includes(response.status)) {
    throw new Error(
      `${label} returned ${response.status}; expected ${accepted.join('/')} ` +
        `${String(response.text || '').slice(0, 500)}`,
    );
  }
  return response;
}

export async function adminApi(method, path, {
  body,
  baseUrl = TARGET,
  jwt = adminJwt(),
  ip = '10.252.0.10',
  timeoutMs = 120_000,
  expected = 200,
  label = `${method} ${path}`,
} = {}) {
  const response = await api(method, path, { body, baseUrl, jwt, ip, timeoutMs });
  return expectStatus(response, expected, label);
}

export async function rawRequest(method, path, {
  baseUrl = TARGET,
  jwt = adminJwt(),
  ip = '10.252.0.11',
  body,
  headers = {},
  timeoutMs = 120_000,
  networkRetries = 0,
  retryDelayMs = 100,
} = {}) {
  if (!Number.isSafeInteger(networkRetries) || networkRetries < 0 || networkRetries > 1) {
    throw new Error(`networkRetries must be 0 or 1 (got ${networkRetries})`);
  }
  const requestHeaders = { ...headers };
  if (jwt) requestHeaders.authorization = `Bearer ${jwt}`;
  if (ip) requestHeaders['x-real-ip'] = ip;
  let response;
  let attempt = 0;
  for (;;) {
    try {
      response = await fetch(`${baseUrl}${path}`, {
        method,
        headers: requestHeaders,
        body,
        signal: AbortSignal.timeout(timeoutMs),
      });
      break;
    } catch (error) {
      if (attempt >= networkRetries) throw error;
      attempt += 1;
      await new Promise((resolve) => setTimeout(resolve, retryDelayMs));
    }
  }
  const bytes = new Uint8Array(await response.arrayBuffer());
  const text = new TextDecoder().decode(bytes);
  let json;
  try {
    json = text ? JSON.parse(text) : undefined;
  } catch {
    json = undefined;
  }
  return { status: response.status, headers: response.headers, bytes, text, json, attempts: attempt + 1 };
}

export async function multipartRequest(path, {
  filename,
  content,
  contentType = 'application/octet-stream',
  baseUrl = TARGET,
  jwt = adminJwt(),
  ip = '10.252.0.12',
  expected = 200,
  label = `multipart POST ${path}`,
} = {}) {
  const form = new FormData();
  form.append('file', new Blob([content], { type: contentType }), filename);
  const response = await rawRequest('POST', path, { baseUrl, jwt, ip, body: form });
  return expectStatus(response, expected, label);
}

export function userByEmail(email) {
  const raw = sql(
    `SELECT json_build_object(` +
      `'id',id,'userName',user_name,'email',email,'stamp',security_stamp,'role',role` +
      `)::text FROM "AspNetUsers" WHERE normalized_email=upper(${sqlLiteral(email)}) ` +
      `ORDER BY register_time_utc DESC LIMIT 1`,
  );
  if (!raw) throw new Error(`could not find fixture user ${email}`);
  return JSON.parse(raw);
}

export function teamByName(name) {
  const raw = sql(
    `SELECT json_build_object('id',id,'name',name,'captainId',captain_id)::text ` +
      `FROM "Teams" WHERE name=${sqlLiteral(name)} ORDER BY id DESC LIMIT 1`,
  );
  if (!raw) throw new Error(`could not find fixture team ${name}`);
  return JSON.parse(raw);
}

export function insertBuildRecord({
  challengeId,
  gameId,
  title,
  status,
  trigger = 'Bulk',
  kind = 'Challenge',
  attempt = 1,
  imageRef = null,
  logTail = null,
}) {
  const pending = status === 3 || status === 5;
  return positiveId(
    sql(
      `WITH inserted AS (` +
        `INSERT INTO "BuildRecords"(` +
          `challenge_id,game_id,challenge_title,enqueued_at_utc,started_at_utc,finished_at_utc,` +
          `trigger,kind,attempt,status,digest,image_ref,log_tail) VALUES (` +
          `${positiveId(challengeId, 'build challenge')},${positiveId(gameId, 'build game')},` +
          `${sqlLiteral(title)},clock_timestamp(),clock_timestamp(),` +
          `${pending ? 'NULL' : 'clock_timestamp()'},${sqlLiteral(trigger)},${sqlLiteral(kind)},` +
          `${positiveId(attempt, 'build attempt')},${Number(status)},NULL,${sqlLiteral(imageRef)},` +
          `${sqlLiteral(logTail)}) RETURNING id` +
        `) SELECT id FROM inserted`,
    ),
    'build record id',
  );
}

function disposableAdminGameIdentity(gameId, tag) {
  const id = positiveId(gameId, 'disposable admin game');
  const namespace = String(tag);
  if (!/^adm[a-z0-9]+$/.test(namespace)) {
    throw new Error(`invalid disposable admin namespace ${namespace}`);
  }
  return { id, title: `ADMIN-LIFECYCLE-${namespace}` };
}

function requireDisposableAdminGame(gameId, tag) {
  const identity = disposableAdminGameIdentity(gameId, tag);
  const currentTitle = sql(`SELECT title FROM "Games" WHERE id=${identity.id}`);
  if (!currentTitle) return { ...identity, exists: false };
  if (currentTitle !== identity.title) {
    throw new Error(
      `game ${identity.id} is ${currentTitle}, not the expected disposable admin fixture`,
    );
  }
  return { ...identity, exists: true };
}

/**
 * Snapshot every Docker runtime identity still reachable from the disposable
 * game's database graph. The caller takes this before evidence teardown so a
 * partially successful cleanup cannot erase the only durable runtime handle.
 */
export function disposableAdminGameRuntimeIds(gameId, tag) {
  const { id, exists } = requireDisposableAdminGame(gameId, tag);
  if (!exists) return [];
  const raw = sql(
    `SELECT COALESCE(string_agg(DISTINCT owned.container_id, E'\\n'), '') FROM (` +
      `SELECT container.container_id FROM "Containers" container ` +
        `WHERE container.id IN (` +
          `SELECT instance.container_id FROM "GameInstances" instance ` +
            `WHERE instance.challenge_id IN (` +
              `SELECT challenge.id FROM "GameChallenges" challenge WHERE challenge.game_id=${id}` +
            `) OR instance.participation_id IN (` +
              `SELECT participation.id FROM "Participations" participation WHERE participation.game_id=${id}` +
            `)` +
          ` UNION SELECT challenge.test_container_id FROM "GameChallenges" challenge ` +
            `WHERE challenge.game_id=${id}` +
          ` UNION SELECT challenge.shared_container_id FROM "GameChallenges" challenge ` +
            `WHERE challenge.game_id=${id}` +
        `) ` +
      `UNION ALL SELECT service.container_id FROM "AdTeamServices" service ` +
        `WHERE service.game_id=${id}` +
    `) owned(container_id) WHERE owned.container_id IS NOT NULL AND owned.container_id<>''`,
  );
  return [...new Set(String(raw).split('\n').map((value) => value.trim()).filter(Boolean))];
}

function assertDockerRuntimeAbsent(containerId) {
  const inspected = docker(['container', 'inspect', containerId]);
  if (inspected.status === 0) {
    throw new Error(`disposable admin Docker runtime ${containerId} is still present`);
  }
  if (!/no such (?:container|object)/i.test(inspected.stderr || '')) {
    throw new Error(
      `could not prove disposable admin Docker runtime ${containerId} was removed: ` +
        `${String(inspected.stderr || inspected.stdout).trim()}`,
    );
  }
}

function assertCheckerDirectoryAbsent(gameId) {
  const path = `/data/files/checkers/load/${gameId}`;
  const absent = docker(['exec', RSCTF, 'test', '!', '-e', path]);
  if (absent.status === 0) return;
  const present = docker(['exec', RSCTF, 'test', '-e', path]);
  if (present.status === 0) {
    throw new Error(`disposable admin checker directory ${path} is still present`);
  }
  throw new Error(
    `could not prove disposable admin checker directory ${path} was removed: ` +
      `${String(absent.stderr || present.stderr).trim()}`,
  );
}

// This is deliberately not a general-purpose game deletion path. The public
// endpoint must continue preserving started/scored events. The load harness
// reaches this fallback only after proving its exact external runtimes are gone;
// the transaction below removes only the validated disposable database graph.
export function disposableAdminGameCleanupSql(gameId, tag) {
  const { id, title } = disposableAdminGameIdentity(gameId, tag);
  const expectedTitle = sqlLiteral(title);
  const gameControlLock = createHash('sha256')
    .update(`koth-control:${id}`)
    .digest()
    .readBigInt64BE(0)
    .toString();
  return `DO $admin_fixture_cleanup$
DECLARE
  fixture_team_ids integer[] := ARRAY[]::integer[];
  fixture_user_ids uuid[] := ARRAY[]::uuid[];
  fixture_container_ids uuid[] := ARRAY[]::uuid[];
BEGIN
  -- Match the application engine's cross-replica game-control lock. A tick
  -- that started first finishes before this transaction; no replacement round
  -- or KotH evidence can race the exact graph deletion below.
  PERFORM pg_advisory_xact_lock(${gameControlLock});
  IF EXISTS (SELECT 1 FROM "Games" WHERE id=${id} AND title<>${expectedTitle}) THEN
    RAISE EXCEPTION 'game % is not the expected disposable admin fixture', ${id};
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM "Games" WHERE id=${id} AND title=${expectedTitle} FOR UPDATE
  ) THEN
    RETURN;
  END IF;

  -- Writeups are cleared through the application before this transaction so
  -- its advisory locks, reference counts, and physical blob purge all run.
  -- The admin fixture creates no other blob owner; fail closed if that changes.
  IF EXISTS (
    SELECT 1 FROM "Games" WHERE id=${id} AND poster_hash IS NOT NULL
    UNION ALL
    SELECT 1 FROM "Participations" WHERE game_id=${id} AND writeup_id IS NOT NULL
    UNION ALL
    SELECT 1 FROM "GameChallenges"
      WHERE game_id=${id}
        AND (attachment_id IS NOT NULL OR original_archive_blob_path IS NOT NULL)
    UNION ALL
    SELECT 1 FROM "FlagContexts" flag
      JOIN "GameChallenges" challenge ON challenge.id=flag.challenge_id
      WHERE challenge.game_id=${id} AND flag.attachment_id IS NOT NULL
  ) THEN
    RAISE EXCEPTION 'disposable admin fixture % still owns blob metadata', ${id};
  END IF;

  SELECT COALESCE(array_agg(DISTINCT team_id), ARRAY[]::integer[])
    INTO fixture_team_ids FROM "Participations" WHERE game_id=${id};
  SELECT COALESCE(array_agg(DISTINCT user_id), ARRAY[]::uuid[])
    INTO fixture_user_ids FROM "UserParticipations" WHERE game_id=${id};
  SELECT COALESCE(array_agg(DISTINCT container_id), ARRAY[]::uuid[])
    INTO fixture_container_ids
    FROM (
      SELECT instance.container_id
        FROM "GameInstances" instance
        WHERE instance.challenge_id IN (
          SELECT challenge.id FROM "GameChallenges" challenge WHERE challenge.game_id=${id}
        ) OR instance.participation_id IN (
          SELECT participation.id FROM "Participations" participation WHERE participation.game_id=${id}
        )
      UNION ALL
      SELECT challenge.test_container_id
        FROM "GameChallenges" challenge WHERE challenge.game_id=${id}
      UNION ALL
      SELECT challenge.shared_container_id
        FROM "GameChallenges" challenge WHERE challenge.game_id=${id}
    ) owned_containers(container_id)
    WHERE container_id IS NOT NULL;

  DELETE FROM "TrafficCaptureFailures" failure
    WHERE failure.challenge_id IN (
      SELECT challenge.id FROM "GameChallenges" challenge WHERE challenge.game_id=${id}
    ) OR failure.participation_id IN (
      SELECT participation.id FROM "Participations" participation WHERE participation.game_id=${id}
    ) OR failure.service_id IN (
      SELECT service.id FROM "AdTeamServices" service WHERE service.game_id=${id}
    );
  DELETE FROM "ContainerAccessEvents" WHERE game_id=${id};
  DELETE FROM "FlagEgressEvents" WHERE game_id=${id};
  DELETE FROM "SuspicionEvents" WHERE game_id=${id};
  DELETE FROM "HoneypotHits" WHERE game_id=${id};
  DELETE FROM "ChallengeReviews" WHERE game_id=${id};
  DELETE FROM "CheatInfo" WHERE game_id=${id};
  DELETE FROM "GameManagers" WHERE game_id=${id};
  DELETE FROM "GameEvents" WHERE game_id=${id};
  DELETE FROM "GameNotices" WHERE game_id=${id};
  DELETE FROM "BuildRecords" WHERE game_id=${id};
  DELETE FROM "UserParticipations" WHERE game_id=${id};
  DELETE FROM "GameInstances" instance
    WHERE instance.challenge_id IN (
      SELECT challenge.id FROM "GameChallenges" challenge WHERE challenge.game_id=${id}
    ) OR instance.participation_id IN (
      SELECT participation.id FROM "Participations" participation WHERE participation.game_id=${id}
    );
  DELETE FROM "Containers" WHERE id=ANY(fixture_container_ids);

  -- Cascades cover the modern scoring graph. These explicit owner deletes also
  -- handle upgraded databases whose original event tables predate those FKs.
  DELETE FROM "KothAcquisitions" WHERE game_id=${id};
  DELETE FROM "KothControlResults" WHERE game_id=${id};
  DELETE FROM "KothCrownCycles" WHERE game_id=${id};
  DELETE FROM "KothOfficialConfigs" WHERE game_id=${id};
  DELETE FROM "KothTargets" WHERE game_id=${id};
  DELETE FROM "AdFlagDeliveryResults" result
    WHERE result.round_id IN (SELECT round.id FROM "AdRounds" round WHERE round.game_id=${id})
       OR result.team_service_id IN (
         SELECT service.id FROM "AdTeamServices" service WHERE service.game_id=${id}
       );
  DELETE FROM "AdAttacks" attack
    WHERE attack.round_id IN (SELECT round.id FROM "AdRounds" round WHERE round.game_id=${id})
       OR attack.victim_team_service_id IN (
         SELECT service.id FROM "AdTeamServices" service WHERE service.game_id=${id}
       )
       OR attack.attacker_participation_id IN (
         SELECT participation.id FROM "Participations" participation WHERE participation.game_id=${id}
       );
  DELETE FROM "AdCheckResults" result
    WHERE result.round_id IN (SELECT round.id FROM "AdRounds" round WHERE round.game_id=${id})
       OR result.team_service_id IN (
         SELECT service.id FROM "AdTeamServices" service WHERE service.game_id=${id}
       );
  DELETE FROM "AdFlags" flag
    WHERE flag.round_id IN (SELECT round.id FROM "AdRounds" round WHERE round.game_id=${id})
       OR flag.team_service_id IN (
         SELECT service.id FROM "AdTeamServices" service WHERE service.game_id=${id}
       );
  DELETE FROM "KothTokens" token
    WHERE token.ad_round_id IN (SELECT round.id FROM "AdRounds" round WHERE round.game_id=${id})
       OR token.participation_id IN (
         SELECT participation.id FROM "Participations" participation WHERE participation.game_id=${id}
       )
       OR token.challenge_id IN (
         SELECT challenge.id FROM "GameChallenges" challenge WHERE challenge.game_id=${id}
       );
  DELETE FROM "AdTeamApiTokens" token
    WHERE token.participation_id IN (
      SELECT participation.id FROM "Participations" participation WHERE participation.game_id=${id}
    );
  DELETE FROM "AdSshKeys" key
    WHERE key.participation_id IN (
      SELECT participation.id FROM "Participations" participation WHERE participation.game_id=${id}
    );
  DELETE FROM "AdVpnPeers" WHERE game_id=${id};
  DELETE FROM "AdTeamServices" WHERE game_id=${id};
  DELETE FROM "AdRounds" WHERE game_id=${id};

  DELETE FROM "FlagContexts" flag
    USING "GameChallenges" challenge
    WHERE flag.challenge_id=challenge.id AND challenge.game_id=${id};
  DELETE FROM "Submissions" WHERE game_id=${id};
  DELETE FROM "Participations" WHERE game_id=${id};
  DELETE FROM "GameChallenges" WHERE game_id=${id};
  DELETE FROM "Divisions" WHERE game_id=${id};
  DELETE FROM "Games" WHERE id=${id} AND title=${expectedTitle};

  -- Cohort identities are disposable only when this game was their final
  -- participation. Never alter a team or user still owned by another event.
  DELETE FROM "TeamMembers" member
    WHERE member.team_id=ANY(fixture_team_ids)
      AND NOT EXISTS (
        SELECT 1 FROM "Participations" participation WHERE participation.team_id=member.team_id
      );
  DELETE FROM "Teams" team
    WHERE team.id=ANY(fixture_team_ids)
      AND (team.name LIKE 'LT%\\_%' OR team.name LIKE 'ltlive%')
      AND NOT EXISTS (
        SELECT 1 FROM "Participations" participation WHERE participation.team_id=team.id
      );
  DELETE FROM "AspNetUsers" account
    WHERE account.id=ANY(fixture_user_ids)
      AND account.email LIKE '%@load.test'
      AND NOT EXISTS (
        SELECT 1 FROM "TeamMembers" member WHERE member.user_id=account.id
      )
      AND NOT EXISTS (
        SELECT 1 FROM "UserParticipations" participation WHERE participation.user_id=account.id
      );
END
$admin_fixture_cleanup$`;
}

export function deleteDisposableAdminGame(gameId, tag, { runtimeIds = [] } = {}) {
  const { id, exists } = requireDisposableAdminGame(gameId, tag);
  if (!exists) return false;
  const ownedRuntimeIds = disposableAdminGameRuntimeIds(id, tag);
  for (const containerId of new Set([...runtimeIds, ...ownedRuntimeIds])) {
    if (containerId) assertDockerRuntimeAbsent(containerId);
  }
  assertCheckerDirectoryAbsent(id);
  sql(disposableAdminGameCleanupSql(id, tag));
  const residual = Number(sql(`SELECT count(*) FROM "Games" WHERE id=${id}`));
  if (residual !== 0) throw new Error(`disposable admin fixture ${id} survived exact SQL cleanup`);
  return true;
}

export function tagFixtureImage(source, tag, managedContainerId) {
  if (!/^rsctf\/admin-lifecycle-adm[a-z0-9]+:(?:single|prune)$/.test(tag)) {
    throw new Error(`invalid disposable admin fixture image tag ${tag}`);
  }
  const canonicalTag = `docker.io/${tag}`;
  if (!managedContainerId) {
    throw new Error(`cannot create owned fixture image ${tag} without a managed container`);
  }
  const scopeInspection = docker([
    'container',
    'inspect',
    managedContainerId,
    '--format',
    '{{json .Config.Labels}}',
  ]);
  if (scopeInspection.status !== 0) {
    throw new Error(
      `inspect managed fixture container ${managedContainerId}: ${scopeInspection.stderr.trim()}`,
    );
  }
  let labels;
  try {
    labels = JSON.parse(scopeInspection.stdout);
  } catch (error) {
    throw new Error(`managed fixture container labels are invalid JSON: ${error.message}`);
  }
  const scope = labels?.['rsctf.scope'];
  if (!scope || labels?.['rsctf.managed'] !== scope) {
    throw new Error(`container ${managedContainerId} is not owned by one rsctf installation`);
  }

  const created = docker(['container', 'create', source]);
  if (created.status !== 0) {
    throw new Error(`create fixture image source container: ${created.stderr.trim()}`);
  }
  const temporaryContainer = created.stdout.trim();
  let committed = false;
  try {
    const result = docker([
      'container',
      'commit',
      '--change',
      `LABEL rsctf.image.scope=${scope}`,
      '--change',
      `LABEL rsctf.image.ref=${canonicalTag}`,
      temporaryContainer,
      tag,
    ]);
    if (result.status !== 0) {
      throw new Error(`create labeled fixture image ${tag}: ${result.stderr.trim()}`);
    }
    committed = true;
  } finally {
    const removed = docker(['container', 'rm', '-f', temporaryContainer]);
    if (removed.status !== 0 && !/no such (?:container|object)/i.test(removed.stderr)) {
      if (committed) docker(['image', 'rm', '-f', tag]);
      throw new Error(`remove fixture image source container: ${removed.stderr.trim()}`);
    }
  }

  const imageInspection = docker(['image', 'inspect', tag, '--format', '{{json .Config.Labels}}']);
  if (imageInspection.status !== 0) {
    removeFixtureImage(tag);
    throw new Error(`inspect labeled fixture image ${tag}: ${imageInspection.stderr.trim()}`);
  }
  let imageLabels;
  try {
    imageLabels = JSON.parse(imageInspection.stdout);
  } catch (error) {
    removeFixtureImage(tag);
    throw new Error(`fixture image ${tag} labels are invalid JSON: ${error.message}`);
  }
  if (
    imageLabels?.['rsctf.image.scope'] !== scope ||
    imageLabels?.['rsctf.image.ref'] !== canonicalTag
  ) {
    removeFixtureImage(tag);
    throw new Error(`fixture image ${tag} does not carry its exact ownership labels`);
  }
  return tag;
}

export function removeFixtureImage(tag) {
  const result = docker(['image', 'rm', '-f', tag]);
  if (result.status !== 0 && !/no such image/i.test(result.stderr)) {
    throw new Error(`remove fixture image ${tag}: ${result.stderr.trim()}`);
  }
}

export function createWorkerCsr() {
  const directory = mkdtempSync(join(tmpdir(), 'rsctf-admin-worker-'));
  const key = join(directory, 'worker.key');
  const csr = join(directory, 'worker.csr');
  try {
    execFileSync('openssl', [
      'req',
      '-new',
      '-newkey',
      'ec',
      '-pkeyopt',
      'ec_paramgen_curve:P-256',
      '-nodes',
      '-subj',
      '/CN=rsctf-admin-lifecycle-worker',
      '-keyout',
      key,
      '-out',
      csr,
    ], { stdio: 'ignore' });
    return readFileSync(csr, 'utf8');
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

export function persistRecovery(path, state) {
  const temporary = `${path}.${process.pid}.tmp`;
  try {
    writeFileSync(temporary, `${JSON.stringify(state, null, 2)}\n`, {
      mode: 0o600,
      flag: 'w',
    });
    renameSync(temporary, path);
    chmodSync(path, 0o600);
  } finally {
    rmSync(temporary, { force: true });
  }
}

export function removeRecovery(path) {
  rmSync(path, { force: true });
}
