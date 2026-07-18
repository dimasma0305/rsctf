import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { materializeFixtures } from "../fixtures.mjs";

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

function request(port, path, headers = {}) {
  return new Promise((resolve, reject) => {
    const operation = http.request(
      { host: "127.0.0.1", port, path, headers, method: "GET", timeout: 2_000 },
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
    (_, index) => `koth_test_token_test_token_${String(index).padStart(3, "0")}`,
  );
  const burst = await Promise.all(
    tokens.map((token) => request(port, "/capture", { "X-Koth-Token": token })),
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
