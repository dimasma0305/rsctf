#!/usr/bin/env node

import { spawn, spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import {
  chmodSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  renameSync,
  unlinkSync,
  watch,
  writeFileSync,
} from "node:fs";
import { isIP } from "node:net";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const composeFile = join(repositoryRoot, "deploy", "compose.development.yml");
const publicComposeFile = join(
  repositoryRoot,
  "deploy",
  "compose.development.public.yml",
);
const stateDirectory = join(repositoryRoot, ".rsctf-dev");
const projectName = "rsctf-source-dev";

const DEFAULT_PORTS = Object.freeze({
  backend: 18080,
  frontend: 63000,
  postgres: 55432,
  redis: 56379,
});

function log(message) {
  process.stdout.write(`[rsctf-dev] ${message}\n`);
}

function fail(message) {
  throw new Error(`[rsctf-dev] ${message}`);
}

export function parseDevelopmentPort(name, value, fallback) {
  const raw =
    value === undefined || value === "" ? String(fallback) : String(value);
  if (!/^\d+$/.test(raw))
    fail(`${name} must be an integer from 1024 through 65535`);
  const port = Number(raw);
  if (!Number.isSafeInteger(port) || port < 1024 || port > 65535) {
    fail(`${name} must be an integer from 1024 through 65535`);
  }
  return port;
}

export function developmentPorts(environment = process.env) {
  const ports = {
    backend: parseDevelopmentPort(
      "RSCTF_DEV_BACKEND_PORT",
      environment.RSCTF_DEV_BACKEND_PORT,
      DEFAULT_PORTS.backend,
    ),
    frontend: parseDevelopmentPort(
      "RSCTF_DEV_FRONTEND_PORT",
      environment.RSCTF_DEV_FRONTEND_PORT,
      DEFAULT_PORTS.frontend,
    ),
    postgres: parseDevelopmentPort(
      "RSCTF_DEV_DB_PORT",
      environment.RSCTF_DEV_DB_PORT,
      DEFAULT_PORTS.postgres,
    ),
    redis: parseDevelopmentPort(
      "RSCTF_DEV_REDIS_PORT",
      environment.RSCTF_DEV_REDIS_PORT,
      DEFAULT_PORTS.redis,
    ),
  };
  if (new Set(Object.values(ports)).size !== Object.keys(ports).length) {
    fail("development ports must be distinct");
  }
  return ports;
}

export function developmentFrontendHost(environment = process.env) {
  const host = environment.RSCTF_DEV_FRONTEND_HOST || "127.0.0.1";
  if (isIP(host) !== 4 || host === "0.0.0.0") {
    fail(
      "RSCTF_DEV_FRONTEND_HOST must be one explicit IPv4 interface address, not a wildcard",
    );
  }
  return host;
}

export function sanitizedBaseEnvironment(environment = process.env) {
  return Object.fromEntries(
    Object.entries(environment).filter(
      ([name]) => !name.startsWith("RSCTF_") && !name.startsWith("VITE_"),
    ),
  );
}

function validSecret(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
}

function validateSecrets(value) {
  if (
    value?.schemaVersion !== 1 ||
    !validSecret(value.jwtSecret) ||
    !validSecret(value.identityHashKey) ||
    !validSecret(value.bootstrapToken) ||
    new Set([value.jwtSecret, value.identityHashKey, value.bootstrapToken])
      .size !== 3
  ) {
    fail(
      ".rsctf-dev/secrets.json is invalid; repair it instead of silently rotating identity data",
    );
  }
  return value;
}

export function loadOrCreateDevelopmentSecrets(directory = stateDirectory) {
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const directoryMetadata = lstatSync(directory);
  if (!directoryMetadata.isDirectory() || directoryMetadata.isSymbolicLink()) {
    fail(`${directory} must be a real directory, not a symbolic link`);
  }
  chmodSync(directory, 0o700);

  const path = join(directory, "secrets.json");
  if (existsSync(path)) {
    const metadata = lstatSync(path);
    if (!metadata.isFile() || metadata.isSymbolicLink()) {
      fail(`${path} must be a regular file, not a symbolic link`);
    }
    chmodSync(path, 0o600);
    return validateSecrets(JSON.parse(readFileSync(path, "utf8")));
  }

  const secrets = {
    schemaVersion: 1,
    jwtSecret: randomBytes(32).toString("hex"),
    identityHashKey: randomBytes(32).toString("hex"),
    bootstrapToken: randomBytes(32).toString("hex"),
  };
  const temporary = join(directory, `secrets.${process.pid}.tmp`);
  try {
    writeFileSync(temporary, `${JSON.stringify(secrets, null, 2)}\n`, {
      flag: "wx",
      mode: 0o600,
    });
    renameSync(temporary, path);
  } catch (error) {
    try {
      unlinkSync(temporary);
    } catch (cleanupError) {
      if (cleanupError?.code !== "ENOENT") throw cleanupError;
    }
    throw error;
  }
  return secrets;
}

function canonicalDevelopmentOrigin(value, frontendPort) {
  const raw = value || `http://localhost:${frontendPort}`;
  let url;
  try {
    url = new URL(raw);
  } catch {
    fail("RSCTF_DEV_PUBLIC_URL must be an absolute HTTP(S) origin");
  }
  if (
    !["http:", "https:"].includes(url.protocol) ||
    url.username ||
    url.password ||
    url.pathname !== "/" ||
    url.search ||
    url.hash
  ) {
    fail(
      "RSCTF_DEV_PUBLIC_URL must be an HTTP(S) origin without credentials, path, query, or fragment",
    );
  }
  return url.origin;
}

export function developmentBackendEnvironment({
  environment = process.env,
  ports = developmentPorts(environment),
  secrets,
  directory = stateDirectory,
} = {}) {
  const resolvedSecrets = secrets ?? loadOrCreateDevelopmentSecrets(directory);
  const publicUrl = canonicalDevelopmentOrigin(
    environment.RSCTF_DEV_PUBLIC_URL,
    ports.frontend,
  );
  const files = join(directory, "files");
  mkdirSync(files, { recursive: true, mode: 0o700 });
  return {
    ...sanitizedBaseEnvironment(environment),
    CARGO_TERM_COLOR: "always",
    RUST_BACKTRACE: "1",
    RUST_LOG: environment.RSCTF_DEV_RUST_LOG || "rsctf=debug,tower_http=debug",
    RSCTF_ROLE: "development",
    RSCTF_BIND: `127.0.0.1:${ports.backend}`,
    RSCTF_DATABASE_URL: `postgres://rsctf_dev:rsctf_dev@127.0.0.1:${ports.postgres}/rsctf_dev`,
    RSCTF_REDIS_URL: `redis://127.0.0.1:${ports.redis}`,
    RSCTF_DB_MAX_CONNECTIONS: "28",
    RSCTF_PROVISIONING_CONCURRENCY: "4",
    RSCTF_REPO_SCAN_CONCURRENCY: "1",
    RSCTF_MIGRATE: "1",
    RSCTF_JWT_SECRET: resolvedSecrets.jwtSecret,
    RSCTF_IDENTITY_HASH_KEY: resolvedSecrets.identityHashKey,
    RSCTF_BOOTSTRAP_TOKEN: resolvedSecrets.bootstrapToken,
    RSCTF_PUBLIC_URL: publicUrl,
    RSCTF_COOKIE_SECURE: publicUrl.startsWith("https://") ? "true" : "false",
    RSCTF_TRUSTED_PROXY_CIDRS: "",
    RSCTF_STORAGE_ROOT: files,
    RSCTF_STATIC_DIR: join(directory, "no-static-build"),
    RSCTF_CONTAINER_BACKEND: "none",
    RSCTF_TRAFFIC_CAPTURE_ENABLED: "false",
    RSCTF_AD_VPN_ENABLED: "false",
    RSCTF_AD_VPN_REQUIRED: "false",
    RSCTF_DISTRIBUTED_RATELIMIT: "false",
    RSCTF_EMAIL_CONFIRM: "false",
    RSCTF_ADMIN_CONFIRM: "false",
    RSCTF_ACTIVE_ON_REGISTER: "true",
    RSCTF_USE_CAPTCHA: "false",
  };
}

function composeArguments(command, publicGateway = false) {
  const args = [
    "compose",
    "--project-name",
    projectName,
    "--file",
    composeFile,
  ];
  if (publicGateway) args.push("--file", publicComposeFile);
  return [...args, ...command];
}

function composeEnvironment(ports, frontendHost = "127.0.0.1", publicUrl) {
  const publicOrigin = new URL(
    publicUrl || `http://localhost:${ports.frontend}`,
  );
  return {
    ...process.env,
    RSCTF_DEV_DB_PORT: String(ports.postgres),
    RSCTF_DEV_REDIS_PORT: String(ports.redis),
    RSCTF_DEV_FRONTEND_PORT: String(ports.frontend),
    RSCTF_DEV_FRONTEND_HOST: frontendHost,
    RSCTF_DEV_DOMAIN: publicOrigin.hostname,
  };
}

function runChecked(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? repositoryRoot,
    env: options.env ?? process.env,
    stdio: options.stdio ?? "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    fail(`${command} exited with status ${result.status ?? "unknown"}`);
  }
}

function checkTool(command, args) {
  const result = spawnSync(command, args, { stdio: "ignore" });
  if (result.error || result.status !== 0) {
    fail(`${command} is required and must be available on PATH`);
  }
}

function startDependencies(ports, frontendHost, publicUrl, publicGateway) {
  const environment = composeEnvironment(ports, frontendHost, publicUrl);
  runChecked("docker", composeArguments(["config", "--quiet"], publicGateway), {
    env: environment,
  });
  runChecked(
    "docker",
    composeArguments(
      ["up", "--detach", "--wait", "--wait-timeout", "120"],
      publicGateway,
    ),
    { env: environment },
  );
}

function childIsRunning(child) {
  return child && child.exitCode === null && child.signalCode === null;
}

function signalChildGroup(child, signal) {
  if (
    !childIsRunning(child) ||
    !Number.isSafeInteger(child.pid) ||
    child.pid <= 0
  )
    return;
  try {
    if (process.platform === "win32") child.kill(signal);
    else process.kill(-child.pid, signal);
  } catch (error) {
    if (error?.code !== "ESRCH") throw error;
  }
}

function waitForChild(child) {
  if (!childIsRunning(child)) return Promise.resolve();
  return new Promise((resolvePromise) => child.once("close", resolvePromise));
}

async function stopChild(child, label) {
  if (!childIsRunning(child)) return;
  log(`stopping ${label}`);
  const closed = waitForChild(child);
  signalChildGroup(child, "SIGTERM");
  const graceful = await Promise.race([
    closed.then(() => true),
    new Promise((resolvePromise) =>
      setTimeout(() => resolvePromise(false), 5_000),
    ),
  ]);
  if (!graceful) {
    signalChildGroup(child, "SIGKILL");
    await closed;
  }
}

function spawnBackend(environment) {
  log("starting Rust backend (automatic restart is enabled)");
  return spawn("cargo", ["run", "--locked", "--bin", "rsctf"], {
    cwd: repositoryRoot,
    env: environment,
    stdio: "inherit",
    detached: process.platform !== "win32",
  });
}

function spawnFrontend(environment, ports, frontendHost) {
  log("starting Vite frontend with hot module replacement");
  return spawn(
    "pnpm",
    [
      "exec",
      "vite",
      "--host",
      frontendHost,
      "--port",
      String(ports.frontend),
      "--strictPort",
    ],
    {
      cwd: join(repositoryRoot, "web"),
      env: {
        ...sanitizedBaseEnvironment(environment),
        FORCE_COLOR: "1",
        VITE_BACKEND_URL: `http://127.0.0.1:${ports.backend}`,
        VITE_APP_GIT_NAME: "source-development",
        VITE_APP_GIT_SHA: "working-tree",
        VITE_APP_BUILD_TIMESTAMP: new Date().toISOString(),
      },
      stdio: "inherit",
      detached: process.platform !== "win32",
    },
  );
}

function backendWatchers(onChange) {
  const watchers = [];
  const watchDirectory = (path) => {
    if (!existsSync(path)) return;
    watchers.push(
      watch(path, { recursive: true, encoding: "utf8" }, (_event, filename) => {
        if (!filename || /(?:^|\/)(?:target|\.git)(?:\/|$)/.test(filename))
          return;
        if (/\.(?:rs|sql)$/.test(filename)) onChange(filename);
      }),
    );
  };
  const watchFile = (path) => {
    if (!existsSync(path)) return;
    watchers.push(watch(path, () => onChange(path)));
  };
  watchDirectory(join(repositoryRoot, "src"));
  watchDirectory(join(repositoryRoot, "lib", "worker-protocol"));
  watchFile(join(repositoryRoot, "Cargo.toml"));
  watchFile(join(repositoryRoot, "Cargo.lock"));
  watchFile(join(repositoryRoot, "build.rs"));
  return watchers;
}

async function waitForExactResponse(
  url,
  expected,
  timeoutMilliseconds,
  shouldStop = () => false,
) {
  const deadline = Date.now() + timeoutMilliseconds;
  while (Date.now() < deadline && !shouldStop()) {
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(1_000) });
      if (response.ok && (await response.text()) === expected) return true;
    } catch {
      // Compilation and process restarts make temporary connection failures expected.
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 500));
  }
  return false;
}

function help() {
  process.stdout.write(`Usage: node scripts/dev.mjs [option]\n\n`);
  process.stdout.write(
    `  (no option)   Start isolated dependencies, Rust auto-reload, and Vite HMR\n`,
  );
  process.stdout.write(
    `  --public      Start the same source stack with its trusted HTTPS gateway\n`,
  );
  process.stdout.write(
    `  --deps-only   Start only the isolated PostgreSQL and Redis containers\n`,
  );
  process.stdout.write(`  --status      Show development dependency status\n`);
  process.stdout.write(
    `  --down        Stop development dependencies while preserving their volumes\n`,
  );
  process.stdout.write(
    `  --token       Print the local first-admin bootstrap token\n`,
  );
  process.stdout.write(`  --help        Show this help\n`);
}

async function runDevelopment() {
  const option = process.argv[2];
  if (process.argv.length > 3) fail("only one option is accepted; use --help");
  if (option === "--help") {
    help();
    return;
  }

  const ports = developmentPorts();
  if (option === "--token") {
    process.stdout.write(
      `${loadOrCreateDevelopmentSecrets().bootstrapToken}\n`,
    );
    return;
  }

  checkTool("docker", ["compose", "version"]);
  if (option === "--status") {
    runChecked("docker", composeArguments(["ps"], true), {
      env: composeEnvironment(ports),
    });
    return;
  }
  if (option === "--down") {
    runChecked("docker", composeArguments(["down", "--remove-orphans"], true), {
      env: composeEnvironment(ports),
    });
    log("development dependencies stopped; database and files are preserved");
    return;
  }

  const secrets = loadOrCreateDevelopmentSecrets();
  if (option && option !== "--deps-only" && option !== "--public")
    fail(`unknown option ${option}; use --help`);

  const publicGateway = option === "--public";
  const frontendHost = developmentFrontendHost();
  const publicUrl = canonicalDevelopmentOrigin(
    process.env.RSCTF_DEV_PUBLIC_URL,
    ports.frontend,
  );
  if (
    publicGateway &&
    (frontendHost.startsWith("127.") || !publicUrl.startsWith("https://"))
  ) {
    fail(
      "--public requires an HTTPS RSCTF_DEV_PUBLIC_URL and a non-loopback RSCTF_DEV_FRONTEND_HOST",
    );
  }

  startDependencies(ports, frontendHost, publicUrl, publicGateway);
  if (option === "--deps-only") {
    log(`PostgreSQL: 127.0.0.1:${ports.postgres}`);
    log(`Redis:      127.0.0.1:${ports.redis}`);
    return;
  }

  checkTool("cargo", ["--version"]);
  checkTool("pnpm", ["--version"]);
  if (
    !existsSync(join(repositoryRoot, "web", "node_modules", ".modules.yaml"))
  ) {
    log("installing locked frontend dependencies");
    runChecked("pnpm", ["install", "--frozen-lockfile"], {
      cwd: join(repositoryRoot, "web"),
    });
  }

  const backendEnvironment = developmentBackendEnvironment({ ports, secrets });
  let backend = spawnBackend(backendEnvironment);
  let frontend = spawnFrontend(backendEnvironment, ports, frontendHost);
  let shuttingDown = false;
  let restartTimer;
  let restartQueue = Promise.resolve();
  const watchers = backendWatchers((path) => {
    if (shuttingDown) return;
    clearTimeout(restartTimer);
    restartTimer = setTimeout(() => {
      restartQueue = restartQueue.then(async () => {
        log(`backend source changed (${path}); rebuilding`);
        await stopChild(backend, "Rust backend");
        if (!shuttingDown) backend = spawnBackend(backendEnvironment);
      });
    }, 300);
  });

  const shutdown = async (signal, exitCode = 0) => {
    if (shuttingDown) return;
    shuttingDown = true;
    clearTimeout(restartTimer);
    log(`received ${signal}; stopping source processes`);
    for (const watcher of watchers) watcher.close();
    await Promise.all([
      stopChild(frontend, "Vite frontend"),
      stopChild(backend, "Rust backend"),
    ]);
    log(
      "PostgreSQL and Redis remain available for the next run; use --down to stop them",
    );
    process.exitCode = exitCode;
  };

  process.once("SIGINT", () => void shutdown("SIGINT"));
  process.once("SIGTERM", () => void shutdown("SIGTERM"));
  frontend.once("close", (code, signal) => {
    if (!shuttingDown) {
      log(
        `Vite exited unexpectedly (${signal || code}); stopping the development session`,
      );
      void shutdown("Vite exit", code || 1);
    }
  });
  backend.on("close", (code, signal) => {
    if (!shuttingDown && backend.exitCode !== null) {
      log(
        `backend stopped (${signal || code}); it will restart after the next Rust change`,
      );
    }
  });

  log(`browser URL: ${publicUrl}`);
  log(`first-admin bootstrap token: ${secrets.bootstrapToken}`);
  log(
    "remote host: forward the frontend port over SSH instead of exposing Vite publicly",
  );
  const ready = await waitForExactResponse(
    `http://127.0.0.1:${ports.backend}/healthz`,
    "ok",
    20 * 60 * 1_000,
    () => shuttingDown,
  );
  if (!shuttingDown) {
    if (ready)
      log(
        "backend is healthy; edit web/src for instant HMR or src for automatic rebuild",
      );
    else
      log(
        "backend did not become healthy within 20 minutes; inspect the Rust output above",
      );
  }
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  runDevelopment().catch((error) => {
    process.stderr.write(`${error?.stack || error}\n`);
    process.exitCode = 1;
  });
}
