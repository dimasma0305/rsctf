import assert from "node:assert/strict";
import test from "node:test";

import { cheatK6Environment } from "../cheat-environment.js";

test("anti-cheat k6 receives only allowlisted runtime and scenario values", () => {
  const environment = cheatK6Environment(
    {
      PATH: "/custom/bin",
      LANG: "C.UTF-8",
      SSL_CERT_FILE: "/etc/ssl/certs/ca-certificates.crt",
      HOME: "/root",
      DATABASE_URL: "postgres://secret",
      RSCTF_JWT_SECRET: "must-not-leak",
      K6_HTTP_DEBUG: "full",
      K6_OUT: "json=/tmp/leak.json",
      HTTPS_PROXY: "https://user:password@example.test",
    },
    { CHEAT_CONFIG: "/tmp/private-run/input.json" },
    "/tmp/private-run",
  );

  assert.deepEqual(environment, {
    PATH: "/custom/bin",
    SSL_CERT_FILE: "/etc/ssl/certs/ca-certificates.crt",
    LANG: "C.UTF-8",
    HOME: "/tmp/private-run",
    TMPDIR: "/tmp/private-run",
    TMP: "/tmp/private-run",
    TEMP: "/tmp/private-run",
    CHEAT_CONFIG: "/tmp/private-run/input.json",
  });
  for (const forbidden of [
    "DATABASE_URL",
    "RSCTF_JWT_SECRET",
    "K6_HTTP_DEBUG",
    "K6_OUT",
    "HTTPS_PROXY",
  ]) {
    assert.equal(Object.hasOwn(environment, forbidden), false);
  }
});

test("anti-cheat k6 rejects debug injection and non-absolute scenario paths", () => {
  assert.throws(
    () =>
      cheatK6Environment(
        process.env,
        { CHEAT_CONFIG: "/tmp/input.json", K6_HTTP_DEBUG: "full" },
        "/tmp/private-run",
      ),
    /rejects unexpected key K6_HTTP_DEBUG/,
  );
  assert.throws(
    () =>
      cheatK6Environment(
        process.env,
        { CHEAT_CONFIG: "input.json" },
        "/tmp/private-run",
      ),
    /absolute CHEAT_CONFIG/,
  );
});
