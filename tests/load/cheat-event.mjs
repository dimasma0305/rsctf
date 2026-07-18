// Deterministic anti-cheat drill for a retained lifecycle event.
//
// This runner only targets the load-test namespace from provision.mjs. It creates
// one dynamic-flag audit challenge, gives every existing team a unique flag, and
// drives three known-bad behaviours through the public HTTP surface:
//   * four teams submit another team's valid flag;
//   * one team coordinates 40 machine-speed wrong submissions across five accounts;
//   * one team follows three authenticated same-origin honeypot routes.
// Each run takes six actors without prior actionable evidence, then freezes every
// other roster member as a clean control. Only post-baseline evidence is judged,
// so ordinary-play history cannot hide a drill false positive or satisfy its actor
// gates. Credentials live only in a temporary k6 input file.
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

import * as A from "./applib.mjs";
import { freezeCheatCohort } from "./cheat-cohort.js";
import { cheatK6Environment } from "./cheat-environment.js";
import {
  cheatRetentionPolicy,
  inheritedCheatOrchestrationToken,
  recordCheatSimulation,
} from "./cheat-retention.js";
import { writeCheatResult } from "./cheat-result.js";
import {
  acquireExclusiveProcessLock,
  loadOrchestrationLockPath,
} from "./process-control.mjs";
import { loadAuthoritativeAfterConcurrentSweep } from "./report-convergence.js";
import { TARGET, mintJwt, sql } from "./lib.mjs";

const REQUIRED_TEAMS = 100;
const RETENTION = cheatRetentionPolicy(process.env);
const STOLEN_ACTORS = 4;
const BRUTE_ACCOUNTS = 5;
const BRUTE_ATTEMPTS_PER_ACCOUNT = 8;
const HONEYPOT_BAITS = ["/.env", "/.git/config", "/wp-login.php"];
const CONTEXT_KINDS = [1, 2, 3, 4, 5, 6, 22, 23, 26, 32, 37];
const EVIDENCE_KIND = Object.freeze({
  stolenFlag: 0,
  highWrongRate: 24,
  automatedPattern: 25,
  honeypotHit: 28,
  honeypotChain: 31,
});
const ORIGIN = process.env.ORIGIN || "https://tcp.1pc.tf";

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
  const offenderCount = STOLEN_ACTORS + 2;
  const { offenderIndices, cleanIndices } = freezeCheatCohort(
    state.adPartIds,
    actionable,
    offenderCount,
  );
  const offenderSet = new Set(offenderIndices);
  const victimIndices = state.adPartIds
    .map((_, index) => index)
    .filter((index) => !offenderSet.has(index))
    .slice(0, STOLEN_ACTORS);
  if (victimIndices.length !== STOLEN_ACTORS) {
    throw new Error(
      "the anti-cheat fixture does not have enough distinct victims",
    );
  }

  return {
    stolenIndices: offenderIndices.slice(0, STOLEN_ACTORS),
    bruteIndex: offenderIndices[STOLEN_ACTORS],
    honeypotIndex: offenderIndices[STOLEN_ACTORS + 1],
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
    `INSERT INTO "AspNetUsers" ` +
      `(id,user_name,normalized_user_name,email,normalized_email,email_confirmed,password_hash,` +
      `security_stamp,concurrency_stamp,role,register_time_utc,last_signed_in_utc,last_visited_utc,` +
      `lockout_enabled,access_failed_count,phone_number_confirmed,two_factor_enabled,ip,bio,real_name,std_number,exercise_visible) ` +
      `SELECT gen_random_uuid(),${literal(prefix)}||g,upper(${literal(prefix)}||g),` +
      `${literal(prefix)}||g||'@load.test',upper(${literal(prefix)}||g||'@load.test'),` +
      `true,'x-load-placeholder',gen_random_uuid()::text,gen_random_uuid()::text,1,` +
      `now(),now(),now(),true,0,false,false,'0.0.0.0','','','',false ` +
      `FROM generate_series(1,${BRUTE_ACCOUNTS}) g ` +
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
    `INSERT INTO "TeamMembers"(team_id,user_id) ` +
      `SELECT ${teamId},id FROM "AspNetUsers" account ` +
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

function assertReport(config, report) {
  const rows = report?.suspicionList;
  if (!Array.isArray(rows))
    throw new Error("cheat report did not return a suspicion list");
  const byPid = new Map(rows.map((row) => [Number(row.participationId), row]));

  for (const pid of config.stolen.map((entry) => entry.participationId)) {
    const row = byPid.get(pid);
    if (row?.band !== "evidenced" || !eventTypes(row).has("StolenFlag")) {
      throw new Error(
        `stolen-flag actor ${pid} was not classified as evidenced`,
      );
    }
  }

  const brutePid = config.brute.tokens[0].participationId;
  const bruteEvents = eventTypes(byPid.get(brutePid));
  if (
    byPid.get(brutePid)?.band !== "investigate" ||
    !bruteEvents.has("HighWrongRate") ||
    !bruteEvents.has("AutomatedPattern")
  ) {
    throw new Error(
      `brute-force actor ${brutePid} is missing strong automation evidence`,
    );
  }

  const honeypotPid = config.honeypot.participationId;
  const honeypotEvents = eventTypes(byPid.get(honeypotPid));
  if (
    byPid.get(honeypotPid)?.band !== "investigate" ||
    !honeypotEvents.has("HoneypotHit") ||
    !honeypotEvents.has("HoneypotChain")
  ) {
    throw new Error(
      `scanner actor ${honeypotPid} is missing honeypot-chain evidence`,
    );
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
        `SELECT COALESCE(max(id),0) FROM "HoneypotHits" WHERE game_id=${gameId}`,
      ),
    ),
    suspicionEventId: Number(
      sql(
        `SELECT COALESCE(max(id),0) FROM "SuspicionEvents" WHERE game_id=${gameId}`,
      ),
    ),
  };
}

function assertDatabase(state, challengeId, config, reportRows, baseline) {
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
  const bruteAnswers = Array.from(
    {
      length: config.brute.tokens.length * config.brute.attemptsPerToken,
    },
    (_, attempt) =>
      `flag{invalid_${config.runId}_${attempt % config.brute.tokens.length}_${attempt}}`,
  );
  const bruteAnswerList = bruteAnswers.map(literal).join(",");
  const honeypotBaitList = config.honeypot.baits.map(literal).join(",");
  const challengeEvidenceKey = literal(`challenge:${challengeId}`);
  const expectedScores = reportRows
    .map((row) => {
      const participationId = positiveInteger(
        row.participationId,
        "reported participation id",
      );
      const score = (row.events || []).reduce((total, event) => {
        const delta = Number(event.scoreDelta);
        if (!Number.isSafeInteger(delta) || delta < 0) {
          throw new Error(
            `invalid score delta for participation ${participationId}: ${event.scoreDelta}`,
          );
        }
        return total + delta;
      }, 0);
      return `(${participationId},${score})`;
    })
    .join(",");
  const expectedScoreCte = expectedScores || "(NULL::integer,NULL::bigint)";
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
          `SELECT count(*) FROM "HoneypotHits" WHERE game_id=${gameId} ` +
            `AND participation_id=${honeypotPid} AND id>${baseline.honeypotHitId}`,
        ),
      ) === HONEYPOT_BAITS.length,
    "honeypot bait coverage":
      Number(
        sql(
          `SELECT count(DISTINCT bait) FROM "HoneypotHits" ` +
            `WHERE game_id=${gameId} AND participation_id=${honeypotPid} ` +
            `AND id>${baseline.honeypotHitId} AND bait IN (${honeypotBaitList})`,
        ),
      ) === HONEYPOT_BAITS.length,
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
    "current honeypot-hit evidence":
      Number(
        sql(
          `SELECT count(*) FROM "SuspicionEvents" WHERE game_id=${gameId} ` +
            `AND id>${baseline.suspicionEventId} AND participation_id=${honeypotPid} ` +
            `AND challenge_id IS NULL AND kind=${EVIDENCE_KIND.honeypotHit} ` +
            `AND evidence_key='global'`,
        ),
      ) === 1,
    "current honeypot-chain evidence":
      Number(
        sql(
          `SELECT count(*) FROM "SuspicionEvents" WHERE game_id=${gameId} ` +
            `AND id>${baseline.suspicionEventId} AND participation_id=${honeypotPid} ` +
            `AND challenge_id IS NULL AND kind=${EVIDENCE_KIND.honeypotChain} ` +
            `AND evidence_key='global'`,
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
    "suspicion score matches evidence":
      Number(
        sql(
          `WITH expected(participation_id,score) AS (VALUES ${expectedScoreCte}) ` +
            `SELECT count(*) FROM "Participations" participation ` +
            `LEFT JOIN expected ON expected.participation_id=participation.id ` +
            `WHERE participation.game_id=${gameId} ` +
            `AND participation.suspicion_score<>COALESCE(expected.score,0)`,
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

  const report = await loadReports(current.mixGame);
  const reportResult = assertReport(config, report);
  const integrity = assertDatabase(
    current,
    challengeId,
    config,
    reportResult.rows,
    baseline,
  );
  const completedAtMs = A.nowMs();
  const offenderPids = [
    ...config.stolen.map((entry) => entry.participationId),
    config.brute.tokens[0].participationId,
    config.honeypot.participationId,
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
      schemaVersion: 3,
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
