#!/usr/bin/env node
// Read-only post-deployment check; never restart a backend or print credentials.
import { execFileSync } from 'node:child_process';
import { readlinkSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

export function assertSharedNamespace(backend, dns) {
  if (!backend || backend !== dns) {
    throw new Error('VPN DNS is in a stale network namespace; recreate event-vpn-dns against the current backend');
  }
}

function run(command, args) {
  return execFileSync(command, args, { encoding: 'utf8', timeout: 5000, stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}

export function checkEventVpn(backendName, dnsName) {
  const inspect = name => JSON.parse(run('docker', ['inspect', name]))[0];
  const backend = inspect(backendName);
  const dns = inspect(dnsName);
  for (const container of [backend, dns]) {
    if (!container.State.Running || container.State.Pid <= 0) throw new Error('Backend and VPN DNS must both be running');
  }
  assertSharedNamespace(readlinkSync(`/proc/${backend.State.Pid}/ns/net`), readlinkSync(`/proc/${dns.State.Pid}/ns/net`));
  const env = Object.fromEntries(dns.Config.Env.map(entry => {
    const at = entry.indexOf('=');
    return [entry.slice(0, at), entry.slice(at + 1)];
  }));
  const address = env.RSCTF_EVENT_VPN_HUB_ADDRESS;
  const domain = env.RSCTF_DOMAIN;
  const ingress = env.RSCTF_EVENT_VPN_INGRESS_IP;
  if (!/^\d+\.\d+\.\d+\.\d+$/.test(address ?? '') || !/^[a-zA-Z0-9][a-zA-Z0-9.-]+$/.test(domain ?? '')) {
    throw new Error('Missing or invalid VPN DNS address/domain');
  }
  for (const transport of ['+notcp', '+tcp']) {
    const query = name => run('nsenter', ['-t', String(backend.State.Pid), '-n', 'dig', `@${address}`, name, 'A', transport, '+time=1', '+tries=1', '+short']);
    if (!query(domain).split('\n').includes(ingress)) throw new Error(`Wrong private event DNS answer (${transport})`);
    if (!query('example.com').split('\n').some(line => /^\d+\.\d+\.\d+\.\d+$/.test(line))) {
      throw new Error(`VPN upstream DNS unavailable (${transport})`);
    }
  }
  console.log('VPN DNS: current backend namespace; private and upstream answers pass over UDP and TCP');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  if (args.length !== 2 || args.some(name => !/^[a-zA-Z0-9][a-zA-Z0-9_.-]*$/.test(name))) {
    console.error('Usage: node scripts/check-event-vpn.mjs BACKEND_CONTAINER DNS_CONTAINER');
    process.exitCode = 2;
  } else {
    try { checkEventVpn(...args); }
    catch (error) { console.error(error.message); process.exitCode = 1; }
  }
}
