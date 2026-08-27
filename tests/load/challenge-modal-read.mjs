// Discover one real challenge and run bounded modal reads at a fixed arrival rate.
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { discover, GAME, runK6, sql, TARGET } from "./lib.mjs";

const game = Number(GAME);
if (!Number.isSafeInteger(game) || game <= 0)
  throw new Error("GAME must be a positive integer");

async function assertHealth(stage) {
  const response = await fetch(`${TARGET}/healthz`);
  const body = await response.text();
  if (response.status !== 200 || body !== "ok")
    throw new Error(
      `${stage} health check failed: HTTP ${response.status}, body ${JSON.stringify(body)}`,
    );
}

await assertHealth("pre-load");
const tokens = discover().tokens;
if (tokens.length === 0)
  throw new Error(`game ${game} has no accepted-participation users`);

const challenge = Number(
  process.env.CHALLENGE ||
    sql(
      `SELECT challenge.id FROM "GameChallenges" challenge ` +
        `LEFT JOIN "FirstSolves" solve ON solve.challenge_id=challenge.id ` +
        `WHERE challenge.game_id=${game} AND challenge.is_enabled AND challenge.review_status=0 ` +
        `GROUP BY challenge.id ORDER BY COUNT(solve.participation_id) DESC, challenge.id LIMIT 1`,
    ),
);
if (!Number.isSafeInteger(challenge) || challenge <= 0)
  throw new Error(`game ${game} has no visible challenge for the modal read`);

console.log(
  `challenge-modal load → ${TARGET} game=${game} challenge=${challenge} ` +
    `rate=${process.env.RATE || 10}/s players=${tokens.length}`,
);
const tokenDirectory = mkdtempSync(
  join(tmpdir(), "rsctf-challenge-modal-read-"),
);
const tokenFile = join(tokenDirectory, "tokens.json");
writeFileSync(tokenFile, JSON.stringify(tokens), { mode: 0o600 });

let status = 1;
try {
  status = runK6("challenge-modal-read.js", {
    TARGET,
    GAME: game,
    CHALLENGE: challenge,
    TOKENS_FILE: tokenFile,
    RATE: process.env.RATE || 10,
    VUS: process.env.VUS || 20,
    DURATION: process.env.DURATION || "30s",
    SUMMARY_JSON: process.env.SUMMARY_JSON || "",
  });
  await assertHealth("post-load");
} finally {
  rmSync(tokenDirectory, { recursive: true, force: true });
}
process.exit(status);
