import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import http from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import test from "node:test";

const ingressSeconds = 330;
const image = process.env.RSCTF_TEST_TRAEFIK_IMAGE;
const docker = (...args) => execFileSync("docker", args, {
  encoding: "utf8", timeout: 15_000, stdio: ["ignore", "pipe", "pipe"],
}).trim();

async function listen(server) {
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  return server.address().port;
}

async function unusedPort() {
  const server = http.createServer();
  const port = await listen(server);
  await new Promise((resolve) => server.close(resolve));
  return port;
}

test("documented ingress lets the bounded application upload deadline expire first", () => {
  const upload = readFileSync(new URL("../../../src/utils/upload.rs", import.meta.url), "utf8");
  const guide = readFileSync(new URL("../../../docs/reference/source-development.md", import.meta.url), "utf8");
  const seconds = Number(upload.match(/REQUEST_BODY_DEADLINE[^\n]*from_secs\((\d+)\)/)?.[1]);
  assert.ok(seconds > 0 && ingressSeconds > seconds);
  assert.match(guide, new RegExp(`respondingtimeouts\\.readtimeout=${ingressSeconds}s`));
});

// Opt-in Linux/Docker regression: two bounded, paced uploads to a loopback-only
// fixture. No production routes, credentials, Docker socket, or event data.
test("a 70-second upload fails at the old ingress deadline but succeeds at 330s", {
  skip: !image && "set RSCTF_TEST_TRAEFIK_IMAGE to a locally available immutable digest",
  timeout: 95_000,
}, async () => {
  assert.match(image, /@sha256:[a-f0-9]{64}$/);
  const directory = mkdtempSync(join(tmpdir(), "rsctf-ingress-upload-"));
  const name = `rsctf-ingress-upload-${process.pid}`;
  const chunk = Buffer.alloc(75_000, 65);
  const totalBytes = chunk.length * 71;
  const upstream = http.createServer((request, response) => {
    let received = 0;
    request.on("error", () => {}); // Expected when the old proxy drops the body.
    request.on("data", (data) => { received += data.length; });
    request.on("end", () => response.end(String(received)));
  });
  let started = false;
  try {
    const upstreamPort = await listen(upstream);
    const oldPort = await unusedPort();
    let fixedPort = await unusedPort();
    while (fixedPort === oldPort) fixedPort = await unusedPort();
    writeFileSync(join(directory, "routes.yml"), JSON.stringify({ http: {
      routers: {
        upload: { entryPoints: ["old", "fixed"], rule: "Path(`/upload`)", service: "sink" },
      },
      services: { sink: { loadBalancer: { servers: [{ url: `http://127.0.0.1:${upstreamPort}` }] } } },
    } }));
    docker("run", "-d", "--name", name, "--pull=never", "--network=host",
      "--cpus=0.25", "--memory=128m", "--pids-limit=64", "--cap-drop=ALL",
      "--security-opt=no-new-privileges", "--read-only",
      "-v", `${directory}/routes.yml:/routes.yml:ro`, image,
      "--providers.file.filename=/routes.yml",
      `--entrypoints.old.address=127.0.0.1:${oldPort}`,
      `--entrypoints.fixed.address=127.0.0.1:${fixedPort}`,
      `--entrypoints.fixed.transport.respondingtimeouts.readtimeout=${ingressSeconds}s`);
    started = true;
    for (let attempt = 0; ; attempt++) {
      try {
        const response = await fetch(`http://127.0.0.1:${fixedPort}/upload`, { signal: AbortSignal.timeout(1000) });
        assert.equal(response.status, 200);
        assert.equal(await response.text(), "0");
        break;
      } catch (error) {
        if (attempt === 100) throw error; // file provider may load after the entrypoint answers
        await delay(100);
      }
    }
    async function upload(port) {
      const start = performance.now();
      let settled = false;
      let request;
      const result = new Promise((resolve) => {
        const finish = (value) => {
          if (settled) return;
          settled = true;
          resolve({ ...value, durationMs: Math.round(performance.now() - start) });
        };
        request = http.request({
          host: "127.0.0.1", port, path: "/upload", method: "POST",
          headers: { "Content-Length": totalBytes }, agent: false,
        }, (response) => {
          let body = "";
          response.on("data", (data) => { body += data; });
          response.on("end", () => finish({ status: response.statusCode, body }));
          response.on("error", (error) => finish({ error: error.code }));
        });
        request.on("error", (error) => finish({ error: error.code }));
      });
      const timeout = setTimeout(() => request.destroy(new Error("test upload deadline")), 80_000);
      try {
        request.flushHeaders();
        for (let index = 0; index < 71 && !settled; index++) {
          if (index) await delay(Math.max(0, start + index * 1000 - performance.now()));
          if (!settled) request.write(chunk);
        }
        if (!settled) request.end();
        return await result;
      } finally {
        clearTimeout(timeout);
        request.destroy();
      }
    }
    const [old, fixed] = await Promise.all([upload(oldPort), upload(fixedPort)]);
    console.log(JSON.stringify({ old, fixed, totalBytes }));
    assert.notEqual(old.status, 200);
    assert.ok(old.durationMs >= 55_000 && old.durationMs < 68_000);
    assert.equal(fixed.status, 200);
    assert.equal(Number(fixed.body), totalBytes);
    assert.ok(fixed.durationMs >= 69_000);
  } catch (error) {
    if (started) console.error(docker("logs", "--tail=30", name));
    throw error;
  } finally {
    if (started) docker("rm", "-f", name);
    upstream.closeAllConnections();
    await new Promise((resolve) => upstream.close(resolve));
    rmSync(directory, { recursive: true, force: true });
  }
});
