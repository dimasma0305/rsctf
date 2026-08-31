import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";

import {
  developmentBackendEnvironment,
  developmentFrontendHost,
  developmentPorts,
  loadOrCreateDevelopmentSecrets,
  parseDevelopmentPort,
  sanitizedBaseEnvironment,
} from "../../../scripts/dev.mjs";

const root = resolve(import.meta.dirname, "../../..");
const compose = readFileSync(
  join(root, "deploy/compose.development.yml"),
  "utf8",
);
const runner = readFileSync(join(root, "scripts/dev.mjs"), "utf8");
const viteConfig = readFileSync(join(root, "web/vite.config.mts"), "utf8");
const guide = readFileSync(
  join(root, "docs/reference/source-development.md"),
  "utf8",
);

test("source development dependencies are isolated and loopback-only", () => {
  assert.match(compose, /^name: rsctf-source-dev$/m);
  assert.match(compose, /127\.0\.0\.1:\$\{RSCTF_DEV_DB_PORT:-55432\}:5432/);
  assert.match(compose, /127\.0\.0\.1:\$\{RSCTF_DEV_REDIS_PORT:-56379\}:6379/);
  assert.match(compose, /^  db:$/m);
  assert.match(compose, /^  redis:$/m);
  assert.doesNotMatch(
    compose,
    /docker\.sock|network_mode|external:|rsctf:\s*$/m,
  );
});

test("development runner rejects invalid and colliding ports", () => {
  assert.equal(parseDevelopmentPort("PORT", undefined, 63000), 63000);
  assert.equal(parseDevelopmentPort("PORT", "18080", 63000), 18080);
  assert.throws(
    () => parseDevelopmentPort("PORT", "0", 63000),
    /1024 through 65535/,
  );
  assert.throws(
    () => parseDevelopmentPort("PORT", "12.5", 63000),
    /1024 through 65535/,
  );
  assert.throws(
    () => developmentPorts({ RSCTF_DEV_BACKEND_PORT: "63000" }),
    /must be distinct/,
  );
});

test("development frontend binding is explicit and never wildcard", () => {
  assert.equal(developmentFrontendHost({}), "127.0.0.1");
  assert.equal(
    developmentFrontendHost({ RSCTF_DEV_FRONTEND_HOST: "172.1.0.1" }),
    "172.1.0.1",
  );
  assert.throws(
    () => developmentFrontendHost({ RSCTF_DEV_FRONTEND_HOST: "0.0.0.0" }),
    /explicit IPv4 interface address/,
  );
  assert.throws(
    () => developmentFrontendHost({ RSCTF_DEV_FRONTEND_HOST: "example.com" }),
    /explicit IPv4 interface address/,
  );
});

test("development gateway exposes the backend health contract", () => {
  assert.match(viteConfig, /'\/healthz': TARGET/);
});

test("development environment cannot inherit production RSCTF or Vite settings", () => {
  const directory = mkdtempSync(join(tmpdir(), "rsctf-source-dev-env-"));
  try {
    const secrets = loadOrCreateDevelopmentSecrets(directory);
    const environment = developmentBackendEnvironment({
      directory,
      secrets,
      ports: { backend: 18080, frontend: 16300, postgres: 15432, redis: 16379 },
      environment: {
        PATH: process.env.PATH,
        RSCTF_DATABASE_URL: "postgres://production.invalid/production",
        RSCTF_JWT_SECRET: "production-secret-must-not-pass-through",
        RSCTF_CONTAINER_BACKEND: "docker",
        VITE_BACKEND_URL: "https://production.invalid",
      },
    });
    assert.equal(environment.PATH, process.env.PATH);
    assert.equal(
      environment.RSCTF_DATABASE_URL,
      "postgres://rsctf_dev:rsctf_dev@127.0.0.1:15432/rsctf_dev",
    );
    assert.equal(environment.RSCTF_JWT_SECRET, secrets.jwtSecret);
    assert.equal(environment.RSCTF_CONTAINER_BACKEND, "none");
    assert.equal(environment.RSCTF_ROLE, "development");
    assert.equal(environment.RSCTF_DB_MAX_CONNECTIONS, "29");
    assert.equal(environment.RSCTF_BIND, "127.0.0.1:18080");
    assert.equal(environment.RSCTF_PUBLIC_URL, "http://localhost:16300");
    assert.equal(environment.RSCTF_TRAFFIC_CAPTURE_ENABLED, "false");
    assert.equal(environment.RSCTF_AD_VPN_ENABLED, "false");
    assert.equal(environment.VITE_BACKEND_URL, undefined);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("development secrets are stable, private, and independent", () => {
  const directory = mkdtempSync(join(tmpdir(), "rsctf-source-dev-secrets-"));
  try {
    const first = loadOrCreateDevelopmentSecrets(directory);
    const second = loadOrCreateDevelopmentSecrets(directory);
    assert.deepEqual(second, first);
    assert.equal(
      new Set([first.jwtSecret, first.identityHashKey, first.bootstrapToken])
        .size,
      3,
    );
    assert.equal(statSync(directory).mode & 0o777, 0o700);
    assert.equal(statSync(join(directory, "secrets.json")).mode & 0o777, 0o600);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("development workflow documents hot reload, SSH forwarding, and the release boundary", () => {
  assert.match(runner, /spawnBackend/);
  assert.match(runner, /backendWatchers/);
  assert.match(runner, /vite/);
  assert.match(guide, /hot module replacement/i);
  assert.match(guide, /ssh -L 63000:127\.0\.0\.1:63000/);
  assert.match(guide, /node scripts\/dev\.mjs --public/);
  assert.match(
    guide,
    /production deployment must still be.*immutable image digest/is,
  );
  assert.deepEqual(
    sanitizedBaseEnvironment({
      PATH: "/bin",
      RSCTF_DATABASE_URL: "production",
      VITE_BACKEND_URL: "production",
    }),
    { PATH: "/bin" },
  );
});
