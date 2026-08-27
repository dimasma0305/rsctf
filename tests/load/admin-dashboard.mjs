// Disposable large-history fixture plus fixed-rate admin-dashboard reads.
import { randomBytes } from 'node:crypto'

import { LOAD_DATABASE_URL, PG_DATABASE, TARGET, mintJwt, runK6, sql } from './lib.mjs'

const origin = String(TARGET).replace(/\/+$/, '')
const databaseName = LOAD_DATABASE_URL ? new URL(LOAD_DATABASE_URL).pathname.slice(1) : PG_DATABASE
const rowCount = Number(process.env.SUBMISSION_ROWS || 100_000)
if (process.env.ADMIN_DASHBOARD_DISPOSABLE !== '1') {
  throw new Error('set ADMIN_DASHBOARD_DISPOSABLE=1 for the bounded dashboard load fixture')
}
if (!/(?:test|acceptance|load)/i.test(databaseName)) {
  throw new Error(`dashboard load database must contain test, acceptance, or load (got ${databaseName})`)
}
if (!/^https?:\/\/(?:127\.0\.0\.1|localhost|\[::1\])(?::\d+)?$/i.test(origin)) {
  if (process.env.ALLOW_REMOTE_ADMIN_DASHBOARD !== origin) {
    throw new Error(`remote dashboard load requires ALLOW_REMOTE_ADMIN_DASHBOARD=${origin}`)
  }
}
if (!Number.isSafeInteger(rowCount) || rowCount < 10_000 || rowCount > 2_000_000) {
  throw new Error('SUBMISSION_ROWS must be an integer from 10000 through 2000000')
}

async function assertHealth(stage) {
  const response = await fetch(`${origin}/healthz`)
  const body = await response.text()
  if (response.status !== 200 || body !== 'ok') {
    throw new Error(`${stage} health check failed: HTTP ${response.status}, body ${JSON.stringify(body)}`)
  }
}

const fixture = JSON.parse(
  sql(
    `SELECT json_build_object('participationId', p.id, 'teamId', p.team_id, ` +
      `'gameId', p.game_id, 'challengeId', c.id)::text ` +
      `FROM "Participations" p JOIN "GameChallenges" c ON c.game_id=p.game_id ` +
      `ORDER BY p.id, c.id LIMIT 1`
  ) || 'null'
)
if (!fixture) throw new Error('dashboard load requires one participation and challenge in the disposable database')

let adminToken = String(process.env.ADMIN_TOKEN || '')
if (!adminToken) {
  const admin = String(
    sql(`SELECT id::text || '|' || security_stamp FROM "AspNetUsers" WHERE role=3 ORDER BY id LIMIT 1`)
  )
  if (!admin) throw new Error('dashboard load requires an Admin account or ADMIN_TOKEN')
  const [id, stamp] = admin.split('|')
  adminToken = mintJwt(id, stamp, 3)
}

const tag = `dashboard-load-${randomBytes(8).toString('hex')}-`
const escapedTag = tag.replaceAll("'", "''")

// Submissions are immutable evidence in a real event. This disposable fixture
// deliberately bypasses user triggers in one transaction so bulk seed rows do
// not enqueue anti-cheat work, and so the exact tagged rows remain removable.
// `SET LOCAL` resets on commit, rollback, or connection close.
function fixtureSql(statement) {
  return sql(`BEGIN; SET LOCAL session_replication_role = replica; ${statement}; COMMIT`)
}

let status = 1
try {
  await assertHealth('pre-load')
  fixtureSql(
    `INSERT INTO "Submissions" ` +
      `(answer, status, submit_time_utc, user_id, team_id, participation_id, game_id, challenge_id) ` +
      // FlagSubmitted rows exercise the same time-bounded aggregate without
      // looking like a brute-force burst to the background cheat reconciler.
      `SELECT '${escapedTag}' || g, 0, ` +
      `clock_timestamp() - (g % 8760) * interval '1 hour', NULL, ` +
      `${fixture.teamId}, ${fixture.participationId}, ${fixture.gameId}, ${fixture.challengeId} ` +
      `FROM generate_series(1, ${rowCount}) g`
  )
  const seeded = Number(sql(`SELECT count(*) FROM "Submissions" WHERE answer LIKE '${escapedTag}%'`))
  if (seeded !== rowCount) throw new Error(`seeded ${seeded} of ${rowCount} dashboard submissions`)

  status = runK6('admin-dashboard.js', {
    TARGET: origin,
    ADMIN_TOKEN: adminToken,
    RATE: process.env.RATE || 1,
    VUS: process.env.VUS || 4,
    DURATION: process.env.DURATION || '30s',
    MAX_P95_MS: process.env.MAX_P95_MS || 1000,
    SUMMARY_JSON: process.env.SUMMARY_JSON || '',
  })
  await assertHealth('post-load')
} finally {
  fixtureSql(`DELETE FROM "Submissions" WHERE answer LIKE '${escapedTag}%'`)
  const residual = Number(sql(`SELECT count(*) FROM "Submissions" WHERE answer LIKE '${escapedTag}%'`))
  if (residual !== 0) throw new Error(`dashboard load cleanup left ${residual} submissions`)
}

process.exit(status)
