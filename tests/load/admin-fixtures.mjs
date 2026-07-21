// Focused helpers for the destructive, disposable admin lifecycle harness.
// Runtime orchestration stays in admin-lifecycle.mjs; this module owns exact
// HTTP/SQL fixture mechanics so route assertions remain readable.
import { execFileSync, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
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

const RUNTIME_IDENTITY_FIELDS = Object.freeze([
  'actualName',
  'containerId',
  'configuredImage',
  'imageId',
  'binarySha256',
  'startedAt',
  'restartCount',
  'running',
  'binaryMountShadowed',
]);

/**
 * Bind a replica acceptance run to one exact server container, image, and
 * executable per declared role. Container id + StartedAt catch replacement and
 * manual restart; RestartCount catches daemon-policy restarts; Docker's image id
 * and the live binary hash catch stale or shadowed deployments. The returned
 * value is safe to persist in an audit manifest.
 */
export function inspectUniformServerRuntimeIdentity(containers, dockerCommand = docker) {
  const names = [...new Set((containers || []).map((name) => String(name || '').trim()).filter(Boolean))];
  if (names.length === 0) throw new Error('at least one server container is required for runtime identity');

  const records = names.map((name) => {
    const inspected = dockerCommand(['inspect', name]);
    if (inspected.status !== 0) {
      throw new Error(`cannot inspect server runtime identity ${name}: ${String(inspected.stderr || '').trim()}`);
    }
    let models;
    try {
      models = JSON.parse(inspected.stdout);
    } catch (error) {
      throw new Error(`cannot parse server runtime identity ${name}: ${error.message}`);
    }
    if (!Array.isArray(models) || models.length !== 1) {
      throw new Error(`server runtime identity ${name} inspection is ambiguous`);
    }
    const model = models[0];
    const actualName = String(model?.Name || '').replace(/^\//, '').trim();
    const containerId = String(model?.Id || '').trim();
    const configuredImage = String(model?.Config?.Image || '').trim();
    const imageId = String(model?.Image || '').trim();
    const startedAt = String(model?.State?.StartedAt || '').trim();
    const restartCount = Number(model?.RestartCount);
    const running = model?.State?.Running === true;
    if (!actualName) throw new Error(`server ${name} has no Docker container name`);
    if (!/^[a-f0-9]{64}$/.test(containerId)) {
      throw new Error(`server ${name} has invalid Docker container identity ${containerId || '<missing>'}`);
    }
    if (!configuredImage) throw new Error(`server ${name} has no configured Docker image`);
    if (!/^sha256:[a-f0-9]{64}$/.test(imageId)) {
      throw new Error(`server ${name} has invalid Docker image identity ${imageId || '<missing>'}`);
    }
    if (!startedAt || Number.isNaN(Date.parse(startedAt))) {
      throw new Error(`server ${name} has invalid Docker StartedAt evidence ${startedAt || '<missing>'}`);
    }
    if (!Number.isSafeInteger(restartCount) || restartCount < 0) {
      throw new Error(`server ${name} has invalid Docker restart count ${model?.RestartCount ?? '<missing>'}`);
    }
    if (!running) throw new Error(`server ${name} is not running`);
    const binaryPath = '/usr/local/bin/rsctf';
    const shadowingMount = (model?.Mounts || []).find((mount) => {
      const destination = String(mount?.Destination || '').replace(/\/+$/, '') || '/';
      return destination === '/' || destination === binaryPath || binaryPath.startsWith(`${destination}/`);
    });
    if (shadowingMount) {
      throw new Error(
        `server ${name} shadows ${binaryPath} with ${shadowingMount.Type || 'a'} mount ` +
          `${shadowingMount.Source || '<unknown source>'}`,
      );
    }

    const hashed = dockerCommand(['exec', name, 'sha256sum', binaryPath]);
    if (hashed.status !== 0) {
      throw new Error(`cannot hash live rsctf binary in ${name}: ${String(hashed.stderr || '').trim()}`);
    }
    const match = String(hashed.stdout || '').trim().match(/^([a-f0-9]{64})\s+\/usr\/local\/bin\/rsctf$/);
    if (!match) throw new Error(`server ${name} returned an invalid live rsctf sha256sum`);
    return Object.freeze({
      name,
      actualName,
      containerId,
      configuredImage,
      imageId,
      binarySha256: match[1],
      startedAt,
      restartCount,
      running,
      binaryMountShadowed: false,
    });
  });

  const imageIds = new Set(records.map(({ imageId }) => imageId));
  const binaryHashes = new Set(records.map(({ binarySha256 }) => binarySha256));
  if (imageIds.size !== 1) {
    throw new Error(`declared server roles use different Docker image ids: ${[...imageIds].join(', ')}`);
  }
  if (binaryHashes.size !== 1) {
    throw new Error(`declared server roles use different live rsctf binaries: ${[...binaryHashes].join(', ')}`);
  }
  return Object.freeze({
    imageId: records[0].imageId,
    binarySha256: records[0].binarySha256,
    containers: Object.freeze(records),
  });
}

/** Fail when any declared server role changed between two acceptance snapshots. */
export function assertServerRuntimeIdentityUnchanged(before, after) {
  if (!before || !after || !Array.isArray(before.containers) || !Array.isArray(after.containers)) {
    throw new Error('server runtime identity comparison requires complete before and after snapshots');
  }
  const beforeNames = before.containers.map(({ name }) => name);
  const afterNames = after.containers.map(({ name }) => name);
  if (new Set(beforeNames).size !== beforeNames.length || new Set(afterNames).size !== afterNames.length) {
    throw new Error('server runtime identity contains duplicate declared roles');
  }
  if (beforeNames.length !== afterNames.length || beforeNames.some((name) => !afterNames.includes(name))) {
    throw new Error(
      `declared server roles changed during acceptance: before=${beforeNames.join(',')} after=${afterNames.join(',')}`,
    );
  }

  const endingByName = new Map(after.containers.map((record) => [record.name, record]));
  for (const starting of before.containers) {
    const ending = endingByName.get(starting.name);
    for (const field of RUNTIME_IDENTITY_FIELDS) {
      if (starting[field] !== ending[field]) {
        throw new Error(
          `server role ${starting.name} changed ${field} during acceptance ` +
            `(before=${JSON.stringify(starting[field])}, after=${JSON.stringify(ending[field])})`,
        );
      }
    }
  }
  if (before.imageId !== after.imageId || before.binarySha256 !== after.binarySha256) {
    throw new Error('uniform server image or binary identity changed during acceptance');
  }
  return after;
}

/**
 * Reinspect the named roles and require that they still resolve to the exact
 * starting containers. Call this on both sides of the ending fatal-log audit.
 */
export function inspectUnchangedServerRuntimeIdentity(
  before,
  containers,
  dockerCommand = docker,
) {
  const after = inspectUniformServerRuntimeIdentity(containers, dockerCommand);
  return assertServerRuntimeIdentityUnchanged(before, after);
}

/** Audit logs by immutable starting container id, never by a replaceable name. */
export function originalServerRuntimeLogTargets(identity) {
  if (!identity || !Array.isArray(identity.containers) || identity.containers.length === 0) {
    throw new Error('server runtime log targets require a complete starting identity');
  }
  return identity.containers.map(({ name, containerId }) => {
    if (!name || !/^[a-f0-9]{64}$/.test(String(containerId || ''))) {
      throw new Error('server runtime identity contains an invalid original log target');
    }
    return Object.freeze({ name, containerId });
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

const RETRYABLE_FETCH_TRANSPORT_CODES = new Set([
  'UND_ERR_SOCKET',
  'UND_ERR_CONNECT_TIMEOUT',
  'ECONNRESET',
  'ECONNREFUSED',
  'EPIPE',
  'ETIMEDOUT',
]);

export function retryableFetchTransportError(error) {
  const code = error?.cause?.code || error?.code;
  return error instanceof TypeError &&
    error.message === 'fetch failed' &&
    RETRYABLE_FETCH_TRANSPORT_CODES.has(code);
}

export async function fetchWithBoundedTransportRetry(request, {
  networkRetries = 0,
  retryDelayMs = 100,
} = {}) {
  if (!Number.isSafeInteger(networkRetries) || networkRetries < 0 || networkRetries > 1) {
    throw new Error(`networkRetries must be 0 or 1 (got ${networkRetries})`);
  }
  let attempt = 0;
  for (;;) {
    try {
      return { response: await request(), attempts: attempt + 1 };
    } catch (error) {
      if (attempt >= networkRetries || !retryableFetchTransportError(error)) throw error;
      attempt += 1;
      await new Promise((resolve) => setTimeout(resolve, retryDelayMs));
    }
  }
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
  const requestHeaders = { ...headers };
  if (jwt) requestHeaders.authorization = `Bearer ${jwt}`;
  if (ip) requestHeaders['x-real-ip'] = ip;
  const { response, attempts } = await fetchWithBoundedTransportRetry(
    () => fetch(`${baseUrl}${path}`, {
      method,
      headers: requestHeaders,
      body,
      signal: AbortSignal.timeout(timeoutMs),
    }),
    { networkRetries, retryDelayMs },
  );
  const bytes = new Uint8Array(await response.arrayBuffer());
  const text = new TextDecoder().decode(bytes);
  let json;
  try {
    json = text ? JSON.parse(text) : undefined;
  } catch {
    json = undefined;
  }
  return { status: response.status, headers: response.headers, bytes, text, json, attempts };
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
  timeoutMs = 120_000,
} = {}) {
  const form = new FormData();
  form.append('file', new Blob([content], { type: contentType }), filename);
  const response = await rawRequest('POST', path, { baseUrl, jwt, ip, body: form, timeoutMs });
  return expectStatus(response, expected, label);
}

/**
 * Build a trusted-import package whose two conventional src/Dockerfile trees
 * contain only `FROM scratch`. The application owns tag creation, reserved
 * labels, immutable identity publication, and ownership-ledger persistence.
 */
export function scratchChallengeArchive(entries) {
  if (!Array.isArray(entries) || entries.length === 0) {
    throw new Error('scratch challenge archive requires at least one entry');
  }
  const root = mkdtempSync(join(tmpdir(), 'rsctf-admin-scratch-'));
  const archive = `${root}.zip`;
  try {
    entries.forEach((entry, index) => {
      if (!entry || typeof entry.title !== 'string' || typeof entry.flag !== 'string') {
        throw new Error(`scratch challenge entry ${index} is invalid`);
      }
      const directory = join(root, `challenge-${index + 1}`);
      mkdirSync(join(directory, 'src'), { recursive: true });
      writeFileSync(join(directory, 'src', 'Dockerfile'), 'FROM scratch\n', { mode: 0o600 });
      writeFileSync(
        join(directory, 'challenge.yaml'),
        [
          `name: ${JSON.stringify(entry.title)}`,
          'author: "admin lifecycle"',
          'description: "Disposable owned-image acceptance fixture."',
          'type: StaticContainer',
          'category: Pwn',
          'minScoreRate: 0.25',
          'difficulty: 1',
          'flags:',
          `  - ${JSON.stringify(entry.flag)}`,
          'container:',
          '  memoryLimit: 64',
          '  cpuCount: 1',
          '  exposePort: 31337',
          '  enableTrafficCapture: false',
          '',
        ].join('\n'),
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
          `${positiveId(attempt, 'build attempt')},${Number(status)},NULL,NULL,` +
          `${sqlLiteral(logTail)}) RETURNING id` +
        `) SELECT id FROM inserted`,
    ),
    'build record id',
  );
}

export function repositoryCleanupRescheduleSql(gameId, bindingId, challengeId, tag) {
  const id = positiveId(gameId, 'repository cleanup game');
  const binding = positiveId(bindingId, 'repository cleanup binding');
  const protectedChallenge = positiveId(challengeId, 'repository cleanup protected challenge');
  const namespace = String(tag);
  if (!/^adm[a-z0-9]+$/.test(namespace)) {
    throw new Error(`invalid repository cleanup namespace ${namespace}`);
  }
  return `UPDATE "Games" game SET ` +
    `start_time_utc=clock_timestamp()+interval '1 day', ` +
    `end_time_utc=clock_timestamp()+interval '2 days' ` +
    `WHERE game.id=${id} AND game.title=${sqlLiteral(`LOADTEST-ADMIN-REPO-${namespace}`)} ` +
    `AND game.repo_binding_id IS NULL AND game.deletion_pending=FALSE ` +
    `AND EXISTS (SELECT 1 FROM "GameChallenges" challenge ` +
      `WHERE challenge.game_id=game.id AND challenge.id=${protectedChallenge} ` +
      `AND challenge.source_yaml_path LIKE ${sqlLiteral(`binding/${binding}/%`)}) ` +
    `AND NOT EXISTS (SELECT 1 FROM "GameChallenges" challenge ` +
      `WHERE challenge.game_id=game.id AND (` +
        `challenge.source_yaml_path IS NULL OR challenge.source_yaml_path NOT LIKE ` +
        `${sqlLiteral(`binding/${binding}/%`)})) RETURNING game.id`;
}

function disposableAdminGameIdentity(gameId, tag) {
  const id = positiveId(gameId, 'disposable admin game');
  const namespace = String(tag);
  if (!/^adm[a-z0-9]+$/.test(namespace)) {
    throw new Error(`invalid disposable admin namespace ${namespace}`);
  }
  return { id, title: `ADMIN-LIFECYCLE-${namespace}` };
}

function requireExactDisposableGame(identity, label) {
  const currentTitle = sql(`SELECT title FROM "Games" WHERE id=${identity.id}`);
  if (!currentTitle) return { ...identity, exists: false };
  if (currentTitle !== identity.title) {
    throw new Error(
      `game ${identity.id} is ${currentTitle}, not the expected ${label}`,
    );
  }
  return { ...identity, exists: true };
}

function requireDisposableAdminGame(gameId, tag) {
  return requireExactDisposableGame(
    disposableAdminGameIdentity(gameId, tag),
    'disposable admin fixture',
  );
}

function disposableGameRuntimeIds(id) {
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

/**
 * Snapshot every Docker runtime identity still reachable from the disposable
 * game's database graph. The caller takes this before evidence teardown so a
 * partially successful cleanup cannot erase the only durable runtime handle.
 */
export function disposableAdminGameRuntimeIds(gameId, tag) {
  const { id, exists } = requireDisposableAdminGame(gameId, tag);
  if (!exists) return [];
  return disposableGameRuntimeIds(id);
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
function exactDisposableGameCleanupSql({ id, title }) {
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

export function disposableAdminGameCleanupSql(gameId, tag) {
  return exactDisposableGameCleanupSql(disposableAdminGameIdentity(gameId, tag));
}

function deleteExactDisposableGame(identity, { runtimeIds = [] } = {}) {
  const { id, exists } = requireExactDisposableGame(identity, 'disposable load fixture');
  if (!exists) return false;
  const ownedRuntimeIds = disposableGameRuntimeIds(id);
  const safeRuntimeIds = Array.isArray(runtimeIds) ? runtimeIds : [];
  for (const containerId of new Set([...safeRuntimeIds, ...ownedRuntimeIds])) {
    if (containerId) assertDockerRuntimeAbsent(containerId);
  }
  assertCheckerDirectoryAbsent(id);
  sql(exactDisposableGameCleanupSql(identity));
  const residual = Number(sql(`SELECT count(*) FROM "Games" WHERE id=${id}`));
  if (residual !== 0) throw new Error(`disposable load fixture ${id} survived exact SQL cleanup`);
  return true;
}

export function deleteDisposableAdminGame(gameId, tag, { runtimeIds = [] } = {}) {
  return deleteExactDisposableGame(disposableAdminGameIdentity(gameId, tag), { runtimeIds });
}

export function deleteDisposableLoadGame(gameId, expectedTitle, { runtimeIds = [] } = {}) {
  const id = positiveId(gameId, 'disposable load game');
  const title = String(expectedTitle);
  if (!/^(?:LOADTEST-[A-Za-z0-9-]+|MULTI-DOMAIN-[a-z0-9][a-z0-9-]{0,31})$/.test(title)) {
    throw new Error(`invalid disposable load title ${title}`);
  }
  return deleteExactDisposableGame({ id, title }, { runtimeIds });
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

export function shouldRetainLifecycleManifest({ completed, cleanupVerified, keep }) {
  return !completed || !cleanupVerified || keep === '1';
}
