import http from "k6/http";
import exec from "k6/execution";
import { Rate, Trend } from "k6/metrics";
import {
  clearInterval,
  clearTimeout,
  setInterval,
  setTimeout,
} from "k6/timers";
import { WebSocket } from "k6/websockets";

import {
  signalrHandshake,
  signalrPing,
  unsupportedSignalrInvocation,
  validAbuseClose,
} from "../read-only-websocket-flood.js";

const TARGET = __ENV.TARGET;
const WS_TARGET = TARGET.replace(/^http/, "ws");
const GAME = __ENV.GAME;
const RATE = Number(__ENV.RATE || 20);
const VUS = Number(__ENV.VUS || 40);
const MAX_VUS = Number(__ENV.MAX_VUS || 160);
const FRAME_BYTES = Number(__ENV.FRAME_BYTES || 65_536);
if (!TARGET || !/^\d+$/.test(GAME) || FRAME_BYTES !== 65_536) {
  throw new Error("TARGET, GAME, and exact FRAME_BYTES=65536 are required");
}

const invalid = new Rate("readonly_ws_invalid");
const server5xx = new Rate("server_5xx");
const closeMs = new Trend("readonly_ws_close_ms", true);

export const options = {
  scenarios: {
    inboundFlood: {
      executor: "constant-arrival-rate",
      rate: RATE,
      timeUnit: "1s",
      duration: __ENV.DURATION || "30s",
      preAllocatedVUs: VUS,
      maxVUs: MAX_VUS,
    },
    health: {
      executor: "constant-arrival-rate",
      exec: "health",
      rate: 2,
      timeUnit: "1s",
      duration: __ENV.DURATION || "30s",
      preAllocatedVUs: 4,
      maxVUs: 8,
    },
  },
  thresholds: {
    readonly_ws_invalid: ["rate==0"],
    server_5xx: ["rate==0"],
    dropped_iterations: ["count==0"],
    readonly_ws_close_ms: ["p(95)<4000"],
    "http_req_duration{lane:health}": ["p(95)<250"],
    "http_req_failed{lane:health}": ["rate==0"],
  },
};

export default function () {
  const sequence = exec.scenario.iterationInTest;
  const mode = sequence % 5;
  const signalr = mode >= 2;
  const url = signalr
    ? `${WS_TARGET}/hub/attack?game=${GAME}`
    : `${WS_TARGET}/hub/attack/ws?game=${GAME}`;
  const started = Date.now();
  let greeting = false;
  let abuseSent = false;
  let sustainedTimer;
  let finalized = false;
  const socket = new WebSocket(url);
  const failSafe = setTimeout(() => {
    if (finalized) return;
    finalized = true;
    if (sustainedTimer !== undefined) clearInterval(sustainedTimer);
    closeMs.add(Date.now() - started);
    invalid.add(true);
    // A graceful close can block while CONNECTING or wait indefinitely for an
    // uncooperative peer. Abort the gate so every VU has a hard lifecycle bound.
    exec.test.abort("server did not reject inbound WebSocket traffic within 5s");
  }, 5000);

  socket.addEventListener("open", () => {
    if (signalr) socket.send(signalrHandshake());
  });
  socket.addEventListener("message", (event) => {
    if (signalr && !greeting && String(event.data).startsWith("{}")) {
      greeting = true;
      abuseSent = true;
      if (mode === 2) {
        socket.send(unsupportedSignalrInvocation(sequence));
      } else if (mode === 3) {
        // The handshake already consumes one frame token. A burst above the
        // remaining per-connection capacity must close with policy 1008.
        for (let index = 0; index < 40; index += 1) socket.send(signalrPing());
      } else {
        // Sustain 20 frames/second above the eight-frame refill rate. The
        // initial burst capacity delays rejection, so this exercises the
        // refill path instead of only a one-tick burst.
        sustainedTimer = setInterval(() => socket.send(signalrPing()), 50);
      }
    } else if (
      !signalr &&
      !greeting &&
      String(event.data).includes('"kind":"hello"')
    ) {
      greeting = true;
      abuseSent = true;
      // The exact configured ceiling must reach application metering and
      // close as a read-only policy violation. One additional byte must be
      // rejected by the transport envelope as message-too-big.
      socket.send("x".repeat(FRAME_BYTES + (mode === 1 ? 1 : 0)));
    }
  });
  // k6 emits `error` before `close` for expected non-1000 server policy
  // closes. The close event's exact code is therefore authoritative.
  socket.addEventListener("error", () => {});
  socket.addEventListener("close", (event) => {
    if (finalized) return;
    finalized = true;
    clearTimeout(failSafe);
    if (sustainedTimer !== undefined) clearInterval(sustainedTimer);
    closeMs.add(Date.now() - started);
    invalid.add(
      !greeting || !abuseSent || !validAbuseClose(event.code, mode === 1),
    );
  });
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, {
    responseType: "text",
    tags: { lane: "health" },
  });
  server5xx.add(response.status >= 500);
  invalid.add(response.status !== 200 || response.body !== "ok");
}
