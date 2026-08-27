import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

function source(relativePath) {
  return readFileSync(new URL(relativePath, import.meta.url), 'utf8');
}

test('every first-party normal-submit load client sends an opaque attempt id', () => {
  for (const path of [
    '../k6/lifecycle.js',
    '../k6/cheat-event.js',
    '../k6/team-event.js',
  ]) {
    const script = source(path);
    assert.match(script, /submitAttemptId/);
    assert.match(script, /attemptId/);
  }

  for (const path of ['../admin-lifecycle.mjs', '../cheat-acceptance.mjs']) {
    const script = source(path);
    assert.match(script, /randomUUID/);
    assert.match(script, /attemptId:\s*randomUUID\(\)/);
  }
});

test('load-generated attempt identities are RFC-4122-shaped SHA-256 projections', () => {
  const generator = source('../submit-attempt-id.js');
  assert.match(generator, /crypto\.sha256/);
  assert.match(generator, /4\$\{hex\.slice\(13, 16\)\}/);
  assert.match(generator, /a\$\{hex\.slice\(17, 20\)\}/);
});
