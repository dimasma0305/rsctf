import assert from "node:assert/strict";
import test from "node:test";
import { sessionScore, geometryFailures } from "./layout-shift-metrics.mjs";

test("CLS uses session windows, not the sum of a long navigation session", () => {
  assert.equal(
    sessionScore([
      { startTime: 0, value: 0.02 },
      { startTime: 500, value: 0.03 },
      { startTime: 1500, value: 0.04 },
    ]),
    0.05,
  );
  assert.equal(
    sessionScore(
      Array.from({ length: 8 }, (_, i) => ({
        startTime: i * 900,
        value: 0.01,
      })),
    ),
    0.060000000000000005,
  );
});
test("diagnostic score exposes input-adjacent shifts excluded from standard CLS", () => {
  const entries = [
    { startTime: 100, value: 0.2, hadRecentInput: true },
    { startTime: 700, value: 0.02, hadRecentInput: false },
  ];
  assert.equal(sessionScore(entries), 0.02);
  assert.equal(sessionScore(entries, true), 0.22);
});
test("geometry catches disappearing shells and late controls even when CLS is zero", () => {
  const rect = { x: 0, y: 0, width: 200, height: 100 };
  const frames = [
    { rail: rect },
    { time: 100, rail: null, destination: true, main: rect, title: rect },
    {
      time: 1000,
      rail: rect,
      destination: true,
      main: rect,
      title: { ...rect, y: 20 },
    },
  ];
  assert.deepEqual(geometryFailures(frames), [
    "rail disappeared at 100ms",
    "destination title moved during loading",
  ]);
});
