import { spawn } from "node:child_process";
import { once } from "node:events";

import { acquireExclusiveProcessLock } from "../../process-control.mjs";

const lockPath = process.env.RSCTF_LOCK_TEST_PATH;
const inheritedToken = process.env.RSCTF_LOCK_TEST_INHERITED_TOKEN || null;

function send(message) {
  return new Promise((resolve, reject) => {
    process.send?.(message, (error) => (error ? reject(error) : resolve()));
  });
}

async function spawnInheritedChild(lock) {
  const child = spawn(process.execPath, [new URL(import.meta.url).pathname], {
    stdio: ["ignore", "ignore", "pipe", "ipc"],
    detached: true,
    env: {
      ...process.env,
      RSCTF_LOCK_TEST_WAIT_FOR_GO: "0",
      RSCTF_LOCK_TEST_EXIT_AFTER_ACQUIRE: "1",
      RSCTF_LOCK_TEST_INHERITED_TOKEN: lock.token,
    },
  });
  let stderr = "";
  child.stderr.on("data", (chunk) => (stderr += chunk));
  const closed = once(child, "close");
  const [message] = await once(child, "message");
  const [code, signal] = await closed;
  if (code !== 0 || signal) {
    throw new Error(
      `inherited worker failed: ${signal || code} ${stderr.trim()}`,
    );
  }
  return message;
}

async function main() {
  if (process.env.RSCTF_LOCK_TEST_WAIT_FOR_GO === "1") {
    await send({ type: "ready" });
    while (true) {
      const [message] = await once(process, "message");
      if (message?.type === "go") break;
    }
  }

  let lock;
  try {
    lock = await acquireExclusiveProcessLock(lockPath, {
      label: "process-control test worker",
      inheritedToken,
    });
  } catch (error) {
    await send({
      type: "rejected",
      code: error?.code || null,
      message: error.message,
    });
    process.exitCode = 2;
    return;
  }
  await send({
    type: "acquired",
    inherited: lock.inherited,
    token: lock.token,
  });

  if (process.env.RSCTF_LOCK_TEST_EXIT_AFTER_ACQUIRE === "1") {
    await lock.release();
    return;
  }

  while (true) {
    const [message] = await once(process, "message");
    if (message?.type === "spawnInherited") {
      await send({
        type: "inheritedResult",
        result: await spawnInheritedChild(lock),
      });
    } else if (message?.type === "release") {
      await send({ type: "released", released: await lock.release() });
      return;
    }
  }
}

main().catch(async (error) => {
  try {
    await send({ type: "workerError", message: error?.stack || String(error) });
  } finally {
    process.exitCode = 1;
  }
});
