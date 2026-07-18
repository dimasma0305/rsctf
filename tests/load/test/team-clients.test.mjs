import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  lstatSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  MANDATORY_TEAM_EVIDENCE_COUNTERS,
  MAX_TEAM_RUNNER_LOG_BYTES,
  TEAM_EVIDENCE_SCHEMA_VERSION,
  TEAM_RUNNER_LOG_FILENAME,
  validateWorkloadEvidenceConservation,
} from "../team-evidence.js";
import { buildPlayerProfiles } from "../player-model.js";

process.env.RSCTF_JWT_SECRET ||= "team-client-command-test-secret";
const { TEAM_RUNNER_FILE_LIMIT_BLOCKS, teamRunnerCommand } = await import(
  "../team-clients.mjs"
);

test("team runner retains k6 logs in the mounted evidence directory", () => {
  assert.equal(MAX_TEAM_RUNNER_LOG_BYTES, 1024 * 1024);
  assert.equal(TEAM_RUNNER_FILE_LIMIT_BLOCKS, 2048);
  const command = teamRunnerCommand();
  assert.match(command, /ulimit -c 0 && ulimit -f 2048/);
  assert.ok(
    command.endsWith(
      `exec /k6 run --log-output=file=/evidence/${TEAM_RUNNER_LOG_FILENAME} /team.js`,
    ),
  );
  assert.equal(
    command.match(/--log-output=/g)?.length,
    1,
    "runner command must configure exactly one log sink",
  );

  assert.match(
    teamRunnerCommand("team-003.runner.log"),
    /--log-output=file=\/evidence\/team-003\.runner\.log \/team\.js$/,
  );
});

test(
  "caught team runtime errors abort nonzero and retain schema-v9 evidence",
  { skip: !existsSync(process.env.K6_BIN || "/usr/local/bin/k6") },
  () => {
    const k6Binary = process.env.K6_BIN || "/usr/local/bin/k6";
    const directory = mkdtempSync(join(tmpdir(), "rsctf-k6-runtime-error-"));
    const loadDirectory = join(directory, "load");
    const scriptDirectory = join(loadDirectory, "k6");
    const configDirectory = join(directory, "config");
    const evidenceDirectory = join(directory, "evidence");
    const scriptPath = join(scriptDirectory, "team-event.js");
    const configPath = join(configDirectory, "bot.json");
    const evidencePath = join(evidenceDirectory, "summary.json");
    const runnerLog = join(evidenceDirectory, TEAM_RUNNER_LOG_FILENAME);
    const competitionSeed = "runtime-error-regression";
    const profile = buildPlayerProfiles(2, competitionSeed)[0];

    try {
      mkdirSync(scriptDirectory, { recursive: true });
      mkdirSync(configDirectory, { recursive: true });
      mkdirSync(evidenceDirectory, { recursive: true });
      copyFileSync(
        new URL("../player-model.js", import.meta.url),
        join(loadDirectory, "player-model.js"),
      );
      copyFileSync(
        new URL("../team-evidence.js", import.meta.url),
        join(loadDirectory, "team-evidence.js"),
      );

      const sourcePath = new URL("../k6/team-event.js", import.meta.url);
      const originalSource = readFileSync(sourcePath, "utf8");
      const initialization = "  initializeEvidenceCounters();\n";
      assert.equal(
        originalSource.split(initialization).length - 1,
        1,
        "fault injection must target exactly one production initialization site",
      );
      const faultInjectedSource = originalSource
        .replace(
          "open('/config/bot.json')",
          `open(${JSON.stringify(configPath)})`,
        )
        .replace(
          "[`/evidence/${CONFIG.evidenceFile}`]",
          `[${JSON.stringify(`${evidenceDirectory}/`)} + CONFIG.evidenceFile]`,
        )
        .replace(
          initialization,
          initialization +
            "  activeIterations.add(1);\n" +
            "  http.get('http://127.0.0.1:1', { timeout: '100ms' });\n" +
            "  throw new Error('runtime-error-regression-sentinel');\n",
        );
      assert.notEqual(faultInjectedSource, originalSource);
      writeFileSync(scriptPath, faultInjectedSource);
      writeFileSync(
        configPath,
        JSON.stringify({
          teamCount: 2,
          teamIndex: 0,
          ownListener: "127.0.0.1:31337",
          teamIds: [501, 502],
          participationId: 501,
          target: "http://127.0.0.1:1",
          duration: "5s",
          thinkSeconds: profile.thinkSeconds,
          evidenceFile: "summary.json",
          realisticCompetition: true,
          competitionRunId: "runtime-error-regression-0001",
          eventCreatedAtMs: Date.now(),
          competitionSeed,
          competitionModelVersion: 2,
          profile,
          defenseKey: "runtime-error-defense-key",
          jwt: "runtime-error-jwt",
          adToken: `ad_${"a".repeat(43)}`,
          gameId: 44,
          adChallengeId: 148,
          kothChallengeId: 149,
          epochStartRound: 12,
        }),
      );

      const result = spawnSync(
        "sh",
        [
          "-c",
          `ulimit -c 0 && ulimit -f ${TEAM_RUNNER_FILE_LIMIT_BLOCKS} && ` +
            'exec "$1" run --log-output="file=$2" "$3"',
          "team-runtime-error-test",
          k6Binary,
          runnerLog,
          scriptPath,
        ],
        { encoding: "utf8", timeout: 10_000 },
      );
      assert.equal(result.error, undefined);
      assert.equal(result.signal, null);
      assert.ok(
        Number.isInteger(result.status) && result.status > 0,
        "exec.test.abort must return a nonzero team-runner exit status",
      );

      const runnerMetadata = lstatSync(runnerLog);
      assert.equal(runnerMetadata.isFile(), true);
      assert.ok(runnerMetadata.size > 0);
      assert.ok(runnerMetadata.size <= MAX_TEAM_RUNNER_LOG_BYTES);
      assert.match(
        readFileSync(runnerLog, "utf8"),
        /team 0 iteration runtime error: Error: runtime-error-regression-sentinel/,
      );

      const evidenceMetadata = lstatSync(evidencePath);
      assert.equal(evidenceMetadata.isFile(), true);
      const evidence = JSON.parse(readFileSync(evidencePath, "utf8"));
      assert.equal(evidence.schemaVersion, TEAM_EVIDENCE_SCHEMA_VERSION);
      assert.equal(evidence.thresholdsPassed, false);
      assert.equal(evidence.team.index, 0);
      assert.equal(evidence.event.runId, "runtime-error-regression-0001");
      for (const name of MANDATORY_TEAM_EVIDENCE_COUNTERS) {
        assert.equal(
          Number.isSafeInteger(evidence.metrics?.[name]?.values?.count),
          true,
          `schema-v9 summary is missing integer counter ${name}`,
        );
      }

      const counts = Object.fromEntries(
        MANDATORY_TEAM_EVIDENCE_COUNTERS.map((name) => [
          name,
          evidence.metrics[name].values.count,
        ]),
      );
      assert.equal(counts.active_iterations, 1);
      assert.equal(counts.idle_iterations, 0);
      assert.equal(counts.iterations_completed, 0);
      assert.equal(counts.iteration_runtime_errors, 1);
      assert.throws(
        () => validateWorkloadEvidenceConservation(counts, profile),
        /records 1 caught iteration runtime error/,
      );
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  },
);

test(
  "team runner file limit stops a k6 log overflow at one MiB",
  { skip: !existsSync(process.env.K6_BIN || "/usr/local/bin/k6") },
  () => {
    const k6Binary = process.env.K6_BIN || "/usr/local/bin/k6";
    const directory = mkdtempSync(join(tmpdir(), "rsctf-k6-log-limit-"));
    const runnerLog = join(directory, TEAM_RUNNER_LOG_FILENAME);
    const script =
      "import exec from 'k6/execution';" +
      "export const options={vus:1,iterations:1};" +
      "export default function(){const payload='x'.repeat(512);" +
      "for(let i=0;i<3000;i++) console.error('iteration runtime error: '+payload);" +
      "exec.test.abort('runner log overflow sentinel');}";
    try {
      const result = spawnSync(
        "sh",
        [
          "-c",
          `ulimit -c 0 && ulimit -f ${TEAM_RUNNER_FILE_LIMIT_BLOCKS} && ` +
            'exec "$1" run --log-output="file=$2" -',
          "team-runner-log-limit-test",
          k6Binary,
          runnerLog,
        ],
        {
          encoding: "utf8",
          input: script,
          timeout: 10_000,
        },
      );
      assert.equal(result.error, undefined);
      assert.ok(
        result.status !== 0 || result.signal !== null,
        "overflowing the retained log must fail the runner",
      );
      const metadata = lstatSync(runnerLog);
      assert.equal(metadata.isFile(), true);
      assert.equal(metadata.size, MAX_TEAM_RUNNER_LOG_BYTES);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  },
);

test("team runner rejects unsafe evidence log names", () => {
  for (const filename of [
    "../runner.log",
    "/tmp/runner.log",
    "runner log.log",
    "runner.txt",
    ".log",
  ]) {
    assert.throws(() => teamRunnerCommand(filename), /safe \.log filename/);
  }
});
