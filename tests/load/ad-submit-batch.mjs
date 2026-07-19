// Thin launcher for the explicitly configured, mutating A&D batch scenario.
// It deliberately does not import lib.mjs, inspect PostgreSQL, mint a JWT, or
// discover a flag.
import { spawnSync } from 'node:child_process';

import { MAX_AD_BATCH, parseAdSubmitBatchConfig } from './ad-submit-batch.js';

let config;
try {
  config = parseAdSubmitBatchConfig(process.env);
} catch (error) {
  console.error(`configuration error: ${error.message}`);
  process.exit(2);
}

const args = ['run'];
const summaryJson = String(process.env.SUMMARY_JSON || '').trim();
if (summaryJson) args.push('--summary-export', summaryJson);
args.push(new URL('./k6/ad-submit-batch.js', import.meta.url).pathname);

console.log(
  `A&D ${config.batchShape}-flag batch emulation → ${config.target} game=${config.gameId} ` +
    `rate=${config.rate}/s batch=${MAX_AD_BATCH} duration=${config.duration}; credentials redacted`,
);

const result = spawnSync('k6', args, {
  stdio: 'inherit',
  encoding: 'utf8',
  env: {
    ...process.env,
    TARGET: config.target,
    GAME: String(config.gameId),
    TOKEN: config.token,
    FLAG: config.flag,
    BATCH_SHAPE: config.batchShape,
    RATE: String(config.rate),
    VUS: String(config.vus),
    MAX_VUS: String(config.maxVus),
    DURATION: config.duration,
    REQUEST_TIMEOUT: config.requestTimeout,
    CONFIRM_MUTATING_LOAD: '1',
  },
});

if (result.error) {
  console.error(`failed to start k6: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
