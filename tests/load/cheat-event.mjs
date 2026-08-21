// Deterministic anti-cheat drill for a retained lifecycle event.
//
// This runner only targets the load-test namespace from provision.mjs. It creates
// one dynamic-flag audit challenge, gives every existing team a unique flag, and
// drives two known-bad behaviours and one raw-telemetry control through the
// public HTTP surface:
//   * four teams submit another team's valid flag;
//   * one team coordinates 40 machine-speed wrong submissions across five accounts;
//   * one clean team follows three authenticated same-origin honeypot routes.
// Each run takes five offenders without prior actionable evidence and one clean
// actor without any prior suspicion evidence for the telemetry check, then freezes
// every other roster member as a clean control. Only post-baseline evidence is judged,
// so ordinary-play history cannot hide a drill false positive or satisfy its actor
// gates. Credentials live only in a temporary k6 input file.
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

import * as A from "./applib.mjs";
import {
  CHEAT_SCENARIO_RULES,
  SUSPICION_RULE_BY_KIND,
  computeExpectedBreakdown,
  effectiveRuleWeights,
  validateDetectorCapabilities,
} from "./cheat-contract.js";
import { freezeCheatCohort } from "./cheat-cohort.js";
import { cheatK6Environment } from "./cheat-environment.js";
import {
  cheatRetentionPolicy,
  inheritedCheatOrchestrationToken,
  recordCheatSimulation,
} from "./cheat-retention.js";
import {
  CHEAT_RESULT_SCHEMA_VERSION,
  writeCheatResult,
} from "./cheat-result.js";
import {
  acquireExclusiveProcessLock,
  loadOrchestrationLockPath,
} from "./process-control.mjs";
import { loadAuthoritativeAfterConcurrentSweep } from "./report-convergence.js";
import { TARGET, mintJwt, sleep, sql } from "./lib.mjs";

const REQUIRED_TEAMS = 100;
const RETENTION = cheatRetentionPolicy(process.env);
const STOLEN_ACTORS = 4;
const BRUTE_ACCOUNTS = 5;
const BRUTE_ATTEMPTS_PER_ACCOUNT = 8;
const HONEYPOT_BAITS = ["/.env", "/.git/config", "/wp-login.php"];
const CONTEXT_KINDS = [1, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 21, 22, 23, 26, 28, 29, 31, 32, 36, 37];
const EVIDENCE_KIND = Object.freeze({
  stolenFlag: 0,
  highWrongRate: 24,
  automatedPattern: 25,
});
const ORIGIN = process.env.ORIGIN || process.env.BROWSER_ORIGIN || TARGET;

let activeK6 = null;
let activeTemporaryDirectory = null;
let orchestrationLock = null;
let shutdownSignal = null;
let shutdownEscalation = null;

function forwardShutdownSignal(signal) {
  shutdownSignal ??= signal;
  if (activeK6?.exitCode === null && activeK6?.signalCode === null) {
    activeK6.kill(signal);
    if (!shutdownEscalation) {
      shutdownEscalation = setTimeout(() => {
        if (activeK6?.exitCode === null && activeK6?.signalCode === null) {
          activeK6.kill("SIGKILL");
        }
      }, 2_000);
      shutdownEscalation.unref();
    }
    return;
  }

  // No k6 child exists yet, so synchronous cleanup is sufficient before
  // restoring the signal's normal process-termination behavior.
  if (activeTemporaryDirectory) {
    rmSync(activeTemporaryDirectory, { recursive: true, force: true });
    activeTemporaryDirectory = null;
  }
  process.removeListener(signal, forwardShutdownHandlers[signal]);
  process.kill(process.pid, signal);
}

const forwardShutdownHandlers = Object.fromEntries(
  ["SIGINT", "SIGTERM"].map((signal) => [
    signal,
    () => forwardShutdownSignal(signal),
  ]),
);
for (const [signal, handler] of Object.entries(forwardShutdownHandlers)) {
  process.on(signal, handler);
}

async function runK6Async(script, environment, sandboxDirectory) {
  const child = spawn(
    "k6",
    ["run", new URL(`./k6/${script}`, import.meta.url).pathname],
    {
      stdio: "inherit",
      env: cheatK6Environment(process.env, environment, sandboxDirectory),
    },
  );
  activeK6 = child;
  try {
    return await new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", (code, signal) => resolve({ code, signal }));
    });
  } finally {
    activeK6 = null;
    if (shutdownEscalation) clearTimeout(shutdownEscalation);
    shutdownEscalation = null;
  }
}

function requireOptIn() {
  if (process.env.CHEAT_SIMULATION !== "1") {
    throw new Error(
      "refusing to generate cheat evidence without CHEAT_SIMULATION=1",
    );
  }
  if (!RETENTION.integrated && process.env.KEEP !== "1") {
    throw new Error(
      "the cheat drill requires KEEP=1 so its event and evidence remain available",
    );
  }
  if (
    RETENTION.integrated &&
    process.env.INTEGRATED_CHEAT_SIMULATION !== "1"
  ) {
    throw new Error(
      "embedded cheat mode requires INTEGRATED_CHEAT_SIMULATION=1 from the lifecycle parent",
    );
  }
  if (
    RETENTION.integrated &&
    process.env.RETAIN_EVENT === "1" &&
    process.env.KEEP !== "1"
  ) {
    throw new Error("a retained lifecycle cheat drill requires KEEP=1");
  }
  if (RETENTION.integrated && !process.env.RSCTF_CHEAT_RESULT_PATH) {
    throw new Error("embedded cheat mode requires an explicit result path from the lifecycle parent");
  }
}

function positiveInteger(value, label) {
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number <= 0)
    throw new Error(`invalid ${label}: ${value}`);
  return number;
}

function literal(value) {
  return `'${String(value).replaceAll("'", "''")}'`;
}

function parseJsonQuery(query, label) {
  const value = sql(query);
  try {
    return JSON.parse(value || "[]");
  } catch (error) {
    throw new Error(`${label} returned malformed JSON: ${error.message}`);
  }
}

function configuredSuspicionRules() {
  return parseJsonQuery(
    `SELECT COALESCE(json_agg(json_build_object(` +
      `'ruleCode',rule_code,'weight',weight) ORDER BY id),'[]'::json)::text ` +
      `FROM "SuspicionRules"`,
    "configured suspicion-rule query",
  );
}

function suspicionEvidence(gameId, afterId = 0) {
  return parseJsonQuery(
    `SELECT COALESCE(json_agg(json_build_object(` +
      `'id',id,'participationId',participation_id,'challengeId',challenge_id,` +
      `'kind',kind,'evidenceKey',evidence_key,'scoreDelta',score_delta,` +
      `'createdAtMs',floor(extract(epoch from created_at)*1000)::bigint,` +
      `'createdAtMicros',floor(extract(epoch from created_at)*1000000)::bigint) ORDER BY id),'[]'::json)::text ` +
      `FROM "SuspicionEvents" WHERE game_id=${positiveInteger(gameId, "game id")} ` +
      `AND id>${positiveIntegerOrZero(afterId, "suspicion evidence floor")}`,
    "suspicion evidence query",
  );
}

function honeypotTelemetryState(config, baseline) {
  const hitFloor = positiveIntegerOrZero(
    baseline.honeypotHitId,
    "honeypot hit floor",
  );
  const eventFloor = positiveIntegerOrZero(
    baseline.suspicionEventId,
    "honeypot suspicion-event floor",
  );
  const outboxFloor = positiveIntegerOrZero(
    baseline.suspicionOutboxId,
    "honeypot outbox floor",
  );
  return parseJsonQuery(
    `SELECT json_build_object(` +
      `'hits',(SELECT COALESCE(json_agg(json_build_array(` +
      `hit.id,hit.user_id,hit.game_id,hit.participation_id,hit.bait,` +
      `hit.remote_ip,hit.user_agent,hit.hit_at_utc) ORDER BY hit.id),'[]'::json) ` +
      `FROM "HoneypotHits" hit WHERE hit.id>${hitFloor} ` +
      `AND hit.user_agent=${literal(config.honeypot.honeypotUserAgent)}),` +
      `'outboxJobs',(SELECT count(*) FROM "SuspicionEvaluationOutbox" job ` +
      `WHERE job.id>${outboxFloor} AND (` +
      `job.rule_kind IN (28,29,31) OR EXISTS (` +
      `SELECT 1 FROM "HoneypotHits" hit WHERE job.source_kind=1 ` +
      `AND job.source_id=hit.id AND hit.id>${hitFloor} ` +
      `AND hit.user_agent=${literal(config.honeypot.honeypotUserAgent)}))),` +
      `'suspicionEvents',(SELECT count(*) FROM "SuspicionEvents" ` +
      `WHERE id>${eventFloor} AND kind IN (28,29,31)),` +
      `'storedScore',(SELECT suspicion_score FROM "Participations" ` +
      `WHERE id=${positiveInteger(config.honeypot.participationId, "honeypot participation id")} ` +
      `AND game_id=${positiveInteger(config.gameId, "honeypot game id")})` +
      `)::text`,
    "honeypot raw telemetry query",
  );
}

function assertHoneypotTelemetry(config, baseline) {
  const state = honeypotTelemetryState(config, baseline);
  const hits = Array.isArray(state.hits) ? state.hits : [];
  const actualBaits = hits.map((row) => row[4]).sort();
  const exactAttribution = hits.every(
    (row) =>
      String(row[1]).toLowerCase() === config.honeypot.userId.toLowerCase() &&
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

function positiveIntegerOrZero(value, label) {
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number < 0)
    throw new Error(`invalid ${label}: ${value}`);
  return number;
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

function challengeExists(gameId, challengeId) {
  if (!Number.isSafeInteger(challengeId) || challengeId <= 0) return false;
  return (
    Number(
      sql(
        `SELECT count(*) FROM "GameChallenges" WHERE game_id=${gameId} AND id=${challengeId}`,
      ),
    ) === 1
  );
}

function findAuditChallenge(gameId, title) {
  const challengeId = Number(
    sql(
      `SELECT id FROM "GameChallenges" ` +
        `WHERE game_id=${gameId} AND title=${literal(title)} ` +
        `ORDER BY id LIMIT 1`,
    ),
  );
  return Number.isSafeInteger(challengeId) && challengeId > 0
    ? challengeId
    : undefined;
}

async function ensureAuditChallenge(state) {
  const gameId = positiveInteger(state.mixGame, "mixed-event game id");
  const title = `anti-cheat-drill-${state.createdAtMs}`;
  let challengeId = Number(state.cheatSimulation?.challengeId);
  let requiresConfiguration = false;
  if (!challengeExists(gameId, challengeId)) {
    challengeId = findAuditChallenge(gameId, title);
    if (!challengeId) {
      challengeId = await A.createChallenge(gameId, {
        title,
        category: "Misc",
        type: "DynamicContainer",
      });
    }
    requiresConfiguration = true;
  }

  if (requiresConfiguration) {
    await A.setChallenge(gameId, challengeId, {
      content:
        "Controlled dynamic-flag fixture for the retained anti-cheat simulation.",
      originalScore: 1000,
      minScoreRate: 0.25,
      difficulty: 5,
      submissionLimit: 0,
      containerImage: "nginx:alpine",
      memoryLimit: 64,
      cpuCount: 1,
      exposePort: 80,
    });
    const placeholder = `flag{anti_cheat_placeholder_${state.createdAtMs}}`;
    const placeholderExists = Number(
      sql(
        `SELECT count(*) FROM "FlagContexts" ` +
          `WHERE challenge_id=${challengeId} AND flag=${literal(placeholder)}`,
      ),
    );
    if (placeholderExists === 0) {
      await A.addFlags(gameId, challengeId, [placeholder]);
    }
    await A.setChallenge(gameId, challengeId, { isEnabled: true });
    if (!RETENTION.integrated) {
      A.writeState(
        recordCheatSimulation(
          state,
          {
            challengeId,
            completed: false,
          },
          RETENTION,
        ),
      );
    }
  }

  const desired = state.adPartIds
    .map(
      (pid) =>
        `(${positiveInteger(pid, "participation id")},${literal(`flag{anti_cheat_${state.createdAtMs}_${pid}}`)})`,
    )
    .join(",");
  sql(
    `WITH desired(participation_id,flag) AS (VALUES ${desired}) ` +
      `INSERT INTO "FlagContexts"(flag,is_occupied,challenge_id) ` +
      `SELECT desired.flag,true,${challengeId} FROM desired ` +
      `WHERE NOT EXISTS (` +
      `SELECT 1 FROM "FlagContexts" context ` +
      `WHERE context.challenge_id=${challengeId} AND context.flag=desired.flag)`,
  );
  sql(
    `WITH desired(participation_id,flag) AS (VALUES ${desired}) ` +
      `INSERT INTO "GameInstances"` +
      `(challenge_id,participation_id,is_loaded,last_container_operation,flag_id,container_id) ` +
      `SELECT ${challengeId},desired.participation_id,false,now(),context.id,NULL ` +
      `FROM desired JOIN LATERAL (` +
      `SELECT id FROM "FlagContexts" WHERE challenge_id=${challengeId} AND flag=desired.flag ` +
      `ORDER BY id LIMIT 1) context ON true ` +
      `ON CONFLICT (participation_id,challenge_id) DO UPDATE SET flag_id=EXCLUDED.flag_id`,
  );

  const instanceCount = Number(
    sql(
      `SELECT count(*) FROM "GameInstances" WHERE challenge_id=${challengeId}`,
    ),
  );
  if (instanceCount !== state.adPartIds.length) {
    throw new Error(
      `anti-cheat fixture has ${instanceCount}/${state.adPartIds.length} team instances`,
    );
  }
  return challengeId;
}

function chooseCohort(state) {
  const gameId = positiveInteger(state.mixGame, "mixed-event game id");
  const actionable = parseJsonQuery(
    `SELECT COALESCE(json_agg(DISTINCT participation_id),'[]'::json)::text ` +
      `FROM "SuspicionEvents" WHERE game_id=${gameId} ` +
      `AND kind NOT IN (${CONTEXT_KINDS.join(",")})`,
    "actionable participation query",
  );
  const anySuspicion = new Set(
    parseJsonQuery(
      `SELECT COALESCE(json_agg(participation.id ORDER BY participation.id),'[]'::json)::text ` +
        `FROM "Participations" participation WHERE participation.game_id=${gameId} AND (` +
        `participation.suspicion_score<>0 OR EXISTS (` +
        `SELECT 1 FROM "SuspicionEvents" event ` +
        `WHERE event.game_id=${gameId} AND event.participation_id=participation.id))`,
      "any-suspicion participation query",
    ).map(Number),
  );
  const offenderCount = STOLEN_ACTORS + 1;
  const { offenderIndices, cleanIndices } = freezeCheatCohort(
    state.adPartIds,
    actionable,
    offenderCount,
  );
  const offenderSet = new Set(offenderIndices);
  const honeypotIndex = cleanIndices.find(
    (index) => !anySuspicion.has(Number(state.adPartIds[index])),
  );
  if (honeypotIndex === undefined) {
    throw new Error("the anti-cheat fixture needs a zero-score actor for honeypot telemetry");
  }
  const victimIndices = state.adPartIds
    .map((_, index) => index)
    .filter((index) => !offenderSet.has(index) && index !== honeypotIndex)
    .slice(0, STOLEN_ACTORS);
  if (victimIndices.length !== STOLEN_ACTORS) {
    throw new Error(
      "the anti-cheat fixture does not have enough distinct victims",
    );
  }

  return {
    stolenIndices: offenderIndices.slice(0, STOLEN_ACTORS),
    bruteIndex: offenderIndices[STOLEN_ACTORS],
    honeypotIndex,
    victimIndices,
    cleanIndices,
  };
}

function ensureBruteAccounts(state, bruteIndex) {
  const gameId = positiveInteger(state.mixGame, "mixed-event game id");
  const teamId = positiveInteger(
    state.adTeamIds[bruteIndex],
    "brute-force team id",
  );
  const participationId = positiveInteger(
    state.adPartIds[bruteIndex],
    "brute-force participation id",
  );
  const prefix = `lt${gameId}_cheatbot_`;
  const botNames = Array.from(
    { length: BRUTE_ACCOUNTS },
    (_, index) => `${prefix}${index + 1}`,
  );
  const botNameList = botNames.map(literal).join(",");
  sql(
    `WITH neutral_provisioning AS MATERIALIZED (` +
      `SELECT set_config('rsctf.identity_neutral_insert','1',true)` +
      `) INSERT INTO "AspNetUsers" ` +
      `(id,user_name,normalized_user_name,email,normalized_email,email_confirmed,password_hash,` +
      `security_stamp,concurrency_stamp,role,register_time_utc,last_signed_in_utc,last_visited_utc,` +
      `lockout_enabled,access_failed_count,phone_number_confirmed,two_factor_enabled,ip,bio,real_name,std_number,exercise_visible) ` +
      `SELECT gen_random_uuid(),${literal(prefix)}||g,upper(${literal(prefix)}||g),` +
      `${literal(prefix)}||g||'@load.test',upper(${literal(prefix)}||g||'@load.test'),` +
      `true,'x-load-placeholder',gen_random_uuid()::text,gen_random_uuid()::text,1,` +
      `now(),now(),now(),true,0,false,false,'0.0.0.0','','','',false ` +
      `FROM generate_series(1,${BRUTE_ACCOUNTS}) g ` +
      `CROSS JOIN neutral_provisioning ` +
      `ON CONFLICT (user_name) DO NOTHING`,
  );
  // A fresh security stamp gives every rerun a fresh authenticated limiter
  // partition without weakening the production policy or waiting for refill.
  sql(
    `UPDATE "AspNetUsers" SET security_stamp=gen_random_uuid()::text ` +
      `WHERE user_name IN (${botNameList})`,
  );
  sql(
    `DELETE FROM "TeamMembers" WHERE user_id IN (` +
      `SELECT id FROM "AspNetUsers" WHERE user_name IN (${botNameList})) ` +
      `AND team_id<>${teamId}`,
  );
  sql(
    `WITH neutral_provisioning AS MATERIALIZED (` +
      `SELECT set_config('rsctf.identity_neutral_insert','1',true)` +
      `) INSERT INTO "TeamMembers"(team_id,user_id) ` +
      `SELECT ${teamId},id FROM "AspNetUsers" account ` +
      `CROSS JOIN neutral_provisioning ` +
      `WHERE account.user_name IN (${botNameList}) AND NOT EXISTS (` +
      `SELECT 1 FROM "TeamMembers" member WHERE member.team_id=${teamId} AND member.user_id=account.id)`,
  );
  sql(
    `INSERT INTO "UserParticipations"(user_id,game_id,team_id,participation_id) ` +
      `SELECT id,${gameId},${teamId},${participationId} FROM "AspNetUsers" ` +
      `WHERE user_name IN (${botNameList}) ` +
      `ON CONFLICT (user_id,game_id) DO UPDATE SET ` +
      `team_id=EXCLUDED.team_id,participation_id=EXCLUDED.participation_id`,
  );
  return parseJsonQuery(
    `SELECT COALESCE(json_agg(json_build_object('id',id,'stamp',security_stamp) ORDER BY user_name),'[]'::json)::text ` +
      `FROM "AspNetUsers" WHERE user_name IN (${botNameList})`,
    "brute-force account query",
  );
}

function teamFlags(challengeId) {
  return parseJsonQuery(
    `SELECT COALESCE(json_agg(json_build_object('pid',instance.participation_id,'flag',context.flag) ` +
      `ORDER BY instance.participation_id),'[]'::json)::text ` +
      `FROM "GameInstances" instance JOIN "FlagContexts" context ON context.id=instance.flag_id ` +
      `WHERE instance.challenge_id=${challengeId}`,
    "team flag query",
  );
}

function actor(state, index, ip) {
  const userId = state.adUsers[index];
  const stamp = state.userStamps[userId];
  if (!userId || !stamp)
    throw new Error(`missing player identity at roster index ${index}`);
  return {
    userId,
    jwt: mintJwt(userId, stamp, 1),
    ip,
    participationId: positiveInteger(
      state.adPartIds[index],
      "participation id",
    ),
  };
}

function buildK6Config(state, challengeId, bots, cohort, runId) {
  const byPid = new Map(
    teamFlags(challengeId).map((row) => [Number(row.pid), row.flag]),
  );
  const stolen = cohort.stolenIndices.map((actorIndex, index) => ({
    ...actor(state, actorIndex, `198.51.100.${10 + index}`),
    victimFlag: byPid.get(Number(state.adPartIds[cohort.victimIndices[index]])),
  }));
  if (stolen.some((entry) => !entry.victimFlag))
    throw new Error("a victim flag is missing");

  const bruteTokens = [];
  const bruteParticipationId = positiveInteger(
    state.adPartIds[cohort.bruteIndex],
    "brute-force participation id",
  );
  for (const [index, bot] of bots.entries()) {
    bruteTokens.push({
      jwt: mintJwt(bot.id, bot.stamp, 1),
      ip: `198.51.100.${20 + index}`,
      participationId: bruteParticipationId,
    });
  }
  if (bruteTokens.length !== BRUTE_ACCOUNTS) {
    throw new Error(
      `brute-force fixture has ${bruteTokens.length}/${BRUTE_ACCOUNTS} accounts`,
    );
  }

  return {
    target: TARGET.replace(/\/$/, ""),
    origin: ORIGIN,
    runId: positiveInteger(runId, "anti-cheat run id"),
    gameId: state.mixGame,
    challengeId,
    stolen,
    brute: {
      tokens: bruteTokens,
      attemptsPerToken: BRUTE_ATTEMPTS_PER_ACCOUNT,
    },
    honeypot: {
      ...actor(state, cohort.honeypotIndex, "198.51.100.30"),
      baits: HONEYPOT_BAITS,
      honeypotUserAgent: `rsctf-cheat-drill/${runId}`,
    },
    clean: cohort.cleanIndices.map((index, offset) =>
      actor(state, index, `203.0.113.${(offset % 240) + 10}`),
    ),
  };
}

function unwrap(response) {
  return response?.json && Object.hasOwn(response.json, "data")
    ? response.json.data
    : response?.json;
}

async function loadReports(gameId) {
  const { sweep, authoritative } =
    await loadAuthoritativeAfterConcurrentSweep(
      (index) =>
        A.api("GET", `/api/game/${gameId}/cheatreport`, {
          jwt: A.adminJwt(),
          ip: `192.0.2.${50 + index}`,
          timeoutMs: 120_000,
        }),
      3,
    );
  for (const response of [...sweep, authoritative]) {
    if (response.status !== 200) {
      throw new Error(
        `cheat report sweep failed: ${response.status} ${response.text?.slice(0, 300)}`,
      );
    }
  }
  return unwrap(authoritative);
}

function eventTypes(record) {
  return new Set((record?.events || []).map((event) => event.type));
}

function expectedScenarioEvidence(config, phase) {
  const expected = new Map();
  for (const entry of config.stolen) {
    expected.set(entry.participationId, {
      role: "stolen",
      codes: CHEAT_SCENARIO_RULES.stolen[phase],
    });
  }
  expected.set(config.brute.tokens[0].participationId, {
    role: "brute",
    // H1 intentionally matures for five minutes so a subsequent solve can
    // suppress it. The retained public drill validates raw/outbox evidence
    // immediately, then waits for the immutable source timestamps to mature.
    codes: phase === "live" ? [] : CHEAT_SCENARIO_RULES.brute[phase],
  });
  if (expected.size !== STOLEN_ACTORS + 1) {
    throw new Error("anti-cheat scenario actors are not distinct");
  }
  return expected;
}

function assertExactScenarioEvidence(config, evidence, phase, configuredWeights) {
  if (!Array.isArray(evidence)) throw new Error(`${phase} suspicion evidence is not an array`);
  const expected = expectedScenarioEvidence(config, phase);
  const byPid = new Map([...expected.keys()].map((pid) => [pid, []]));
  for (const row of evidence) {
    const pid = positiveInteger(row.participationId, "evidence participation id");
    if (byPid.has(pid)) byPid.get(pid).push(row);
  }

  for (const [pid, actor] of expected) {
    const actual = byPid.get(pid);
    const actualCodes = actual.map((row) => {
      const rule = SUSPICION_RULE_BY_KIND.get(Number(row.kind));
      if (!rule) throw new Error(`${phase} evidence for ${pid} has unsupported kind ${row.kind}`);
      const expectedWeight = configuredWeights.get(rule.code) ?? rule.defaultWeight;
      if (Number(row.scoreDelta) !== expectedWeight) {
        throw new Error(
          `${phase} ${rule.code} for ${pid} stored delta ${row.scoreDelta}; expected configured ${expectedWeight}`,
        );
      }
      if (actor.role === "stolen") {
        if (
          Number(row.challengeId) !== Number(config.challengeId) ||
          !/^submission:\d+$/.test(String(row.evidenceKey))
        ) {
          throw new Error(`${phase} stolen-flag evidence for ${pid} has the wrong incident identity`);
        }
      } else if (actor.role === "brute") {
        if (
          Number(row.challengeId) !== Number(config.challengeId) ||
          row.evidenceKey !== `challenge:${config.challengeId}`
        ) {
          throw new Error(`${phase} brute-force evidence for ${pid} has the wrong challenge identity`);
        }
      } else {
        throw new Error(`${phase} evidence has unsupported scenario role ${actor.role}`);
      }
      return rule.code;
    }).sort();
    const expectedCodes = [...actor.codes].sort();
    const allowedCodes = phase === "live"
      ? actor.role === "brute"
        // AutomatedPattern may be produced directly from the fresh cadence,
        // but HighWrongRate must remain absent until its five-minute solve
        // suppression window has matured.
        ? ["AutomatedPattern"]
        : [...CHEAT_SCENARIO_RULES[actor.role].reconciled].sort()
      : expectedCodes;
    const hasRequired = containsMembers(actualCodes, expectedCodes);
    const hasOnlyAllowed = containsMembers(allowedCodes, actualCodes);
    const identities = actual.map((row) => `${row.kind}|${row.evidenceKey}`);
    const hasDuplicateIdentity = new Set(identities).size !== identities.length;
    if (!hasRequired || !hasOnlyAllowed || hasDuplicateIdentity ||
        (phase !== "live" && !sameMembers(actualCodes, expectedCodes))) {
      throw new Error(
        `${phase} ${actor.role} actor ${pid} has [${actualCodes.join(", ")}], ` +
          `${phase === "live" ? "requires" : "expected exactly"} [${expectedCodes.join(", ")}]` +
          `${phase === "live" ? ` and allows only [${allowedCodes.join(", ")}]` : ""}`,
      );
    }
  }

  return true;
}

async function waitForScenarioEvidence(
  config,
  gameId,
  afterId,
  phase,
  configuredWeights,
) {
  const timeoutMs = positiveInteger(
    process.env.CHEAT_RECONCILE_TIMEOUT_MS || 45_000,
    "cheat evidence timeout",
  );
  const deadline = Date.now() + timeoutMs;
  let lastError;
  do {
    const evidence = suspicionEvidence(gameId, afterId);
    try {
      assertExactScenarioEvidence(config, evidence, phase, configuredWeights);
      return evidence;
    } catch (error) {
      lastError = error;
    }
    await sleep(200);
  } while (Date.now() < deadline);
  throw new Error(
    `${phase} anti-cheat evidence did not converge within ${timeoutMs} ms: ${lastError?.message}`,
  );
}

function ledgerSnapshot(gameId) {
  const id = positiveInteger(gameId, "game id");
  return sql(
    `SELECT json_build_object(` +
      `'events',(SELECT COALESCE(json_agg(json_build_array(` +
      `event.id,event.participation_id,event.challenge_id,event.kind,event.evidence_key,` +
      `event.score_delta,event.created_at) ORDER BY event.id),'[]'::json) ` +
      `FROM "SuspicionEvents" event WHERE event.game_id=${id}),` +
      `'scores',(SELECT COALESCE(json_agg(json_build_array(` +
      `participation.id,participation.suspicion_score) ORDER BY participation.id),'[]'::json) ` +
      `FROM "Participations" participation WHERE participation.game_id=${id}),` +
      `'outbox',(SELECT COALESCE(json_agg(json_build_array(` +
      `job.id,job.completed_at_utc,job.attempts,job.last_error,job.lease_token,` +
      `job.lease_expires_at_utc) ORDER BY job.id),'[]'::json) ` +
      `FROM "SuspicionEvaluationOutbox" job WHERE job.game_id=${id}),` +
      `'sources',json_build_array(` +
      `(SELECT count(*) FROM "Submissions" WHERE game_id=${id}),` +
      `(SELECT count(*) FROM "HoneypotHits" WHERE game_id=${id}),` +
      `(SELECT count(*) FROM "IdentityObservations" WHERE game_id=${id}))` +
      `)::text`,
  );
}

async function stableLedgerSnapshot(gameId) {
  let previous = ledgerSnapshot(gameId);
  for (let attempt = 0; attempt < 3; attempt += 1) {
    await sleep(500);
    const current = ledgerSnapshot(gameId);
    if (current === previous) return current;
    previous = current;
  }
  throw new Error("anti-cheat ledger did not settle before the read-only report check");
}

function reportEventSignature(event) {
  return [
    Number(event.eventId ?? event.id),
    event.type,
    Number(event.scoreDelta),
    Number(event.appliedDelta),
    event.tier,
    Boolean(event.counted),
    Number(event.time),
  ].join("|");
}

function assertIndependentReportScoring(gameId, reportRows, configuredWeights) {
  const events = suspicionEvidence(gameId);
  const scoreRows = parseJsonQuery(
    `SELECT COALESCE(json_agg(json_build_object(` +
      `'participationId',id,'score',suspicion_score) ORDER BY id),'[]'::json)::text ` +
      `FROM "Participations" WHERE game_id=${positiveInteger(gameId, "game id")}`,
    "participation suspicion-score query",
  );
  const storedScores = new Map(
    scoreRows.map((row) => [Number(row.participationId), Number(row.score)]),
  );
  const eventsByPid = new Map();
  for (const event of events) {
    const pid = positiveInteger(event.participationId, "evidence participation id");
    if (!eventsByPid.has(pid)) eventsByPid.set(pid, []);
    eventsByPid.get(pid).push(event);
  }

  const reportByPid = new Map();
  for (const row of reportRows) {
    const pid = positiveInteger(row.participationId, "reported participation id");
    if (reportByPid.has(pid)) throw new Error(`cheat report contains duplicate participation ${pid}`);
    reportByPid.set(pid, row);
  }
  const unexpectedRows = [...reportByPid.keys()].filter((pid) => !eventsByPid.has(pid));
  if (unexpectedRows.length) {
    throw new Error(`cheat report contains participations without evidence: ${unexpectedRows.join(", ")}`);
  }

  for (const [pid, participantEvents] of eventsByPid) {
    const expected = computeExpectedBreakdown(participantEvents, configuredWeights);
    const row = reportByPid.get(pid);
    if (!row) throw new Error(`cheat report omitted participation ${pid}`);
    for (const [field, value] of Object.entries({
      score: expected.total,
      band: expected.band,
      hard: expected.hard,
      strong: expected.strong,
      behavioral: expected.behavioral,
      corroboration: expected.corroboration,
    })) {
      if (row[field] !== value) {
        throw new Error(
          `cheat report ${field} for ${pid} is ${row[field]}; independent contract expects ${value}`,
        );
      }
    }
    const actualEvents = (row.events || []).map(reportEventSignature).sort();
    const expectedEvents = expected.events.map(reportEventSignature).sort();
    if (
      actualEvents.length !== expectedEvents.length ||
      actualEvents.some((signature, index) => signature !== expectedEvents[index])
    ) {
      throw new Error(`cheat report event scoring for ${pid} diverges from the independent contract`);
    }
    if (storedScores.get(pid) !== expected.total) {
      throw new Error(
        `stored suspicion score for ${pid} is ${storedScores.get(pid)}; ` +
          `independent tiered scoring expects ${expected.total}`,
      );
    }
  }
  for (const [pid, score] of storedScores) {
    if (!eventsByPid.has(pid) && score !== 0) {
      throw new Error(`participation ${pid} has score ${score} without suspicion evidence`);
    }
  }
  return true;
}

function assertReport(config, report) {
  validateDetectorCapabilities(report?.detectorCapabilities);
  const rows = report?.suspicionList;
  if (!Array.isArray(rows))
    throw new Error("cheat report did not return a suspicion list");
  const byPid = new Map(rows.map((row) => [Number(row.participationId), row]));

  for (const pid of config.stolen.map((entry) => entry.participationId)) {
    const row = byPid.get(pid);
    if (!eventTypes(row).has("StolenFlag")) {
      throw new Error(
        `stolen-flag actor ${pid} is missing StolenFlag evidence`,
      );
    }
  }

  const brutePid = config.brute.tokens[0].participationId;
  const bruteEvents = eventTypes(byPid.get(brutePid));
  if (
    !bruteEvents.has("HighWrongRate") ||
    !bruteEvents.has("AutomatedPattern")
  ) {
    throw new Error(
      `brute-force actor ${brutePid} is missing strong automation evidence`,
    );
  }

  const honeypotPid = positiveInteger(
    config.honeypot.participationId,
    "honeypot participation id",
  );
  const honeypotRow = byPid.get(honeypotPid);
  if (honeypotRow && Number(honeypotRow.score) !== 0) {
    throw new Error(`raw honeypot telemetry actor ${honeypotPid} received a report score`);
  }
  if (
    Number(
      sql(
        `SELECT suspicion_score FROM "Participations" ` +
          `WHERE game_id=${positiveInteger(config.gameId, "honeypot game id")} ` +
          `AND id=${honeypotPid}`,
      ),
    ) !== 0
  ) {
    throw new Error(`raw honeypot telemetry actor ${honeypotPid} received a report score`);
  }

  const clean = new Set(config.clean.map((entry) => entry.participationId));
  const cleanRows = rows.filter((row) =>
    clean.has(Number(row.participationId)),
  );
  const cleanContextCount = cleanRows.filter(
    (row) =>
      ["clean", "context"].includes(row.band) &&
      (row.events || []).every((event) => event.tier === "context"),
  ).length;
  const duplicatePids = rows
    .map((row) => Number(row.participationId))
    .filter((pid, index, all) => all.indexOf(pid) !== index);
  if (duplicatePids.length) {
    throw new Error(`cheat report contains duplicate participation rows: ${duplicatePids.join(", ")}`);
  }
  return { rows, cleanContextCount };
}

function databaseBaseline(gameId) {
  return {
    submissionId: Number(
      sql(
        `SELECT COALESCE(max(id),0) FROM "Submissions" WHERE game_id=${gameId}`,
      ),
    ),
    honeypotHitId: Number(
      sql(
        `SELECT COALESCE(max(id),0) FROM "HoneypotHits"`,
      ),
    ),
    suspicionEventId: Number(
      sql(
        `SELECT COALESCE(max(id),0) FROM "SuspicionEvents" WHERE game_id=${gameId}`,
      ),
    ),
    suspicionOutboxId: Number(
      sql(
        `SELECT COALESCE(max(id),0) FROM "SuspicionEvaluationOutbox"`,
      ),
    ),
  };
}

function bruteFixtureAnswers(config) {
  return Array.from(
    {
      length: config.brute.tokens.length * config.brute.attemptsPerToken,
    },
    (_, attempt) =>
      `flag{invalid_${config.runId}_${attempt % config.brute.tokens.length}_${attempt}}`,
  );
}

function preReportEvidenceCounts(state, challengeId, config, baseline) {
  const gameId = positiveInteger(state.mixGame, "mixed-event game id");
  const brutePid = positiveInteger(
    config.brute.tokens[0].participationId,
    "brute-force participation id",
  );
  const stolenPairs = config.stolen
    .map((entry) =>
      `(${positiveInteger(entry.participationId, "stolen participation id")},` +
        `${literal(entry.victimFlag)})`,
    )
    .join(",");
  const bruteAnswers = bruteFixtureAnswers(config).map(literal).join(",");
  const submissionJobs = Number(
    sql(
      `SELECT count(*) FROM "SuspicionEvaluationOutbox" job ` +
        `JOIN "Submissions" submission ON job.job_kind=0 AND job.source_id=submission.id ` +
        `AND job.game_id=submission.game_id AND job.participation_id=submission.participation_id ` +
        `WHERE job.game_id=${gameId} AND submission.id>${baseline.submissionId} ` +
        `AND submission.challenge_id=${challengeId} AND (` +
        `(submission.participation_id,submission.answer) IN (${stolenPairs}) OR ` +
        `(submission.participation_id=${brutePid} AND submission.answer IN (${bruteAnswers}))) ` +
        `AND job.attempts>=1 AND job.completed_at_utc IS NOT NULL AND job.last_error IS NULL`,
    ),
  );
  const honeypotState = honeypotTelemetryState(config, baseline);
  const honeypotHits = Array.isArray(honeypotState.hits) ? honeypotState.hits : [];
  return {
    stolen: Number(
      sql(
        `SELECT count(*) FROM "Submissions" WHERE game_id=${gameId} ` +
          `AND challenge_id=${challengeId} AND status=3 AND id>${baseline.submissionId} ` +
          `AND (participation_id,answer) IN (${stolenPairs})`,
      ),
    ),
    brute: Number(
      sql(
        `SELECT count(*) FROM "Submissions" WHERE game_id=${gameId} ` +
          `AND challenge_id=${challengeId} AND status=2 AND id>${baseline.submissionId} ` +
          `AND participation_id=${brutePid} AND answer IN (${bruteAnswers})`,
      ),
    ),
    honeypot: honeypotHits.length,
    honeypotAttributed: honeypotHits.filter(
      (row) =>
        String(row[1]).toLowerCase() === config.honeypot.userId.toLowerCase() &&
        row[2] === null &&
        row[3] === null,
    ).length,
    submissionJobs,
    honeypotJobs: Number(honeypotState.outboxJobs),
    honeypotSuspicion: Number(honeypotState.suspicionEvents),
    honeypotScore: Number(honeypotState.storedScore),
  };
}

async function waitForPreReportEvidence(state, challengeId, config, baseline) {
  const expected = {
    stolen: STOLEN_ACTORS,
    brute: BRUTE_ACCOUNTS * BRUTE_ATTEMPTS_PER_ACCOUNT,
    honeypot: HONEYPOT_BAITS.length,
    honeypotAttributed: HONEYPOT_BAITS.length,
    submissionJobs: STOLEN_ACTORS + BRUTE_ACCOUNTS * BRUTE_ATTEMPTS_PER_ACCOUNT,
    honeypotJobs: 0,
    honeypotSuspicion: 0,
    honeypotScore: 0,
  };
  const deadline = Date.now() + positiveInteger(
    process.env.CHEAT_RECONCILE_TIMEOUT_MS || 45_000,
    "pre-report evidence timeout",
  );
  let observed;
  do {
    observed = preReportEvidenceCounts(state, challengeId, config, baseline);
    const oversized = Object.keys(expected).filter((key) => observed[key] > expected[key]);
    if (oversized.length) {
      throw new Error(`pre-report fixture has unexpected extra evidence: ${oversized.join(", ")}`);
    }
    if (Object.keys(expected).every((key) => observed[key] === expected[key])) {
      assertHoneypotTelemetry(config, baseline);
      return observed;
    }
    await sleep(200);
  } while (Date.now() < deadline);
  throw new Error(
    `pre-report raw/outbox evidence did not converge: ${JSON.stringify(observed)}; ` +
      `expected ${JSON.stringify(expected)}`,
  );
}

async function awaitBruteFixtureMaturity(state, challengeId, config, baseline) {
  const gameId = positiveInteger(state.mixGame, "mixed-event game id");
  const brutePid = positiveInteger(
    config.brute.tokens[0].participationId,
    "brute-force participation id",
  );
  const answers = bruteFixtureAnswers(config).map(literal).join(",");
  const expected = BRUTE_ACCOUNTS * BRUTE_ATTEMPTS_PER_ACCOUNT;
  const fixture = parseJsonQuery(
    `SELECT json_build_object(` +
      `'submissions',count(*),` +
      `'anchorMs',floor(extract(epoch from min(submission.submit_time_utc))*1000)::bigint,` +
      `'eventEndMs',floor(extract(epoch from min(game.end_time_utc))*1000)::bigint` +
      `)::text FROM "Submissions" submission JOIN "Games" game ` +
      `ON game.id=submission.game_id WHERE submission.game_id=${gameId} ` +
      `AND submission.id>${baseline.submissionId} AND submission.challenge_id=${challengeId} ` +
      `AND submission.participation_id=${brutePid} AND submission.status=2 ` +
      `AND submission.answer IN (${answers})`,
    "immutable retained brute-force fixture",
  );
  const anchorMs = Number(fixture.anchorMs);
  const eventEndMs = Number(fixture.eventEndMs);
  const maturityMs = anchorMs + 5 * 60 * 1000;
  if (
    Number(fixture.submissions) !== expected ||
    !Number.isSafeInteger(anchorMs) ||
    !Number.isSafeInteger(eventEndMs) ||
    maturityMs >= eventEndMs
  ) {
    throw new Error(
      "the retained H1 fixture is incomplete or cannot mature before the event ends",
    );
  }
  while (Date.now() < maturityMs) {
    await sleep(Math.min(1_000, Math.max(1, maturityMs - Date.now())));
  }
}

function assertDatabase(state, challengeId, config, baseline) {
  const gameId = Number(state.mixGame);
  const cleanIds = config.clean
    .map((entry) =>
      positiveInteger(entry.participationId, "clean participation id"),
    )
    .join(",");
  const brutePid = positiveInteger(
    config.brute.tokens[0].participationId,
    "brute-force participation id",
  );
  const honeypotPid = positiveInteger(
    config.honeypot.participationId,
    "honeypot participation id",
  );
  const stolenPairs = config.stolen
    .map(
      (entry) =>
        `(${positiveInteger(entry.participationId, "stolen-flag participation id")},${literal(entry.victimFlag)})`,
    )
    .join(",");
  const bruteAnswers = bruteFixtureAnswers(config);
  const bruteAnswerList = bruteAnswers.map(literal).join(",");
  const honeypotBaitList = config.honeypot.baits.map(literal).join(",");
  const challengeEvidenceKey = literal(`challenge:${challengeId}`);
  const checks = {
    "stolen submissions":
      Number(
        sql(
          `SELECT count(*) FROM "Submissions" WHERE game_id=${gameId} ` +
            `AND challenge_id=${challengeId} AND status=3 AND id>${baseline.submissionId} ` +
            `AND (participation_id,answer) IN (${stolenPairs})`,
        ),
      ) === STOLEN_ACTORS,
    "distinct stolen actors and answers":
      Number(
        sql(
          `SELECT count(DISTINCT (participation_id,answer)) FROM "Submissions" ` +
            `WHERE game_id=${gameId} AND challenge_id=${challengeId} AND status=3 ` +
            `AND id>${baseline.submissionId} ` +
            `AND (participation_id,answer) IN (${stolenPairs})`,
        ),
      ) === STOLEN_ACTORS,
    "brute-force submissions":
      Number(
        sql(
          `SELECT count(*) FROM "Submissions" WHERE game_id=${gameId} ` +
            `AND challenge_id=${challengeId} AND participation_id=${brutePid} ` +
            `AND status=2 AND id>${baseline.submissionId} AND answer IN (${bruteAnswerList})`,
        ),
      ) ===
      BRUTE_ACCOUNTS * BRUTE_ATTEMPTS_PER_ACCOUNT,
    "distinct brute-force answers":
      Number(
        sql(
          `SELECT count(DISTINCT answer) FROM "Submissions" WHERE game_id=${gameId} ` +
            `AND challenge_id=${challengeId} AND participation_id=${brutePid} ` +
            `AND status=2 AND id>${baseline.submissionId} AND answer IN (${bruteAnswerList})`,
        ),
      ) === bruteAnswers.length,
    "honeypot row count":
      Number(
        sql(
          `SELECT count(*) FROM "HoneypotHits" WHERE id>${baseline.honeypotHitId} ` +
            `AND user_agent=${literal(config.honeypot.honeypotUserAgent)}`,
        ),
      ) === HONEYPOT_BAITS.length,
    "honeypot bait coverage":
      Number(
        sql(
          `SELECT count(DISTINCT bait) FROM "HoneypotHits" ` +
            `WHERE id>${baseline.honeypotHitId} ` +
            `AND user_agent=${literal(config.honeypot.honeypotUserAgent)} ` +
            `AND bait IN (${honeypotBaitList})`,
        ),
      ) === HONEYPOT_BAITS.length,
    "honeypot attribution isolation":
      Number(
        sql(
          `SELECT count(*) FROM "HoneypotHits" WHERE id>${baseline.honeypotHitId} ` +
            `AND user_agent=${literal(config.honeypot.honeypotUserAgent)} ` +
            `AND user_id=${literal(config.honeypot.userId)}::uuid ` +
            `AND game_id IS NULL AND participation_id IS NULL`,
        ),
      ) === HONEYPOT_BAITS.length,
    "honeypot outbox absent":
      Number(
        sql(
          `SELECT count(*) FROM "SuspicionEvaluationOutbox" job ` +
            `WHERE job.id>${baseline.suspicionOutboxId} AND (` +
            `job.rule_kind IN (28,29,31) OR EXISTS (` +
            `SELECT 1 FROM "HoneypotHits" hit WHERE job.source_kind=1 ` +
            `AND job.source_id=hit.id AND hit.id>${baseline.honeypotHitId} ` +
            `AND hit.user_agent=${literal(config.honeypot.honeypotUserAgent)}))`,
        ),
      ) === 0,
    "honeypot suspicion absent":
      Number(
        sql(
          `SELECT count(*) FROM "SuspicionEvents" WHERE id>${baseline.suspicionEventId} ` +
            `AND kind IN (28,29,31)`,
        ),
      ) === 0 &&
      Number(
        sql(
          `SELECT suspicion_score FROM "Participations" ` +
            `WHERE game_id=${gameId} AND id=${honeypotPid}`,
        ),
      ) === 0,
    "current stolen-flag evidence":
      Number(
        sql(
          `SELECT count(*) FROM "SuspicionEvents" event ` +
            `JOIN "Submissions" submission ON ` +
            `event.evidence_key='submission:'||submission.id::text ` +
            `AND event.participation_id=submission.participation_id ` +
            `AND event.challenge_id=submission.challenge_id ` +
            `WHERE event.game_id=${gameId} AND event.id>${baseline.suspicionEventId} ` +
            `AND event.kind=${EVIDENCE_KIND.stolenFlag} ` +
            `AND submission.id>${baseline.submissionId} ` +
            `AND submission.challenge_id=${challengeId} ` +
            `AND (submission.participation_id,submission.answer) IN (${stolenPairs})`,
        ),
      ) === STOLEN_ACTORS,
    "current high-wrong-rate evidence":
      Number(
        sql(
          `SELECT count(*) FROM "SuspicionEvents" WHERE game_id=${gameId} ` +
            `AND id>${baseline.suspicionEventId} AND participation_id=${brutePid} ` +
            `AND challenge_id=${challengeId} AND kind=${EVIDENCE_KIND.highWrongRate} ` +
            `AND evidence_key=${challengeEvidenceKey}`,
        ),
      ) === 1,
    "current automated-pattern evidence":
      Number(
        sql(
          `SELECT count(*) FROM "SuspicionEvents" WHERE game_id=${gameId} ` +
            `AND id>${baseline.suspicionEventId} AND participation_id=${brutePid} ` +
            `AND challenge_id=${challengeId} AND kind=${EVIDENCE_KIND.automatedPattern} ` +
            `AND evidence_key=${challengeEvidenceKey}`,
        ),
      ) === 1,
    "duplicate suspicion evidence":
      Number(
        sql(
          `SELECT count(*) FROM (` +
            `SELECT game_id,participation_id,kind,evidence_key FROM "SuspicionEvents" WHERE game_id=${gameId} ` +
            `GROUP BY game_id,participation_id,kind,evidence_key HAVING count(*)>1) duplicate`,
        ),
      ) === 0,
    "clean-control actionable suspicion":
      Number(
        sql(
          `SELECT count(*) FROM "SuspicionEvents" ` +
            `WHERE game_id=${gameId} AND participation_id IN (${cleanIds}) ` +
            `AND id>${baseline.suspicionEventId} ` +
            `AND kind NOT IN (${CONTEXT_KINDS.join(",")})`,
        ),
      ) === 0,
  };
  const failed = Object.entries(checks)
    .filter(([, passed]) => !passed)
    .map(([name]) => name);
  if (failed.length)
    throw new Error(`anti-cheat database checks failed: ${failed.join(", ")}`);
  return checks;
}

async function main() {
  requireOptIn();
  orchestrationLock = await acquireExclusiveProcessLock(
    loadOrchestrationLockPath,
    {
      label: "RSCTF anti-cheat drill",
      inheritedToken: inheritedCheatOrchestrationToken(process.env, RETENTION),
      metadata: { stateTag: process.env.LIFECYCLE_STATE_TAG || null },
    },
  );
  await A.preflight();
  const state = A.readState();
  if (!state || state.recovery || !Array.isArray(state.adPartIds)) {
    throw new Error(
      "provision a healthy lifecycle namespace before running the cheat drill",
    );
  }
  if (
    state.adPartIds.length < REQUIRED_TEAMS ||
    state.adUsers.length !== state.adPartIds.length
  ) {
    throw new Error(
      `the retained cheat drill requires at least ${REQUIRED_TEAMS} complete mixed-event teams`,
    );
  }
  const competitionRunId = process.env.COMPETITION_RUN_ID;
  if (RETENTION.integrated && competitionRunId !== state.competitionRunId) {
    throw new Error('integrated anti-cheat child is not bound to the active competition run');
  }

  // Freeze the complete non-offender complement and the evidence boundary
  // before the drill creates a challenge, account, submission, or detector row.
  const cohort = chooseCohort(state);
  const baseline = databaseBaseline(state.mixGame);
  const challengeId = await ensureAuditChallenge(state);
  const current = A.readState();
  const bots = ensureBruteAccounts(current, cohort.bruteIndex);
  const config = buildK6Config(current, challengeId, bots, cohort, A.nowMs());
  const temporary = mkdtempSync(join(tmpdir(), "rsctf-cheat-event-"));
  activeTemporaryDirectory = temporary;
  const configPath = join(temporary, "input.json");
  try {
    writeFileSync(configPath, JSON.stringify(config), { mode: 0o600 });
    const result = await runK6Async("cheat-event.js", {
      CHEAT_CONFIG: configPath,
    }, temporary);
    if (shutdownSignal) {
      throw new Error(`cheat-event interrupted by ${shutdownSignal}`);
    }
    if (result.code !== 0) {
      throw new Error(
        `cheat-event k6 exited with ${result.signal || `status ${result.code}`}`,
      );
    }
  } finally {
    rmSync(temporary, { recursive: true, force: true });
    activeTemporaryDirectory = null;
  }

  const configuredWeights = effectiveRuleWeights(configuredSuspicionRules());
  await waitForScenarioEvidence(
    config,
    current.mixGame,
    baseline.suspicionEventId,
    "live",
    configuredWeights,
  );
  await waitForPreReportEvidence(current, challengeId, config, baseline);

  // HighWrongRate intentionally waits five minutes for a suppressing solve.
  // Canonical submissions are append-only. Wait for their source timestamps
  // rather than mutating the evidence ledger to accelerate reconciliation.
  await awaitBruteFixtureMaturity(current, challengeId, config, baseline);

  await waitForScenarioEvidence(
    config,
    current.mixGame,
    baseline.suspicionEventId,
    "reconciled",
    configuredWeights,
  );
  const ledgerBeforeReport = await stableLedgerSnapshot(current.mixGame);
  const honeypotBeforeReport = JSON.stringify(
    assertHoneypotTelemetry(config, baseline),
  );
  const report = await loadReports(current.mixGame);
  const ledgerAfterReport = ledgerSnapshot(current.mixGame);
  const honeypotAfterReport = JSON.stringify(
    assertHoneypotTelemetry(config, baseline),
  );
  if (
    ledgerAfterReport !== ledgerBeforeReport ||
    honeypotAfterReport !== honeypotBeforeReport
  ) {
    throw new Error("GET /cheatreport changed sources, evidence, scores, or outbox state");
  }
  const reportResult = assertReport(config, report);
  assertExactScenarioEvidence(
    config,
    suspicionEvidence(current.mixGame, baseline.suspicionEventId),
    "reconciled",
    configuredWeights,
  );
  assertIndependentReportScoring(
    current.mixGame,
    reportResult.rows,
    configuredWeights,
  );
  const integrity = {
    "live exact offender evidence": true,
    "reconciled exact offender evidence": true,
    "configured scoring contract": true,
    ...assertDatabase(
      current,
      challengeId,
      config,
      baseline,
    ),
  };
  const completedAtMs = A.nowMs();
  const offenderPids = [
    ...config.stolen.map((entry) => entry.participationId),
    config.brute.tokens[0].participationId,
  ];
  const simulation = {
    challengeId,
    completed: true,
    completedAtMs,
    offenderPids,
    cleanControlCount: config.clean.length,
    suspicionRows: reportResult.rows.length,
    cleanContextCount: reportResult.cleanContextCount,
    integrity,
  };
  const completedState = recordCheatSimulation(current, simulation, RETENTION);
  if (RETENTION.integrated) {
    writeCheatResult(process.env.RSCTF_CHEAT_RESULT_PATH, {
      schemaVersion: CHEAT_RESULT_SCHEMA_VERSION,
      runId: competitionRunId,
      gameId: current.mixGame,
      eventCreatedAtMs: current.createdAtMs,
      ...simulation,
    });
  } else {
    A.writeState(completedState);
  }

  const base = ORIGIN.replace(/\/$/, "");
  console.log(
    `anti-cheat drill passed; ${completedState.retained === true ? "retained " : ""}mixed event ${current.mixGame}, challenge ${challengeId}`,
  );
  console.log(
    `offenders: ${offenderPids.join(", ")}; clean controls: ${config.clean.length}`,
  );
  console.log(
    `admin evidence: ${base}/games/${current.mixGame}/monitor/CheatCheck?tab=analysis`,
  );
  console.log(
    `submissions: ${base}/games/${current.mixGame}/monitor/Submissions`,
  );
  console.log(`event view: ${base}/games/${current.mixGame}/challenges`);
  if (completedState.retained === true) {
    console.log(
      "the lifecycle namespace was retained; deletion now requires DELETE_RETAINED_EVENT=1",
    );
  } else {
    console.log("the lifecycle parent controls cleanup for this embedded drill");
  }
}

main()
  .catch((error) => {
    console.error(error?.stack || error);
    process.exitCode = 1;
  })
  .finally(async () => {
    await orchestrationLock?.release();
  });
