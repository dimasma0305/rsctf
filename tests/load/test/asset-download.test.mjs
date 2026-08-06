import assert from "node:assert/strict";
import test from "node:test";

import { assetHashFromPath, assetRange } from "../asset-download-model.js";

const HASH = "c5a573e275a0fca6cf6929d324dcc0a6d20882bc922009f1ca0ca022d8e5709d";

test("asset benchmark accepts only a same-origin content-addressed path", () => {
  assert.equal(assetHashFromPath(`/assets/${HASH}/challenge.zip`), HASH);
  assert.equal(
    assetHashFromPath(`https://example.test/assets/${HASH}/challenge.zip`),
    null,
  );
  assert.equal(assetHashFromPath("/assets/not-a-hash/challenge.zip"), null);
  assert.equal(assetHashFromPath(`/assets/${HASH}`), null);
  assert.equal(assetHashFromPath(`/assets/${HASH}/nested/challenge.zip`), null);
  assert.equal(assetHashFromPath(`/assets/${HASH}/challenge.zip?redirect=1`), null);
});

test("asset benchmark covers the short final range and wraps deterministically", () => {
  assert.deepEqual(assetRange(0, 10, 4), { start: 0, end: 3, length: 4 });
  assert.deepEqual(assetRange(2, 10, 4), { start: 8, end: 9, length: 2 });
  assert.deepEqual(assetRange(3, 10, 4), { start: 0, end: 3, length: 4 });
});

test("asset benchmark rejects zero, oversized, and unsafe range inputs", () => {
  assert.throws(() => assetRange(0, 10, 0));
  assert.throws(() => assetRange(0, 10, 11));
  assert.throws(() => assetRange(-1, 10, 4));
});
