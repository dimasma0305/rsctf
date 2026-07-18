import assert from 'node:assert/strict';
import test from 'node:test';

import { retainedManifestMatchesGame } from '../retention-identity.mjs';

const retained = {
  retained: true,
  createdAtMs: 1_784_126_910_831,
  jeoGame: 115,
  mixGame: 116,
};

test('retained manifests bind protection to the exact generated game identity', () => {
  assert.equal(retainedManifestMatchesGame(retained, 115, 'LOADTEST-JEO-1784126910831'), true);
  assert.equal(retainedManifestMatchesGame(retained, 116, 'LOADTEST-MIX-1784126910831'), true);
  assert.equal(retainedManifestMatchesGame(retained, 115, 'LOADTEST-JEO-1784200000000'), false);
  assert.equal(retainedManifestMatchesGame(retained, 117, 'LOADTEST-JEO-1784126910831'), false);
});

test('non-retained and malformed manifests cannot claim a current game', () => {
  assert.equal(
    retainedManifestMatchesGame({ ...retained, retained: false }, 115, 'LOADTEST-JEO-1784126910831'),
    false
  );
  assert.throws(
    () => retainedManifestMatchesGame({ ...retained, createdAtMs: null }, 115, ''),
    /invalid createdAtMs/
  );
});
