// Fixed-rate attachment range benchmark. The hash must reference an existing
// static challenge attachment; the runner mints short-lived participant JWTs locally and
// never writes them to the repository or command line.
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { assetHashFromPath } from "./asset-download-model.js";
import { mintJwt, runK6, sql, TARGET } from "./lib.mjs";

const ASSET_URL = String(process.env.ASSET_URL || "").trim();
const hash = assetHashFromPath(ASSET_URL);
if (!hash) {
  throw new Error(
    "ASSET_URL must be a same-origin /assets/<64-hex-hash>/<filename> path",
  );
}
const discoveredSize = Number(
  sql(
    `SELECT file_size FROM "Files" WHERE hash='${hash}' AND reference_count > 0 LIMIT 1`,
  ),
);
const ASSET_SIZE = Number(process.env.ASSET_SIZE || discoveredSize);
if (!Number.isSafeInteger(ASSET_SIZE) || ASSET_SIZE <= 0) {
  throw new Error(
    `ASSET_SIZE must be a positive safe integer; database returned ${discoveredSize}`,
  );
}

const TOKEN_COUNT = Number(process.env.TOKEN_COUNT || 100);
if (
  !Number.isSafeInteger(TOKEN_COUNT) ||
  TOKEN_COUNT < 1 ||
  TOKEN_COUNT > 1000
) {
  throw new Error("TOKEN_COUNT must be between 1 and 1000");
}
const accounts = sql(
  `WITH target_games AS (
       SELECT DISTINCT challenge.game_id
         FROM "Files" file
         JOIN "Attachments" attachment ON attachment.local_file_id = file.id
         JOIN "GameChallenges" challenge ON challenge.attachment_id = attachment.id
        WHERE file.hash = '${hash}'
          AND file.reference_count > 0
          AND challenge.is_enabled
          AND challenge.review_status = 0
   )
   SELECT DISTINCT account.id::text || '|' || account.security_stamp || '|' || account.role::text
     FROM target_games target
     JOIN "UserParticipations" membership ON membership.game_id = target.game_id
     JOIN "Participations" participation
       ON participation.id = membership.participation_id
      AND participation.game_id = target.game_id
      AND participation.team_id = membership.team_id
      AND participation.status = 1
     JOIN "AspNetUsers" account ON account.id = membership.user_id
    WHERE account.security_stamp IS NOT NULL
      AND account.role <> 0
    ORDER BY 1
    LIMIT ${TOKEN_COUNT}`,
)
  .split("\n")
  .filter(Boolean);
if (accounts.length === 0)
  throw new Error(
    "the asset must back an enabled static challenge with at least one accepted participant",
  );

// Use the actual stored role: the request then exercises the same live
// participation authorization tail as a player download.
const tokens = accounts.map((entry) => {
  const [id, stamp, role] = entry.split("|");
  return mintJwt(id, stamp, Number(role));
});

console.log(
  `asset download load → ${TARGET}${ASSET_URL} size=${ASSET_SIZE} ` +
    `rate=${process.env.RATE || 20}/s range=${process.env.RANGE_BYTES || 1048576}B tokens=${tokens.length}`,
);
const directory = mkdtempSync(join(tmpdir(), "rsctf-asset-download-"));
const tokenFile = join(directory, "tokens.json");
writeFileSync(tokenFile, JSON.stringify(tokens), { mode: 0o600 });
let status = 1;
try {
  status = runK6("asset-download.js", {
    TARGET,
    ASSET_URL,
    ASSET_SIZE,
    TOKENS_FILE: tokenFile,
    RATE: process.env.RATE || 20,
    RANGE_BYTES: process.env.RANGE_BYTES || 1048576,
    VUS: process.env.VUS || 64,
    DURATION: process.env.DURATION || "30s",
    SUMMARY_JSON: process.env.SUMMARY_JSON || "",
  });
} finally {
  rmSync(directory, { recursive: true, force: true });
}
process.exit(status);
