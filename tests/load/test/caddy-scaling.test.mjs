import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const caddyfile = readFileSync(
  new URL('../../../deploy/Caddyfile', import.meta.url),
  'utf8',
);

const occurrences = (pattern) => [...caddyfile.matchAll(pattern)].length;

test('Caddy refreshes Docker DNS and retries removed replica addresses', () => {
  assert.equal(occurrences(/dynamic a /g), 3);
  assert.equal(occurrences(/^\s*refresh 1s$/gm), 3);
  assert.equal(occurrences(/^\s*resolvers 127\.0\.0\.11$/gm), 3);
  assert.equal(occurrences(/^\s*versions ipv4$/gm), 3);
  assert.equal(occurrences(/^\s*lb_try_duration 3s$/gm), 3);
  assert.equal(occurrences(/^\s*lb_try_interval 100ms$/gm), 3);
  assert.equal(occurrences(/^\s*dial_timeout 500ms$/gm), 3);
  assert.equal(occurrences(/^\s*keepalive 30s$/gm), 2);
  assert.equal(occurrences(/^\s*keepalive off$/gm), 1);
  assert.match(caddyfile, /@mutation method POST PUT PATCH DELETE/);
  assert.ok(caddyfile.indexOf('handle @mutation') < caddyfile.lastIndexOf('handle {'));
});
