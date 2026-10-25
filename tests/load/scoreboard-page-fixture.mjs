// Explicitly disposable PostgreSQL seed for the scoreboard-page acceptance run.
import { generateKeyPairSync } from 'node:crypto';
import { sql, PG } from './lib.mjs';

const teams = Number(process.env.TEAMS || 500);
if (process.env.SCOREBOARD_DISPOSABLE_SEED !== '1' || !Number.isInteger(teams) || teams < 100 || teams > 1000) {
  throw new Error('SCOREBOARD_DISPOSABLE_SEED=1 and TEAMS=100..1000 are required');
}
if (PG === 'rsctf-db-1' && !process.env.RSCTF_LOAD_DATABASE_URL) {
  throw new Error('refusing the default production database container; select a disposable PG_CONTAINER');
}
if (sql('SELECT COUNT(*) FROM "Games"') !== '0') {
  throw new Error('seed requires an empty, migrated disposable database');
}
const keys = generateKeyPairSync('ed25519');
const publicKey = keys.publicKey.export({ type: 'spki', format: 'der' }).subarray(-32);
const seed = keys.privateKey.export({ type: 'pkcs8', format: 'der' }).subarray(-32);
const privateKey = seed; // RSCTF stores the 32-byte Ed25519 signing seed.
let game;
try { game = sql(`BEGIN;
  SET LOCAL rsctf.identity_neutral_insert = '1';
  INSERT INTO "AspNetUsers" (id, user_name, normalized_user_name, email_confirmed, security_stamp,
    phone_number_confirmed, two_factor_enabled, lockout_enabled, access_failed_count,
    role, ip, last_signed_in_utc, last_visited_utc, register_time_utc, bio, real_name, std_number, exercise_visible)
  SELECT gen_random_uuid(), 'PLATFORM_BENCH_' || lpad(i::text, 5, '0'),
    'PLATFORM_BENCH_' || lpad(i::text, 5, '0'), TRUE, 'benchmark-stamp',
    FALSE, FALSE, FALSE, 0, 1, '', now(), now(), now(), '', '', '', TRUE
  FROM generate_series(1, ${teams}) i;
  INSERT INTO "AspNetUsers" (id, user_name, normalized_user_name, email_confirmed, security_stamp,
    phone_number_confirmed, two_factor_enabled, lockout_enabled, access_failed_count,
    role, ip, last_signed_in_utc, last_visited_utc, register_time_utc, bio, real_name, std_number, exercise_visible)
  VALUES (gen_random_uuid(), 'PLATFORM_BENCH_ADMIN', 'PLATFORM_BENCH_ADMIN', TRUE, 'benchmark-stamp',
    FALSE, FALSE, FALSE, 0, 3, '', now(), now(), now(), '', '', '', TRUE);
  INSERT INTO "Games" (title, public_key, private_key, hidden, practice_mode, summary, content,
    accept_without_review, allow_user_submissions, writeup_required, team_member_count_limit,
    container_count_limit, start_time_utc, end_time_utc, writeup_deadline, writeup_note,
    blood_bonus_value, ad_allow_snapshot_download, ad_scoring_paused)
  VALUES ('PLATFORM_BENCH_SCOREBOARD', '${publicKey.toString('base64')}', '${privateKey.toString('base64')}',
    FALSE, FALSE, '', '', TRUE, FALSE, FALSE, 1, 1, now() - interval '6 hours',
    now() + interval '24 hours', now() + interval '25 hours', '', 0, FALSE, FALSE);
  INSERT INTO "Teams" (name, locked, invite_token, captain_id)
  SELECT user_name, FALSE, 'benchmark-' || user_name, id FROM "AspNetUsers" WHERE role = 1;
  INSERT INTO "TeamMembers" (team_id, user_id) SELECT id, captain_id FROM "Teams";
  INSERT INTO "Divisions" (game_id, name, default_permissions)
  SELECT id, 'Open', 2147483647 FROM "Games"
  UNION ALL SELECT id, 'Student', 2147483647 FROM "Games";
  INSERT INTO "Participations" (status, token, game_id, team_id, division_id, suspicion_score)
  SELECT 1, 'benchmark-participation', g.id, t.id,
    (SELECT id FROM "Divisions" WHERE game_id = g.id AND name = CASE WHEN t.id % 2 = 0 THEN 'Open' ELSE 'Student' END),
    0 FROM "Teams" t CROSS JOIN "Games" g;
  INSERT INTO "UserParticipations" (user_id, game_id, team_id, participation_id)
  SELECT t.captain_id, p.game_id, p.team_id, p.id FROM "Participations" p JOIN "Teams" t ON t.id = p.team_id;
  INSERT INTO "GameChallenges" (game_id, title, content, category, "Type", is_enabled, submission_limit,
    accepted_count, submission_count, review_status, build_status, enable_traffic_capture,
    enable_shared_container, original_score, min_score_rate, difficulty, score_curve,
    ad_allow_egress, ad_allow_self_reset, ad_ssh_requires_flag, ad_self_hosted)
  SELECT g.id, 'Challenge ' || lpad(i::text, 2, '0'), '', i % 5, 0, TRUE, 0, ${teams}, ${teams},
    0, 4, FALSE, FALSE, 1000, 0.1, 500, 0, FALSE, FALSE, FALSE, FALSE
  FROM "Games" g CROSS JOIN generate_series(1, 20) i;
  INSERT INTO "Submissions" (answer, status, submit_time_utc, user_id, team_id, participation_id, game_id, challenge_id)
  SELECT 'flag{benchmark}', 1, g.start_time_utc + (p.id * 20 + c.id) * interval '1 second',
    t.captain_id, t.id, p.id, g.id, c.id FROM "Participations" p JOIN "Games" g ON g.id = p.game_id
    JOIN "Teams" t ON t.id = p.team_id JOIN "GameChallenges" c ON c.game_id = g.id;
  INSERT INTO "FirstSolves" (participation_id, challenge_id, submission_id)
  SELECT participation_id, challenge_id, id FROM "Submissions";
  INSERT INTO "IdentityObservations" (user_id, game_id, team_id, participation_id,
    kind, value_hash, value_hint, source, observed_at_utc)
  SELECT u.user_id, u.game_id, u.team_id, u.participation_id, 'Fingerprint',
    sha256(convert_to(u.user_id::text, 'UTF8')), 'disposable-benchmark', 'GameJoin', now()
  FROM "UserParticipations" u;
  COMMIT;
  SELECT id FROM "Games" WHERE title = 'PLATFORM_BENCH_SCOREBOARD';`); }
catch (error) {
  // execFileSync's default exception includes the full seed SQL and its keys.
  throw new Error(`disposable seed failed: ${String(error.stderr || 'PostgreSQL failure').trim()}`);
}
console.log(JSON.stringify({ game: Number(game), teams, challenges: 20, solves: teams * 20 }));
