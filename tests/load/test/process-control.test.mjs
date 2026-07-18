import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  acquireExclusiveProcessLock,
  InFlightMutationDrain,
  processGroupRunning,
  runInterruptibleStep,
  stopChildTree,
  waitForCompletion,
} from "../process-control.mjs";

const workerPath = new URL(
  "./fixtures/process-lock-worker.mjs",
  import.meta.url,
).pathname;

function worker(lockPath, environment = {}) {
  return spawn(process.execPath, [workerPath], {
    stdio: ["ignore", "ignore", "pipe", "ipc"],
    env: {
      ...process.env,
      RSCTF_LOCK_TEST_PATH: lockPath,
      ...environment,
    },
  });
}

function nextMessage(child, timeoutMs = 5_000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => finish(new Error("timed out waiting for lock worker")),
      timeoutMs,
    );
    const onMessage = (message) => finish(null, message);
    const onError = (error) => finish(error);
    const onExit = (code, signal) =>
      finish(
        new Error(`lock worker exited before replying: ${signal || code}`),
      );
    const finish = (error, message) => {
      clearTimeout(timer);
      child.removeListener("message", onMessage);
      child.removeListener("error", onError);
      child.removeListener("exit", onExit);
      if (error) reject(error);
      else resolve(message);
    };
    child.once("message", onMessage);
    child.once("error", onError);
    child.once("exit", onExit);
  });
}

async function waitForExit(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((resolve) => child.once("close", resolve));
}

async function releaseWorker(child) {
  const response = nextMessage(child);
  child.send({ type: "release" });
  assert.deepEqual(await response, { type: "released", released: true });
  await waitForExit(child);
}

function reapWorkers(context, children) {
  context.after(async () => {
    for (const child of children) {
      if (child.exitCode === null && child.signalCode === null)
        child.kill("SIGKILL");
    }
    await Promise.all([...children].map(waitForExit));
  });
}

test("exclusive process lock rejects an unrelated token and permits its direct child", async (context) => {
  const directory = mkdtempSync(join(tmpdir(), "rsctf-process-lock-"));
  const lockPath = join(directory, "lifecycle.lock");
  context.after(() => rmSync(directory, { recursive: true, force: true }));
  const children = new Set();
  reapWorkers(context, children);

  const owner = worker(lockPath);
  children.add(owner);
  const acquired = await nextMessage(owner);
  assert.equal(acquired.type, "acquired");
  assert.equal(acquired.inherited, false);
  assert.equal(statSync(`${lockPath}.owner.json`).mode & 0o777, 0o600);

  const unrelated = worker(lockPath, {
    RSCTF_LOCK_TEST_INHERITED_TOKEN: acquired.token,
    RSCTF_LOCK_TEST_EXIT_AFTER_ACQUIRE: "1",
  });
  children.add(unrelated);
  const rejected = await nextMessage(unrelated);
  assert.equal(rejected.type, "rejected");
  assert.match(rejected.message, /invalid or stale inherited process lock/);
  await waitForExit(unrelated);

  const inherited = nextMessage(owner);
  owner.send({ type: "spawnInherited" });
  assert.deepEqual((await inherited).result, {
    type: "acquired",
    inherited: true,
    token: acquired.token,
  });
  await releaseWorker(owner);
  assert.equal(existsSync(`${lockPath}.owner.json`), false);

  const successor = await acquireExclusiveProcessLock(lockPath, {
    label: "lifecycle",
  });
  assert.equal(await successor.release(), true);
});

test("a dead owner releases atomically to exactly one concurrent successor", async (context) => {
  const directory = mkdtempSync(join(tmpdir(), "rsctf-stale-lock-"));
  const lockPath = join(directory, "lifecycle.lock");
  context.after(() => rmSync(directory, { recursive: true, force: true }));
  const children = new Set();
  reapWorkers(context, children);

  const oldOwner = worker(lockPath);
  children.add(oldOwner);
  assert.equal((await nextMessage(oldOwner)).type, "acquired");
  oldOwner.kill("SIGKILL");
  await waitForExit(oldOwner);
  assert.equal(
    existsSync(`${lockPath}.owner.json`),
    true,
    "SIGKILL deliberately leaves diagnostic metadata",
  );

  const contenders = [
    worker(lockPath, { RSCTF_LOCK_TEST_WAIT_FOR_GO: "1" }),
    worker(lockPath, { RSCTF_LOCK_TEST_WAIT_FOR_GO: "1" }),
  ];
  contenders.forEach((child) => children.add(child));
  assert.deepEqual(
    await Promise.all(contenders.map((child) => nextMessage(child))),
    [{ type: "ready" }, { type: "ready" }],
  );
  const outcomes = contenders.map((child) => nextMessage(child));
  contenders.forEach((child) => child.send({ type: "go" }));
  const results = await Promise.all(outcomes);
  assert.equal(
    results.filter((result) => result.type === "acquired").length,
    1,
  );
  assert.equal(
    results.filter(
      (result) => result.type === "rejected" && result.code === "ELOCKED",
    ).length,
    1,
  );

  const winner =
    contenders[results.findIndex((result) => result.type === "acquired")];
  const loser =
    contenders[results.findIndex((result) => result.type === "rejected")];
  await waitForExit(loser);
  await releaseWorker(winner);
  assert.equal(existsSync(`${lockPath}.owner.json`), false);

  const successor = await acquireExclusiveProcessLock(lockPath, {
    label: "lifecycle",
  });
  assert.equal(await successor.release(), true);
});

test("process-group shutdown reaps a leader and its child", async (context) => {
  const leader = spawn(
    process.execPath,
    [
      "-e",
      `const { spawn } = require("node:child_process");
       spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { stdio: "ignore" });
       setInterval(() => {}, 1000);`,
    ],
    { detached: true, stdio: "ignore" },
  );
  context.after(async () => {
    await stopChildTree(leader, { processGroup: true, graceMs: 1_000 });
  });
  assert.ok(Number.isSafeInteger(leader.pid) && leader.pid > 0);
  await new Promise((resolve) => setTimeout(resolve, 100));
  assert.equal(processGroupRunning(leader.pid), true);

  await stopChildTree(leader, { processGroup: true, graceMs: 1_000 });
  assert.equal(processGroupRunning(leader.pid), false);
  assert.notEqual(leader.signalCode, null);
});

test("completion waits clear their timeout on both outcomes", async () => {
  assert.equal(await waitForCompletion(Promise.resolve(), 1_000), true);
  const started = Date.now();
  assert.equal(await waitForCompletion(new Promise(() => {}), 20), false);
  assert.ok(Date.now() - started >= 15);
});

test("shutdown cleanup drains a mutation that commits after its caller is interrupted", async () => {
  const mutations = new InFlightMutationDrain();
  let commit;
  let resourceExists = false;
  const lateCreate = new Promise((resolve) => {
    commit = () => {
      resourceExists = true;
      resolve(41);
    };
  });
  const shutdown = Promise.resolve("SIGTERM").then((signal) => {
    throw new Error(`interrupted by ${signal}`);
  });

  await assert.rejects(
    Promise.race([mutations.track(lateCreate), shutdown]),
    /SIGTERM/,
  );
  assert.equal(mutations.pendingCount, 1);

  let cleanupObserved = null;
  const cleanup = mutations.drain().then(() => {
    cleanupObserved = resourceExists;
    resourceExists = false;
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(cleanupObserved, null, "cleanup must wait for the late create");

  commit();
  await cleanup;
  assert.equal(cleanupObserved, true);
  assert.equal(resourceExists, false);
  assert.equal(mutations.pendingCount, 0);
});

test("preparation checks interruption after each completed operation", async () => {
  let interrupted = false;
  const completed = [];
  const checkInterrupted = () => {
    if (interrupted) throw new Error("preparation interrupted");
  };

  await runInterruptibleStep(checkInterrupted, async () => {
    completed.push("token");
    interrupted = true;
  }).then(
    () => assert.fail("the post-operation interruption check must reject"),
    (error) => assert.match(error.message, /preparation interrupted/),
  );
  await assert.rejects(
    runInterruptibleStep(checkInterrupted, async () =>
      completed.push("config"),
    ),
    /preparation interrupted/,
  );
  assert.deepEqual(
    completed,
    ["token"],
    "no later preparation operation may start",
  );
});
