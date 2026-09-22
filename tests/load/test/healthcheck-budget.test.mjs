import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import test from "node:test";

const root = resolve(import.meta.dirname, "../../..");
const read = (path) => readFileSync(join(root, path), "utf8");

test("RSCTF health checks use the native bounded probe", () => {
  const dockerfile = read("Dockerfile");
  assert.match(dockerfile, /CMD \["\/usr\/local\/bin\/rsctf", "healthcheck"\]/);
  assert.doesNotMatch(dockerfile, /urllib\.request[^\n]*healthz/);

  for (const path of [
    "docker-compose.yml",
    "deploy/compose.yml",
    "deploy/compose.roles.yml",
  ]) {
    const source = read(path);
    assert.match(
      source,
      /(?:test: \["CMD", "\/usr\/local\/bin\/rsctf", "healthcheck"\]|- \/usr\/local\/bin\/rsctf\s*\n\s*- healthcheck)/,
    );
    assert.doesNotMatch(source, /urllib\.request[^\n]*healthz/);
  }
  const development = read("compose.dev.yml");
  assert.match(development, /\/opt\/rsctf-debug\/rsctf\s*\n\s*- healthcheck/);
  assert.doesNotMatch(development, /urllib\.request[^\n]*healthz/);
});

test("application services log through the bounded local driver", () => {
  const driverOf = (source, service) => {
    const block = source.split(/\n(?=  [a-z][a-z0-9_-]*:\n)/).find((chunk) => chunk.startsWith(`  ${service}:`));
    assert.ok(block, `${service} service block`);
    const anchor = block.match(/logging: \*([a-z-]+)/);
    if (anchor) {
      const definition = source.match(new RegExp(`x-${anchor[1]}: &${anchor[1]}\\n  driver: ([a-z-]+)`));
      assert.ok(definition, `${anchor[1]} anchor`);
      return definition[1];
    }
    const inline = block.match(/logging:\n\s+driver: ([a-z-]+)/);
    assert.ok(inline, `${service} logging block`);
    return inline[1];
  };

  const expectations = new Map([
    ["docker-compose.yml", { local: ["rsctf", "rsctf-proxy-firewall"], "json-file": ["db", "redis"] }],
    ["deploy/compose.yml", { local: ["rsctf"], "json-file": ["db", "redis"] }],
    ["deploy/compose.roles.yml", { local: ["rsctf-control"], "json-file": [] }],
    ["deploy/compose.docker.yml", { local: ["rsctf-proxy-firewall"], "json-file": [] }],
    ["deploy/compose.caddy.yml", { local: ["caddy"], "json-file": [] }],
    ["deploy/compose.event-vpn-ingress.yml", { local: ["event-vpn-dns"], "json-file": [] }],
    ["compose.dev.yml", { local: ["backend", "koth-reporter", "event-vpn-dns"], "json-file": [] }],
  ]);
  for (const [path, expected] of expectations) {
    const source = read(path);
    for (const [driver, services] of Object.entries(expected)) {
      for (const service of services) {
        assert.equal(driverOf(source, service), driver, `${path}: ${service} log driver`);
      }
    }
  }
});

test("steady-state Docker health checks have a low fixed cadence", () => {
  const expectedIntervals = new Map([
    ["Dockerfile", 1],
    ["docker-compose.yml", 4],
    ["deploy/compose.yml", 3],
    ["deploy/compose.roles.yml", 1],
    ["deploy/compose.development.yml", 2],
    // Backend plus the private managed-KotH callback proxy. Both use one
    // 30-second probe; challenge targets do not poll public ingress.
    ["compose.dev.yml", 2],
    ["deploy/compose.docker.yml", 1],
  ]);
  for (const [path, expected] of expectedIntervals) {
    const source = read(path);
    const intervals = source.match(/(?:--interval=|interval:\s*)30s/g) ?? [];
    assert.equal(intervals.length, expected, `${path} has an unexpected probe cadence`);
  }
});
