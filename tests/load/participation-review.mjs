// Read-only fixed-rate acceptance for a 12k-team participation review event.
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  LOAD_DATABASE_URL,
  PG_DATABASE,
  TARGET,
  mintJwt,
  runK6,
  sql,
} from "./lib.mjs";

const origin = String(TARGET).replace(/\/+$/, "");
const databaseName = LOAD_DATABASE_URL
  ? new URL(LOAD_DATABASE_URL).pathname.slice(1)
  : PG_DATABASE;
if (!/(?:test|acceptance|load)/i.test(databaseName)) {
  throw new Error(`participation review database must contain test, acceptance, or load (got ${databaseName})`);
}
if (!/^https?:\/\/(?:127\.0\.0\.1|localhost|\[::1\])(?::\d+)?$/i.test(origin)) {
  if (process.env.ALLOW_REMOTE_PARTICIPATION_REVIEW !== origin) {
    throw new Error(`remote participation review load requires ALLOW_REMOTE_PARTICIPATION_REVIEW=${origin}`);
  }
}

const explicitGame = process.env.PARTICIPATION_REVIEW_GAME || process.env.GAME;
const game = explicitGame
  ? Number(explicitGame)
  : Number(
      sql(
        `SELECT game_id FROM "Participations" GROUP BY game_id ` +
          `HAVING COUNT(*) >= 12000 ORDER BY COUNT(*) DESC, game_id LIMIT 1`,
      ),
    );
if (!Number.isSafeInteger(game) || game <= 0) {
  throw new Error("PARTICIPATION_REVIEW_GAME (or GAME) must identify a 12k-team event");
}

const participationCount = Number(
  sql(`SELECT COUNT(*) FROM "Participations" WHERE game_id=${game}`),
);
if (!Number.isSafeInteger(participationCount) || participationCount < 12_000) {
  throw new Error(`participation review requires 12,000 teams; game ${game} has ${participationCount}`);
}
const participation = Number(
  sql(
    `SELECT id FROM "Participations" WHERE game_id=${game} ` +
      `ORDER BY team_id, id LIMIT 1`,
  ),
);
const divisionValue = sql(
  `SELECT division_id FROM "Participations" WHERE game_id=${game} ` +
    `AND division_id IS NOT NULL ORDER BY division_id LIMIT 1`,
);
const division = divisionValue ? Number(divisionValue) : null;
if (!Number.isSafeInteger(participation) || participation <= 0) {
  throw new Error(`game ${game} has no valid participation fixture`);
}
if (division !== null && (!Number.isSafeInteger(division) || division <= 0)) {
  throw new Error(`game ${game} returned an invalid division fixture`);
}

const account = sql(
  `SELECT id::text || '|' || security_stamp || '|' || role::text FROM (` +
    `SELECT account.id, account.security_stamp, account.role, 0 AS priority ` +
    `FROM "GameManagers" manager JOIN "AspNetUsers" account ON account.id=manager.user_id ` +
    `WHERE manager.game_id=${game} AND account.security_stamp IS NOT NULL AND account.role IN (1,2,3) ` +
    `UNION ALL ` +
    `SELECT account.id, account.security_stamp, account.role, 1 AS priority ` +
    `FROM "AspNetUsers" account WHERE account.role=3 AND account.security_stamp IS NOT NULL` +
    `) authorized ORDER BY priority, id LIMIT 1`,
);
if (!account) throw new Error(`game ${game} needs one manager or Admin account`);
const [accountId, securityStamp, roleText] = account.split("|");
const token = mintJwt(accountId, securityStamp, Number(roleText));

const tokenDirectory = mkdtempSync(join(tmpdir(), "rsctf-participation-review-"));
const tokenFile = join(tokenDirectory, "token.json");
writeFileSync(tokenFile, JSON.stringify(token), { mode: 0o600 });

async function assertHealth(stage) {
  const response = await fetch(`${origin}/healthz`);
  const body = await response.text();
  if (response.status !== 200 || body !== "ok") {
    throw new Error(`${stage} health check failed: HTTP ${response.status}, body ${JSON.stringify(body)}`);
  }
}

let status = 1;
try {
  await assertHealth("pre-load");
  status = runK6("participation-review.js", {
    TARGET: origin,
    GAME: game,
    PARTICIPATION: participation,
    DIVISION: division ?? "",
    PARTICIPATION_REVIEW_TOKEN_FILE: tokenFile,
    RATE: process.env.RATE || 2,
    VUS: process.env.VUS || 4,
    DURATION: process.env.DURATION || "30s",
    MAX_P95_MS: process.env.MAX_P95_MS || 1000,
    SUMMARY_JSON: process.env.SUMMARY_JSON || "",
  });
  await assertHealth("post-load");
} finally {
  rmSync(tokenDirectory, { recursive: true, force: true });
}

process.exit(status);
