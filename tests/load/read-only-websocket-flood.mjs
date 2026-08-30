// Fixed-rate application-frame flood against the public read-only attack feeds.
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";

import { mintJwt, sql, stat, TARGET } from "./lib.mjs";

const game = Number(process.env.WEBSOCKET_GAME || process.env.GAME);
if (!Number.isSafeInteger(game) || game <= 0)
  throw new Error("WEBSOCKET_GAME (or GAME) is required");
const maxCpuPercent = Number(process.env.MAX_CPU_PERCENT || 400);
const maxMemoryMib = Number(process.env.MAX_MEMORY_MIB || 4096);
if (!Number.isFinite(maxCpuPercent) || maxCpuPercent <= 0)
  throw new Error("MAX_CPU_PERCENT must be positive");
if (!Number.isFinite(maxMemoryMib) || maxMemoryMib <= 0)
  throw new Error("MAX_MEMORY_MIB must be positive");
const targetUrl = new URL(TARGET);
if (process.env.READONLY_WS_FLOOD_ACK !== "1")
  throw new Error("set READONLY_WS_FLOOD_ACK=1 for this inbound-abuse gate");
if (!["127.0.0.1", "localhost", "[::1]"].includes(targetUrl.hostname)) {
  throw new Error(
    "read-only WebSocket resource evidence requires a loopback TARGET bound to RSCTF_CONTAINER",
  );
}
const livePublic = () =>
  sql(
    `SELECT COUNT(*) FROM "Games" WHERE id=${game} AND hidden=FALSE ` +
      `AND start_time_utc<=clock_timestamp() AND end_time_utc>=clock_timestamp()`,
  );

function memoryBytes(value) {
  const match = String(value)
    .trim()
    .match(/^([0-9.]+)\s*([KMGT]?i?B)$/i);
  if (!match)
    throw new Error(`unsupported docker memory value ${JSON.stringify(value)}`);
  const powers = { b: 0, kib: 1, mib: 2, gib: 3, tib: 4 };
  return Number(match[1]) * 1024 ** powers[match[2].toLowerCase()];
}

const wait = (milliseconds) =>
  new Promise((resolve) => setTimeout(resolve, milliseconds));

function websocketUrl(path) {
  const url = new URL(path, `${TARGET}/`);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.toString();
}

function openSocket(path, label) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(websocketUrl(path));
    const timeout = setTimeout(() => {
      socket.close();
      reject(new Error(`${label} WebSocket upgrade timed out`));
    }, 10_000);
    socket.addEventListener(
      "open",
      () => {
        clearTimeout(timeout);
        resolve(socket);
      },
      { once: true },
    );
    socket.addEventListener(
      "error",
      () => {
        clearTimeout(timeout);
        reject(new Error(`${label} WebSocket upgrade failed`));
      },
      { once: true },
    );
    socket.addEventListener(
      "close",
      () => {
        clearTimeout(timeout);
        reject(new Error(`${label} WebSocket closed before upgrade`));
      },
      { once: true },
    );
  });
}

function closeSocket(socket) {
  if (!socket || socket.readyState === WebSocket.CLOSED)
    return Promise.resolve();
  return new Promise((resolve) => {
    const timeout = setTimeout(resolve, 2_000);
    const finish = () => {
      clearTimeout(timeout);
      resolve();
    };
    socket.addEventListener("close", finish, { once: true });
    socket.addEventListener("error", finish, { once: true });
    socket.close();
  });
}

function waitForClose(socket, label, timeoutMs) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      socket.close();
      reject(new Error(`${label} did not close within ${timeoutMs}ms`));
    }, timeoutMs);
    socket.addEventListener(
      "close",
      (event) => {
        clearTimeout(timeout);
        resolve(event);
      },
      { once: true },
    );
    socket.addEventListener(
      "error",
      () => {},
      { once: true },
    );
  });
}

function completeSignalRHandshake(socket, label) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(
      () => reject(new Error(`${label} SignalR handshake timed out`)),
      5_000,
    );
    const onMessage = (event) => {
      const frames = String(event.data).split("\u001e").filter(Boolean);
      if (!frames.includes("{}")) return;
      clearTimeout(timeout);
      socket.removeEventListener("message", onMessage);
      resolve();
    };
    socket.addEventListener("message", onMessage);
    socket.send('{"protocol":"json","version":1}\u001e');
  });
}

async function assertBadHandshakeClose() {
  const socket = await openSocket(
    `/hub/attack?game=${game}`,
    "bad-handshake probe",
  );
  try {
    const closed = waitForClose(socket, "bad-handshake probe", 7_000);
    socket.send("{}\u001e");
    const event = await closed;
    if (event.code !== 1008)
      throw new Error(
        `bad SignalR handshake closed with ${event.code}, expected 1008`,
      );
  } finally {
    await closeSocket(socket);
  }
}

async function expectAdmissionRejected(path) {
  await new Promise((resolve, reject) => {
    const socket = new WebSocket(websocketUrl(path));
    const timeout = setTimeout(() => {
      socket.close();
      reject(new Error("129th same-client WebSocket did not settle"));
    }, 10_000);
    const rejected = () => {
      clearTimeout(timeout);
      resolve();
    };
    socket.addEventListener(
      "open",
      () => {
        clearTimeout(timeout);
        socket.close();
        reject(new Error("129th same-client WebSocket bypassed admission"));
      },
      { once: true },
    );
    socket.addEventListener("error", rejected, { once: true });
    socket.addEventListener("close", rejected, { once: true });
  });
}

async function openReplacementAfterRelease(path) {
  const deadline = Date.now() + 5_000;
  let lastError;
  do {
    try {
      return await openSocket(path, "post-rejection re-admission probe");
    } catch (error) {
      lastError = error;
      await wait(100);
    }
  } while (Date.now() < deadline);
  throw new Error(
    `released WebSocket permit was not reusable: ${lastError?.message || "unknown error"}`,
  );
}

async function assertConnectionAdmissionAndRelease(token) {
  const path = `/hub/attack/ws?game=${game}`;
  const before = await realtimeMetrics(token);
  const attempts = await Promise.allSettled(
    Array.from({ length: 128 }, (_, index) =>
      openSocket(path, `admission probe ${index + 1}`),
    ),
  );
  const sockets = attempts
    .filter(({ status }) => status === "fulfilled")
    .map(({ value }) => value);
  const failures = attempts.filter(({ status }) => status === "rejected");
  if (failures.length > 0) {
    await Promise.all(sockets.map(closeSocket));
    throw new Error(
      `only ${sockets.length}/128 same-client WebSockets were admitted`,
    );
  }
  try {
    await expectAdmissionRejected(path);
    const rejected = await realtimeMetrics(token);
    if (
      rejected.websocket.connectionsRejected <=
      before.websocket.connectionsRejected
    ) {
      throw new Error(
        "129th WebSocket failed without incrementing the server admission metric",
      );
    }
    await closeSocket(sockets.pop());
    sockets.push(await openReplacementAfterRelease(path));
  } finally {
    await Promise.all(sockets.map(closeSocket));
  }
}

async function assertIdleTimeout() {
  const socket = await openSocket(
    `/hub/attack?game=${game}`,
    "idle-timeout probe",
  );
  try {
    await completeSignalRHandshake(socket, "idle-timeout probe");
    const started = Date.now();
    const event = await waitForClose(socket, "idle-timeout probe", 96_000);
    const elapsed = Date.now() - started;
    if (event.code !== 1008 || elapsed < 89_000 || elapsed > 95_000) {
      throw new Error(
        `idle-timeout close was code=${event.code} elapsed=${elapsed}ms`,
      );
    }
  } finally {
    await closeSocket(socket);
  }
}

function adminToken() {
  const row = sql(
    "SELECT id::text || '|' || security_stamp FROM \"AspNetUsers\" " +
      "WHERE role=3 AND security_stamp IS NOT NULL " +
      "AND (lockout_end IS NULL OR lockout_end<=clock_timestamp()) ORDER BY id LIMIT 1",
  );
  if (!row)
    throw new Error(
      "read-only WebSocket event probe requires one active Admin account",
    );
  const [id, stamp] = row.split("|");
  return mintJwt(id, stamp, 3);
}

async function realtimeMetrics(token) {
  const response = await fetch(new URL("/api/admin/realtime/metrics", TARGET), {
    headers: { Authorization: `Bearer ${token}` },
    signal: AbortSignal.timeout(10_000),
  });
  const body = await response.json();
  if (
    response.status !== 200 ||
    !Number.isSafeInteger(body?.websocket?.inboundFrames) ||
    !Number.isSafeInteger(body?.websocket?.inboundQuotaRejections) ||
    !Number.isSafeInteger(body?.websocket?.quotaCloses) ||
    !Number.isSafeInteger(body?.fanout?.rejectedEvents) ||
    body.fanout.rejectedEvents < 0
  ) {
    throw new Error(
      `realtime metrics failed: ${response.status} ${JSON.stringify(body)}`,
    );
  }
  return body;
}

async function waitForFloodActivity(token, baseline) {
  const deadline = Date.now() + 10_000;
  do {
    const current = await realtimeMetrics(token);
    if (
      current.websocket.inboundFrames > baseline.websocket.inboundFrames &&
      current.websocket.inboundQuotaRejections >
        baseline.websocket.inboundQuotaRejections &&
      current.websocket.quotaCloses > baseline.websocket.quotaCloses
    ) {
      return;
    }
    await wait(200);
  } while (Date.now() < deadline);
  throw new Error(
    "fixed-rate WebSocket load did not reach its metered quota-close phase",
  );
}

function waitForNotice(socket, content) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(
      () =>
        reject(new Error("notice event was not delivered during the flood")),
      10_000,
    );
    const onMessage = (event) => {
      for (const frame of String(event.data).split("\u001e").filter(Boolean)) {
        let message;
        try {
          message = JSON.parse(frame);
        } catch {
          continue;
        }
        const notice =
          message.target === "ReceivedGameNotice"
            ? message.arguments?.[0]
            : undefined;
        if (notice?.values?.[0] !== content) continue;
        clearTimeout(timeout);
        socket.removeEventListener("message", onMessage);
        resolve(notice);
        return;
      }
    };
    socket.addEventListener("message", onMessage);
  });
}

async function assertEventDeliveryDuringFlood(token, baseline) {
  await waitForFloodActivity(token, baseline);
  const socket = await openSocket(
    `/hub/user?game=${game}`,
    "notice-delivery probe",
  );
  let noticeId;
  try {
    await completeSignalRHandshake(socket, "notice-delivery probe");
    const content = `rsctf-readonly-ws-${randomUUID()}`;
    const delivered = waitForNotice(socket, content);
    const response = await fetch(
      new URL(`/api/edit/games/${game}/notices`, TARGET),
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          content,
          operationId: randomUUID(),
          publishAt: null,
        }),
        signal: AbortSignal.timeout(10_000),
      },
    );
    const body = await response.json();
    if (response.status !== 200 || !Number.isSafeInteger(body.id)) {
      throw new Error(
        `notice event fixture failed: ${response.status} ${JSON.stringify(body)}`,
      );
    }
    noticeId = body.id;
    const observed = await delivered;
    if (observed.id !== noticeId)
      throw new Error(
        `notice event delivered id ${observed.id}, expected ${noticeId}`,
      );
  } finally {
    if (noticeId !== undefined) {
      const cleanup = await fetch(
        new URL(`/api/edit/games/${game}/notices/${noticeId}`, TARGET),
        {
          method: "DELETE",
          headers: { Authorization: `Bearer ${token}` },
          signal: AbortSignal.timeout(10_000),
        },
      );
      if (cleanup.status !== 200)
        throw new Error(`notice event cleanup failed with ${cleanup.status}`);
    }
    await closeSocket(socket);
  }
}

async function runObservedK6(token) {
  const samples = [];
  const sampleErrors = [];
  const sample = () => {
    try {
      const resource = stat();
      if (!Number.isFinite(resource.cpu))
        throw new Error("docker stats returned an invalid CPU sample");
      samples.push({
        at: Date.now(),
        cpuPercent: resource.cpu,
        memoryBytes: memoryBytes(resource.mem),
      });
    } catch (error) {
      sampleErrors.push(error.message);
    }
  };
  sample();
  const args = ["run"];
  const summaryJson = process.env.SUMMARY_JSON || "";
  if (summaryJson) args.push("--summary-export", summaryJson);
  args.push(
    new URL("./k6/read-only-websocket-flood.js", import.meta.url).pathname,
  );
  const baseline = await realtimeMetrics(token);
  const child = spawn("k6", args, {
    stdio: "inherit",
    env: {
      ...process.env,
      TARGET,
      GAME: String(game),
      RATE: process.env.RATE || "20",
      VUS: process.env.VUS || "40",
      MAX_VUS: process.env.MAX_VUS || "160",
      DURATION: process.env.DURATION || "30s",
      FRAME_BYTES: process.env.FRAME_BYTES || "65536",
    },
  });
  const eventDelivery = assertEventDeliveryDuringFlood(token, baseline).then(
    () => ({ error: null }),
    (error) => ({ error }),
  );
  const timer = setInterval(sample, 1000);
  let status;
  try {
    status = await new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", (code) => resolve(code ?? 1));
    });
  } finally {
    clearInterval(timer);
    sample();
  }
  const delivery = await eventDelivery;
  if (delivery.error) throw delivery.error;
  if (sampleErrors.length > 0)
    throw new Error(
      `read-only WebSocket resource sampling failed: ${sampleErrors.join("; ")}`,
    );
  if (samples.length < 2)
    throw new Error("read-only WebSocket resource window is incomplete");
  const peakCpuPercent = Math.max(...samples.map((entry) => entry.cpuPercent));
  const peakMemoryBytes = Math.max(
    ...samples.map((entry) => entry.memoryBytes),
  );
  if (peakCpuPercent > maxCpuPercent) {
    throw new Error(
      `read-only WebSocket CPU peak ${peakCpuPercent}% exceeded ${maxCpuPercent}%`,
    );
  }
  if (peakMemoryBytes > maxMemoryMib * 1024 * 1024) {
    throw new Error(
      `read-only WebSocket memory peak ${peakMemoryBytes} exceeded ${maxMemoryMib} MiB`,
    );
  }
  console.log(
    JSON.stringify({
      resourceSamples: samples.length,
      peakCpuPercent,
      peakMemoryBytes,
    }),
  );
  return status;
}

if (livePublic() !== "1")
  throw new Error(
    `game ${game} must be live and public for the read-only feed drill`,
  );
const token = adminToken();
await assertBadHandshakeClose();
const status = await runObservedK6(token);
await assertIdleTimeout();
await assertConnectionAdmissionAndRelease(token);
if (livePublic() !== "1")
  throw new Error(
    `game ${game} lost its public lifecycle invariant during the read-only feed drill`,
  );
process.exit(status);
