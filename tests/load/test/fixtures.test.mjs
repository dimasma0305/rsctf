import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { materializeFixtures } from "../fixtures.mjs";

test("managed images replace the base server healthcheck with their real probes", () => {
  const fixtures = materializeFixtures();
  const kothDockerfile = readFileSync(fixtures.kothDockerfile, "utf8");
  const managedKothDockerfile = readFileSync(fixtures.managedKothDockerfile, "utf8");
  const adDockerfile = readFileSync(fixtures.adDockerfile, "utf8");
  assert.match(kothDockerfile, /HEALTHCHECK .*http:\/\/127\.0\.0\.1:8080\//);
  assert.match(managedKothDockerfile, /HEALTHCHECK .*http:\/\/127\.0\.0\.1:8080\/healthz/);
  assert.match(adDockerfile, /HEALTHCHECK .*http:\/\/127\.0\.0\.1:8080\/health/);
  assert.doesNotMatch(kothDockerfile, /healthz/);
  assert.doesNotMatch(adDockerfile, /healthz/);
});

test("managed Leaderboard fixture consumes only the injected reporter contract", () => {
  const fixture = materializeFixtures().managedKothService;
  const source = readFileSync(fixture, "utf8");
  const compiled = spawnSync("python3", ["-m", "py_compile", fixture], { encoding: "utf8" });
  assert.equal(compiled.status, 0, compiled.stderr);
  for (const name of [
    "RSCTF_KOTH_GAME_ID",
    "RSCTF_KOTH_CHALLENGE_ID",
    "RSCTF_KOTH_PLATFORM_URL",
    "RSCTF_KOTH_CONTEXT_URL",
    "RSCTF_KOTH_OBSERVATION_URL",
    "RSCTF_KOTH_REPORTER_SECRET",
  ]) {
    assert.match(source, new RegExp(`"${name}"`));
  }
  assert.match(source, /if any\(reporter_values\) and not all\(reporter_values\)/);
  assert.match(source, /if REPORTER_CONFIGURED:/);
  assert.match(source, /"X-RSCTF-API-Version": "v2"/);
  assert.match(source, /hmac\.new\(REPORTER_SECRET\.encode\(\), message, hashlib\.sha256\)/);
  assert.match(source, /accepted\.get\("submittedWaves"\) != len\(waves\)/);
  assert.match(source, /accepted\.get\("submittedTeams"\) != len\(selected\)/);
  assert.match(source, /set\(accepted\) != \{/);
  assert.match(source, /accepted_at = accepted\.get\("acceptedAt"\)/);
  assert.match(source, /selected = ordered/);
  assert.match(source, /sum\(1 for _, score in ranked if score > 0\) != ACTIVE_FLEET/);
  assert.match(source, /len\(leaders\) != 1/);
  assert.match(source, /scoreable = score > 0/);
  assert.match(source, /context\.get\("objectiveIds"\) == \[\]/);
  assert.match(source, /context\.get\("objectiveIds"\) == OBJECTIVE_IDS/);
  assert.match(source, /worker_slots = threading\.BoundedSemaphore\(128\)/);
  assert.match(source, /self\.connection\.settimeout\(5\)/);
  assert.match(source, /len\(eligible\) <= 2_000/);
  assert.match(source, /if error\.code in \(401, 429\)/);
  assert.match(source, /response_headers\["Retry-After"\] = retry_after/);
  assert.match(source, /self\.send_json\(error\.code, \{"accepted": False\}, response_headers\)/);
  assert.doesNotMatch(source, /"reporterSecret"|"credential"|"token"\s*:\s*REPORTER_SECRET/);
});

test("managed Leaderboard dense boundary fits one bounded observation body", () => {
  const hashes = Array.from({ length: 2_000 }, (_, index) =>
    index.toString(16).padStart(64, "0"));
  const teams = hashes.map((tokenHash, index) => ({
    tokenHash,
    activity: { earned: 1, possible: 1 },
    objectives: [{ earned: index < 64 ? 1_000 - index : 0, possible: 1_000 }],
    isCrown: index === 0,
  }));
  const body = JSON.stringify({
    context: "a".repeat(64),
    objectiveIds: ["official-score"],
    waves: [{ waveId: "load-1-1-dense", endedAtUnixMs: 1_800_000_000_000, teams }],
  });
  assert.equal(teams.length, 2_000);
  assert.equal(teams.filter(({ objectives }) => objectives[0].earned === 0).length, 1_936);
  assert.ok(Buffer.byteLength(body) <= 512 * 1_024, `dense body is ${Buffer.byteLength(body)} bytes`);
});

test("managed image builds use a pinned compact Python base", () => {
  const source = readFileSync(new URL("../applib.mjs", import.meta.url), "utf8");
  assert.match(source, /python:3\.12-alpine@sha256:[a-f0-9]{64}/);
  assert.match(source, /LOAD_FIXTURE_PYTHON_IMAGE must be an immutable repository digest or image ID/);
  assert.doesNotMatch(source, /LOAD_FIXTURE_PYTHON_IMAGE \|\|\s*['"]python:3\.12-alpine['"]/);
});

test("load mutation helpers supply current operation and revision identities", () => {
  const source = readFileSync(new URL("../applib.mjs", import.meta.url), "utf8");
  const createGame = source.slice(
    source.indexOf("export async function createGame"),
    source.indexOf("export async function setGameSchedule"),
  );
  assert.match(createGame, /headers: \{ 'idempotency-key': randomUUID\(\) \}/);
  const gameSchedule = source.slice(
    source.indexOf("export async function setGameSchedule"),
    source.indexOf("export async function createChallenge"),
  );
  assert.match(gameSchedule, /operationId: randomUUID\(\)/);
  const challengeMutations = source.slice(
    source.indexOf("export async function createChallenge"),
    source.indexOf("export async function addFlags"),
  );
  assert.match(challengeMutations, /operationId: body\.operationId \|\| randomUUID\(\)/);
  assert.match(challengeMutations, /expectedRevision: body\.expectedRevision \?\? current\.revision/);
  assert.match(challengeMutations, /headers: \{ 'idempotency-key': randomUUID\(\) \}/);
  const flagImport = source.slice(
    source.indexOf("export async function addFlags"),
    source.indexOf("export async function deleteGame"),
  );
  assert.match(flagImport, /operationId: randomUUID\(\)/);
  assert.match(flagImport, /flags: flags\.map/);
  const scoringPause = source.slice(
    source.indexOf("export async function setAdScoringPaused"),
    source.indexOf("export function adScoringPaused"),
  );
  assert.match(scoringPause, /body: \{ paused: desired, revision: Number\(current\.revision\) \}/);
  const assetUpload = source.slice(
    source.indexOf("export async function uploadAsset"),
    source.indexOf("export async function setAttachment"),
  );
  assert.match(assetUpload, /\/api\/assets\?operationId=\$\{operationId\}/);
  assert.match(assetUpload, /uploaded\?\.uploadId/);
  const attachmentMutation = source.slice(
    source.indexOf("export async function setAttachment"),
    source.indexOf("\/\/ ── Real BYOC fleet"),
  );
  assert.match(attachmentMutation, /fileHash: uploaded\.hash, uploadId: uploaded\.uploadId/);
});

function reservePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close((error) => (error ? reject(error) : resolve(port)));
    });
  });
}

function request(port, path, headers = {}, timeoutMs = 2_000) {
  return new Promise((resolve, reject) => {
    const operation = http.request(
      {
        host: "127.0.0.1",
        port,
        path,
        headers,
        method: "GET",
        timeout: timeoutMs,
      },
      (response) => {
        const chunks = [];
        response.on("data", (chunk) => chunks.push(chunk));
        response.on("end", () =>
          resolve({
            status: response.statusCode,
            headers: response.headers,
            body: Buffer.concat(chunks),
          }),
        );
      },
    );
    operation.once("timeout", () =>
      operation.destroy(new Error("fixture request timed out")),
    );
    operation.once("error", reject);
    operation.end();
  });
}

async function waitUntilReady(port, fixtureProcess) {
  for (let attempt = 0; attempt < 50; attempt++) {
    if (fixtureProcess.exitCode !== null) {
      throw new Error(`KotH fixture exited with ${fixtureProcess.exitCode}`);
    }
    try {
      const response = await request(port, "/");
      if (response.status === 200) return;
    } catch {
      // The Python listener may not have bound yet.
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error("KotH fixture did not become ready");
}

async function waitForService(port, fixtureProcess) {
  for (let attempt = 0; attempt < 50; attempt++) {
    if (fixtureProcess.exitCode !== null) {
      throw new Error(`service fixture exited with ${fixtureProcess.exitCode}`);
    }
    try {
      const response = await request(port, "/health");
      if (response.status === 200) return;
    } catch {
      // The Python listener may not have bound yet.
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error("service fixture did not become ready");
}

test("A&D patch incidents affect checker traffic until a player repairs them", async (context) => {
  const directory = mkdtempSync(join(tmpdir(), "rsctf-ad-fixture-"));
  const port = await reservePort();
  const fixture = materializeFixtures().service;
  const fixtureProcess = spawn("python3", [fixture], {
    env: {
      ...process.env,
      DEFENSE_KEY: "repair-capability",
      PORT: String(port),
    },
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  fixtureProcess.stderr.on("data", (chunk) => {
    stderr += chunk.toString();
  });
  context.after(() => {
    fixtureProcess.kill("SIGTERM");
    rmSync(directory, { recursive: true, force: true });
  });

  await waitForService(port, fixtureProcess);
  assert.equal(
    (await request(port, "/plant?team=7&flag=flag%7Bfixture%7D")).status,
    200,
  );
  assert.equal(
    (await request(port, "/exploit?team=7&technique=1")).body.toString().trim(),
    "flag{fixture}",
  );

  const auth = { "X-Defense-Key": "repair-capability" };
  assert.equal(
    (await request(port, "/defense?level=1&incident=mumble", auth)).status,
    200,
  );
  assert.equal(
    (await request(port, "/flag?team=7")).body.toString().trim(),
    "service-mumble",
  );
  assert.equal((await request(port, "/defense?repair=1", auth)).status, 200);
  assert.equal(
    (await request(port, "/exploit?team=7&technique=2")).body.toString().trim(),
    "flag{fixture}",
  );

  assert.equal(
    (await request(port, "/defense?level=2&incident=offline", auth)).status,
    200,
  );
  assert.equal((await request(port, "/flag?team=7")).status, 503);
  assert.equal((await request(port, "/defense?repair=1", auth)).status, 200);
  assert.equal(
    (await request(port, "/exploit?team=7&technique=3")).body.toString().trim(),
    "flag{fixture}",
  );
  assert.equal(fixtureProcess.exitCode, null, stderr);
});

test("KotH capture commits before ack and accepts a 100-way burst", async (context) => {
  const directory = mkdtempSync(join(tmpdir(), "rsctf-koth-fixture-"));
  const marker = join(directory, "king");
  const port = await reservePort();
  const fixture = materializeFixtures().kothService;
  const fixtureProcess = spawn("python3", [fixture], {
    env: { ...process.env, KOTH_KING_PATH: marker, PORT: String(port) },
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  fixtureProcess.stderr.on("data", (chunk) => {
    stderr += chunk.toString();
  });
  context.after(() => {
    fixtureProcess.kill("SIGTERM");
    rmSync(directory, { recursive: true, force: true });
  });

  await waitUntilReady(port, fixtureProcess);
  const firstToken = "koth_test_token_test_token_000";
  const first = await request(port, "/capture", { "X-Koth-Token": firstToken });
  assert.equal(first.status, 204, stderr);
  assert.equal(first.headers["content-length"], "0");
  assert.equal(first.headers.connection, "close");
  assert.equal(first.body.length, 0);
  assert.equal(readFileSync(marker, "utf8"), firstToken);

  const tokens = Array.from(
    { length: 100 },
    (_, index) =>
      `koth_test_token_test_token_${String(index).padStart(3, "0")}`,
  );
  const burst = await Promise.all(
    tokens.map((token) =>
      request(port, "/capture", { "X-Koth-Token": token }, 10_000),
    ),
  );
  assert.equal(
    burst.filter((response) => response.status === 204).length,
    100,
    stderr,
  );
  assert.ok(tokens.includes(readFileSync(marker, "utf8")));
  assert.equal(fixtureProcess.exitCode, null, stderr);
});

test("KotH holder patches affect takeovers and a replacement starts pristine", async (context) => {
  const directory = mkdtempSync(join(tmpdir(), "rsctf-koth-patch-fixture-"));
  const materialized = materializeFixtures();
  const fixture = materialized.kothService;
  const processes = [];
  context.after(() => {
    for (const fixtureProcess of processes) fixtureProcess.kill("SIGTERM");
    rmSync(directory, { recursive: true, force: true });
  });

  const start = async (name) => {
    const port = await reservePort();
    const fixtureProcess = spawn("python3", [fixture], {
      env: {
        ...process.env,
        KOTH_KING_PATH: join(directory, name),
        PORT: String(port),
      },
      stdio: ["ignore", "ignore", "pipe"],
    });
    processes.push(fixtureProcess);
    await waitUntilReady(port, fixtureProcess);
    return { port, fixtureProcess };
  };
  const checkerExit = (port) =>
    spawnSync("python3", [materialized.kothChecker], {
      env: {
        ...process.env,
        RSCTF_TARGET_IP: "127.0.0.1",
        RSCTF_TARGET_PORT: String(port),
      },
    }).status;

  const holder = "koth_PatchHolder123456";
  const challenger = "koth_PatchChallenger123456";
  const first = await start("first-king");
  const firstStatus = await request(first.port, "/status");
  assert.match(
    firstStatus.body.toString().trim(),
    /^instance=[a-f0-9]{16};patch=0;state=healthy$/,
  );
  assert.equal(checkerExit(first.port), 0);
  assert.equal(
    (await request(first.port, "/capture", { "X-Koth-Token": holder })).status,
    204,
  );
  assert.equal(
    (
      await request(first.port, "/defense?level=2&incident=healthy", {
        "X-Koth-Token": holder,
      })
    ).status,
    200,
  );

  const blocked = await request(first.port, "/capture?technique=2", {
    "X-Koth-Token": challenger,
  });
  assert.equal(blocked.status, 403);
  assert.equal(blocked.headers["x-koth-defense"], "blocked");
  assert.equal(readFileSync(join(directory, "first-king"), "utf8"), holder);

  const bypassed = await request(first.port, "/capture?technique=3", {
    "X-Koth-Token": challenger,
  });
  assert.equal(bypassed.status, 204);
  assert.equal(bypassed.headers["x-koth-defense"], "bypassed");
  assert.equal(
    (
      await request(first.port, "/defense?level=2&incident=mumble", {
        "X-Koth-Token": challenger,
      })
    ).status,
    200,
  );
  const mumble = await request(first.port, "/capture?technique=3", {
    "X-Koth-Token": holder,
  });
  assert.equal(mumble.status, 409);
  assert.equal(mumble.headers["x-koth-defense"], "mumble");
  assert.equal(checkerExit(first.port), 1);
  assert.equal(
    (
      await request(first.port, "/defense?repair=1", {
        "X-Koth-Token": challenger,
      })
    ).status,
    200,
  );
  assert.match(
    (await request(first.port, "/status")).body.toString().trim(),
    /^instance=[a-f0-9]{16};patch=2;state=healthy$/,
  );
  assert.equal(checkerExit(first.port), 0);
  assert.equal(
    (
      await request(first.port, "/defense?level=2&incident=offline", {
        "X-Koth-Token": challenger,
      })
    ).status,
    200,
  );
  assert.equal(checkerExit(first.port), 2);
  assert.equal(
    (
      await request(first.port, "/defense?repair=1", {
        "X-Koth-Token": challenger,
      })
    ).status,
    200,
  );
  assert.equal(checkerExit(first.port), 0);

  const replacement = await start("replacement-king");
  const replacementStatus = await request(replacement.port, "/status");
  assert.match(
    replacementStatus.body.toString().trim(),
    /^instance=[a-f0-9]{16};patch=0;state=healthy$/,
  );
  assert.notEqual(
    replacementStatus.headers["x-koth-instance"],
    firstStatus.headers["x-koth-instance"],
  );
  assert.equal(checkerExit(replacement.port), 0);
  assert.equal(first.fixtureProcess.exitCode, null);
  assert.equal(replacement.fixtureProcess.exitCode, null);
});
