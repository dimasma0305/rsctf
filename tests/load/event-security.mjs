// Destructive, disposable-stack fixed-rate resource comparison for the bounded
// event telemetry ingest path. It never creates packet data or raw identities.
import { execFile, execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";

import {
  EVENT_TELEMETRY_LOGICAL_LIMIT,
  boundedInteger,
  k6PhaseSummary,
  parsePeerFixture,
  parseProcessStat,
  parseUsage,
  summarizeResourceSamples,
} from "./event-security-load.js";
import { GAME, PG, RSCTF, sql, TARGET } from "./lib.mjs";

const execFileAsync = promisify(execFile);
if (process.env.EVENT_SECURITY_STRESS_ACK !== "1") {
  throw new Error(
    "EVENT_SECURITY_STRESS_ACK=1 is required: this test writes bounded telemetry to the selected event",
  );
}
const gameId = boundedInteger(GAME, "GAME", 1, 2_147_483_647);
const rate = boundedInteger(process.env.RATE || 2, "RATE", 1, 50);
const rowsPerBatch = boundedInteger(process.env.ROWS_PER_BATCH || 256, "ROWS_PER_BATCH", 1, 4096);
const vus = boundedInteger(process.env.VUS || 16, "VUS", 1, 512);
const duration = String(process.env.DURATION || "60s");
if (!/^([1-9][0-9]*)(s|m)$/.test(duration)) throw new Error("DURATION must use positive k6 seconds or minutes");
const token = String(process.env.RSCTF_EVENT_SENSOR_TOKEN || "");
if (token.length < 32 || /\s/.test(token)) throw new Error("RSCTF_EVENT_SENSOR_TOKEN must contain 32 non-whitespace characters");
const processPid = process.env.EVENT_SECURITY_PROCESS_PID
  ? boundedInteger(process.env.EVENT_SECURITY_PROCESS_PID, "EVENT_SECURITY_PROCESS_PID", 1, 2_147_483_647)
  : null;
const processClockTicks = processPid
  ? boundedInteger(execFileSync("getconf", ["CLK_TCK"], { encoding: "utf8" }).trim(), "CLK_TCK", 1, 1_000_000)
  : null;
const processPageSize = processPid
  ? boundedInteger(execFileSync("getconf", ["PAGESIZE"], { encoding: "utf8" }).trim(), "PAGESIZE", 1, 1_048_576)
  : null;
const dockerNames = [...new Set([
  processPid ? null : (process.env.EVENT_SECURITY_APP_CONTAINER || RSCTF),
  PG,
  process.env.EVENT_SENSOR_CONTAINER,
].filter(Boolean))];

const peer = parsePeerFixture(sql(
  `SELECT peer.user_id::text || '|' || peer.participation_id::text || '|' || peer.id::text || '|' ||
          (CEIL(EXTRACT(EPOCH FROM game.start_time_utc) / 300) * 300000)::bigint::text
     FROM "EventVpnUserPeers" peer
     JOIN "Games" game ON game.id = peer.game_id
    WHERE peer.game_id = ${gameId}
      AND peer.revoked_at_utc IS NULL
      AND game.vpn_behavior_telemetry_enabled = TRUE
      AND game.start_time_utc < game.end_time_utc
    ORDER BY peer.id LIMIT 1`,
));

function usage() {
  return parseUsage(sql(
    `SELECT COALESCE(usage.logical_bytes, 0)::text || '|' ||
            COALESCE(usage.row_count, 0)::text || '|' ||
            COALESCE(usage.disabled_at_utc IS NOT NULL, FALSE)::text || '|' ||
            (pg_total_relation_size('"VpnFlowTelemetryBuckets"'::regclass)
             + pg_total_relation_size('"VpnDnsProviderBuckets"'::regclass)
             + pg_total_relation_size('"VpnPeerNetworkObservations"'::regclass)
             + pg_total_relation_size('"VpnFlagTransportEvents"'::regclass))::text
       FROM (SELECT 1) seed
       LEFT JOIN "AntiCheatTelemetryUsage" usage ON usage.game_id = ${gameId}`,
  ));
}

async function exactHealth() {
  const response = await fetch(new URL("/healthz", TARGET));
  const body = await response.text();
  if (response.status !== 200 || body !== "ok") throw new Error(`healthz failed: ${response.status} ${JSON.stringify(body)}`);
}

async function dockerSample() {
  if (dockerNames.length === 0) return { containers: [] };
  try {
    const { stdout } = await execFileAsync(
      "docker",
      ["stats", "--no-stream", "--format", "{{json .}}", ...dockerNames],
      { encoding: "utf8", timeout: 5000 },
    );
    return {
      containers: stdout.trim().split("\n").filter(Boolean).map((line) => {
        const row = JSON.parse(line);
        const memory = String(row.MemUsage || "0B").split("/")[0].trim();
        const match = memory.match(/^([0-9.]+)([KMGT]?i?B)$/i);
        const units = { B: 1, KiB: 1024, MiB: 1024 ** 2, GiB: 1024 ** 3, TiB: 1024 ** 4 };
        return {
          name: row.Name,
          cpuPercent: Number.parseFloat(String(row.CPUPerc || "0").replace("%", "")),
          memoryBytes: match ? Number(match[1]) * units[match[2]] : 0,
        };
      }),
    };
  } catch (error) {
    return { containers: [], error: error.message };
  }
}

async function resourceSample(processState) {
  const docker = await dockerSample();
  if (!processPid) return docker;
  try {
    const parsed = parseProcessStat(
      readFileSync(`/proc/${processPid}/stat`, "utf8"),
      processClockTicks,
      processPageSize,
      processState.value,
    );
    processState.value = parsed.state;
    return {
      containers: [...docker.containers, ...(parsed.sample ? [parsed.sample] : [])],
      error: docker.error,
    };
  } catch (error) {
    return {
      containers: docker.containers,
      error: [docker.error, `process ${processPid}: ${error.message}`].filter(Boolean).join("; "),
    };
  }
}

async function runPhase(mode, fixturePath, summaryPath) {
  const samples = [];
  const processState = { value: null };
  const child = (await import("node:child_process")).spawn(
    "k6",
    ["run", "--summary-export", summaryPath, resolve(new URL("./k6/event-security.js", import.meta.url).pathname)],
    {
      stdio: "inherit",
      env: {
        ...process.env,
        TARGET,
        MODE: mode,
        EVENT_SENSOR_TOKEN: token,
        EVENT_SECURITY_FIXTURE: fixturePath,
        RATE: String(rate),
        ROWS_PER_BATCH: String(rowsPerBatch),
        VUS: String(vus),
        DURATION: duration,
      },
    },
  );
  let pendingSample = null;
  const sample = () => {
    if (pendingSample) return pendingSample;
    pendingSample = resourceSample(processState)
      .then((value) => samples.push(value))
      .finally(() => { pendingSample = null; });
    return pendingSample;
  };
  await sample();
  const interval = setInterval(() => { void sample(); }, 1000);
  const status = await new Promise((resolveStatus, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => resolveStatus(code ?? 1));
  }).finally(() => clearInterval(interval));
  if (pendingSample) await pendingSample;
  await sample();
  if (status !== 0) throw new Error(`${mode} k6 phase failed with exit ${status}`);
  return {
    metrics: k6PhaseSummary(JSON.parse(readFileSync(summaryPath, "utf8"))),
    resources: summarizeResourceSamples(samples),
    samplingErrors: samples.map((sample) => sample.error).filter(Boolean),
  };
}

await exactHealth();
const before = usage();
if (before.disabled) throw new Error("event telemetry is already disabled by its quota; purge or select a disposable event");
const directory = mkdtempSync(join(tmpdir(), "rsctf-event-security-load-"));
const fixturePath = join(directory, "fixture.json");
writeFileSync(fixturePath, JSON.stringify({ gameId, ...peer }), { mode: 0o600 });
try {
  const control = await runPhase("control", fixturePath, join(directory, "control.json"));
  const afterControl = usage();
  if (afterControl.rowCount !== before.rowCount || afterControl.logicalBytes !== before.logicalBytes) {
    throw new Error("empty control batches unexpectedly changed telemetry accounting");
  }
  const ingest = await runPhase("ingest", fixturePath, join(directory, "ingest.json"));
  await exactHealth();
  const after = usage();
  if (after.logicalBytes > EVENT_TELEMETRY_LOGICAL_LIMIT) {
    throw new Error(`event logical quota exceeded: ${after.logicalBytes}`);
  }
  const report = {
    schemaVersion: 1,
    generatedAt: new Date().toISOString(),
    target: TARGET,
    gameId,
    fixedLoad: { ratePerSecond: rate, rowsPerBatch, vus, duration },
    control,
    ingest,
    storage: {
      before,
      after,
      logicalBytesAdded: after.logicalBytes - before.logicalBytes,
      rowsAdded: after.rowCount - before.rowCount,
      physicalBytesAdded: after.physicalBytes - before.physicalBytes,
      eventLogicalLimitBytes: EVENT_TELEMETRY_LOGICAL_LIMIT,
    },
  };
  const output = process.env.SUMMARY_JSON;
  if (output) writeFileSync(output, `${JSON.stringify(report, null, 2)}\n`);
  console.log(JSON.stringify(report, null, 2));
} finally {
  rmSync(directory, { recursive: true, force: true });
}
