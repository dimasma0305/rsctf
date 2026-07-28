import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const app = readFileSync(new URL('../applib.mjs', import.meta.url), 'utf8');
const fixtures = readFileSync(new URL('../admin-fixtures.mjs', import.meta.url), 'utf8');

test('protected load teardown clears supported blob owners through application APIs', () => {
  assert.match(
    app,
    /api\('DELETE', `\/api\/edit\/games\/\$\{gameId\}\/writeups`/,
  );
  assert.match(app, /body:\s*\{\s*attachmentType:\s*'None'\s*\}/);
  assert.match(
    app,
    /await setAdScoringPaused\(gameId, true\);[\s\S]*await clearDisposableLoadBlobOwners\(gameId, title\);[\s\S]*return exactLoadGameCleanup/,
  );
  assert.equal(
    (app.match(/await exactProtectedLoadGameCleanup\(/g) || []).length,
    2,
    'both protected-deletion recovery branches must use blob-safe cleanup',
  );
});

test('exact fallback remains fail-closed for unsupported blob owner classes', () => {
  for (const predicate of [
    'poster_hash IS NOT NULL',
    'original_archive_blob_path IS NOT NULL',
    'flag.attachment_id IS NOT NULL',
  ]) {
    assert.ok(fixtures.includes(predicate), `missing fail-closed predicate: ${predicate}`);
  }
  assert.match(
    fixtures,
    /RAISE EXCEPTION 'disposable admin fixture % still owns blob metadata'/,
  );
});

test('protected teardown snapshots every game-owned runtime before deletion can erase its identity', () => {
  assert.match(fixtures, /export function disposableLoadGameRuntimeIds\(gameId, expectedTitle\)/);
  assert.match(
    fixtures,
    /requireExactDisposableGame\(\{ id, title \}, 'disposable load fixture'\)/,
  );
  assert.match(
    app,
    /for \(const containerId of disposableLoadGameRuntimeIds\(gameId, title\)\) \{\s*managedEventContainers\.add\(containerId\);/,
  );
  assert.match(
    app,
    /removeManagedKothContainers\(runtimeIds\);[\s\S]*exactProtectedLoadGameCleanup\(g, title, runtimeIds\)/,
  );
});
