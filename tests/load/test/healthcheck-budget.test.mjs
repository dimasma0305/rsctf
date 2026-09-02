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

test("steady-state Docker health checks have a low fixed cadence", () => {
  const expectedIntervals = new Map([
    ["Dockerfile", 1],
    ["docker-compose.yml", 4],
    ["deploy/compose.yml", 3],
    ["deploy/compose.roles.yml", 1],
    ["deploy/compose.development.yml", 2],
    ["compose.dev.yml", 1],
    ["deploy/compose.docker.yml", 1],
  ]);
  for (const [path, expected] of expectedIntervals) {
    const source = read(path);
    const intervals = source.match(/(?:--interval=|interval:\s*)30s/g) ?? [];
    assert.equal(intervals.length, expected, `${path} has an unexpected probe cadence`);
  }
});
