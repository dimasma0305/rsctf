// Fixed-rate runner for the bounded monitor event/submission history feeds.
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { mintJwt, runK6, sql, TARGET } from './lib.mjs';

const game = Number(process.env.MONITOR_HISTORY_GAME || process.env.GAME);
if (!Number.isSafeInteger(game) || game <= 0) {
  throw new Error('MONITOR_HISTORY_GAME (or GAME) must be a positive integer');
}

const [eventCount, durableEventCount, submissionCount, durableSubmissionCount] = sql(
  `SELECT (SELECT COUNT(*) FROM "GameEvents" WHERE game_id=${game})::text || '|' || ` +
    `(SELECT COUNT(*) FROM "GameEvents" WHERE game_id=${game} AND feed_cursor IS NOT NULL)::text || '|' || ` +
    `(SELECT COUNT(*) FROM "Submissions" WHERE game_id=${game})::text || '|' || ` +
    `(SELECT COUNT(*) FROM "Submissions" WHERE game_id=${game} AND feed_cursor IS NOT NULL)::text`,
).split('|').map(Number);
if (
  !Number.isSafeInteger(eventCount) ||
  !Number.isSafeInteger(durableEventCount) ||
  !Number.isSafeInteger(submissionCount) ||
  !Number.isSafeInteger(durableSubmissionCount) ||
  eventCount < 10000 ||
  durableEventCount !== eventCount ||
  submissionCount < 10000 ||
  durableSubmissionCount !== submissionCount
) {
  throw new Error(
    `monitor-history requires at least 10,000 cursor-backed events and cursor-backed submissions in game ${game}; ` +
      `found events=${eventCount} durableEvents=${durableEventCount} submissions=${submissionCount} ` +
      `durableSubmissions=${durableSubmissionCount}`,
  );
}

const accounts = sql(
  `SELECT id::text || '|' || security_stamp || '|' || role::text ` +
    `FROM "AspNetUsers" WHERE role IN (2,3) AND security_stamp IS NOT NULL ORDER BY id LIMIT 16`,
).split('\n').filter(Boolean);
if (accounts.length === 0) throw new Error('one disposable Monitor/Admin account is required');
const tokens = accounts.map((entry) => {
  const [id, stamp, role] = entry.split('|');
  return mintJwt(id, stamp, Number(role));
});

const tokenDirectory = mkdtempSync(join(tmpdir(), 'rsctf-monitor-history-'));
const tokenFile = join(tokenDirectory, 'tokens.json');
writeFileSync(tokenFile, JSON.stringify(tokens), { mode: 0o600 });
console.log(
  `monitor history/read-backfill load → ${TARGET} game=${game} events=${eventCount} ` +
    `submissions=${submissionCount} durableSubmissions=${durableSubmissionCount} ` +
    `rate=${process.env.RATE || 1}/s`,
);

let status = 1;
try {
  status = runK6('monitor-history.js', {
    TARGET,
    GAME: game,
    TOKENS_FILE: tokenFile,
    RATE: process.env.RATE || 1,
    VUS: process.env.VUS || Math.max(4, tokens.length),
    DURATION: process.env.DURATION || '20s',
    SUMMARY_JSON: process.env.SUMMARY_JSON || '',
  });
} finally {
  rmSync(tokenDirectory, { recursive: true, force: true });
}
process.exit(status);
