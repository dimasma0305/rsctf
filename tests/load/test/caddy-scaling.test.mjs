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
  assert.equal(occurrences(/^\s*keepalive 30s$/gm), 1);
  assert.equal(occurrences(/^\s*keepalive off$/gm), 2);
  assert.match(caddyfile, /@mutation method POST PUT PATCH DELETE/);
  assert.ok(caddyfile.indexOf('handle @mutation') < caddyfile.lastIndexOf('handle {'));
});

test('Caddy sends singleton-owned terminal and KotH recovery routes to the network owner', () => {
  const matcher = caddyfile.match(/@network path_regexp network (\S+)/)?.[1];
  assert.ok(matcher, 'network-owner route matcher missing');
  const route = new RegExp(matcher);
  for (const path of [
    '/hub/containerExec',
    '/hub/containerExec/negotiate',
    '/hub/containerExec/games/37',
    '/hub/containerExec/games/37/negotiate',
    '/api/edit/games/17/ad/koth/23/recover',
    '/api/stateful/edit/games/17/ad/koth/23/recover',
  ]) {
    assert.equal(route.test(path), true, path);
  }
  for (const path of [
    '/hub/containerExec/games/0',
    '/hub/containerExec/games/not-a-game',
    '/hub/containerExec/games/37/extra',
    '/api/edit/games/0/ad/koth/23/recover',
    '/api/edit/games/17/ad/koth/0/recover',
    '/api/edit/games/17/ad/koth/23/recover/extra',
  ]) {
    assert.equal(route.test(path), false, path);
  }
  const networkHandle = caddyfile.indexOf('handle @network');
  const mutationHandle = caddyfile.indexOf('handle @mutation');
  assert.ok(networkHandle >= 0 && networkHandle < mutationHandle);
  const networkBlock = caddyfile.slice(networkHandle, mutationHandle);
  assert.match(networkBlock, /^\s*keepalive off$/m);
});

test('Caddy excludes bearer-bearing hub request URIs from access logs', () => {
  assert.match(caddyfile, /^\s*@hubBearer path \/hub\/\*$/m);
  assert.match(caddyfile, /^\s*log_skip @hubBearer$/m);
});
