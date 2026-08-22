import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const read = (path) => readFileSync(new URL(path, import.meta.url), 'utf8');
const baseCompose = read('../../../deploy/compose.yml');
const dockerCompose = read('../../../deploy/compose.docker.yml');
const roleDockerCompose = read('../../../deploy/compose.roles.docker.yml');
const roleCompose = read('../../../deploy/compose.roles.yml');
const localCompose = read('../../../docker-compose.yml');
const firewall = read('../../../scripts/docker-proxy-firewall.sh');
const gameContainers = read('../../../src/controllers/game/containers.rs');
const sharedGameContainers = read('../../../src/controllers/game/containers/shared.rs');
const gameContainerSources = `${gameContainers}\n${sharedGameContainers}`;

test('Compose gives rsctf a private bridge target for PlatformProxy ports', () => {
  for (const compose of [baseCompose, localCompose]) {
    assert.match(compose, /RSCTF_DOCKER_PROXY_BIND:/);
    assert.ok(
      /^\s+challenge-proxy:\s*\{\}\s*$/m.test(compose)
        || /^\s+- challenge-proxy\s*$/m.test(compose),
      'rsctf is not attached to challenge-proxy',
    );
    assert.match(compose, /^\s{2}challenge-proxy:\s*$/m);
    assert.match(compose, /^\s+gateway: .*RSCTF_DOCKER_PROXY_BIND/m);
    assert.match(compose, /com\.docker\.network\.bridge\.name:/);
  }
  assert.match(roleCompose, /RSCTF_DOCKER_PROXY_BIND:/);
  assert.match(roleCompose, /^\s+challenge-proxy:\s*\{\}\s*$/m);
});

test('Docker-backed app roles are health-gated on a minimal firewall sidecar', () => {
  for (const compose of [dockerCompose, localCompose]) {
    assert.match(compose, /^\s{2}rsctf-proxy-firewall:\s*$/m);
    assert.match(compose, /^\s+network_mode: host\s*$/m);
    assert.match(compose, /^\s+- NET_ADMIN\s*$/m);
    assert.match(compose, /^\s+read_only: true\s*$/m);
    assert.match(compose, /^\s+condition: service_healthy\s*$/m);
  }
  assert.match(roleDockerCompose, /rsctf-proxy-firewall:/);
  assert.match(roleDockerCompose, /condition: service_healthy/);
});

test('firewall covers userland-proxy and kernel-DNAT paths and supports cleanup', () => {
  assert.match(firewall, /ipt -I INPUT 1 -d "\$bind" -j "\$chain"/);
  assert.match(firewall, /ipt -I DOCKER-USER 1/);
  assert.match(firewall, /--ctdir ORIGINAL --ctorigdst "\$bind"/);
  assert.match(firewall, /-i "\$bridge" -s "\$subnet" -j RETURN/);
  assert.match(firewall, /ipt -A "\$chain" -j DROP/);
  assert.match(firewall, /remove_all_rule_copies INPUT/);
  assert.match(firewall, /RSCTF_PROXY_FIREWALL_RECONCILE_SECONDS/);
});

test('legacy and aggregate Jeopardy creation both carry the proxy decision', () => {
  assert.equal((gameContainerSources.match(/proxy_only: is_proxy/g) ?? []).length, 2);
  assert.equal(
    (gameContainerSources.match(/\.create_workload\([^\n]+is_proxy\)/g) ?? []).length,
    2,
  );
});
