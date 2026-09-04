// Fixed-rate PostgreSQL micro-harness for the Jeopardy scoreboard's canonical
// first-solve read. The fixture lives in an isolated schema in a disposable
// PostgreSQL container and is removed even when a phase fails.
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawn, spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';

const PG_CONTAINER = process.env.SCOREBOARD_BENCH_PG || 'rsctf-repo-test-pg';
const PG_USER = process.env.SCOREBOARD_BENCH_USER || 'postgres';
const PG_DATABASE = process.env.SCOREBOARD_BENCH_DATABASE || 'postgres';
const RATE = positiveInt(process.env.RATE || 5, 'RATE', 1, 1000);
const DURATION = positiveInt(process.env.DURATION || 30, 'DURATION', 5, 600);
const WARMUP = positiveInt(process.env.WARMUP || 5, 'WARMUP', 1, 60);
const CLIENTS = positiveInt(process.env.VUS || 8, 'VUS', 1, 256);
const TEAMS = positiveInt(process.env.TEAMS || 100, 'TEAMS', 2, 1000);
const CHALLENGES = positiveInt(process.env.CHALLENGES || 20, 'CHALLENGES', 1, 100);
const ACCEPTED_PER_SOLVE = positiveInt(
  process.env.ACCEPTED_PER_SOLVE || 20,
  'ACCEPTED_PER_SOLVE',
  1,
  1000,
);
const SUMMARY_JSON = String(process.env.SUMMARY_JSON || '').trim();

if (PG_CONTAINER === 'rsctf-db-1' && process.env.ALLOW_PRODUCTION_DATABASE !== 'I_ACCEPT') {
  throw new Error(
    'refusing to benchmark against rsctf-db-1; use the disposable default or set ' +
      'ALLOW_PRODUCTION_DATABASE=I_ACCEPT explicitly',
  );
}

function positiveInt(value, name, min, max) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < min || parsed > max) {
    throw new Error(`${name} must be an integer from ${min} through ${max} (got ${value})`);
  }
  return parsed;
}

function docker(args, options = {}) {
  const result = spawnSync('docker', args, { encoding: 'utf8', ...options });
  if (result.status !== 0) {
    throw new Error(
      `docker ${args.join(' ')} failed: ${(result.stderr || result.stdout || '').trim()}`,
    );
  }
  return result.stdout.trim();
}

function psql(sql) {
  docker(
    ['exec', '-i', PG_CONTAINER, 'psql', '-X', '-v', 'ON_ERROR_STOP=1', '-q', '-U', PG_USER, '-d', PG_DATABASE],
    { input: sql },
  );
}

function percentile(sorted, fraction) {
  if (sorted.length === 0) return 0;
  const index = Math.min(sorted.length - 1, Math.ceil(sorted.length * fraction) - 1);
  return sorted[index];
}

function summarize(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const total = sorted.reduce((sum, value) => sum + value, 0);
  return {
    samples: sorted.length,
    average: sorted.length === 0 ? 0 : total / sorted.length,
    p50: percentile(sorted, 0.5),
    p95: percentile(sorted, 0.95),
    p99: percentile(sorted, 0.99),
    max: sorted.at(-1) || 0,
  };
}

function memoryBytes(value) {
  const match = String(value).trim().match(/^([0-9.]+)([kmgt]?i?b)$/i);
  if (!match) return 0;
  const powers = { b: 0, kb: 1, kib: 1, mb: 2, mib: 2, gb: 3, gib: 3, tb: 4, tib: 4 };
  return Number(match[1]) * 1024 ** (powers[match[2].toLowerCase()] ?? 0);
}

function startResourceSampler() {
  const samples = [];
  const errors = [];
  let stopping = false;
  const done = (async () => {
    while (!stopping) {
      try {
        const sampled = await runCommand('docker', [
          'stats',
          '--no-stream',
          '--format',
          '{{json .}}',
          PG_CONTAINER,
        ]);
        if (sampled.status !== 0) {
          errors.push((sampled.stderr || sampled.stdout || 'docker stats failed').trim());
          continue;
        }
        const cleaned = sampled.stdout
          .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '')
          .trim();
        const row = JSON.parse(cleaned);
        samples.push({
          at: Date.now(),
          cpu: Number(String(row.CPUPerc || '').replace('%', '')),
          memoryBytes: memoryBytes(String(row.MemUsage || '').split('/')[0]),
        });
      } catch (error) {
        errors.push(`invalid docker stats row: ${error.message}`);
      }
    }
  })();
  return {
    samples,
    errors,
    async stop() {
      stopping = true;
      await done;
    },
  };
}

async function runCommand(command, args) {
  return await new Promise((resolveCommand, rejectCommand) => {
    const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => (stdout += chunk));
    child.stderr.on('data', (chunk) => (stderr += chunk));
    child.once('error', rejectCommand);
    child.once('close', (status) => resolveCommand({ status, stdout, stderr }));
  });
}

function parseLatencyLogs(paths) {
  const latencies = [];
  for (const path of paths) {
    for (const line of readFileSync(path, 'utf8').split('\n')) {
      if (!line.trim()) continue;
      const fields = line.trim().split(/\s+/);
      const latencyUs = Number(fields[2]);
      if (!Number.isFinite(latencyUs) || latencyUs < 0) {
        throw new Error(`invalid pgbench latency row: ${line.slice(0, 200)}`);
      }
      latencies.push(latencyUs / 1000);
    }
  }
  return latencies;
}

async function phase(name, scriptPath, containerScript, scratch) {
  const common = [
    'exec',
    PG_CONTAINER,
    'pgbench',
    '-n',
    '-M',
    'prepared',
    '-c',
    String(CLIENTS),
    '-j',
    String(Math.min(CLIENTS, 4)),
    '-R',
    String(RATE),
    '--random-seed=20260802',
    '-U',
    PG_USER,
    '-d',
    PG_DATABASE,
    '-f',
    containerScript,
  ];

  const warmup = await runCommand('docker', [...common, '-T', String(WARMUP)]);
  if (warmup.status !== 0) {
    throw new Error(`${name} warmup failed: ${(warmup.stderr || warmup.stdout).trim()}`);
  }

  const logPrefix = `/tmp/rsctf-scoreboard-${name}-${process.pid}-${Date.now()}`;
  const sampler = startResourceSampler();
  const startedAt = Date.now();
  const measured = await runCommand('docker', [
    ...common,
    '-T',
    String(DURATION),
    '-l',
    `--log-prefix=${logPrefix}`,
  ]);
  const endedAt = Date.now();
  await sampler.stop();
  if (measured.status !== 0) {
    throw new Error(`${name} phase failed: ${(measured.stderr || measured.stdout).trim()}`);
  }
  if (sampler.errors.filter(Boolean).length > 0) {
    throw new Error(`${name} resource sampler failed: ${sampler.errors.join('; ')}`);
  }

  const containerLogs = docker([
    'exec',
    PG_CONTAINER,
    'find',
    '/tmp',
    '-maxdepth',
    '1',
    '-type',
    'f',
    '-name',
    `${basename(logPrefix)}.*`,
    '-print',
  ])
    .split('\n')
    .filter(Boolean);
  if (containerLogs.length === 0) throw new Error(`${name} pgbench produced no latency log`);
  const localLogs = [];
  for (const source of containerLogs) {
    const destination = join(scratch, `${name}-${basename(source)}`);
    docker(['cp', `${PG_CONTAINER}:${source}`, destination]);
    localLogs.push(destination);
  }
  docker(['exec', PG_CONTAINER, 'rm', '-f', ...containerLogs]);

  const latency = summarize(parseLatencyLogs(localLogs));
  const resourceWindow = sampler.samples
    .filter((sample, index) => sample.at >= startedAt && sample.at <= endedAt && index > 0);
  if (resourceWindow.length < Math.max(3, Math.floor(DURATION / 3))) {
    throw new Error(
      `${name} resource window is incomplete (${resourceWindow.length} samples for ${DURATION}s)`,
    );
  }
  const cpu = summarize(resourceWindow.map((sample) => sample.cpu));
  const memoryPeakBytes = Math.max(...resourceWindow.map((sample) => sample.memoryBytes));
  const scheduled = RATE * DURATION;
  if (latency.samples < scheduled * 0.7 || latency.samples > scheduled * 1.3 + CLIENTS) {
    throw new Error(
      `${name} processed ${latency.samples} transactions; expected approximately ${scheduled}`,
    );
  }

  return {
    name,
    targetRate: RATE,
    durationSeconds: DURATION,
    transactions: latency.samples,
    achievedRate: latency.samples / DURATION,
    latencyMs: latency,
    postgresCpuPercent: cpu,
    postgresMemoryPeakMiB: memoryPeakBytes / 1024 / 1024,
    pgbench: `${measured.stdout}\n${measured.stderr}`.trim(),
    scriptPath,
  };
}

function fixed(value, digits = 3) {
  return Number(value).toFixed(digits);
}

const runId = `${process.pid}_${Date.now()}`;
const schema = `rsctf_scoreboard_bench_${runId}`;
const scratch = mkdtempSync(join(tmpdir(), 'rsctf-scoreboard-evidence-'));
const containerScripts = [];
const quote = (identifier) => `"${identifier}"`;
const qSchema = quote(schema);
const beforeSql = `SELECT submission.participation_id, submission.challenge_id,
       submission.submit_time_utc, submission.user_id
  FROM ${qSchema}."Submissions" submission
  JOIN ${qSchema}."GameChallenges" challenge
    ON challenge.id = submission.challenge_id
   AND challenge.game_id = submission.game_id
   AND challenge.is_enabled
   AND challenge.review_status = 0
 WHERE submission.game_id = 1 AND submission.status = 1;
`;
const afterSql = `SELECT submission.participation_id, submission.challenge_id,
       submission.submit_time_utc, submission.user_id
  FROM ${qSchema}."FirstSolves" first_solve
  JOIN ${qSchema}."Submissions" submission
    ON submission.id = first_solve.submission_id
   AND submission.participation_id = first_solve.participation_id
   AND submission.challenge_id = first_solve.challenge_id
  JOIN ${qSchema}."Games" game
    ON game.id = submission.game_id
   AND submission.submit_time_utc >= game.start_time_utc
   AND submission.submit_time_utc < game.end_time_utc
  JOIN ${qSchema}."GameChallenges" challenge
    ON challenge.id = submission.challenge_id
   AND challenge.game_id = submission.game_id
   AND challenge.is_enabled
   AND challenge.review_status = 0
 WHERE submission.game_id = 1 AND submission.status = 1;
`;

let result;
try {
  docker(['inspect', PG_CONTAINER]);
  psql(`
    CREATE SCHEMA ${qSchema};
    CREATE UNLOGGED TABLE ${qSchema}."Games" (
      id integer PRIMARY KEY, start_time_utc timestamptz NOT NULL,
      end_time_utc timestamptz NOT NULL
    );
    CREATE UNLOGGED TABLE ${qSchema}."GameChallenges" (
      id integer PRIMARY KEY, game_id integer NOT NULL,
      is_enabled boolean NOT NULL, review_status smallint NOT NULL
    );
    CREATE UNLOGGED TABLE ${qSchema}."Submissions" (
      id bigint PRIMARY KEY, participation_id integer NOT NULL,
      challenge_id integer NOT NULL, game_id integer NOT NULL,
      status smallint NOT NULL, submit_time_utc timestamptz NOT NULL,
      user_id uuid
    );
    CREATE UNLOGGED TABLE ${qSchema}."FirstSolves" (
      participation_id integer NOT NULL, challenge_id integer NOT NULL,
      submission_id bigint NOT NULL,
      PRIMARY KEY (participation_id, challenge_id)
    );
    CREATE INDEX ix_submissions_game_status
      ON ${qSchema}."Submissions" (game_id, status);
    CREATE INDEX ix_submissions_part_challenge
      ON ${qSchema}."Submissions" (participation_id, challenge_id);
    CREATE UNIQUE INDEX ux_submissions_id_part_challenge
      ON ${qSchema}."Submissions" (id, participation_id, challenge_id);
    CREATE INDEX ix_firstsolves_challenge
      ON ${qSchema}."FirstSolves" (challenge_id, participation_id);
    INSERT INTO ${qSchema}."Games" (id, start_time_utc, end_time_utc)
      VALUES (1, timestamptz '2026-01-01 00:00:00+00', timestamptz '2026-01-02 00:00:00+00');
    INSERT INTO ${qSchema}."GameChallenges" (id, game_id, is_enabled, review_status)
      SELECT challenge_id, 1, TRUE, 0
        FROM generate_series(1, ${CHALLENGES}) AS challenge_id;
    INSERT INTO ${qSchema}."Submissions"
      (id, participation_id, challenge_id, game_id, status, submit_time_utc, user_id)
      SELECT ((team_id - 1) * ${CHALLENGES} + challenge_id - 1) * ${ACCEPTED_PER_SOLVE} + attempt,
             team_id, challenge_id, 1, 1,
             timestamptz '2026-01-01 00:00:00+00' +
               make_interval(secs => attempt), NULL
        FROM generate_series(1, ${TEAMS}) AS team_id
       CROSS JOIN generate_series(1, ${CHALLENGES}) AS challenge_id
       CROSS JOIN generate_series(1, ${ACCEPTED_PER_SOLVE}) AS attempt;
    INSERT INTO ${qSchema}."FirstSolves" (participation_id, challenge_id, submission_id)
      SELECT team_id, challenge_id,
             ((team_id - 1) * ${CHALLENGES} + challenge_id - 1) * ${ACCEPTED_PER_SOLVE} + 1
        FROM generate_series(1, ${TEAMS}) AS team_id
       CROSS JOIN generate_series(1, ${CHALLENGES}) AS challenge_id;
    ANALYZE ${qSchema}."Games";
    ANALYZE ${qSchema}."GameChallenges";
    ANALYZE ${qSchema}."Submissions";
    ANALYZE ${qSchema}."FirstSolves";
  `);

  const scripts = [
    ['before', beforeSql],
    ['after', afterSql],
  ];
  for (const [name, sql] of scripts) {
    const localPath = join(scratch, `${name}.sql`);
    const containerPath = `/tmp/rsctf-scoreboard-${name}-${runId}.sql`;
    writeFileSync(localPath, sql, { mode: 0o600 });
    docker(['cp', localPath, `${PG_CONTAINER}:${containerPath}`]);
    containerScripts.push(containerPath);
  }

  console.log(
    `scoreboard evidence benchmark → ${PG_CONTAINER} rate=${RATE}/s duration=${DURATION}s ` +
      `fixture=${TEAMS} teams × ${CHALLENGES} challenges × ${ACCEPTED_PER_SOLVE} accepted rows`,
  );
  const before = await phase('before', 'accepted-submission history', containerScripts[0], scratch);
  const after = await phase('after', 'canonical FirstSolves', containerScripts[1], scratch);
  if (before.transactions !== after.transactions) {
    throw new Error(
      `fixed schedule drifted between phases (${before.transactions} before, ${after.transactions} after)`,
    );
  }
  result = {
    generatedAt: new Date().toISOString(),
    fixture: {
      teams: TEAMS,
      challenges: CHALLENGES,
      acceptedRowsPerSolve: ACCEPTED_PER_SOLVE,
      acceptedSubmissionRows: TEAMS * CHALLENGES * ACCEPTED_PER_SOLVE,
      canonicalFirstSolveRows: TEAMS * CHALLENGES,
    },
    load: { targetTransactionsPerSecond: RATE, durationSeconds: DURATION, clients: CLIENTS },
    before,
    after,
  };

  console.log('\n| Phase | tx/s | p50 | p95 | p99 | PG CPU mean | PG CPU p95 | Peak RAM |');
  console.log('| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |');
  for (const phaseResult of [before, after]) {
    console.log(
      `| ${phaseResult.scriptPath} | ${fixed(phaseResult.achievedRate)} | ` +
        `${fixed(phaseResult.latencyMs.p50)} ms | ${fixed(phaseResult.latencyMs.p95)} ms | ` +
        `${fixed(phaseResult.latencyMs.p99)} ms | ` +
        `${fixed(phaseResult.postgresCpuPercent.average)}% | ` +
        `${fixed(phaseResult.postgresCpuPercent.p95)}% | ` +
        `${fixed(phaseResult.postgresMemoryPeakMiB, 1)} MiB |`,
    );
  }
  const p95Change = ((after.latencyMs.p95 / before.latencyMs.p95) - 1) * 100;
  const cpuChange =
    ((after.postgresCpuPercent.average / before.postgresCpuPercent.average) - 1) * 100;
  console.log(`\np95 change: ${fixed(p95Change, 1)}%; PostgreSQL CPU change: ${fixed(cpuChange, 1)}%`);

  if (SUMMARY_JSON) {
    const destination = resolve(SUMMARY_JSON);
    writeFileSync(destination, `${JSON.stringify(result, null, 2)}\n`, { mode: 0o600 });
    console.log(`summary: ${destination}`);
  }
} finally {
  for (const path of containerScripts) {
    try {
      docker(['exec', PG_CONTAINER, 'rm', '-f', path]);
    } catch {
      // Best-effort cleanup continues with the isolated schema below.
    }
  }
  try {
    psql(`DROP SCHEMA IF EXISTS ${qSchema} CASCADE;`);
  } catch (error) {
    console.error(`warning: benchmark schema cleanup failed: ${error.message}`);
  }
  rmSync(scratch, { recursive: true, force: true });
}
