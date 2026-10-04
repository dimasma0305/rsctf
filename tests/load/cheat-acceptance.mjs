// Small, destructive-by-design anti-cheat acceptance for a fresh CI database.
//
// This is intentionally separate from cheat-event.mjs: it needs no retained
// 100-team lifecycle event, k6, or container runtime. The explicit isolation
// gates below prevent it from ever targeting the shared development/production
// stack.
import { randomUUID } from "node:crypto";

import * as A from "./applib.mjs";
import { cohortSeedQuery, parseCohortSeedResult } from "./cohort-seed.js";
import {
  CHEAT_SCENARIO_RULES,
  SUSPICION_RULE_BY_KIND,
  assertCanonicalRuleProfile,
  computeExpectedBreakdown,
  validateDetectorCapabilities,
} from "./cheat-contract.js";
import {
  LOAD_DATABASE_URL,
  TARGET,
  mintJwt,
  sleep,
  sql,
} from "./lib.mjs";

const COHORT_SIZE = 19;
const ORIGIN = process.env.ORIGIN || TARGET;
const WAIT_MS = 20_000;
const ADMIN_ID = "00000000-0000-4000-8000-0000000000ac";
const JOINER_ID = "00000000-0000-4000-8000-0000000000ae";
const NAT_IP = "198.18.0.42";
const REVIEWABLE_SHARED_IP = "203.0.120.42";
const HONEYPOT_BAITS = ["/.env", "/.git/config", "/wp-login.php"];
const X_REAL_IP_SPOOF = "203.0.113.254";
const TELEMETRY_ONLY_KINDS = new Set([12, 13, 14, 21, 22, 28, 29, 31]);

function literal(value) {
  return `'${String(value).replaceAll("'", "''")}'`;
}

function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`invalid ${label}: ${value}`);
  }
  return parsed;
}

function jsonQuery(query, label) {
  const raw = sql(query);
  try {
    return JSON.parse(raw || "[]");
  } catch (error) {
    throw new Error(`${label} returned malformed JSON: ${error.message}`);
  }
}

function unwrap(response) {
  return response?.json && Object.hasOwn(response.json, "data")
    ? response.json.data
    : response?.json;
}

function isolationGate() {
  if (process.env.CHEAT_ACCEPTANCE_ISOLATED !== "1") {
    throw new Error("CHEAT_ACCEPTANCE_ISOLATED=1 is required");
  }
  if (process.env.RSCTF_SUSPICION_FINALIZE_GRACE_SECONDS !== "1") {
    throw new Error("isolated acceptance requires a one-second suspicion finalization grace");
  }
  if (!LOAD_DATABASE_URL) {
    throw new Error("RSCTF_LOAD_DATABASE_URL is required for isolated acceptance");
  }
  const database = new URL(LOAD_DATABASE_URL);
  const target = new URL(TARGET);
  if (!new Set(["127.0.0.1", "localhost", "::1"]).has(database.hostname)) {
    throw new Error("isolated acceptance requires a loopback PostgreSQL host");
  }
  if (!/(?:test|acceptance)/i.test(database.pathname)) {
    throw new Error("isolated acceptance requires a test/acceptance database name");
  }
  if (!new Set(["127.0.0.1", "localhost", "::1"]).has(target.hostname)) {
    throw new Error("isolated acceptance requires a loopback HTTP target");
  }
  if (Number(sql(`SELECT count(*) FROM "Games"`)) !== 0) {
    throw new Error("isolated acceptance requires a fresh database with no games");
  }
}

function ensureAdmin() {
  sql(
    `WITH neutral_provisioning AS MATERIALIZED (` +
      `SELECT set_config('rsctf.identity_neutral_insert','1',true)` +
      `) INSERT INTO "AspNetUsers" ` +
      `(id,user_name,normalized_user_name,email,normalized_email,email_confirmed,password_hash,` +
      `security_stamp,concurrency_stamp,role,register_time_utc,last_signed_in_utc,last_visited_utc,` +
      `lockout_enabled,access_failed_count,phone_number_confirmed,two_factor_enabled,ip,bio,real_name,std_number,exercise_visible) ` +
      `SELECT ` +
      `${literal(ADMIN_ID)}::uuid,'cheat-acceptance-admin','CHEAT-ACCEPTANCE-ADMIN',` +
      `'cheat-acceptance-admin@load.test','CHEAT-ACCEPTANCE-ADMIN@LOAD.TEST',true,` +
      `'x-load-placeholder',gen_random_uuid()::text,gen_random_uuid()::text,3,` +
      `now(),now(),now(),true,0,false,false,'127.0.0.1','','','',false ` +
      `FROM neutral_provisioning ` +
      `ON CONFLICT (id) DO NOTHING`,
  );
}

function assertNeutralProvisioningGuard() {
  const unmarkedId = "00000000-0000-4000-8000-0000000000ad";
  let rejected = false;
  try {
    sql(
      `INSERT INTO "AspNetUsers" ` +
        `(id,user_name,normalized_user_name,email,normalized_email,email_confirmed,password_hash,` +
        `security_stamp,concurrency_stamp,role,register_time_utc,last_signed_in_utc,last_visited_utc,` +
        `lockout_enabled,access_failed_count,phone_number_confirmed,two_factor_enabled,ip,bio,real_name,std_number,exercise_visible) ` +
        `VALUES (` +
        `${literal(unmarkedId)}::uuid,'unmarked-acceptance-user','UNMARKED-ACCEPTANCE-USER',` +
        `'unmarked-acceptance-user@load.test','UNMARKED-ACCEPTANCE-USER@LOAD.TEST',true,` +
        `'x-load-placeholder',gen_random_uuid()::text,gen_random_uuid()::text,1,` +
        `now(),now(),now(),true,0,false,false,'0.0.0.0','','','',false)`,
    );
  } catch (error) {
    const diagnostic = `${error?.stderr || ""}\n${error?.message || ""}`;
    if (!diagnostic.includes("account insert lacks same-transaction identity adjudication")) {
      throw new Error(`unmarked account insert failed for the wrong reason: ${diagnostic.trim()}`);
    }
    rejected = true;
  }
  if (
    !rejected ||
    Number(
      sql(`SELECT count(*) FROM "AspNetUsers" WHERE id=${literal(unmarkedId)}::uuid`),
    ) !== 0
  ) {
    throw new Error("identity transition guard accepted an unmarked fixture account insert");
  }
}

function provisionNeutralJoiner() {
  const userId = sql(
    `WITH neutral_provisioning AS MATERIALIZED (` +
      `SELECT set_config('rsctf.identity_neutral_insert','1',true)` +
      `), inserted AS (` +
      `INSERT INTO "AspNetUsers" ` +
      `(id,user_name,normalized_user_name,email,normalized_email,email_confirmed,password_hash,` +
      `security_stamp,concurrency_stamp,role,register_time_utc,last_signed_in_utc,last_visited_utc,` +
      `lockout_enabled,access_failed_count,phone_number_confirmed,two_factor_enabled,ip,bio,real_name,std_number,exercise_visible) ` +
      `SELECT ${literal(JOINER_ID)}::uuid,'cheat-acceptance-joiner','CHEAT-ACCEPTANCE-JOINER',` +
      `'cheat-acceptance-joiner@load.test','CHEAT-ACCEPTANCE-JOINER@LOAD.TEST',true,` +
      `'x-load-placeholder',gen_random_uuid()::text,gen_random_uuid()::text,1,` +
      `now(),now(),now(),true,0,false,false,'0.0.0.0','','','',false ` +
      `FROM neutral_provisioning RETURNING id` +
      `) SELECT id FROM inserted`,
  );
  if (userId !== JOINER_ID) {
    throw new Error("identity-neutral joiner provisioning returned the wrong account");
  }
  const stamp = sql(
    `SELECT security_stamp FROM "AspNetUsers" WHERE id=${literal(userId)}::uuid`,
  );
  return { userId, jwt: mintJwt(userId, stamp, 1) };
}

async function exerciseIdentityAwareTeamAccept(joiner, teamId) {
  const team = jsonQuery(
    `SELECT json_build_object('id',id,'name',name,'inviteToken',invite_token)::text ` +
      `FROM "Teams" WHERE id=${positiveInteger(teamId, "identity-accept team id")}`,
    "identity-accept team",
  );
  const challengeResponse = await A.api("GET", "/api/account/fingerprintchallenge", {
    ip: X_REAL_IP_SPOOF,
    headers: { Origin: ORIGIN, "X-Forwarded-For": "198.51.200.20" },
  });
  const challenge = unwrap(challengeResponse);
  const requiredSignals = [
    "lie_count",
    "headless_rating",
    "platform_consistent",
    "ua_consistent",
    "webgl_consistent",
  ];
  if (
    challengeResponse.status !== 200 ||
    typeof challenge?.nonce !== "string" ||
    !Array.isArray(challenge?.requiredSignals) ||
    challenge.requiredSignals.length !== requiredSignals.length ||
    challenge.requiredSignals.some((signal, index) => signal !== requiredSignals[index]) ||
    Number(challenge?.expiresInSeconds) !== 120
  ) {
    throw new Error("fingerprint challenge did not expose the exact fresh-proof contract");
  }
  const fingerprint = "a5".repeat(32);
  const signals = {
    lie_count: "0",
    headless_rating: "0",
    platform_consistent: "1",
    ua_consistent: "1",
    webgl_consistent: "1",
  };
  const proof = JSON.stringify({
    version: 1,
    fingerprint,
    nonce: challenge.nonce,
    signalOrder: challenge.requiredSignals,
    signals,
  });
  const body = {
    code: `${team.name}:${team.id}:${team.inviteToken}`,
    fingerprint,
    fingerprintProof: proof,
  };
  sql(
    `INSERT INTO "Configs"(config_key,value,cache_keys) ` +
      `VALUES ('AccountPolicy:EnableBrowserFingerprint','true',NULL) ` +
      `ON CONFLICT (config_key) DO UPDATE SET value=EXCLUDED.value`,
  );
  let response;
  let replay;
  try {
    response = await A.api("POST", "/api/team/accept", {
      jwt: joiner.jwt,
      ip: X_REAL_IP_SPOOF,
      headers: { Origin: ORIGIN, "X-Forwarded-For": "198.51.200.20" },
      body,
    });
    if (response.status !== 200) {
      throw new Error(`identity-aware team accept failed: ${response.status} ${response.text}`);
    }
    replay = await A.api("POST", "/api/team/accept", {
      jwt: joiner.jwt,
      ip: X_REAL_IP_SPOOF,
      headers: { Origin: ORIGIN, "X-Forwarded-For": "198.51.200.20" },
      body,
    });
  } finally {
    sql(`DELETE FROM "Configs" WHERE config_key='AccountPolicy:EnableBrowserFingerprint'`);
  }
  if (replay.status !== 400 || !/expired|reused/i.test(replay.text)) {
    throw new Error("identity-aware team accept reused a consumed fingerprint challenge");
  }
  const retained = jsonQuery(
    `SELECT json_build_object(` +
      `'members',(SELECT count(*) FROM "TeamMembers" ` +
      `WHERE team_id=${team.id} AND user_id=${literal(joiner.userId)}::uuid),` +
      `'ipRows',(SELECT count(*) FROM "IdentityObservations" ` +
      `WHERE user_id=${literal(joiner.userId)}::uuid AND game_id IS NULL ` +
      `AND kind='Ip' AND source='TeamJoin'),` +
      `'ipHint',(SELECT min(value_hint) FROM "IdentityObservations" ` +
      `WHERE user_id=${literal(joiner.userId)}::uuid AND game_id IS NULL ` +
      `AND kind='Ip' AND source='TeamJoin'),` +
      `'fingerprintRows',(SELECT count(*) FROM "IdentityObservations" ` +
      `WHERE user_id=${literal(joiner.userId)}::uuid AND game_id IS NULL ` +
      `AND kind='Fingerprint' AND source='TeamJoin'),` +
      `'fingerprintHashes',(SELECT count(DISTINCT value_hash) FROM "IdentityObservations" ` +
      `WHERE user_id=${literal(joiner.userId)}::uuid AND game_id IS NULL ` +
      `AND kind='Fingerprint' AND source='TeamJoin' AND octet_length(value_hash)=32),` +
      `'rawFingerprint',(SELECT browser_fingerprint FROM "AspNetUsers" ` +
      `WHERE id=${literal(joiner.userId)}::uuid),` +
      `'consumedChallenges',(SELECT count(*) FROM "FingerprintChallenges" ` +
      `WHERE consumed_at_utc IS NOT NULL)` +
      `)::text`,
    "identity-aware team accept retention",
  );
  if (
    Number(retained.members) !== 1 ||
    Number(retained.ipRows) !== 1 ||
    retained.ipHint !== "198.51.200.x" ||
    Number(retained.fingerprintRows) !== 1 ||
    Number(retained.fingerprintHashes) !== 1 ||
    retained.rawFingerprint !== null ||
    Number(retained.consumedChallenges) !== 1
  ) {
    throw new Error("identity-aware team accept did not retain its exact proof/admission result");
  }
}

function configuredRules() {
  return jsonQuery(
    `SELECT COALESCE(json_agg(json_build_object(` +
      `'ruleCode',rule_code,'weight',weight) ORDER BY id),'[]'::json)::text ` +
      `FROM "SuspicionRules"`,
    "suspicion-rule profile",
  );
}

function evidence(gameId) {
  return jsonQuery(
    `SELECT COALESCE(json_agg(json_build_object(` +
      `'id',id,'participationId',participation_id,'challengeId',challenge_id,` +
      `'kind',kind,'evidenceKey',evidence_key,'scoreDelta',score_delta,` +
      `'createdAtMs',floor(extract(epoch from created_at)*1000)::bigint,` +
      `'createdAtMicros',floor(extract(epoch from created_at)*1000000)::bigint) ORDER BY id),'[]'::json)::text ` +
      `FROM "SuspicionEvents" WHERE game_id=${positiveInteger(gameId, "game id")}`,
    "suspicion evidence",
  );
}

function honeypotTelemetryState(gameId, subject, afterId) {
  const id = positiveInteger(gameId, "honeypot game id");
  const floor = Number(afterId);
  if (!Number.isSafeInteger(floor) || floor < 0) {
    throw new Error(`invalid honeypot hit floor: ${afterId}`);
  }
  return jsonQuery(
    `SELECT json_build_object(` +
      `'hits',(SELECT COALESCE(json_agg(json_build_array(` +
      `hit.id,hit.user_id,hit.game_id,hit.participation_id,hit.bait,` +
      `hit.remote_ip,hit.user_agent,hit.hit_at_utc) ORDER BY hit.id),'[]'::json) ` +
      `FROM "HoneypotHits" hit WHERE hit.id>${floor} ` +
      `AND hit.user_agent=${literal(subject.honeypotUserAgent)}),` +
      `'outboxJobs',(SELECT count(*) FROM "SuspicionEvaluationOutbox" job WHERE ` +
      `job.rule_kind IN (28,29,31) OR EXISTS (` +
      `SELECT 1 FROM "HoneypotHits" hit WHERE job.source_kind=1 ` +
      `AND job.source_id=hit.id AND hit.id>${floor} ` +
      `AND hit.user_agent=${literal(subject.honeypotUserAgent)})),` +
      `'suspicionEvents',(SELECT count(*) FROM "SuspicionEvents" ` +
      `WHERE kind IN (28,29,31)),` +
      `'storedScore',(SELECT suspicion_score FROM "Participations" ` +
      `WHERE id=${positiveInteger(subject.participationId, "honeypot participation id")} ` +
      `AND game_id=${id})` +
      `)::text`,
    "honeypot raw telemetry state",
  );
}

function assertHoneypotTelemetry(gameId, subject, afterId) {
  const state = honeypotTelemetryState(gameId, subject, afterId);
  const hits = Array.isArray(state.hits) ? state.hits : [];
  const actualBaits = hits.map((row) => row[4]).sort();
  const exactAttribution = hits.every(
    (row) =>
      String(row[1]).toLowerCase() === subject.userId.toLowerCase() &&
      row[2] === null &&
      row[3] === null,
  );
  if (
    hits.length !== HONEYPOT_BAITS.length ||
    !sameMembers(actualBaits, HONEYPOT_BAITS) ||
    !exactAttribution ||
    Number(state.outboxJobs) !== 0 ||
    Number(state.suspicionEvents) !== 0 ||
    state.storedScore === null ||
    Number(state.storedScore) !== 0
  ) {
    throw new Error(
      `honeypot telemetry diverged: ${JSON.stringify({
        hitCount: hits.length,
        actualBaits,
        exactAttribution,
        outboxJobs: state.outboxJobs,
        suspicionEvents: state.suspicionEvents,
        storedScore: state.storedScore,
      })}`,
    );
  }
  return state;
}

function codesFor(rows, participationId) {
  return rows
    .filter((row) => Number(row.participationId) === Number(participationId))
    .map((row) => {
      const rule = SUSPICION_RULE_BY_KIND.get(Number(row.kind));
      if (!rule) throw new Error(`unsupported persisted suspicion kind ${row.kind}`);
      if (Number(row.scoreDelta) !== rule.defaultWeight) {
        throw new Error(
          `${rule.code} persisted ${row.scoreDelta}; canonical acceptance expects ${rule.defaultWeight}`,
        );
      }
      return rule.code;
    })
    .sort();
}

function sameMembers(actual, expected) {
  const left = [...actual].sort();
  const right = [...expected].sort();
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function containsMembers(actual, required) {
  const remaining = new Map();
  for (const value of actual) remaining.set(value, (remaining.get(value) ?? 0) + 1);
  for (const value of required) {
    const count = remaining.get(value) ?? 0;
    if (count === 0) return false;
    remaining.set(value, count - 1);
  }
  return true;
}

function scenarioCodesFor(rows, participationId, role, challengeId) {
  const actorRows = rows.filter(
    (row) => Number(row.participationId) === Number(participationId),
  );
  const codes = codesFor(actorRows, participationId);
  for (const row of actorRows) {
    const code = SUSPICION_RULE_BY_KIND.get(Number(row.kind))?.code;
    if (!code) throw new Error(`unsupported scenario evidence kind ${row.kind}`);
    if (role === "stolen") {
      if (Number(row.challengeId) !== challengeId || !/^submission:\d+$/.test(row.evidenceKey)) {
        throw new Error(`StolenFlag evidence for ${participationId} has the wrong source identity`);
      }
    } else if (role === "brute") {
      if (Number(row.challengeId) !== challengeId || row.evidenceKey !== `challenge:${challengeId}`) {
        throw new Error(`${code} evidence for ${participationId} has the wrong challenge identity`);
      }
    } else {
      throw new Error(`unsupported acceptance scenario role ${role}`);
    }
  }
  const identities = actorRows.map((row) => `${row.kind}|${row.evidenceKey}`);
  if (new Set(identities).size !== identities.length) {
    throw new Error(`${role} evidence for ${participationId} duplicated an incident identity`);
  }
  return codes;
}

async function waitFor(label, probe, timeoutMs = WAIT_MS) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  do {
    try {
      const value = await probe();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await sleep(100);
  } while (Date.now() < deadline);
  throw new Error(`${label} did not converge within ${timeoutMs} ms${lastError ? `: ${lastError.message}` : ""}`);
}

function actor(cohort, index, ip) {
  const userId = cohort.userIds[index];
  const stamp = sql(
    `SELECT security_stamp FROM "AspNetUsers" WHERE id=${literal(userId)}::uuid`,
  );
  if (!stamp) throw new Error(`missing security stamp for cohort actor ${index}`);
  return {
    userId,
    teamId: cohort.teamIds[index],
    participationId: cohort.partIds[index],
    jwt: mintJwt(userId, stamp, 1),
    ip,
  };
}

function playerOptions(subject, body) {
  return {
    jwt: subject.jwt,
    ip: subject.ip,
    body,
    headers: {
      Origin: ORIGIN,
      "X-Forwarded-For": subject.ip,
      ...(subject.honeypotUserAgent
        ? { "User-Agent": subject.honeypotUserAgent }
        : {}),
    },
  };
}

async function submitFlag(gameId, challengeId, subject, flag, label) {
  const response = await A.api(
    "POST",
    `/api/game/${gameId}/challenges/${challengeId}`,
    playerOptions(subject, { flag, attemptId: randomUUID() }),
  );
  if (response.status !== 200) {
    throw new Error(`${label} submission failed: ${response.status} ${response.text}`);
  }
  const submissionId = Number(unwrap(response));
  if (!Number.isSafeInteger(submissionId) || submissionId <= 0) {
    throw new Error(`${label} did not return a submission id`);
  }
  return { submissionId, response };
}

async function assertSubmissionOutcome(
  gameId,
  challengeId,
  subject,
  submissionId,
  expected,
  label,
) {
  const visible = await A.api(
    "GET",
    `/api/game/${gameId}/challenges/${challengeId}/status/${submissionId}`,
    playerOptions(subject),
  );
  if (visible.status !== 200 || unwrap(visible) !== expected) {
    throw new Error(
      `${label} player outcome is ${visible.status}/${JSON.stringify(unwrap(visible))}; ` +
        `expected ${expected}`,
    );
  }
  const expectedStatus = expected === "Accepted" ? 1 : 2;
  if (Number(sql(`SELECT status FROM "Submissions" WHERE id=${submissionId}`)) !== expectedStatus) {
    throw new Error(`${label} did not retain ${expected} in the submission ledger`);
  }
  if (expected === "Accepted") {
    const firstSolve = Number(
      sql(
        `SELECT count(*) FROM "FirstSolves" WHERE participation_id=${subject.participationId} ` +
          `AND challenge_id=${challengeId} AND submission_id=${submissionId}`,
      ),
    );
    if (firstSolve !== 1) throw new Error(`${label} did not establish its canonical FirstSolve`);
  } else if (
    Number(
      sql(
        `SELECT count(*) FROM "FirstSolves" WHERE participation_id=${subject.participationId} ` +
          `AND challenge_id=${challengeId} AND submission_id=${submissionId}`,
      ),
    ) !== 0
  ) {
    throw new Error(`${label} incorrectly established a FirstSolve for ${expected}`);
  }
}

async function createChallenge(gameId, type, title, flag, extra = {}) {
  const challengeId = await A.createChallenge(gameId, {
    title,
    category: "Misc",
    type,
  });
  await A.setChallenge(gameId, challengeId, {
    content: "isolated anti-cheat acceptance fixture",
    originalScore: 1000,
    minScoreRate: 0.25,
    difficulty: 5,
    submissionLimit: 0,
    ...extra,
  });
  if (flag) await A.addFlags(gameId, challengeId, [flag]);
  await A.setChallenge(gameId, challengeId, { isEnabled: true });
  return challengeId;
}

function seedDynamicInstances(challengeId, cohort, stamp) {
  const desired = cohort.partIds
    .map((participationId) =>
      `(${positiveInteger(participationId, "participation id")},` +
        `${literal(`flag{cheat_acceptance_${stamp}_${participationId}}`)})`,
    )
    .join(",");
  sql(
    `WITH desired(participation_id,flag) AS (VALUES ${desired}) ` +
      `INSERT INTO "FlagContexts"(flag,is_occupied,challenge_id) ` +
      `SELECT flag,true,${challengeId} FROM desired`,
  );
  sql(
    `WITH desired(participation_id,flag) AS (VALUES ${desired}) ` +
      `INSERT INTO "GameInstances"` +
      `(challenge_id,participation_id,is_loaded,last_container_operation,flag_id,container_id) ` +
      `SELECT ${challengeId},desired.participation_id,false,now(),context.id,NULL ` +
      `FROM desired JOIN "FlagContexts" context ` +
      `ON context.challenge_id=${challengeId} AND context.flag=desired.flag`,
  );
  return new Map(
    jsonQuery(
      `SELECT COALESCE(json_agg(json_build_object(` +
        `'participationId',instance.participation_id,'flag',context.flag) ` +
        `ORDER BY instance.participation_id),'[]'::json)::text ` +
        `FROM "GameInstances" instance JOIN "FlagContexts" context ON context.id=instance.flag_id ` +
        `WHERE instance.challenge_id=${challengeId}`,
      "dynamic flags",
    ).map((row) => [Number(row.participationId), row.flag]),
  );
}

function seedWrongAttempts(gameId, challengeId, subject, count, timestampsSql, prefix) {
  sql(
    `INSERT INTO "Submissions"` +
      `(answer,status,submit_time_utc,user_id,team_id,participation_id,game_id,challenge_id) ` +
      `SELECT ${literal(prefix)}||series.n||'}',2,series.at,` +
      `${literal(subject.userId)}::uuid,${subject.teamId},${subject.participationId},` +
      `${gameId},${challengeId} FROM (${timestampsSql}) series(n,at) ` +
      `ORDER BY series.n`,
  );
  const stored = Number(
    sql(
      `SELECT count(*) FROM "Submissions" WHERE game_id=${gameId} ` +
        `AND participation_id=${subject.participationId} AND challenge_id=${challengeId} ` +
        `AND answer LIKE ${literal(`${prefix}%`)}`,
    ),
  );
  if (stored !== count) throw new Error(`seeded ${stored}/${count} ${prefix} wrong attempts`);
}

async function exerciseSharedIpLogins(
  gameId,
  cohort,
  indices,
  ip,
  label,
  proveXRealIpIgnored = false,
) {
  let sharedObservationFloor = Number(
    sql(`SELECT COALESCE(max(id),0) FROM "IdentityObservations"`),
  );
  const actors = [];
  for (const index of indices) {
    const userId = cohort.userIds[index];
    const reset = await A.api(
      "DELETE",
      `/api/admin/users/${encodeURIComponent(userId)}/password?operationId=${randomUUID()}`,
      { jwt: A.adminJwt(), ip: "192.0.2.44" },
    );
    const password = unwrap(reset);
    if (reset.status !== 200 || typeof password !== "string" || password.length < 8) {
      throw new Error(`${label} password reset failed: ${reset.status} ${reset.text}`);
    }
    const userName = sql(
      `SELECT user_name FROM "AspNetUsers" WHERE id=${literal(userId)}::uuid`,
    );
    if (proveXRealIpIgnored && actors.length === 0) {
      const spoofFloor = Number(
        sql(`SELECT COALESCE(max(id),0) FROM "IdentityObservations"`),
      );
      const spoof = await A.api("POST", "/api/account/login", {
        ip: X_REAL_IP_SPOOF,
        body: { userName, password },
        headers: { Origin: ORIGIN },
      });
      if (spoof.status !== 200) {
        throw new Error(`${label} X-Real-IP negative login failed: ${spoof.status} ${spoof.text}`);
      }
      const spoofShape = jsonQuery(
        `SELECT json_build_object(` +
          `'rows',count(*),'hint',min(value_hint),'maxHint',max(value_hint)` +
          `)::text FROM "IdentityObservations" WHERE id>${spoofFloor} ` +
          `AND game_id=${gameId} AND user_id=${literal(userId)}::uuid ` +
          `AND kind='Ip' AND source='Password'`,
        `${label} X-Real-IP negative observation`,
      );
      if (
        Number(spoofShape.rows) !== 1 ||
        spoofShape.hint !== "127.0.0.x" ||
        spoofShape.maxHint !== spoofShape.hint
      ) {
        throw new Error(
          `${label} accepted X-Real-IP as identity instead of the trusted loopback peer`,
        );
      }
      sharedObservationFloor = Number(
        sql(`SELECT COALESCE(max(id),0) FROM "IdentityObservations"`),
      );
    }
    const login = await A.api("POST", "/api/account/login", {
      ip,
      body: { userName, password },
      headers: { Origin: ORIGIN, "X-Forwarded-For": ip },
    });
    if (login.status !== 200) {
      throw new Error(`${label} login for cohort actor ${index} failed: ${login.status} ${login.text}`);
    }
    actors.push(actor(cohort, index, ip));
  }

  const ids = actors.map((subject) => `${literal(subject.userId)}::uuid`).join(",");
  const shape = jsonQuery(
    `SELECT json_build_object(` +
      `'rows',count(*),'users',count(DISTINCT user_id),` +
      `'teams',count(DISTINCT team_id),'participations',count(DISTINCT participation_id),` +
      `'exactHashes',count(DISTINCT value_hash),` +
      `'subnetHashes',count(DISTINCT subnet_group_hash),` +
      `'broadHashes',count(DISTINCT broad_network_hash),` +
      `'exactHash',min(encode(value_hash,'hex')),` +
      `'subnetHash',min(encode(subnet_group_hash,'hex')),` +
      `'hint',min(value_hint),'maxHint',max(value_hint)` +
      `)::text FROM "IdentityObservations" WHERE id>${sharedObservationFloor} ` +
      `AND game_id=${gameId} AND kind='Ip' AND source='Password' AND user_id IN (${ids})`,
    `${label} identity observations`,
  );
  for (const key of ["rows", "users", "teams", "participations"]) {
    if (Number(shape[key]) !== actors.length) {
      throw new Error(`${label} retained ${shape[key]}/${actors.length} distinct ${key}`);
    }
  }
  for (const key of ["exactHashes", "subnetHashes", "broadHashes"]) {
    if (Number(shape[key]) !== 1) {
      throw new Error(`${label} retained ${shape[key]} ${key}; expected exactly one`);
    }
  }
  if (
    !/^[0-9a-f]{64}$/.test(String(shape.exactHash)) ||
    !/^[0-9a-f]{64}$/.test(String(shape.subnetHash)) ||
    shape.hint !== shape.maxHint ||
    shape.hint === ip
  ) {
    throw new Error(`${label} did not retain one consistently hashed, redacted network identity`);
  }
  return {
    actors,
    exactEvidenceKey: `cross-team-ip:${shape.exactHash}`,
    subnetEvidenceKey: `subnet-overlap:${shape.subnetHash}`,
    hint: shape.hint,
    ip,
  };
}

async function exerciseDistinctIpLogins(gameId, cohort, identities) {
  const actors = [];
  for (const { index, ip, label } of identities) {
    const observed = await exerciseSharedIpLogins(
      gameId,
      cohort,
      [index],
      ip,
      `${label} live identity`,
    );
    actors.push(observed.actors[0]);
  }
  return actors;
}

async function resetLoginCredentials(subject, label) {
  const reset = await A.api(
    "DELETE",
    `/api/admin/users/${encodeURIComponent(subject.userId)}/password?operationId=${randomUUID()}`,
    { jwt: A.adminJwt(), ip: "192.0.2.44" },
  );
  const password = unwrap(reset);
  if (reset.status !== 200 || typeof password !== "string" || password.length < 8) {
    throw new Error(`${label} password reset failed: ${reset.status} ${reset.text}`);
  }
  const userName = sql(
    `SELECT user_name FROM "AspNetUsers" WHERE id=${literal(subject.userId)}::uuid`,
  );
  return { userName, password };
}

async function startHeldSharedIpLogin(gameId, subject, ip, holdSeconds, credentials) {
  const gid = positiveInteger(gameId, "held-login game id");
  const seconds = positiveInteger(holdSeconds, "held-login seconds");
  sql(
    `CREATE OR REPLACE FUNCTION cheat_acceptance_hold_identity() ` +
      `RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN ` +
      `IF NEW.game_id=${gid} AND NEW.user_id=${literal(subject.userId)}::uuid THEN ` +
      `PERFORM pg_sleep(${seconds}); END IF; RETURN NEW; END $$; ` +
      `DROP TRIGGER IF EXISTS tr_cheat_acceptance_hold_identity ON "IdentityObservations"; ` +
      `CREATE TRIGGER tr_cheat_acceptance_hold_identity AFTER INSERT ` +
      `ON "IdentityObservations" FOR EACH ROW EXECUTE FUNCTION cheat_acceptance_hold_identity()`,
  );
  const login = A.api("POST", "/api/account/login", {
    ip: X_REAL_IP_SPOOF,
    body: credentials,
    headers: { Origin: ORIGIN, "X-Forwarded-For": ip },
  });
  void login.catch(() => {});
  try {
    await waitFor("production identity writer hold", () =>
      Number(
        sql(
          `SELECT count(*) FROM pg_stat_activity WHERE wait_event='PgSleep' ` +
            `AND datname=current_database() ` +
            `AND query LIKE 'WITH contexts AS MATERIALIZED (%'`,
        ),
      ) === 1,
    5_000);
  } catch (error) {
    await login.catch(() => undefined);
    sql(
      `DROP TRIGGER IF EXISTS tr_cheat_acceptance_hold_identity ON "IdentityObservations"; ` +
        `DROP FUNCTION IF EXISTS cheat_acceptance_hold_identity()`,
    );
    throw error;
  }
  return {
    credentials,
    async completion() {
      let response;
      try {
        response = await login;
      } finally {
        sql(
          `DROP TRIGGER IF EXISTS tr_cheat_acceptance_hold_identity ON "IdentityObservations"; ` +
            `DROP FUNCTION IF EXISTS cheat_acceptance_hold_identity()`,
        );
      }
      if (response.status !== 200) {
        throw new Error(`held shared-IP login failed: ${response.status} ${response.text}`);
      }
      return credentials;
    },
  };
}

async function assertPostEndLoginExcluded(gameId, subject, ip, credentials) {
  const gameRowsBefore = Number(
    sql(
      `SELECT count(*) FROM "IdentityObservations" WHERE game_id=${gameId} ` +
        `AND user_id=${literal(subject.userId)}::uuid AND kind='Ip' AND source='Password'`,
    ),
  );
  const globalRowsBefore = Number(
    sql(
      `SELECT count(*) FROM "IdentityObservations" WHERE game_id IS NULL ` +
        `AND user_id=${literal(subject.userId)}::uuid AND kind='Ip' AND source='Password'`,
    ),
  );
  const response = await A.api("POST", "/api/account/login", {
    ip: X_REAL_IP_SPOOF,
    body: credentials,
    headers: { Origin: ORIGIN, "X-Forwarded-For": ip },
  });
  if (response.status !== 200) {
    throw new Error(`post-end identity login failed: ${response.status} ${response.text}`);
  }
  const shape = jsonQuery(
    `SELECT json_build_object(` +
      `'gameRows',(SELECT count(*) FROM "IdentityObservations" ` +
      `WHERE game_id=${gameId} AND user_id=${literal(subject.userId)}::uuid ` +
      `AND kind='Ip' AND source='Password'),` +
      `'globalRows',(SELECT count(*) FROM "IdentityObservations" ` +
      `WHERE game_id IS NULL AND user_id=${literal(subject.userId)}::uuid ` +
      `AND kind='Ip' AND source='Password'),` +
      `'latestHint',(SELECT value_hint FROM "IdentityObservations" ` +
      `WHERE game_id IS NULL AND user_id=${literal(subject.userId)}::uuid ` +
      `AND kind='Ip' AND source='Password' ORDER BY id DESC LIMIT 1)` +
      `)::text`,
    "post-end identity exclusion",
  );
  if (
    Number(shape.gameRows) !== gameRowsBefore ||
    Number(shape.globalRows) !== globalRowsBefore + 1 ||
    shape.latestHint !== ip.replace(/\.\d+$/, ".x")
  ) {
    throw new Error("post-end login was not confined to the global identity ledger");
  }
}

async function exerciseDownload(
  gameId,
  challengeId,
  subject,
  hash,
  filename,
  flag,
  backdate,
  label,
) {
  const response = await fetch(`${TARGET}/assets/${hash}/${filename}`, {
    headers: {
      Authorization: `Bearer ${subject.jwt}`,
      Origin: ORIGIN,
      "X-Forwarded-For": subject.ip,
    },
  });
  if (response.status !== 200 || (await response.text()).length === 0) {
    throw new Error(`${label} journey failed: ${response.status}`);
  }
  await waitFor("attachment download audit", () =>
    Number(
      sql(
        `SELECT count(*) FROM "GameEvents" WHERE game_id=${gameId} ` +
          `AND team_id=${subject.teamId} AND "Type"=5 AND "values"->>0=${literal(String(challengeId))}`,
      ),
    ) === 1,
  );
  if (backdate) {
    sql(
      `UPDATE "GameEvents" SET publish_time_utc=clock_timestamp()-interval '3 minutes' ` +
        `WHERE game_id=${gameId} AND team_id=${subject.teamId} AND "Type"=5 ` +
        `AND "values"->>0=${literal(String(challengeId))}`,
    );
  }
  const result = await submitFlag(gameId, challengeId, subject, flag, label);
  await assertSubmissionOutcome(
    gameId,
    challengeId,
    subject,
    result.submissionId,
    "Accepted",
    label,
  );
  const snapshot = jsonQuery(
    `SELECT json_build_object(` +
      `'downloadDeltaMs',floor(extract(epoch from (` +
      `submit_time_utc-first_download_at_submit))*1000)::bigint,` +
      `'openIsNull',first_open_at_submit IS NULL,` +
      `'containerIsNull',first_container_start_at_submit IS NULL` +
      `)::text FROM "Submissions" WHERE id=${result.submissionId}`,
    `${label} immutable download snapshot`,
  );
  const downloadDeltaMs = Number(snapshot.downloadDeltaMs);
  if (
    snapshot.downloadDeltaMs === null ||
    !Number.isSafeInteger(downloadDeltaMs) ||
    snapshot.openIsNull !== true ||
    snapshot.containerIsNull !== true ||
    (backdate
      ? downloadDeltaMs < 120_000
      : downloadDeltaMs < 0 || downloadDeltaMs >= 120_000)
  ) {
    throw new Error(`${label} retained an invalid immutable download timing snapshot`);
  }
  return result;
}

async function exerciseContainer(
  gameId,
  challengeId,
  subject,
  title,
  flag,
  backdate,
  label,
) {
  const eventTime = backdate
    ? "clock_timestamp()-interval '3 minutes'"
    : "clock_timestamp()";
  sql(
    `INSERT INTO "GameEvents"(game_id,"Type","values",publish_time_utc,user_id,team_id) ` +
      `VALUES (${gameId},1,jsonb_build_array(${literal(String(challengeId))},${literal(title)}),` +
      `${eventTime},${literal(subject.userId)}::uuid,${subject.teamId})`,
  );
  sql(
    `INSERT INTO "GameInstances"` +
      `(challenge_id,participation_id,is_loaded,last_container_operation,flag_id,container_id) ` +
      `VALUES (${challengeId},${subject.participationId},true,${eventTime},NULL,NULL)`,
  );
  const result = await submitFlag(gameId, challengeId, subject, flag, label);
  await assertSubmissionOutcome(
    gameId,
    challengeId,
    subject,
    result.submissionId,
    "Accepted",
    label,
  );
  const snapshot = jsonQuery(
    `SELECT json_build_object(` +
      `'loaded',container_was_loaded_at_submit,` +
      `'instanceDeltaMs',floor(extract(epoch from (` +
      `submit_time_utc-container_last_operation_at_submit))*1000)::bigint` +
      `,'containerStartDeltaMs',floor(extract(epoch from (` +
      `submit_time_utc-first_container_start_at_submit))*1000)::bigint` +
      `,'openIsNull',first_open_at_submit IS NULL,` +
      `'downloadIsNull',first_download_at_submit IS NULL` +
      `)::text FROM "Submissions" WHERE id=${result.submissionId}`,
    `${label} immutable container snapshot`,
  );
  const instanceDeltaMs = Number(snapshot.instanceDeltaMs);
  const containerStartDeltaMs = Number(snapshot.containerStartDeltaMs);
  if (
    snapshot.loaded !== true ||
    snapshot.instanceDeltaMs === null ||
    snapshot.containerStartDeltaMs === null ||
    snapshot.openIsNull !== true ||
    snapshot.downloadIsNull !== true ||
    !Number.isSafeInteger(instanceDeltaMs) ||
    !Number.isSafeInteger(containerStartDeltaMs) ||
    (backdate
      ? instanceDeltaMs < 120_000 || containerStartDeltaMs < 120_000
      : instanceDeltaMs < 0 ||
        instanceDeltaMs >= 120_000 ||
        containerStartDeltaMs < 0 ||
        containerStartDeltaMs >= 120_000)
  ) {
    throw new Error(`${label} retained an invalid immutable container timing snapshot`);
  }
  return result;
}

function reconciliationAttempts(gameId) {
  return Number(
    sql(
      `SELECT COALESCE(attempts,0) FROM "SuspicionReconciliationState" ` +
        `WHERE game_id=${gameId}`,
    ) || 0,
  );
}

function reconciliationState(gameId) {
  return jsonQuery(
    `SELECT COALESCE((SELECT json_build_object(` +
      `'attempts',attempts,` +
      `'lastReconciledAtMicros',floor(extract(epoch from last_reconciled_at_utc)*1000000)::bigint,` +
      `'sealedAtMicros',floor(extract(epoch from sealed_at_utc)*1000000)::bigint,` +
      `'lastError',last_error,` +
      `'dbNowMicros',floor(extract(epoch from clock_timestamp())*1000000)::bigint` +
      `) FROM "SuspicionReconciliationState" WHERE game_id=${gameId}),` +
      `'null'::json)::text`,
    "suspicion reconciliation state",
  );
}

function outboxPending(gameId) {
  return Number(
    sql(
      `SELECT count(*) FROM "SuspicionEvaluationOutbox" ` +
        `WHERE game_id=${gameId} AND completed_at_utc IS NULL`,
    ),
  );
}

function assertNoCleanEvidence(rows, cleanParticipationIds) {
  const clean = new Set(cleanParticipationIds.map(Number));
  const unexpected = rows.filter((row) => clean.has(Number(row.participationId)));
  if (unexpected.length) {
    throw new Error(
      `benign acceptance actors received suspicion: ${unexpected
        .map((row) => `${row.participationId}:${SUSPICION_RULE_BY_KIND.get(Number(row.kind))?.code || row.kind}`)
        .join(", ")}`,
    );
  }
}

function assertNoTelemetryOnlyEvents(rows) {
  const unexpected = rows.filter((row) => TELEMETRY_ONLY_KINDS.has(Number(row.kind)));
  if (unexpected.length !== 0) {
    throw new Error(
      `telemetry-only kinds created suspicion events: ${unexpected
        .map((row) => `${row.kind}:${row.evidenceKey}`)
        .join(", ")}`,
    );
  }
}

function assertSharedContextEvidence(rows, group) {
  const expectedCodes = ["CrossTeamIP", "SubnetOverlap"].sort();
  const groupKeys = new Map([
    ["CrossTeamIP", group.exactEvidenceKey],
    ["SubnetOverlap", group.subnetEvidenceKey],
  ]);
  for (const subject of group.actors) {
    const actorRows = rows.filter(
      (row) => Number(row.participationId) === Number(subject.participationId),
    );
    if (!sameMembers(codesFor(actorRows, subject.participationId), expectedCodes)) {
      return false;
    }
    for (const row of actorRows) {
      const code = SUSPICION_RULE_BY_KIND.get(Number(row.kind))?.code;
      if (row.challengeId !== null || row.evidenceKey !== groupKeys.get(code)) {
        throw new Error(
          `${code} evidence for ${subject.participationId} has the wrong network identity`,
        );
      }
    }
  }
  return true;
}

function ledgerSnapshot(gameId) {
  return sql(
    `SELECT json_build_object(` +
      `'events',(SELECT COALESCE(json_agg(json_build_array(id,participation_id,kind,evidence_key,score_delta) ORDER BY id),'[]'::json) ` +
      `FROM "SuspicionEvents" WHERE game_id=${gameId}),` +
      `'scores',(SELECT COALESCE(json_agg(json_build_array(id,suspicion_score) ORDER BY id),'[]'::json) ` +
      `FROM "Participations" WHERE game_id=${gameId}),` +
      `'outbox',(SELECT COALESCE(json_agg(json_build_array(` +
      `id,completed_at_utc,attempts,last_error,lease_token,lease_expires_at_utc) ` +
      `ORDER BY id),'[]'::json) ` +
      `FROM "SuspicionEvaluationOutbox" WHERE game_id=${gameId}),` +
      `'reconciliation',(SELECT json_build_object(` +
      `'gameId',game_id,'lastReconciledAt',last_reconciled_at_utc,` +
      `'sealedAt',sealed_at_utc,'attempts',attempts,'lastError',last_error) ` +
      `FROM "SuspicionReconciliationState" WHERE game_id=${gameId}),` +
      `'sources',json_build_array(` +
      `(SELECT count(*) FROM "Submissions" WHERE game_id=${gameId}),` +
      `(SELECT count(*) FROM "HoneypotHits" WHERE game_id=${gameId}),` +
      `(SELECT count(*) FROM "IdentityObservations" WHERE game_id=${gameId}))` +
      `)::text`,
  );
}

function assertCompetitiveTimeFence(gameId) {
  const lateRows = Number(
    sql(
      `SELECT count(*) FROM (` +
        `SELECT submit_time_utc AS observed_at FROM "Submissions" WHERE game_id=${gameId} ` +
        `UNION ALL SELECT hit_at_utc FROM "HoneypotHits" WHERE game_id=${gameId} ` +
        `UNION ALL SELECT observed_at_utc FROM "IdentityObservations" WHERE game_id=${gameId} ` +
        `UNION ALL SELECT publish_time_utc FROM "GameEvents" WHERE game_id=${gameId} ` +
        `UNION ALL SELECT observed_at_utc FROM "SuspicionEvaluationOutbox" WHERE game_id=${gameId} ` +
        `UNION ALL SELECT created_at FROM "SuspicionEvents" WHERE game_id=${gameId}` +
        `) source CROSS JOIN "Games" game ` +
        `WHERE game.id=${gameId} AND source.observed_at>=game.end_time_utc`,
    ),
  );
  if (lateRows !== 0) {
    throw new Error(`${lateRows} competitive source/evidence rows crossed the final game boundary`);
  }
}

async function exerciseFinalizationGraceControl() {
  const fixtureNow = A.nowMs();
  const gameId = await A.createGame({
    title: `LOADTEST-CHEAT-FINALIZE-GRACE-${fixtureNow}`,
    hidden: false,
    practiceMode: false,
    acceptWithoutReview: true,
    start: fixtureNow + 10 * 60 * 1000,
    end: fixtureNow + 2 * 60 * 60 * 1000,
    teamMemberCountLimit: 0,
  });
  const cohort = parseCohortSeedResult(sql(cohortSeedQuery(gameId, 2)), 2);
  await A.setGameSchedule(
    gameId,
    A.nowMs() - 60 * 60 * 1000,
    A.nowMs() + 60 * 60 * 1000,
  );
  const group = await exerciseSharedIpLogins(
    gameId,
    cohort,
    [0, 1],
    "192.0.2.222",
    "finalization-grace shared network",
  );
  const closeout = jsonQuery(
    `WITH boundary AS MATERIALIZED (` +
      `SELECT clock_timestamp()+interval '500 milliseconds' AS ended_at` +
      `), closed AS (` +
      `UPDATE "Games" game SET end_time_utc=boundary.ended_at FROM boundary ` +
      `WHERE game.id=${gameId} AND game.start_time_utc<boundary.ended_at ` +
      `AND game.end_time_utc>boundary.ended_at ` +
      `AND boundary.ended_at>clock_timestamp()+interval '200 milliseconds' ` +
      `RETURNING game.end_time_utc` +
      `) SELECT json_build_object(` +
      `'rows',count(*),` +
      `'endAtMicros',floor(extract(epoch from min(end_time_utc))*1000000)::bigint` +
      `)::text FROM closed`,
    "finalization-grace closeout",
  );
  const endAtMicros = Number(closeout.endAtMicros);
  if (Number(closeout.rows) !== 1 || !Number.isSafeInteger(endAtMicros)) {
    throw new Error("finalization-grace control could not schedule a phase-aligned closeout");
  }
  const stateDuringGrace = await waitFor("phase-aligned finalization-grace window", () => {
    const control = reconciliationState(gameId);
    if (control?.lastError) {
      throw new Error(`finalization-grace control failed: ${control.lastError}`);
    }
    const sampledAtMicros = Number(control?.dbNowMicros);
    if (
      !Number.isSafeInteger(sampledAtMicros) ||
      sampledAtMicros <= endAtMicros
    ) {
      return false;
    }
    if (sampledAtMicros >= endAtMicros + 1_000_000) {
      throw new Error("missed the phase-aligned finalization-grace window");
    }
    return control;
  });
  if (stateDuringGrace?.sealedAtMicros !== null && stateDuringGrace?.sealedAtMicros !== undefined) {
    throw new Error("phase-aligned finalization control sealed before its one-second grace elapsed");
  }
  if (evidence(gameId).length !== 0) {
    throw new Error("final-only identity evidence appeared during phase-aligned finalization grace");
  }
  await waitFor("unblocked finalization grace", () => {
    const state = reconciliationState(gameId);
    if (state?.lastError) {
      throw new Error(`finalization-grace control failed: ${state.lastError}`);
    }
    if (
      !Number.isSafeInteger(Number(state?.sealedAtMicros)) ||
      Number(state.sealedAtMicros) < endAtMicros + 1_000_000
    ) {
      return false;
    }
    return assertSharedContextEvidence(evidence(gameId), group);
  });
  if (
    Number(
      sql(
        `SELECT count(*) FROM "Participations" WHERE game_id=${gameId} ` +
          `AND suspicion_score<>0`,
      ),
    ) !== 0
  ) {
    throw new Error("finalization-grace context control changed a suspicion score");
  }
}

function assertReport(
  report,
  gameId,
  offenders,
  weights,
  contextGroup,
  natGroup,
  stolenOwner,
  honeypot,
) {
  validateDetectorCapabilities(report?.detectorCapabilities);
  const identityOverlaps = report?.identityOverlaps;
  const ipAnalysis = report?.ipAnalysis;
  if (!Array.isArray(identityOverlaps) || !Array.isArray(ipAnalysis)) {
    throw new Error("cheat report omitted identity correlation sections");
  }
  const expectedTeams = jsonQuery(
    `SELECT COALESCE(json_agg(json_build_object('id',id,'name',name) ORDER BY id),'[]'::json)::text ` +
      `FROM "Teams" WHERE id IN (${contextGroup.actors.map((subject) => subject.teamId).join(",")})`,
    "reviewable shared-network teams",
  );
  const expectedTeamNames = expectedTeams.map((team) => team.name).sort();
  if (identityOverlaps.length !== 1) {
    throw new Error(
      `cheat report returned ${identityOverlaps.length} identity groups; expected only the four-team group`,
    );
  }
  const overlap = identityOverlaps[0];
  if (
    overlap.kind !== "ip" ||
    overlap.value !== contextGroup.hint ||
    Number(overlap.teamCount) !== contextGroup.actors.length ||
    !sameMembers(overlap.teamNames || [], expectedTeamNames) ||
    !Array.isArray(overlap.userNames) ||
    overlap.userNames.length !== 0
  ) {
    throw new Error("cheat report returned the wrong reviewable shared-network group");
  }
  if (ipAnalysis.length !== contextGroup.actors.length) {
    throw new Error(
      `cheat report returned ${ipAnalysis.length} IP rows; expected ${contextGroup.actors.length}`,
    );
  }
  const teamNameById = new Map(expectedTeams.map((team) => [Number(team.id), team.name]));
  for (const row of ipAnalysis) {
    const teamId = Number(row.teamId);
    const teamName = teamNameById.get(teamId);
    if (
      !teamName ||
      row.teamName !== teamName ||
      row.type !== "CrossTeamIP" ||
      row.ip !== contextGroup.hint ||
      !sameMembers(row.relatedTeams || [], expectedTeamNames.filter((name) => name !== teamName)) ||
      !Array.isArray(row.userNames) ||
      row.userNames.length !== 0 ||
      !Array.isArray(row.relatedUsers) ||
      row.relatedUsers.length !== 0
    ) {
      throw new Error(`cheat report returned invalid redacted IP analysis for team ${teamId}`);
    }
  }
  const identityJson = JSON.stringify({ identityOverlaps, ipAnalysis });
  const contextUserNames = contextGroup.actors.map((subject) =>
    sql(`SELECT user_name FROM "AspNetUsers" WHERE id=${literal(subject.userId)}::uuid`),
  );
  if (
    identityJson.includes(contextGroup.ip) ||
    identityJson.includes(natGroup.ip) ||
    identityJson.includes(natGroup.hint) ||
    contextUserNames.some((userName) => identityJson.includes(userName))
  ) {
    throw new Error("cheat report leaked a raw/suppressed network or player identity");
  }
  const rows = report?.suspicionList;
  if (!Array.isArray(rows)) throw new Error("cheat report omitted suspicionList");
  const reportByPid = new Map(rows.map((row) => [Number(row.participationId), row]));
  if (reportByPid.size !== rows.length) throw new Error("cheat report duplicated a participation");
  if (reportByPid.has(Number(honeypot.participationId))) {
    throw new Error("raw honeypot telemetry appeared as a scored report row");
  }

  const allEvidence = evidence(gameId);
  const abnormalSolves = report?.abnormalSolves;
  if (!Array.isArray(abnormalSolves) || abnormalSolves.length !== 0) {
    throw new Error("cheat report exposed a telemetry-only fast-solve as an abnormal solve");
  }

  const collusionGroups = report?.collusionGroups;
  if (!Array.isArray(collusionGroups) || collusionGroups.length !== 1) {
    throw new Error("cheat report must contain exactly one stolen-flag collusion pair");
  }
  const collusion = collusionGroups[0];
  const expectedPair = [stolenOwner.participationId, offenders.get("stolen")]
    .map((participationId) =>
      jsonQuery(
        `SELECT json_build_object(` +
          `'teamId',team.id,'teamName',team.name,'participationId',participation.id` +
          `)::text FROM "Participations" participation ` +
          `JOIN "Teams" team ON team.id=participation.team_id ` +
          `WHERE participation.game_id=${gameId} AND participation.id=${participationId}`,
        "stolen-flag collusion participant",
      ),
    )
    .map((row) => `${row.teamId}|${row.teamName}|${row.participationId}`)
    .sort();
  const actualPair = Array.isArray(collusion.teams)
    ? collusion.teams
      .map((row) => `${Number(row.id)}|${row.name}|${Number(row.participationId)}`)
      .sort()
    : [];
  if (
    !sameMembers(actualPair, expectedPair) ||
    Number(collusion.averageRsi) !== 0 ||
    !Array.isArray(collusion.commonSolves) ||
    collusion.commonSolves.length !== 0 ||
    !Array.isArray(collusion.detailedSolves) ||
    collusion.detailedSolves.length !== 0 ||
    typeof collusion.details !== "string" ||
    collusion.details.trim().length === 0
  ) {
    throw new Error("cheat report returned an invalid stolen-flag collusion projection");
  }

  const frozen = new Map([
    [offenders.get("stolen"), { hard: 100, strong: 0, behavioral: 0, corroboration: 0, total: 100, band: "evidenced" }],
    [offenders.get("brute"), { hard: 0, strong: 60, behavioral: 0, corroboration: 0, total: 60, band: "investigate" }],
  ]);
  for (const subject of contextGroup.actors) {
    frozen.set(subject.participationId, {
      hard: 0,
      strong: 0,
      behavioral: 0,
      corroboration: 0,
      total: 0,
      band: "context",
    });
  }
  if (rows.length !== frozen.size) {
    throw new Error(`cheat report returned ${rows.length} rows; expected exactly ${frozen.size}`);
  }
  for (const [participationId, exact] of frozen) {
    const participantEvidence = allEvidence.filter(
      (row) => Number(row.participationId) === Number(participationId),
    );
    const expected = computeExpectedBreakdown(participantEvidence, weights);
    for (const [key, value] of Object.entries(exact)) {
      if (expected[key] !== value) {
        throw new Error(`independent ${key} for ${participationId} is ${expected[key]}; expected ${value}`);
      }
    }
    const reportRow = reportByPid.get(Number(participationId));
    if (!reportRow) throw new Error(`cheat report omitted offender ${participationId}`);
    for (const [key, value] of Object.entries(exact)) {
      const reportKey = key === "total" ? "score" : key;
      if (reportRow[reportKey] !== value) {
        throw new Error(`report ${reportKey} for ${participationId} is ${reportRow[reportKey]}; expected ${value}`);
      }
    }
    const expectedEvents = expected.events
      .map((row) =>
        `${row.id}|${row.type}|${row.scoreDelta}|${row.appliedDelta}|` +
          `${row.tier}|${row.counted}|${row.time}`,
      )
      .sort();
    const actualEvents = (reportRow.events || [])
      .map((row) =>
        `${Number(row.eventId)}|${row.type}|${Number(row.scoreDelta)}|` +
          `${Number(row.appliedDelta)}|${row.tier}|${Boolean(row.counted)}|${Number(row.time)}`,
      )
      .sort();
    if (!sameMembers(actualEvents, expectedEvents)) {
      throw new Error(`report event scoring diverged for offender ${participationId}`);
    }
    const storedScore = Number(
      sql(`SELECT suspicion_score FROM "Participations" WHERE id=${participationId}`),
    );
    if (storedScore !== exact.total) {
      throw new Error(`stored score for ${participationId} is ${storedScore}; expected ${exact.total}`);
    }
  }
}

async function main() {
  isolationGate();
  assertNeutralProvisioningGuard();
  ensureAdmin();
  await A.preflight();
  const weights = assertCanonicalRuleProfile(configuredRules());
  await exerciseFinalizationGraceControl();
  const now = A.nowMs();
  const stagingStart = now + 10 * 60 * 1000;
  const stagingEnd = now + 2 * 60 * 60 * 1000;

  const gameId = await A.createGame({
    title: `LOADTEST-CHEAT-ACCEPTANCE-${now}`,
    hidden: false,
    practiceMode: false,
    acceptWithoutReview: true,
    start: stagingStart,
    end: stagingEnd,
    teamMemberCountLimit: 0,
    containerCountLimit: 3,
    allowUserSubmissions: false,
  });
  const dynamicId = await createChallenge(
    gameId,
    "DynamicContainer",
    "cheat-acceptance-dynamic",
    null,
    { containerImage: "acceptance-unused:latest", memoryLimit: 64, cpuCount: 1, exposePort: 80 },
  );
  const attachmentFlag = `flag{cheat_acceptance_attachment_${now}}`;
  const attachmentId = await createChallenge(
    gameId,
    "StaticAttachment",
    "cheat-acceptance-attachment",
    attachmentFlag,
  );
  const attachmentName = "cheat-acceptance.txt";
  const uploadedAttachment = await A.uploadAsset(
    attachmentName,
    "anti-cheat acceptance attachment\n",
  );
  await A.setAttachment(gameId, attachmentId, uploadedAttachment);
  const containerFlag = `flag{cheat_acceptance_container_${now}}`;
  const containerTitle = "cheat-acceptance-container";
  const containerId = await createChallenge(
    gameId,
    "StaticContainer",
    containerTitle,
    containerFlag,
    { containerImage: "acceptance-unused:latest", memoryLimit: 64, cpuCount: 1, exposePort: 80 },
  );

  const cohort = parseCohortSeedResult(sql(cohortSeedQuery(gameId, COHORT_SIZE)), COHORT_SIZE);
  const flags = seedDynamicInstances(dynamicId, cohort, now);
  const neutralJoiner = provisionNeutralJoiner();
  await exerciseIdentityAwareTeamAccept(neutralJoiner, cohort.teamIds[8]);
  await A.setGameSchedule(
    gameId,
    A.nowMs() - 60 * 60 * 1000,
    A.nowMs() + 60 * 60 * 1000,
  );
  // Keep independent journeys outside one another's /28. This makes a clean
  // actor a real negative control for SubnetOverlap instead of accidentally
  // sharing identity context with an offender.
  const [
    stolen,
    victim,
    brute,
    honeypot,
    wrongBoundary,
    download,
    container,
    solvedWrongWindow,
    fastDownload,
    fastContainer,
  ] = await exerciseDistinctIpLogins(gameId, cohort, [
    { index: 0, ip: "198.51.10.10", label: "stolen-flag actor" },
    { index: 1, ip: "198.51.20.10", label: "flag owner" },
    { index: 2, ip: "198.51.30.10", label: "brute-force actor" },
    { index: 3, ip: "198.51.40.10", label: "honeypot actor" },
    { index: 9, ip: "198.51.50.10", label: "wrong-boundary actor" },
    { index: 10, ip: "198.51.60.10", label: "aged-download actor" },
    { index: 11, ip: "198.51.70.10", label: "aged-container actor" },
    { index: 16, ip: "198.51.90.10", label: "solved-window actor" },
    { index: 17, ip: "198.51.100.10", label: "fast-download actor" },
    { index: 18, ip: "198.51.110.10", label: "fast-container actor" },
  ]);
  honeypot.honeypotUserAgent = `rsctf-cheat-acceptance/${now}`;
  const natInitialGroup = await exerciseSharedIpLogins(
    gameId,
    cohort,
    [4, 5, 6, 7],
    NAT_IP,
    "pre-end shared NAT",
    true,
  );
  const natFifth = actor(cohort, 8, NAT_IP);
  const contextGroup = await exerciseSharedIpLogins(
    gameId,
    cohort,
    [12, 13, 14, 15],
    REVIEWABLE_SHARED_IP,
    "four-team shared network",
  );

  await exerciseDownload(
    gameId,
    attachmentId,
    download,
    uploadedAttachment.hash,
    attachmentName,
    attachmentFlag,
    true,
    "aged attachment",
  );
  await exerciseContainer(
    gameId,
    containerId,
    container,
    containerTitle,
    containerFlag,
    true,
    "aged container",
  );
  await exerciseDownload(
    gameId,
    attachmentId,
    fastDownload,
    uploadedAttachment.hash,
    attachmentName,
    attachmentFlag,
    false,
    "fast attachment",
  );
  await exerciseContainer(
    gameId,
    containerId,
    fastContainer,
    containerTitle,
    containerFlag,
    false,
    "fast container",
  );
  const fastSnapshotActors = [fastDownload, fastContainer];
  const cleanParticipationIds = [
    victim.participationId,
    honeypot.participationId,
    ...natInitialGroup.actors.map((subject) => subject.participationId),
    natFifth.participationId,
    wrongBoundary.participationId,
    download.participationId,
    container.participationId,
    solvedWrongWindow.participationId,
    ...fastSnapshotActors.map((subject) => subject.participationId),
  ];
  const offenders = new Map([
    ["stolen", stolen.participationId],
    ["brute", brute.participationId],
  ]);

  seedWrongAttempts(
    gameId,
    dynamicId,
    wrongBoundary,
    38,
    `SELECT n,clock_timestamp()-interval '50 seconds' + ` +
      `((n-1)+floor((n-1)/10.0)*2)*interval '1 second' ` +
      `FROM generate_series(1,38) n`,
    `flag{boundary_${now}_`,
  );
  const boundaryResult = await submitFlag(
    gameId,
    dynamicId,
    wrongBoundary,
    `flag{boundary_${now}_39}`,
    "39-attempt boundary",
  );
  await assertSubmissionOutcome(
    gameId,
    dynamicId,
    wrongBoundary,
    boundaryResult.submissionId,
    "WrongAnswer",
    "39-attempt boundary",
  );
  if (
    Number(
      sql(
        `SELECT count(*) FROM "Submissions" WHERE game_id=${gameId} ` +
          `AND participation_id=${wrongBoundary.participationId} AND challenge_id=${dynamicId} ` +
          `AND status=2 AND answer LIKE ${literal(`flag{boundary_${now}_%`)}`,
      ),
    ) !== 39
  ) {
    throw new Error("fresh wrong-attempt negative control does not contain exactly 39 attempts");
  }

  const practiceNow = A.nowMs();
  const practiceGameId = await A.createGame({
    title: `LOADTEST-CHEAT-PRACTICE-${practiceNow}`,
    hidden: false,
    practiceMode: true,
    acceptWithoutReview: true,
    start: practiceNow + 10 * 60 * 1000,
    end: practiceNow + 2 * 60 * 60 * 1000,
    teamMemberCountLimit: 0,
  });
  const practiceFlag = `flag{cheat_acceptance_practice_${practiceNow}}`;
  const practiceChallengeId = await createChallenge(
    practiceGameId,
    "StaticAttachment",
    "cheat-acceptance-practice",
    practiceFlag,
  );
  const practiceCohort = parseCohortSeedResult(
    sql(cohortSeedQuery(practiceGameId, 1)),
    1,
  );
  await A.setGameSchedule(
    practiceGameId,
    A.nowMs() - 2 * 60 * 60 * 1000,
    A.nowMs() - 60 * 60 * 1000,
  );
  const practiceActor = actor(practiceCohort, 0, "198.51.80.10");
  const practiceResult = await submitFlag(
    practiceGameId,
    practiceChallengeId,
    practiceActor,
    practiceFlag,
    "post-game practice",
  );
  await assertSubmissionOutcome(
    practiceGameId,
    practiceChallengeId,
    practiceActor,
    practiceResult.submissionId,
    "Accepted",
    "post-game practice",
  );

  const ownerSolve = await submitFlag(
    gameId,
    dynamicId,
    victim,
    flags.get(victim.participationId),
    "owned dynamic flag",
  );
  await assertSubmissionOutcome(
    gameId,
    dynamicId,
    victim,
    ownerSolve.submissionId,
    "Accepted",
    "owned dynamic flag",
  );

  const suppressedPrefix = `flag{suppressed_${now}_`;
  const immutableSuppression = jsonQuery(
    `WITH wrongs AS MATERIALIZED (` +
      `INSERT INTO "Submissions"` +
      `(answer,status,submit_time_utc,user_id,team_id,participation_id,game_id,challenge_id) ` +
      `SELECT ${literal(suppressedPrefix)}||series.n||'}',2,series.at,` +
      `${literal(solvedWrongWindow.userId)}::uuid,${solvedWrongWindow.teamId},` +
      `${solvedWrongWindow.participationId},${gameId},${dynamicId} FROM (` +
      `SELECT n,clock_timestamp()-interval '6 minutes' + ` +
      `((n-1)+floor((n-1)/10.0)*2)*interval '1 second' AS at ` +
      `FROM generate_series(1,40) n) series RETURNING id` +
      `), solve AS MATERIALIZED (` +
      `INSERT INTO "Submissions"` +
      `(answer,status,submit_time_utc,user_id,team_id,participation_id,game_id,challenge_id) ` +
      `VALUES (${literal(flags.get(solvedWrongWindow.participationId))},1,` +
      `clock_timestamp()-interval '5 minutes',` +
      `${literal(solvedWrongWindow.userId)}::uuid,${solvedWrongWindow.teamId},` +
      `${solvedWrongWindow.participationId},${gameId},${dynamicId}) RETURNING id` +
      `), projected AS (` +
      `INSERT INTO "FirstSolves"(participation_id,challenge_id,submission_id) ` +
      `SELECT ${solvedWrongWindow.participationId},${dynamicId},id FROM solve ` +
      `RETURNING submission_id` +
      `) SELECT json_build_object(` +
      `'wrongs',(SELECT count(*) FROM wrongs),` +
      `'solveId',(SELECT id FROM solve),` +
      `'firstSolves',(SELECT count(*) FROM projected))::text`,
    "immutable solve-suppressed wrong window",
  );
  const suppressedSolve = { submissionId: Number(immutableSuppression.solveId) };
  if (
    Number(immutableSuppression.wrongs) !== 40 ||
    !Number.isSafeInteger(suppressedSolve.submissionId) ||
    suppressedSolve.submissionId <= 0 ||
    Number(immutableSuppression.firstSolves) !== 1
  ) {
    throw new Error("immutable solve-suppressed wrong-window fixture is incomplete");
  }
  await assertSubmissionOutcome(
    gameId,
    dynamicId,
    solvedWrongWindow,
    suppressedSolve.submissionId,
    "Accepted",
    "wrong-window recovery solve",
  );
  const suppressionKick = await submitFlag(
    gameId,
    dynamicId,
    solvedWrongWindow,
    `${suppressedPrefix}kick}`,
    "solve-suppressed wrong-window reconciliation kick",
  );
  await assertSubmissionOutcome(
    gameId,
    dynamicId,
    solvedWrongWindow,
    suppressionKick.submissionId,
    "WrongAnswer",
    "solve-suppressed wrong-window reconciliation kick",
  );
  const suppressionShape = jsonQuery(
    `WITH wrongs AS (` +
      `SELECT min(submit_time_utc) AS anchor,count(*) AS total,` +
      `count(*) FILTER (WHERE submit_time_utc<=clock_timestamp()-interval '5 minutes') AS mature ` +
      `FROM "Submissions" WHERE game_id=${gameId} ` +
      `AND participation_id=${solvedWrongWindow.participationId} ` +
      `AND challenge_id=${dynamicId} AND status=2 ` +
      `AND answer LIKE ${literal(`${suppressedPrefix}%`)}` +
      `), solve AS (` +
      `SELECT submit_time_utc FROM "Submissions" WHERE id=${suppressedSolve.submissionId}` +
      `) SELECT json_build_object(` +
      `'total',wrongs.total,'mature',wrongs.mature,` +
      `'solveWithinWindow',solve.submit_time_utc>=wrongs.anchor AND ` +
      `solve.submit_time_utc<=wrongs.anchor+interval '5 minutes')::text FROM wrongs,solve`,
    "solve-suppressed wrong-window shape",
  );
  if (
    Number(suppressionShape.total) !== 41 ||
    Number(suppressionShape.mature) !== 40 ||
    suppressionShape.solveWithinWindow !== true
  ) {
    throw new Error(
      `solve-suppressed wrong window is ${suppressionShape.mature}/${suppressionShape.total} ` +
        `with solveWithinWindow=${suppressionShape.solveWithinWindow}`,
    );
  }

  seedWrongAttempts(
    gameId,
    dynamicId,
    brute,
    40,
    `SELECT n,clock_timestamp()-interval '6 minutes' + ` +
      `(n-1)*interval '1 second' FROM generate_series(1,40) n`,
    `flag{automated_${now}_`,
  );
  const reconciliationBefore = reconciliationAttempts(gameId);
  const stolenResult = await submitFlag(
    gameId,
    dynamicId,
    stolen,
    flags.get(victim.participationId),
    "stolen flag",
  );
  if (stolenResult.response.text.includes("CheatDetected")) {
    throw new Error("the submit response leaked CheatDetected to the player");
  }
  const visibleStatus = await A.api(
    "GET",
    `/api/game/${gameId}/challenges/${dynamicId}/status/${stolenResult.submissionId}`,
    playerOptions(stolen),
  );
  if (visibleStatus.status !== 200 || unwrap(visibleStatus) !== "WrongAnswer") {
    throw new Error("player status did not redact CheatDetected to WrongAnswer");
  }
  if (
    Number(sql(`SELECT status FROM "Submissions" WHERE id=${stolenResult.submissionId}`)) !== 3
  ) {
    throw new Error("stolen-flag submission was not retained as monitor-only CheatDetected evidence");
  }
  if (
    Number(
      sql(
        `SELECT count(*) FROM "FirstSolves" WHERE submission_id=${stolenResult.submissionId}`,
      ),
    ) !== 0
  ) {
    throw new Error("stolen-flag submission incorrectly established a FirstSolve");
  }
  if (
    Number(
      sql(
        `SELECT count(*) FROM "CheatInfo" WHERE game_id=${gameId} ` +
          `AND challenge_id=${dynamicId} AND submission_id=${stolenResult.submissionId} ` +
          `AND submit_participation_id=${stolen.participationId} ` +
          `AND source_participation_id=${victim.participationId} ` +
          `AND evidence_key=${literal(`submission:${stolenResult.submissionId}`)}`,
      ),
    ) !== 1
  ) {
    throw new Error("stolen-flag verdict is missing immutable CheatInfo provenance");
  }

  const bruteKick = await submitFlag(
    gameId,
    dynamicId,
    brute,
    `flag{automated_${now}_kick}`,
    "mature 40-attempt offender reconciliation kick",
  );
  await assertSubmissionOutcome(
    gameId,
    dynamicId,
    brute,
    bruteKick.submissionId,
    "WrongAnswer",
    "mature 40-attempt offender reconciliation kick",
  );
  const bruteAttemptShape = jsonQuery(
    `SELECT json_build_object(` +
      `'total',count(*),` +
      `'mature',count(*) FILTER (WHERE submit_time_utc<=clock_timestamp()-interval '5 minutes')` +
      `)::text FROM "Submissions" WHERE game_id=${gameId} ` +
      `AND participation_id=${brute.participationId} AND challenge_id=${dynamicId} ` +
      `AND status=2 AND answer LIKE ${literal(`flag{automated_${now}_%`)}`,
    "mature wrong-attempt fixture",
  );
  if (Number(bruteAttemptShape.total) !== 41 || Number(bruteAttemptShape.mature) !== 40) {
    throw new Error(
      `mature brute-force fixture is ${bruteAttemptShape.mature}/${bruteAttemptShape.total}; ` +
        "expected 40 mature attempts plus one public kick",
    );
  }
  const honeypotHitFloor = Number(
    sql(`SELECT COALESCE(max(id),0) FROM "HoneypotHits"`),
  );
  for (const bait of HONEYPOT_BAITS) {
    const response = await A.api("GET", bait, playerOptions(honeypot));
    if (response.status !== 404) throw new Error(`honeypot ${bait} returned ${response.status}`);
  }
  await waitFor(
    "authenticated honeypot raw telemetry",
    () => assertHoneypotTelemetry(gameId, honeypot, honeypotHitFloor),
  );

  // Gate actionable request/outbox evidence and raw telemetry before ending the
  // game and before the first /cheatreport read.
  await waitFor("pre-finalization direct evidence", () => {
    const rows = evidence(gameId);
    assertNoTelemetryOnlyEvents(rows);
    assertNoCleanEvidence(rows, cleanParticipationIds);
    for (const [role, participationId] of offenders) {
      const actual = scenarioCodesFor(rows, participationId, role, dynamicId);
      const required = CHEAT_SCENARIO_RULES[role].live;
      const allowed = CHEAT_SCENARIO_RULES[role].reconciled;
      if (!containsMembers(actual, required)) {
        return false;
      }
      if (!containsMembers(allowed, actual)) {
        throw new Error(`${role} has unexpected direct evidence: ${actual.join(", ")}`);
      }
    }
    return rows;
  });
  const natFifthCredentials = await resetLoginCredentials(
    natFifth,
    "fifth shared-NAT actor",
  );

  // Configure a near-future end only after every ordinary competitive journey.
  // One fifth NAT observation takes the same Games FOR SHARE fence as a
  // production identity writer before that boundary, then deliberately commits
  // during finalization. This is the race that used to leave a transient
  // four-team correlation behind when the final population was five.
  const closeout = jsonQuery(
    `WITH boundary AS MATERIALIZED (` +
      `SELECT clock_timestamp()+interval '4 seconds' AS ended_at` +
      `), sealed AS (` +
      `UPDATE "Games" game SET end_time_utc=boundary.ended_at FROM boundary ` +
      `WHERE game.id=${gameId} AND game.start_time_utc<boundary.ended_at ` +
      `AND game.end_time_utc>boundary.ended_at ` +
      `RETURNING game.id,game.end_time_utc` +
      `) SELECT json_build_object(` +
      `'rows',count(*),` +
      `'endAtMicros',floor(extract(epoch from min(end_time_utc))*1000000)::bigint` +
      `)::text FROM sealed`,
    "competitive fixture closeout",
  );
  if (
    Number(closeout.rows) !== 1 ||
    !Number.isSafeInteger(Number(closeout.endAtMicros))
  ) {
    throw new Error("isolated competitive fixture could not schedule one exact closeout");
  }
  const endAtMicros = Number(closeout.endAtMicros);
  const heldNat = await startHeldSharedIpLogin(
    gameId,
    natFifth,
    NAT_IP,
    7,
    natFifthCredentials,
  );
  const natGroup = {
    ...natInitialGroup,
    actors: [...natInitialGroup.actors, natFifth],
  };

  const graceProbeAtMs = Math.floor(endAtMicros / 1000) + 100;
  await sleep(Math.max(0, graceProbeAtMs - A.nowMs()));
  const graceNowMicros = A.nowMs() * 1000;
  if (graceNowMicros >= endAtMicros + 1_000_000) {
    throw new Error("missed the one-second finalization-grace observation window");
  }
  const networkParticipationIds = new Set(
    [...natGroup.actors, ...contextGroup.actors].map((subject) => subject.participationId),
  );
  const prematureNetworkEvidence = evidence(gameId).filter((row) => {
    const code = SUSPICION_RULE_BY_KIND.get(Number(row.kind))?.code;
    return (
      networkParticipationIds.has(Number(row.participationId)) &&
      (code === "CrossTeamIP" || code === "SubnetOverlap")
    );
  });
  if (prematureNetworkEvidence.length !== 0) {
    throw new Error("final-only network correlation ran before finalization grace completed");
  }
  const heldCredentials = await heldNat.completion();
  const heldShape = jsonQuery(
    `SELECT json_build_object(` +
      `'gameRows',count(*) FILTER (WHERE game_id=${gameId}),` +
      `'globalRows',count(*) FILTER (WHERE game_id IS NULL),` +
      `'observedAtMicros',floor(extract(epoch from min(observed_at_utc))*1000000)::bigint,` +
      `'exactHash',min(encode(value_hash,'hex')) FILTER (WHERE game_id=${gameId}),` +
      `'subnetHash',min(encode(subnet_group_hash,'hex')) FILTER (WHERE game_id=${gameId})` +
      `)::text FROM "IdentityObservations" WHERE user_id=${literal(natFifth.userId)}::uuid ` +
      `AND kind='Ip' AND source='Password'`,
    "held fifth-NAT observation",
  );
  if (
    Number(heldShape.gameRows) !== 1 ||
    Number(heldShape.globalRows) !== 1 ||
    !Number.isSafeInteger(Number(heldShape.observedAtMicros)) ||
    Number(heldShape.observedAtMicros) >= endAtMicros ||
    `cross-team-ip:${heldShape.exactHash}` !== natInitialGroup.exactEvidenceKey ||
    `subnet-overlap:${heldShape.subnetHash}` !== natInitialGroup.subnetEvidenceKey
  ) {
    throw new Error("public fifth-NAT login did not commit its exact pre-end identity snapshot");
  }
  await assertPostEndLoginExcluded(gameId, natFifth, NAT_IP, heldCredentials);

  await waitFor("sealed background evidence reconciliation", () => {
    const state = reconciliationState(gameId);
    if (state?.lastError) {
      throw new Error(`final suspicion reconciliation failed: ${state.lastError}`);
    }
    if (
      Number(state?.attempts) <= reconciliationBefore ||
      !Number.isSafeInteger(Number(state?.sealedAtMicros)) ||
      Number(state.sealedAtMicros) < endAtMicros + 1_000_000 ||
      outboxPending(gameId) !== 0
    ) {
      return false;
    }
    const rows = evidence(gameId);
    assertNoTelemetryOnlyEvents(rows);
    assertNoCleanEvidence(rows, cleanParticipationIds);
    if (!assertSharedContextEvidence(rows, contextGroup)) {
      return false;
    }
    for (const [role, participationId] of offenders) {
      if (!sameMembers(
        scenarioCodesFor(rows, participationId, role, dynamicId),
        CHEAT_SCENARIO_RULES[role].reconciled,
      )) {
        return false;
      }
    }
    return rows;
  });
  const nonzeroCleanScores = Number(
    sql(
      `SELECT count(*) FROM "Participations" WHERE game_id=${gameId} ` +
        `AND id IN (${cleanParticipationIds.join(",")}) AND suspicion_score<>0`,
    ),
  );
  if (nonzeroCleanScores !== 0) {
    throw new Error(`${nonzeroCleanScores} benign/telemetry controls received a suspicion score`);
  }
  assertCompetitiveTimeFence(gameId);
  assertHoneypotTelemetry(gameId, honeypot, honeypotHitFloor);
  await waitFor("post-game practice evaluation", () =>
    Number(
      sql(
        `SELECT count(*) FROM "SuspicionEvaluationOutbox" ` +
          `WHERE game_id=${practiceGameId} AND source_id=${practiceResult.submissionId} ` +
          `AND job_kind=0 AND attempts>=1 AND completed_at_utc IS NOT NULL AND last_error IS NULL`,
      ),
    ) === 1,
  );
  if (evidence(practiceGameId).length !== 0) {
    throw new Error("post-game practice submission created competitive suspicion evidence");
  }
  if (
    Number(
      sql(
        `SELECT suspicion_score FROM "Participations" ` +
          `WHERE id=${practiceActor.participationId} AND game_id=${practiceGameId}`,
      ),
    ) !== 0
  ) {
    throw new Error("post-game practice submission changed its suspicion score");
  }

  const beforeReport = ledgerSnapshot(gameId);
  const honeypotBeforeReport = JSON.stringify(
    assertHoneypotTelemetry(gameId, honeypot, honeypotHitFloor),
  );
  const reportResponse = await A.api("GET", `/api/game/${gameId}/cheatreport`, {
    jwt: A.adminJwt(),
    ip: "192.0.2.44",
  });
  if (reportResponse.status !== 200) {
    throw new Error(`cheat report failed: ${reportResponse.status} ${reportResponse.text}`);
  }
  const afterReport = ledgerSnapshot(gameId);
  const honeypotAfterReport = JSON.stringify(
    assertHoneypotTelemetry(gameId, honeypot, honeypotHitFloor),
  );
  if (afterReport !== beforeReport || honeypotAfterReport !== honeypotBeforeReport) {
    throw new Error("GET /cheatreport mutated sources, evidence, scores, or outbox state");
  }
  const report = unwrap(reportResponse);
  assertReport(
    report,
    gameId,
    offenders,
    weights,
    contextGroup,
    natGroup,
    victim,
    honeypot,
  );

  // The frequently-polled report stays compact. Prove that an administrator
  // can lazily review the immutable sources behind real hard/strong incidents
  // without mutating the evidence ledger or receiving raw secret material.
  const reviewCandidates = (report.suspicionList || [])
    .flatMap((record) => record.events || [])
    .filter((event) => ["StolenFlag", "HighWrongRate", "AutomatedPattern"].includes(event.type))
    .slice(0, 4);
  if (!reviewCandidates.some((event) => event.type === "StolenFlag")) {
    throw new Error("anti-cheat report did not expose a reviewable hard event");
  }
  const beforeEvidenceReview = ledgerSnapshot(gameId);
  for (const event of reviewCandidates) {
    if (!Number.isInteger(event.eventId) || event.eventId <= 0) {
      throw new Error(`review candidate ${event.type} is missing its stable event id`);
    }
    const response = await A.api(
      "GET",
      `/api/game/${gameId}/cheatreport/events/${event.eventId}`,
      { jwt: A.adminJwt(), ip: "192.0.2.45" },
    );
    if (response.status !== 200) {
      throw new Error(
        `evidence review ${event.eventId} failed: ${response.status} ${response.text}`,
      );
    }
    const review = unwrap(response);
    if (review.eventId !== event.eventId || !Array.isArray(review.sources)) {
      throw new Error(`evidence review ${event.eventId} returned the wrong source identity`);
    }
    if (event.type === "StolenFlag") {
      if (
        review.assessment !== "directEvidence" ||
        review.sourceStatus !== "verified" ||
        review.isDirectProof !== true ||
        !review.sources.some((source) => source.sourceType === "cheatInfo")
      ) {
        throw new Error("stolen-flag review did not verify its canonical CheatInfo source");
      }
    } else if (review.isDirectProof || review.sourceStatus !== "supporting") {
      throw new Error(`${event.type} was incorrectly presented as direct proof`);
    }
    const serialized = JSON.stringify(review);
    if (
      serialized.includes(flags.get(victim.participationId)) ||
      serialized.includes('"answer"')
    ) {
      throw new Error(`evidence review ${event.eventId} leaked raw flag material`);
    }
  }
  if (ledgerSnapshot(gameId) !== beforeEvidenceReview) {
    throw new Error("GET /cheatreport/events/{eventId} mutated the evidence ledger");
  }

  console.log(
    `isolated anti-cheat acceptance passed: game ${gameId}, ` +
      `2 offenders, ${cleanParticipationIds.length} benign/telemetry controls, ` +
      `${fastSnapshotActors.length} immutable fast-threshold snapshot pairs, 38 capability kinds`,
  );
}

main().catch((error) => {
  console.error(error?.stack || error);
  process.exitCode = 1;
});
