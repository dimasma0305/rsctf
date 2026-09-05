import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { assertSharedNamespace } from '../../../scripts/check-event-vpn.mjs';

test('DNS deployment rejects a healthy process in an obsolete backend namespace', () => {
  assert.doesNotThrow(() => assertSharedNamespace('net:[1]', 'net:[1]'));
  assert.throws(() => assertSharedNamespace('net:[2]', 'net:[1]'), /stale network namespace/);
  assert.throws(() => assertSharedNamespace('', ''), /stale network namespace/);
});

test('both DNS compose deployments follow explicit backend updates without publishing DNS', () => {
  for (const path of ['compose.dev.yml', 'deploy/compose.event-vpn-ingress.yml']) {
    const source = readFileSync(new URL(`../../../${path}`, import.meta.url), 'utf8');
    const dns = source.split('  event-vpn-dns:\n')[1].split('\nnetworks:')[0];
    assert.match(dns, /condition: service_healthy\n\s+restart: true/);
    assert.match(dns, /network_mode: service:/);
    assert.doesNotMatch(dns, /\n\s+ports:/);
  }
});

test('DNS forwarding is bounded and listens only on the VPN hub', () => {
  const source = readFileSync(new URL('../../../deploy/event-vpn/Corefile', import.meta.url), 'utf8');
  assert.match(source, /bind \{\$RSCTF_EVENT_VPN_HUB_ADDRESS\}/);
  assert.match(source, /max_concurrent 256/);
});
