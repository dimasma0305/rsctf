import assert from "node:assert/strict";
import test from "node:test";
import { surfaceFailures } from "./surface-layout-metrics.mjs";
import { contractFixture } from "./layout-contract-fixtures.mjs";

const rect = { x: 10, y: 10, width: 400, height: 200 };
const frame = (time, popup = null) => ({
  time,
  background: [rect],
  popup,
  scroll: [0, 100],
  overflow: false,
});
test("popup entrance is allowed, background compensation is not", () => {
  const frames = [
    frame(0),
    frame(100, { ...rect, y: 18 }),
    frame(400, rect),
    frame(900, rect),
  ];
  assert.deepEqual(surfaceFailures(frames, { popup: true }), []);
  frames[2] = { ...frames[2], background: [{ ...rect, width: 390 }] };
  assert.deepEqual(surfaceFailures(frames, { popup: true }), [
    "background anchor 0 moved",
  ]);
});
test("pointer probes allow declared delayed entrance but still reject later growth", () => {
  const frames = [
    frame(0),
    { ...frame(110, { ...rect, x: 200 }), popupTransitionMs: 300 },
    frame(320, { ...rect, x: 20 }),
    frame(450, rect),
    frame(900, rect),
  ];
  const options = { popup: true, entranceFromFirstPaint: true };
  assert.deepEqual(surfaceFailures(frames, options), []);
  frames.push(frame(1200, { ...rect, height: 250 }));
  assert.deepEqual(surfaceFailures(frames, options), [
    "popup resized or moved after entrance",
  ]);
});
test("entrance waits for the actual CSS transition but later transitions remain observable", () => {
  const frames = [
    frame(0),
    { ...frame(32, { ...rect, x: 300 }), popupTransitionMs: 250 },
    { ...frame(320, { ...rect, x: 30 }), popupAnimating: true },
    frame(380, rect),
    frame(900, rect),
  ];
  const options = { popup: true, entranceFromFirstPaint: true };
  assert.deepEqual(surfaceFailures(frames, options), []);
  frames.push({
    ...frame(1200, { ...rect, height: 250 }),
    popupAnimating: true,
  });
  assert.deepEqual(surfaceFailures(frames, options), [
    "popup resized or moved after entrance",
  ]);
});
test("late popup growth, missing surfaces and lost scroll fail", () => {
  assert.deepEqual(
    surfaceFailures(
      [frame(0), frame(400, rect), frame(900, { ...rect, height: 250 })],
      { popup: true },
    ),
    ["popup resized or moved after entrance"],
  );
  assert.deepEqual(surfaceFailures([frame(0), frame(900)], { popup: true }), [
    "popup did not open",
  ]);
  assert.deepEqual(
    surfaceFailures([frame(0, rect), { ...frame(900), scroll: [0, 0] }], {
      closing: true,
    }),
    ["closing changed scroll position"],
  );
});
test("contract fixtures identify empty reads and reject unknown endpoints", () => {
  assert.equal(contractFixture("/api/nonexistent-layout-fixture"), undefined);
  const profile = contractFixture("/api/account/profile");
  assert(profile?.contract);
  assert.equal(typeof profile.body, "object");
});

test("surfaces cannot briefly appear or hide the background to evade geometry checks", () => {
  const frames = [
    frame(0),
    frame(400, rect),
    { ...frame(900), background: [null] },
  ];
  assert.deepEqual(surfaceFailures(frames, { popup: true }), [
    "background anchor 0 disappeared",
    "popup closed before observation ended",
  ]);
});
