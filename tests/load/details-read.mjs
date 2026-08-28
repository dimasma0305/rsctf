// Mint accepted-player JWTs and exercise the challenge-details poll at a fixed rate.
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { discover, GAME, runK6, TARGET } from "./lib.mjs";

const game = Number(GAME);
if (!Number.isSafeInteger(game) || game <= 0)
  throw new Error("GAME must be a positive integer");

async function assertHealth(stage) {
  const response = await fetch(`${TARGET}/healthz`);
  const body = await response.text();
  if (response.status !== 200 || body !== "ok") {
    throw new Error(
      `${stage} health check failed: HTTP ${response.status}, body ${JSON.stringify(body)}`,
    );
  }
}

await assertHealth("pre-load");
const tokens = discover().tokens.slice(0, 4000);
if (tokens.length === 0) {
  throw new Error(`game ${game} has no accepted-participation users`);
}

console.log(
  `challenge-details load → ${TARGET} game=${game} rate=${process.env.RATE || 10}/s ` +
    `players=${tokens.length}`,
);
const tokenDirectory = mkdtempSync(join(tmpdir(), "rsctf-details-read-"));
const tokenFile = join(tokenDirectory, "tokens.json");
writeFileSync(tokenFile, JSON.stringify(tokens), { mode: 0o600 });

let status = 1;
try {
  status = runK6("details-read.js", {
    TARGET,
    GAME: game,
    TOKENS_FILE: tokenFile,
    RATE: process.env.RATE || 10,
    VUS: process.env.VUS || 20,
    DURATION: process.env.DURATION || "30s",
    REQUIRE_FIXED_PROJECTION: process.env.REQUIRE_FIXED_PROJECTION || "1",
    SUMMARY_JSON: process.env.SUMMARY_JSON || "",
  });
  await assertHealth("post-load");
} finally {
  rmSync(tokenDirectory, { recursive: true, force: true });
}
process.exit(status);
