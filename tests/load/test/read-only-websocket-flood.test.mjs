import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  signalrHandshake,
  signalrPing,
  unsupportedSignalrInvocation,
  validAbuseClose,
} from "../read-only-websocket-flood.js";

const scenario = readFileSync(
  new URL("../k6/read-only-websocket-flood.js", import.meta.url),
  "utf8",
);
const runner = readFileSync(
  new URL("../read-only-websocket-flood.mjs", import.meta.url),
  "utf8",
);

test("read-only feed protocol fixtures are exact and reject by policy or frame size", () => {
  assert.equal(signalrHandshake(), '{"protocol":"json","version":1}\u001e');
  assert.equal(signalrPing(), '{"type":6}\u001e');
  assert.match(
    unsupportedSignalrInvocation(7),
    /"type":1.*"target":"Upload".*\u001e$/,
  );
  assert.equal(validAbuseClose(1008), true);
  assert.equal(validAbuseClose(1009), false);
  assert.equal(validAbuseClose(1009, true), true);
  assert.equal(validAbuseClose(1008, true), false);
  assert.equal(validAbuseClose(1000), false);
});

test("websocket flood uses fixed connection arrivals and an independent health lane", () => {
  assert.match(scenario, /from ["']k6\/websockets["']/);
  assert.doesNotMatch(scenario, /from ["']k6\/ws["']/);
  assert.match(scenario, /executor: ["']constant-arrival-rate["']/);
  assert.match(scenario, /\/hub\/attack\?game=/);
  assert.match(scenario, /\/hub\/attack\/ws\?game=/);
  assert.doesNotMatch(scenario, /X-Real-IP|X-Forwarded-For|sourceIp/);
  assert.match(scenario, /FRAME_BYTES \+ \(mode === 1 \? 1 : 0\)/);
  assert.match(scenario, /FRAME_BYTES !== 65_536/);
  assert.doesNotMatch(scenario, /transportError/);
  assert.match(scenario, /index < 40/);
  assert.match(scenario, /socket\.send\(signalrPing\(\)\)/);
  assert.match(
    scenario,
    /setInterval\(\(\) => socket\.send\(signalrPing\(\)\), 50\)/,
  );
  assert.match(scenario, /validAbuseClose\(event\.code, mode === 1\)/);
  assert.match(scenario, /if \(finalized\) return/);
  assert.match(scenario, /invalid\.add\(true\)/);
  assert.match(scenario, /exec\.test\.abort\(/);
  assert.doesNotMatch(
    scenario.match(/const failSafe = setTimeout\([\s\S]*?\}, 5000\);/)?.[0] || "",
    /socket\.close\(/,
  );
  assert.match(scenario, /\/healthz/);
  assert.match(scenario, /http_req_duration\{lane:health\}/);
  assert.match(scenario, /p\(95\)<250/);
  assert.match(scenario, /dropped_iterations: \[["']count==0["']\]/);
  assert.match(runner, /READONLY_WS_FLOOD_ACK/);
  assert.match(runner, /resource evidence requires a loopback TARGET/);
  assert.doesNotMatch(runner, /ALLOW_REMOTE_READONLY_WS_FLOOD/);
  assert.match(runner, /setInterval\(sample, 1000\)/);
  assert.match(runner, /peakCpuPercent/);
  assert.match(runner, /peakMemoryBytes/);
  assert.match(runner, /MAX_CPU_PERCENT/);
  assert.match(runner, /MAX_MEMORY_MIB/);
  assert.match(runner, /Array\.from\(\{ length: 128 \}/);
  assert.match(runner, /129th same-client WebSocket bypassed admission/);
  assert.match(runner, /post-rejection re-admission probe/);
  assert.match(runner, /bad SignalR handshake closed with/);
  assert.match(runner, /idle-timeout close was code=/);
  assert.match(runner, /ReceivedGameNotice/);
  assert.match(runner, /notice event delivered id/);
  assert.match(runner, /connectionsRejected/);
  assert.match(runner, /fanout\?\.rejectedEvents/);
  assert.match(runner, /waitForFloodActivity/);
  assert.equal((runner.match(/livePublic\(\) !== ["']1["']/g) || []).length, 2);
});
