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

test('Toolkit downloads always use personal peers while BYOC keeps its hosting peer', () => {
  const controller = readFileSync(new URL('../../../src/controllers/game/ad/vpn.rs', import.meta.url), 'utf8');
  const download = controller.split('pub async fn download_vpn_config(')[1].split('#[cfg(test)]')[0];
  assert.match(download, /event_security::render_user_config\(&st, &user, &part\)/);
  assert.doesNotMatch(download, /render_wg_config|acquire_roster_access|vpn_access_required/);
  const byoc = readFileSync(new URL('../../../src/controllers/game/ad/byoc.rs', import.meta.url), 'utf8');
  assert.match(byoc, /render_wg_config_for_game/);
});

test('personal peer issuance and kernel retention share the transport eligibility rule', () => {
  const peer = readFileSync(new URL('../../../src/services/event_security/peer.rs', import.meta.url), 'utf8');
  const reconcile = readFileSync(new URL('../../../src/services/ad/vpn/reconcile.rs', import.meta.url), 'utf8');
  assert.match(peer, /WHERE game\.id = \$1 AND \(\{PERSONAL_PEER_GAME_ELIGIBLE_SQL\}\)/);
  assert.match(reconcile, /event_security::PERSONAL_PEER_GAME_ELIGIBLE_SQL/);
});
