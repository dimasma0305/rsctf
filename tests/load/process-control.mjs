import { createHash, randomUUID } from "node:crypto";
import {
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createServer } from "node:net";

export const loadOrchestrationLockPath = "/tmp/rsctf-load-orchestration.lock";

const sleep = (milliseconds) =>
  new Promise((resolve) => setTimeout(resolve, milliseconds));

/**
 * Track mutation promises whose effects may commit after their caller is
 * interrupted. Shutdown code drains this set before it discovers or removes
 * resources, closing the race where a late create lands after cleanup.
 */
export class InFlightMutationDrain {
  #pending = new Set();

  track(operation) {
    if (!operation || typeof operation.then !== "function") {
      throw new TypeError("tracked mutation must be a promise");
    }
    const tracked = Promise.resolve(operation);
    this.#pending.add(tracked);
    const remove = () => this.#pending.delete(tracked);
    tracked.then(remove, remove);
    return tracked;
  }

  get pendingCount() {
    return this.#pending.size;
  }

  async drain() {
    while (this.#pending.size > 0) {
      await Promise.allSettled([...this.#pending]);
    }
  }
}

/** Run one preparation operation with an interruption check on both edges. */
export async function runInterruptibleStep(checkInterrupted, operation) {
  if (typeof checkInterrupted !== "function" || typeof operation !== "function") {
    throw new TypeError("an interruption check and operation are required");
  }
  checkInterrupted();
  const pending = operation();
  const asynchronous = pending && typeof pending.then === "function";
  const result = asynchronous ? await pending : pending;
  if (!asynchronous) {
    // Synchronous Docker/filesystem preparation can otherwise form a long
    // microtask chain that delays Node's SIGINT/SIGTERM callback.
    await new Promise((resolve) => setImmediate(resolve));
  }
  checkInterrupted();
  return result;
}

function validPid(value) {
  return Number.isSafeInteger(value) && value > 0;
}

function pidRunning(pid) {
  if (!validPid(pid)) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error?.code === "ESRCH") return false;
    if (error?.code === "EPERM") return true;
    throw error;
  }
}

const lockOwnerFile = (lockPath) => `${lockPath}.owner.json`;

function lockSocketAddress(lockPath) {
  const identity = createHash("sha256").update(lockPath).digest("hex").slice(0, 32);
  return `\0rsctf-load-${identity}`;
}

function readLockOwner(lockPath) {
  try {
    const owner = JSON.parse(readFileSync(lockOwnerFile(lockPath), "utf8"));
    return owner && typeof owner === "object" ? owner : null;
  } catch (error) {
    if (error?.code === "ENOENT" || error instanceof SyntaxError) return null;
    throw error;
  }
}

function writeLockOwner(lockPath, owner) {
  const path = lockOwnerFile(lockPath);
  const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
  try {
    writeFileSync(temporary, `${JSON.stringify(owner, null, 2)}\n`, {
      mode: 0o600,
      flag: "wx",
    });
    renameSync(temporary, path);
  } finally {
    rmSync(temporary, { force: true });
  }
}

function closeServer(server) {
  return new Promise((resolve, reject) => {
    server.close((error) => {
      if (error && error.code !== "ERR_SERVER_NOT_RUNNING") reject(error);
      else resolve();
    });
  });
}

function listenExclusively(lockPath) {
  return new Promise((resolve, reject) => {
    const server = createServer((socket) => socket.destroy());
    const onError = (error) => {
      server.removeListener("listening", onListening);
      reject(error);
    };
    const onListening = () => {
      server.removeListener("error", onError);
      resolve(server);
    };
    server.once("error", onError);
    server.once("listening", onListening);
    server.listen(lockSocketAddress(lockPath));
  });
}

/**
 * Claim a host-local orchestration lease through a Linux abstract Unix socket.
 * The kernel releases the socket when its process dies, so stale PID files never
 * decide ownership. A direct child may inherit the diagnostic owner token, but
 * only the socket-owning process can release the authoritative lease.
 */
export async function acquireExclusiveProcessLock(
  lockPath,
  {
    label = "orchestration",
    inheritedToken = null,
    metadata = {},
  } = {},
) {
  if (typeof lockPath !== "string" || !lockPath.trim()) {
    throw new TypeError("process lock path must be a non-empty string");
  }
  if (process.platform !== "linux") {
    throw new Error("the load orchestration lease requires Linux abstract Unix sockets");
  }
  const inherited = typeof inheritedToken === "string" && inheritedToken.length >= 16;
  if (inherited) {
    const owner = readLockOwner(lockPath);
    if (
      owner?.token !== inheritedToken ||
      Number(owner?.pid) !== process.ppid ||
      !pidRunning(Number(owner?.pid))
    ) {
      throw new Error(`${label} received an invalid or stale inherited process lock`);
    }
    return {
      token: inheritedToken,
      inherited: true,
      owner,
      release: async () => false,
    };
  }

  let server;
  try {
    server = await listenExclusively(lockPath);
  } catch (error) {
    if (error?.code !== "EADDRINUSE") throw error;
    const owner = readLockOwner(lockPath);
    const suffix = validPid(Number(owner?.pid)) ? ` in process ${owner.pid}` : "";
    const busy = new Error(`${label} is already running${suffix}`);
    busy.code = "ELOCKED";
    throw busy;
  }

  const token = randomUUID();
  const owner = {
    ...metadata,
    schemaVersion: 2,
    label,
    pid: process.pid,
    token,
    createdAtMs: Date.now(),
  };
  try {
    writeLockOwner(lockPath, owner);
  } catch (error) {
    await closeServer(server);
    throw error;
  }

  let released = false;
  return {
    token,
    inherited: false,
    owner,
    async release() {
      if (released) return false;
      released = true;
      let metadataError = null;
      try {
        const current = readLockOwner(lockPath);
        if (current?.token === token && Number(current?.pid) === process.pid) {
          rmSync(lockOwnerFile(lockPath), { force: true });
        }
      } catch (error) {
        metadataError = error;
      }
      await closeServer(server);
      if (metadataError) throw metadataError;
      return true;
    },
  };
}

function leaderRunning(child) {
  return Boolean(
    child && child.exitCode === null && child.signalCode === null,
  );
}

export function processGroupRunning(pid) {
  if (!validPid(pid)) return false;
  try {
    process.kill(-pid, 0);
    return true;
  } catch (error) {
    if (error?.code === "ESRCH") return false;
    if (error?.code === "EPERM") return true;
    throw error;
  }
}

export function signalChild(child, signal, processGroup = false) {
  if (!child) return false;
  if (processGroup && validPid(child.pid)) {
    try {
      process.kill(-child.pid, signal);
      return true;
    } catch (error) {
      if (error?.code === "ESRCH") return false;
      throw error;
    }
  }
  return leaderRunning(child) ? child.kill(signal) : false;
}

function stopped(child, processGroup) {
  return (
    !leaderRunning(child) &&
    (!processGroup || !processGroupRunning(child?.pid))
  );
}

async function waitUntilStopped(child, processGroup, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (!stopped(child, processGroup) && Date.now() < deadline) {
    await sleep(50);
  }
  return stopped(child, processGroup);
}

/** Terminate one child, optionally including every descendant in its process group. */
export async function stopChildTree(
  child,
  { processGroup = false, graceMs = 5_000 } = {},
) {
  if (!child || stopped(child, processGroup)) return;
  if (!Number.isSafeInteger(graceMs) || graceMs < 1) {
    throw new Error("child shutdown grace must be a positive integer");
  }

  signalChild(child, "SIGTERM", processGroup);
  if (await waitUntilStopped(child, processGroup, graceMs)) return;

  signalChild(child, "SIGKILL", processGroup);
  if (await waitUntilStopped(child, processGroup, graceMs)) return;

  throw new Error(
    `${processGroup ? "child process group" : "child process"} did not exit after SIGTERM and SIGKILL`,
  );
}

/** Wait for a completion promise without leaving a referenced timeout behind. */
export async function waitForCompletion(completion, timeoutMs) {
  if (!completion || typeof completion.then !== "function") {
    throw new TypeError("completion must be a promise");
  }
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1) {
    throw new RangeError("completion timeout must be a positive integer");
  }

  let timer;
  const timeout = new Promise((resolve) => {
    timer = setTimeout(() => resolve(false), timeoutMs);
  });
  try {
    return await Promise.race([completion.then(() => true), timeout]);
  } finally {
    clearTimeout(timer);
  }
}
