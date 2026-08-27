// Disposable-stack container reaper backlog plus fixed-rate responsiveness.
import crypto from "node:crypto";
import { execFileSync } from "node:child_process";
import { docker, runK6, RSCTF, sql, TARGET } from "./lib.mjs";

if (process.env.CONTAINER_REAP_STRESS_ACK !== "1") {
  throw new Error("set CONTAINER_REAP_STRESS_ACK=1 for this destructive disposable-stack test");
}
const target = new URL(TARGET);
const local = ["127.0.0.1", "localhost", "::1"].includes(target.hostname);
if (!local && process.env.ALLOW_REMOTE_CONTAINER_REAP_STRESS !== target.origin) {
  throw new Error(`remote target requires ALLOW_REMOTE_CONTAINER_REAP_STRESS=${target.origin}`);
}

const boundedInt = (name, fallback, min, max) => {
  const value = Number(process.env[name] || fallback);
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    throw new Error(`${name} must be an integer in ${min}..${max}`);
  }
  return value;
};
const rows = boundedInt("ROWS", 256, 65, 10_000);
const runtimeCount = boundedInt("RUNTIME_N", 4, 0, 32);
const image = String(process.env.RSCTF_REAP_TEST_IMAGE || "").trim();
if (runtimeCount > 0 && !image) {
  throw new Error("RSCTF_REAP_TEST_IMAGE must name an already-present disposable test image");
}
if (runtimeCount > 0 && docker(["image", "inspect", image]).status !== 0) {
  throw new Error(`test image is unavailable locally: ${image}`);
}

const serverEnv = JSON.parse(
  execFileSync("docker", ["inspect", "-f", "{{json .Config.Env}}", RSCTF], {
    encoding: "utf8",
  }).trim(),
);
const envValue = (name) => {
  const prefix = `${name}=`;
  return serverEnv.find((entry) => String(entry).startsWith(prefix))?.slice(prefix.length) || "";
};
const explicitScope = envValue("RSCTF_DOCKER_SCOPE").trim();
const jwtSecret = envValue("RSCTF_JWT_SECRET").trim();
if (!explicitScope && !jwtSecret) {
  throw new Error("the target container exposes neither RSCTF_DOCKER_SCOPE nor RSCTF_JWT_SECRET");
}
const scopeSource = explicitScope ? "explicit" : "jwt";
const scopeIdentity = explicitScope || jwtSecret;
const scope = crypto
  .createHash("sha256")
  .update(`${scopeSource}\0${scopeIdentity}`)
  .digest("hex")
  .slice(0, 32);
const runKey = crypto.randomBytes(8).toString("hex");
const marker = `rsctf-reap-load:${runKey}`;
const runtimeIds = [];

const health = async (stage) => {
  const response = await fetch(new URL("/healthz", TARGET));
  const body = await response.text();
  if (response.status !== 200 || body !== "ok") {
    throw new Error(`${stage} healthz failed: ${response.status} ${JSON.stringify(body)}`);
  }
};

let status = 1;
try {
  await health("pre-load");
  for (let index = 0; index < runtimeCount; index += 1) {
    const name = `rsctf-reap-load-${runKey}-${index}`;
    const created = docker([
      "create",
      "--name",
      name,
      "--label",
      `rsctf.managed=${scope}`,
      "--label",
      `rsctf.scope=${scope}`,
      image,
    ]);
    if (created.status !== 0) throw new Error(`docker create failed: ${created.stderr}`);
    const id = created.stdout.trim();
    runtimeIds.push(id);
    const started = docker(["start", id]);
    if (started.status !== 0) throw new Error(`docker start failed: ${started.stderr}`);
  }

  const runtimeValues = runtimeIds.map((id) => `'${id.replaceAll("'", "''")}'`).join(",");
  if (runtimeValues) {
    sql(
      `INSERT INTO "Containers" ` +
        `(id,image,container_id,status,started_at,expect_stop_at,is_proxy,ip,port) ` +
        `SELECT gen_random_uuid(),'${marker}',runtime_id,0,clock_timestamp()-interval '2 minutes',` +
        `clock_timestamp()-interval '1 minute',FALSE,'',0 ` +
        `FROM unnest(ARRAY[${runtimeValues}]::text[]) runtime_id`,
    );
  }
  const dummyRows = rows - runtimeIds.length;
  if (dummyRows > 0) {
    sql(
      `INSERT INTO "Containers" ` +
        `(id,image,container_id,status,started_at,expect_stop_at,is_proxy,ip,port) ` +
        `SELECT gen_random_uuid(),'${marker}','${marker}:'||value::text,0,` +
        `clock_timestamp()-interval '2 minutes',clock_timestamp()-interval '1 minute',FALSE,'',0 ` +
        `FROM generate_series(1,${dummyRows}) value`,
    );
  }
  const before = Number(sql(`SELECT COUNT(*) FROM "Containers" WHERE image='${marker}'`));
  if (before !== rows) throw new Error(`fixture row mismatch: expected ${rows}, got ${before}`);

  console.log(
    `container maintenance load → ${TARGET} rows=${rows} realRuntimes=${runtimeCount} ` +
      `rate=${process.env.RATE || 20}/s`,
  );
  status = runK6("container-maintenance.js", {
    TARGET,
    RATE: process.env.RATE || 20,
    VUS: process.env.VUS || 40,
    DURATION: process.env.DURATION || "70s",
    SUMMARY_JSON: process.env.SUMMARY_JSON || "",
  });
  await health("post-load");
  const after = Number(sql(`SELECT COUNT(*) FROM "Containers" WHERE image='${marker}'`));
  if (!(after >= 0 && after < before)) {
    throw new Error(`maintenance made no bounded backlog progress: before=${before}, after=${after}`);
  }
  const survivingReal = runtimeIds.filter((id) => docker(["inspect", id]).status === 0);
  if (survivingReal.length > after) {
    throw new Error(
      `runtime/row cleanup diverged: ${survivingReal.length} runtime(s), ${after} row(s) remain`,
    );
  }
  console.log(`bounded backlog progress: before=${before} after=${after}`);
} finally {
  sql(`DELETE FROM "Containers" WHERE image='${marker}'`);
  for (const id of runtimeIds) docker(["rm", "-f", id]);
}
process.exit(status);
