// Pure contracts shared by the Node admin-lifecycle orchestrator, its unit
// tests, and the k6 read-polling scenario. Keep this module free of Node-only
// imports so k6 can consume the same route and response definitions.

const operation = (id, method, path, options = {}) =>
  Object.freeze({
    id,
    method,
    path,
    source: options.source || "admin",
    surface: options.surface || "web",
    auth: options.auth || "admin",
    responseKind: options.responseKind || "object",
    expectedStatuses: Object.freeze([...(options.expectedStatuses || [200])]),
    params: Object.freeze({ ...(options.params || {}) }),
    query: options.query || "",
    poll: options.poll === true,
    mutation: options.mutation === true,
  });

// This catalog deliberately describes method/path operations rather than only
// unique paths: several Axum routes expose independent GET/POST/PUT/DELETE
// contracts. `/api/workers/enroll` belongs here because it completes the
// administrative one-time-token flow even though the request itself is
// authenticated by that token rather than an administrator session.
export const ADMIN_OPERATIONS = Object.freeze([
  operation("admin_my_ip_get", "GET", "/api/admin/MyIp", { poll: true, responseKind: "my-ip" }),
  operation("admin_config_get", "GET", "/api/admin/config", { poll: true, responseKind: "config" }),
  operation("admin_config_update", "PUT", "/api/admin/config", { mutation: true, responseKind: "message" }),
  operation("admin_logo_upload", "POST", "/api/admin/config/logo", { mutation: true, responseKind: "message" }),
  operation("admin_logo_delete", "DELETE", "/api/admin/config/logo", { mutation: true, responseKind: "message" }),
  operation("admin_dashboard_get", "GET", "/api/admin/dashboard", { poll: true, responseKind: "dashboard" }),
  operation("admin_flag_egress_get", "GET", "/api/admin/Games/{id}/FlagEgress", {
    poll: true,
    responseKind: "page",
    params: { id: "gameId" },
    query: "count=25&skip=0",
  }),
  operation("admin_submission_trend_get", "GET", "/api/admin/submissiontrend", {
    poll: true,
    responseKind: "array",
    query: "range=Day",
  }),
  operation("admin_reviews_get", "GET", "/api/admin/reviews", {
    poll: true,
    responseKind: "array",
    query: "count=25&skip=0",
  }),
  operation("admin_cheat_reports_get", "GET", "/api/admin/cheat-reports", {
    poll: true,
    responseKind: "array",
    query: "count=25&skip=0",
  }),
  operation("admin_writeups_get", "GET", "/api/admin/writeups", {
    poll: true,
    responseKind: "array",
    query: "count=25&skip=0",
  }),
  operation("admin_game_writeups_get", "GET", "/api/admin/writeups/{id}", {
    poll: true,
    responseKind: "game-writeups",
    params: { id: "gameId" },
  }),
  operation("admin_writeups_download", "GET", "/api/admin/writeups/{id}/all", {
    responseKind: "zip",
    params: { id: "gameId" },
  }),
  operation("admin_users_get", "GET", "/api/admin/users", {
    poll: true,
    responseKind: "page",
    query: "count=25&skip=0",
  }),
  operation("admin_users_add", "POST", "/api/admin/users", { mutation: true, responseKind: "message" }),
  operation("admin_users_import", "POST", "/api/admin/users/import", { mutation: true, responseKind: "import" }),
  operation("admin_credentials_send", "POST", "/api/admin/users/credentials/send", {
    mutation: true,
    responseKind: "credential-send",
  }),
  operation("admin_users_search", "POST", "/api/admin/users/search", {
    mutation: true,
    responseKind: "page",
    query: "hint=admin-load",
  }),
  operation("admin_user_get", "GET", "/api/admin/users/{userid}", {
    poll: true,
    responseKind: "user",
    params: { userid: "userId" },
  }),
  operation("admin_user_update", "PUT", "/api/admin/users/{userid}", {
    mutation: true,
    responseKind: "message",
    params: { userid: "userId" },
  }),
  operation("admin_user_delete", "DELETE", "/api/admin/users/{userid}", {
    mutation: true,
    responseKind: "string",
    params: { userid: "userId" },
  }),
  operation("admin_user_password_reset", "DELETE", "/api/admin/users/{userid}/password", {
    mutation: true,
    responseKind: "private-string",
    params: { userid: "userId" },
  }),
  operation("admin_teams_get", "GET", "/api/admin/teams", {
    poll: true,
    responseKind: "page",
    query: "count=25&skip=0",
  }),
  operation("admin_teams_search", "POST", "/api/admin/teams/search", {
    mutation: true,
    responseKind: "page",
    query: "hint=admin-load",
  }),
  operation("admin_team_update", "PUT", "/api/admin/teams/{id}", {
    mutation: true,
    responseKind: "message",
    params: { id: "teamId" },
  }),
  operation("admin_team_delete", "DELETE", "/api/admin/teams/{id}", {
    mutation: true,
    responseKind: "string",
    params: { id: "teamId" },
  }),
  operation("admin_participation_update", "PUT", "/api/admin/participation/{id}", {
    mutation: true,
    responseKind: "message",
    auth: "admin-or-manager",
    params: { id: "participationId" },
  }),
  operation("admin_logs_get", "GET", "/api/admin/logs", {
    poll: true,
    responseKind: "array",
    query: "level=All&count=25&skip=0",
  }),
  operation("admin_instances_get", "GET", "/api/admin/instances", {
    poll: true,
    responseKind: "page",
    query: "count=25&skip=0",
  }),
  operation("admin_instance_delete", "DELETE", "/api/admin/instances/{id}", {
    mutation: true,
    responseKind: "message",
    params: { id: "instanceId" },
  }),
  operation("admin_instance_stats_get", "GET", "/api/admin/instances/{id}/stats", {
    poll: true,
    responseKind: "instance-stats",
    params: { id: "instanceId" },
  }),
  operation("admin_files_get", "GET", "/api/admin/files", {
    poll: true,
    responseKind: "page",
    query: "count=25&skip=0",
  }),
  operation("admin_captcha_test", "POST", "/api/admin/captcha/test", { mutation: true, responseKind: "message" }),
  operation("admin_email_test", "POST", "/api/admin/email/test", {
    mutation: true,
    responseKind: "message",
    // The disposable harness deliberately points at a closed SMTP endpoint so
    // it can prove delivery failures are reported truthfully. A deployment
    // with an explicit test SMTP server exercises the 200 branch instead.
    expectedStatuses: [200, 400],
  }),
  operation("admin_game_bulk_rebuild", "POST", "/api/admin/games/{gameId}/bulkrebuild", {
    mutation: true,
    responseKind: "bulk-rebuild",
    params: { gameId: "gameId" },
  }),
  operation("admin_anticheat_blocks_get", "GET", "/api/admin/anticheatblocks", {
    poll: true,
    responseKind: "array",
    query: "count=25",
  }),
  operation("admin_anticheat_block_delete", "DELETE", "/api/admin/anticheatblocks/{id}", {
    mutation: true,
    responseKind: "message",
    params: { id: "antiCheatBlockId" },
  }),
  operation("admin_builds_get", "GET", "/api/admin/builds", {
    poll: true,
    responseKind: "array",
    query: "count=25&skip=0",
  }),
  operation("admin_builds_inprogress_get", "GET", "/api/admin/builds/inprogress", {
    poll: true,
    responseKind: "array",
  }),
  operation("admin_build_images_get", "GET", "/api/admin/builds/images", {
    poll: true,
    responseKind: "array",
  }),
  operation("admin_build_image_delete", "DELETE", "/api/admin/builds/images", {
    mutation: true,
    responseKind: "prune",
    query: "tag=rsctf%2Fadmin-lifecycle-placeholder%3Alatest&force=false",
  }),
  operation("admin_builds_bulk_delete", "POST", "/api/admin/builds/bulkdelete", {
    mutation: true,
    responseKind: "prune",
  }),
  operation("admin_builds_prune_failed", "POST", "/api/admin/builds/prunefailed", {
    mutation: true,
    responseKind: "prune",
  }),
  operation("admin_build_images_prune", "POST", "/api/admin/builds/pruneimages", {
    mutation: true,
    responseKind: "prune",
  }),
  operation("admin_build_delete", "DELETE", "/api/admin/builds/{auditId}", {
    mutation: true,
    responseKind: "message",
    params: { auditId: "buildId" },
  }),
  operation("admin_build_reenqueue", "POST", "/api/admin/builds/{auditId}/reenqueue", {
    mutation: true,
    responseKind: "build",
    params: { auditId: "buildId" },
  }),
  operation("admin_repo_bindings_get", "GET", "/api/admin/repobindings", {
    poll: true,
    responseKind: "array",
  }),
  operation("admin_repo_binding_create", "POST", "/api/admin/repobindings", {
    mutation: true,
    responseKind: "repo-scan-result",
  }),
  operation("admin_repo_binding_update", "PUT", "/api/admin/repobindings/{id}", {
    mutation: true,
    responseKind: "repo-binding",
    params: { id: "bindingId" },
  }),
  operation("admin_repo_binding_delete", "DELETE", "/api/admin/repobindings/{id}", {
    mutation: true,
    responseKind: "message",
    params: { id: "bindingId" },
  }),
  operation("admin_repo_binding_scan", "POST", "/api/admin/repobindings/{id}/scan", {
    mutation: true,
    responseKind: "repo-scan-result",
    params: { id: "bindingId" },
  }),
  operation("admin_repo_binding_scans_get", "GET", "/api/admin/repobindings/{id}/scans", {
    poll: true,
    responseKind: "array",
    params: { id: "bindingId" },
  }),
  operation("ad_admin_services_get", "GET", "/api/ad/admin/{game_id}/Services", {
    source: "ad",
    poll: true,
    responseKind: "array",
    params: { game_id: "adGameId" },
  }),
  operation("ad_admin_service_register", "POST", "/api/ad/admin/{game_id}/Services", {
    source: "ad",
    mutation: true,
    responseKind: "ad-service",
    params: { game_id: "adGameId" },
  }),
  operation("ad_admin_rounds_get", "GET", "/api/ad/admin/{game_id}/Rounds", {
    source: "ad",
    poll: true,
    responseKind: "array",
    params: { game_id: "adGameId" },
  }),
  operation("ad_admin_round_advance_rejected", "POST", "/api/ad/admin/{game_id}/Round/Advance", {
    source: "ad",
    mutation: true,
    responseKind: "error",
    expectedStatuses: [400],
    params: { game_id: "adGameId" },
  }),
  operation("admin_workers_get", "GET", "/api/admin/workers", {
    source: "workers",
    surface: "control",
    poll: true,
    responseKind: "workers",
  }),
  operation("admin_worker_create", "POST", "/api/admin/workers", {
    source: "workers",
    surface: "control",
    mutation: true,
    responseKind: "worker-created",
  }),
  operation("admin_worker_token_issue", "POST", "/api/admin/workers/{id}/token", {
    source: "workers",
    surface: "control",
    mutation: true,
    responseKind: "enrollment-token",
    params: { id: "workerId" },
  }),
  operation("admin_worker_state_update", "PUT", "/api/admin/workers/{id}/state", {
    source: "workers",
    surface: "control",
    mutation: true,
    responseKind: "worker",
    params: { id: "workerId" },
  }),
  operation("worker_enroll", "POST", "/api/workers/enroll", {
    source: "workers",
    surface: "control",
    auth: "enrollment-token",
    mutation: true,
    responseKind: "enrollment",
  }),
]);

export const ADMIN_SIGNALR_SURFACES = Object.freeze([
  Object.freeze({ id: "admin_signalr_connect", method: "GET", path: "/hub/admin", surface: "web" }),
  Object.freeze({ id: "admin_signalr_negotiate", method: "POST", path: "/hub/admin/negotiate", surface: "web" }),
]);

export const ADMIN_OPERATION_IDS = Object.freeze(ADMIN_OPERATIONS.map(({ id }) => id));
export const ADMIN_READ_OPERATIONS = Object.freeze(ADMIN_OPERATIONS.filter(({ poll }) => poll));

const operationById = new Map(ADMIN_OPERATIONS.map((item) => [item.id, item]));

export function positiveInteger(value, label = "value") {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer (got ${value})`);
  }
  return parsed;
}

export function assertExactFailedBuildPruneCandidates(records, expectedId) {
  if (!Array.isArray(records)) throw new TypeError("build prune inventory must be an array");
  const fixtureId = positiveInteger(expectedId, "failed build fixture id");
  const normalized = records.map((record) => {
    if (
      record === null ||
      typeof record !== "object" ||
      !Number.isSafeInteger(Number(record.id)) ||
      !Number.isSafeInteger(Number(record.status))
    ) {
      throw new Error("build prune inventory contains an invalid record");
    }
    return { id: Number(record.id), status: Number(record.status) };
  });
  const active = normalized.filter(({ status }) => status === 3 || status === 5);
  if (active.length) {
    throw new Error(`failed-build prune has active candidates: ${JSON.stringify(active)}`);
  }
  const failed = normalized.filter(({ status }) => status === 2);
  if (failed.length !== 1 || failed[0].id !== fixtureId) {
    throw new Error(
      `failed-build prune candidate set is not the exact fixture: ${JSON.stringify(failed)}`,
    );
  }
  return Object.freeze({ expectedId: fixtureId, candidates: Object.freeze([fixtureId]) });
}

export function assertStableZeroResidualSnapshots(snapshots) {
  if (!Array.isArray(snapshots) || snapshots.length !== 2) {
    throw new Error("cleanup requires exactly two residual snapshots");
  }
  const normalized = snapshots.map((snapshot, index) => {
    if (snapshot === null || typeof snapshot !== "object" || Array.isArray(snapshot)) {
      throw new Error(`cleanup pass ${index + 1} is not an object`);
    }
    for (const [resource, count] of Object.entries(snapshot)) {
      if (!Number.isSafeInteger(count) || count < 0) {
        throw new Error(`cleanup pass ${index + 1} has invalid ${resource} count`);
      }
      if (count !== 0) {
        throw new Error(`cleanup pass ${index + 1} retained ${resource}: ${count}`);
      }
    }
    return snapshot;
  });
  if (JSON.stringify(normalized[0]) !== JSON.stringify(normalized[1])) {
    throw new Error("cleanup did not remain stable between zero passes");
  }
  return Object.freeze({ passes: 2, resources: Object.keys(normalized[0]).length });
}

function coverageIds(recorded) {
  if (recorded instanceof Set) return [...recorded];
  if (recorded instanceof Map) return [...recorded.entries()].filter(([, value]) => value).map(([id]) => id);
  if (Array.isArray(recorded)) return recorded;
  if (recorded && typeof recorded === "object") {
    return Object.entries(recorded).filter(([, value]) => value).map(([id]) => id);
  }
  throw new TypeError("admin coverage must be an array, Set, Map, or object");
}

export function assertCompleteCoverage(recorded, options = {}) {
  const includeSignalR = options.includeSignalR === true;
  const allowExtra = options.allowExtra === true;
  const required = [
    ...ADMIN_OPERATION_IDS,
    ...(includeSignalR ? ADMIN_SIGNALR_SURFACES.map(({ id }) => id) : []),
  ];
  const covered = coverageIds(recorded);
  if (covered.some((id) => typeof id !== "string" || id.length === 0)) {
    throw new Error("admin coverage contains an invalid operation id");
  }
  const coveredSet = new Set(covered);
  const requiredSet = new Set(required);
  const duplicates = covered.filter((id, index) => covered.indexOf(id) !== index);
  const missing = required.filter((id) => !coveredSet.has(id));
  const extra = covered.filter((id) => !requiredSet.has(id));
  if (duplicates.length || missing.length || (!allowExtra && extra.length)) {
    const details = [
      duplicates.length ? `duplicate: ${[...new Set(duplicates)].join(", ")}` : "",
      missing.length ? `missing: ${missing.join(", ")}` : "",
      !allowExtra && extra.length ? `unknown: ${[...new Set(extra)].join(", ")}` : "",
    ].filter(Boolean);
    throw new Error(`incomplete admin lifecycle coverage (${details.join("; ")})`);
  }
  return Object.freeze({ covered: coveredSet.size, required: required.length, missing: [], extra });
}

function parseTargetList(raw) {
  if (Array.isArray(raw)) return raw.map(String);
  const text = String(raw || "").trim();
  if (!text) return [];
  if (text.startsWith("[")) {
    const parsed = JSON.parse(text);
    if (!Array.isArray(parsed)) throw new Error("web targets JSON must be an array");
    return parsed.map(String);
  }
  return text.split(",").map((value) => value.trim()).filter(Boolean);
}

function normalizedHttpUrl(value, label) {
  let parsed;
  try {
    parsed = new URL(String(value));
  } catch {
    throw new Error(`${label} must be an absolute HTTP(S) URL`);
  }
  if (!/^https?:$/.test(parsed.protocol) || parsed.username || parsed.password || parsed.search || parsed.hash) {
    throw new Error(`${label} must be a credential-free HTTP(S) origin`);
  }
  parsed.pathname = parsed.pathname.replace(/\/+$/, "") || "/";
  if (parsed.pathname !== "/") throw new Error(`${label} must not contain a path`);
  return parsed.origin;
}

export function assertSafeAdminTarget(env = {}) {
  if (env.ADMIN_LIFECYCLE_DISPOSABLE !== "1") {
    throw new Error("set ADMIN_LIFECYCLE_DISPOSABLE=1 for the destructive admin lifecycle");
  }
  const target = normalizedHttpUrl(env.TARGET || "", "TARGET");
  const hostname = new URL(target).hostname;
  const loopback = hostname === "127.0.0.1" || hostname === "localhost" || hostname === "::1" || hostname === "[::1]";
  if (!loopback && env.ALLOW_REMOTE_ADMIN_LIFECYCLE !== target) {
    throw new Error(`remote admin lifecycle requires ALLOW_REMOTE_ADMIN_LIFECYCLE=${target}`);
  }
  const webTargets = parseTargetList(env.WEB_TARGETS || env.ADMIN_WEB_TARGETS).map((value, index) =>
    normalizedHttpUrl(value, `WEB_TARGETS[${index}]`),
  );
  if (webTargets.length < 2 || new Set(webTargets).size !== webTargets.length) {
    throw new Error("WEB_TARGETS must contain at least two distinct direct web-replica origins");
  }
  if (webTargets.includes(target)) {
    throw new Error("TARGET must be distinct from every direct WEB_TARGETS origin");
  }
  const controlTarget = normalizedHttpUrl(env.CONTROL_TARGET || "", "CONTROL_TARGET");
  if (controlTarget === target || webTargets.includes(controlTarget)) {
    throw new Error("CONTROL_TARGET must be distinct from TARGET and every direct web target");
  }
  return assertAdminOriginAcknowledgements(env, { target, webTargets, controlTarget });
}

function confirmationOrigin(value) {
  return String(value || "").trim().replace(/\/+$/, "");
}

/// Confirm every origin that will receive an administrator bearer token. This
/// helper intentionally operates on already-validated origins so both Node and
/// k6 can share it (k6 does not provide the WHATWG URL constructor).
export function assertAdminOriginAcknowledgements(env, { target, webTargets, controlTarget }) {
  if (confirmationOrigin(env.CONFIRM_ADMIN_TARGET) !== target) {
    throw new Error(`set CONFIRM_ADMIN_TARGET=${target} to acknowledge the exact disposable target`);
  }
  const confirmedWebTargets = parseTargetList(env.CONFIRM_ADMIN_WEB_TARGETS).map(confirmationOrigin);
  if (
    confirmedWebTargets.length !== webTargets.length ||
    confirmedWebTargets.some((value, index) => value !== webTargets[index])
  ) {
    throw new Error(
      `set CONFIRM_ADMIN_WEB_TARGETS=${JSON.stringify(webTargets)} to acknowledge every direct web origin`,
    );
  }
  if (confirmationOrigin(env.CONFIRM_ADMIN_CONTROL_TARGET) !== controlTarget) {
    throw new Error(`set CONFIRM_ADMIN_CONTROL_TARGET=${controlTarget} to acknowledge the direct control origin`);
  }
  return Object.freeze({
    target,
    webTargets: Object.freeze([...webTargets]),
    controlTarget,
  });
}

const DISPOSABLE_MARKER_ENV = "RSCTF_ADMIN_LIFECYCLE_MARKER";

function validateDisposableContainer(resource, label, marker) {
  if (!resource || typeof resource !== "object") {
    throw new Error(`${label} inspection is missing`);
  }
  const name = String(resource.name || "").trim();
  const project = String(resource.project || "").trim();
  const service = String(resource.service || "").trim();
  if (!name) throw new Error(`${label} container name is missing`);
  if (!project || !service) throw new Error(`${label} ${name} is not a Compose service`);
  if (!Array.isArray(resource.environment)) {
    throw new Error(`${label} ${name} has no inspectable environment`);
  }
  const markerEntries = resource.environment.filter((entry) =>
    String(entry).startsWith(`${DISPOSABLE_MARKER_ENV}=`),
  );
  if (markerEntries.length !== 1 || markerEntries[0] !== `${DISPOSABLE_MARKER_ENV}=${marker}`) {
    throw new Error(`${label} ${name} does not carry the one exact disposable marker`);
  }
  return Object.freeze({ name, project, service });
}

/// Validate the Docker resources that the destructive lifecycle can mutate
/// before its first SQL/Redis/application operation. PostgreSQL and Redis must
/// independently opt in with the same unguessable marker as every server, and
/// all resources must belong to the one declared Compose project.
export function assertDisposableComposeTopology({ marker, servers, postgres, redis } = {}) {
  const expected = String(marker || "").trim();
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]{7,127}$/.test(expected)) {
    throw new Error("ADMIN_LIFECYCLE_STACK_MARKER must name the dedicated server-side disposable marker");
  }
  if (!Array.isArray(servers) || servers.length < 3) {
    throw new Error("declare at least two web replicas and one control replica");
  }

  const serverResources = servers.map((resource, index) =>
    validateDisposableContainer(resource, `server[${index}]`, expected),
  );
  if (new Set(serverResources.map(({ name }) => name)).size !== serverResources.length) {
    throw new Error("declared disposable server containers must be distinct");
  }
  const projects = new Set(serverResources.map(({ project }) => project));
  if (projects.size !== 1) {
    throw new Error("declared web/control containers are not in one Compose project");
  }
  const [composeProject] = projects;
  const services = serverResources.map(({ service }) => service);
  if (
    services.filter((service) => service === "rsctf").length < 2 ||
    services.filter((service) => service === "rsctf-control").length !== 1
  ) {
    throw new Error(
      "declared disposable topology must contain at least two rsctf replicas and exactly one rsctf-control",
    );
  }

  for (const [resource, label, expectedService] of [
    [postgres, "PostgreSQL", "db"],
    [redis, "Redis", "redis"],
  ]) {
    const inspected = validateDisposableContainer(resource, label, expected);
    if (inspected.project !== composeProject || inspected.service !== expectedService) {
      throw new Error(
        `${label} ${inspected.name} is not the ${expectedService} service in Compose project ${composeProject}`,
      );
    }
  }

  return Object.freeze({ composeProject, serverCount: serverResources.length });
}

function directContainerEndpoint(origin, label) {
  let parsed;
  try {
    parsed = new URL(String(origin));
  } catch {
    throw new Error(`${label} must be an absolute direct-container URL`);
  }
  if (
    parsed.protocol !== "http:" ||
    parsed.port !== "8080" ||
    parsed.username ||
    parsed.password ||
    parsed.pathname !== "/" ||
    parsed.search ||
    parsed.hash
  ) {
    throw new Error(`${label} must be exactly http://<declared-container-ip>:8080`);
  }
  return Object.freeze({
    origin: parsed.origin,
    host: parsed.hostname.replace(/^\[|\]$/g, "").toLowerCase(),
  });
}

/// Bind every direct bearer-token destination to exactly one inspected server
/// container. This prevents a valid-but-mistyped loopback/Docker origin from
/// mutating a different stack while cleanup operates on the fenced database.
export function assertDirectAdminOriginBindings({ webTargets, controlTarget, servers } = {}) {
  if (!Array.isArray(webTargets) || !Array.isArray(servers)) {
    throw new Error("direct admin origin binding requires webTargets and inspected servers");
  }
  const declaredWeb = servers.filter(({ service }) => service === "rsctf");
  const declaredControl = servers.filter(({ service }) => service === "rsctf-control");
  if (webTargets.length !== declaredWeb.length || declaredControl.length !== 1) {
    throw new Error(
      "direct admin origins must name every declared rsctf replica and the one rsctf-control replica",
    );
  }

  const normalizedServers = servers.map((server, index) => {
    const name = String(server?.name || "").trim();
    if (!name || !Array.isArray(server.networkAddresses) || server.networkAddresses.length === 0) {
      throw new Error(`server[${index}] ${name || "<unnamed>"} has no inspectable network IP`);
    }
    return {
      name,
      service: String(server.service || ""),
      networkAddresses: new Set(
        server.networkAddresses
          .map((address) => String(address || "").trim().replace(/^\[|\]$/g, "").toLowerCase())
          .filter(Boolean),
      ),
    };
  });
  const targets = [
    ...webTargets.map((origin, index) => ({
      ...directContainerEndpoint(origin, `WEB_TARGETS[${index}]`),
      expectedService: "rsctf",
    })),
    {
      ...directContainerEndpoint(controlTarget, "CONTROL_TARGET"),
      expectedService: "rsctf-control",
    },
  ];

  const boundNames = new Set();
  const bindings = targets.map((target) => {
    const matches = normalizedServers.filter(({ networkAddresses }) => networkAddresses.has(target.host));
    if (matches.length !== 1) {
      throw new Error(`${target.origin} maps to ${matches.length} declared server containers; expected exactly one`);
    }
    const [server] = matches;
    if (server.service !== target.expectedService) {
      throw new Error(
        `${target.origin} maps to ${server.name} (${server.service}), expected ${target.expectedService}`,
      );
    }
    if (boundNames.has(server.name)) {
      throw new Error(`direct admin origins map more than once to ${server.name}`);
    }
    boundNames.add(server.name);
    return Object.freeze({ origin: target.origin, container: server.name, service: server.service });
  });
  if (boundNames.size !== normalizedServers.length) {
    throw new Error("one or more declared server containers has no exact direct admin origin");
  }
  return Object.freeze(bindings);
}

export function resolveOperationPath(operationOrId, context = {}) {
  const item = typeof operationOrId === "string" ? operationById.get(operationOrId) : operationOrId;
  if (!item) throw new Error(`unknown admin operation ${operationOrId}`);
  let path = item.path;
  for (const [placeholder, contextKey] of Object.entries(item.params)) {
    const value = context[contextKey];
    if (value === undefined || value === null || String(value).length === 0) {
      throw new Error(`${item.id} requires admin context ${contextKey}`);
    }
    path = path.replace(`{${placeholder}}`, encodeURIComponent(String(value)));
  }
  if (/\{[^}]+\}/.test(path)) throw new Error(`${item.id} has an unresolved route parameter`);
  return `${path}${item.query ? `?${item.query}` : ""}`;
}

// Build the exhaustive read/origin preflight used by k6 before its timed
/// fixed-rate phase. Unlike the timed rotation, this matrix is finite and
/// therefore proves every live-fixture read reached every eligible origin at
/// least once even when a short duration is selected.
export function buildAdminReadOriginMatrix(context, webOrigins, controlOrigins) {
  if (!Array.isArray(webOrigins) || webOrigins.length === 0) {
    throw new Error("webOrigins must contain at least one origin");
  }
  if (!Array.isArray(controlOrigins) || controlOrigins.length === 0) {
    throw new Error("controlOrigins must contain at least one origin");
  }
  const matrix = [];
  for (const operation of ADMIN_READ_OPERATIONS) {
    const path = resolveOperationPath(operation, context);
    const origins = operation.surface === "control" ? controlOrigins : webOrigins;
    for (const selectedOrigin of origins) {
      matrix.push(Object.freeze({ operation, path, selectedOrigin }));
    }
  }
  return Object.freeze(matrix);
}

function headerValue(headers, name) {
  if (!headers || typeof headers !== "object") return "";
  if (typeof headers.get === "function") return String(headers.get(name) || "");
  const wanted = name.toLowerCase();
  for (const [key, value] of Object.entries(headers)) {
    if (key.toLowerCase() === wanted) return Array.isArray(value) ? value.join(",") : String(value);
  }
  return "";
}

function privateNoStore(headers) {
  const cacheControl = headerValue(headers, "cache-control");
  return /(?:^|,)\s*private\b/i.test(cacheControl) && /(?:^|,)\s*no-store\b/i.test(cacheControl);
}

const object = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const number = (value) => typeof value === "number" && Number.isFinite(value);

function validPage(body) {
  return object(body) && Array.isArray(body.data) && Number.isSafeInteger(body.total) &&
    Number.isSafeInteger(body.length) && body.length === body.data.length && body.total >= body.length;
}

function validMessage(body, status) {
  return object(body) && typeof body.title === "string" && body.status === status;
}

function validRepoScan(body) {
  return object(body) && ["gamesCreated", "gamesUpdated", "challengesImported", "challengesUpdated", "failures"]
    .every((key) => Number.isSafeInteger(body[key]) && body[key] >= 0) && Array.isArray(body.messages);
}

function validWorker(worker) {
  return object(worker) && typeof worker.id === "string" && typeof worker.name === "string" &&
    ["Enabled", "Draining", "Disabled"].includes(worker.administrativeState) &&
    typeof worker.online === "boolean" && object(worker.capacity);
}

export function validateAdminResponse(operationId, response) {
  const item = operationById.get(operationId);
  if (!item) throw new Error(`unknown admin operation ${operationId}`);
  if (!response || typeof response !== "object") return false;
  if (!item.expectedStatuses.includes(Number(response.status))) return false;
  const body = response.body !== undefined ? response.body : response.json;
  const headers = response.headers || {};
  switch (item.responseKind) {
    case "array": return Array.isArray(body);
    case "page": return validPage(body);
    case "message": return validMessage(body, Number(response.status));
    case "string": return typeof body === "string" && body.length > 0;
    case "private-string":
      return typeof body === "string" && body.length > 0 && privateNoStore(headers);
    case "my-ip":
      return object(body) && typeof body.detectedIp === "string" && typeof body.rawConnectionIp === "string" &&
        typeof body.forwardedFor === "string" && typeof body.proxyTrusted === "boolean" &&
        Array.isArray(body.trustedNetworks);
    case "config":
      return object(body) && object(body.accountPolicy) && object(body.globalConfig) &&
        object(body.containerPolicy) && object(body.proxyTrust);
    case "dashboard":
      return object(body) && object(body.systemStats) && Array.isArray(body.topGames) &&
        ["userCount", "teamCount", "activeContainerCount"].every((key) =>
          Number.isSafeInteger(body.systemStats[key]) && body.systemStats[key] >= 0);
    case "game-writeups": return object(body) && object(body.divisions) && Array.isArray(body.writeups);
    case "zip": {
      const archive = body ?? response.bytes ?? response.text;
      return /application\/zip/i.test(headerValue(headers, "content-type")) &&
        (typeof archive === "string" || archive instanceof ArrayBuffer || ArrayBuffer.isView(archive));
    }
    case "import":
      return object(body) && ["total", "created", "updated", "skipped"].every((key) =>
        Number.isSafeInteger(body[key]) && body[key] >= 0) && Array.isArray(body.users) &&
        body.total === body.created + body.updated + body.skipped && privateNoStore(headers);
    case "credential-send":
      return object(body) && Number.isSafeInteger(body.sent) && Number.isSafeInteger(body.failed) &&
        Array.isArray(body.results) && body.sent + body.failed === body.results.length && privateNoStore(headers);
    case "user": return object(body) && typeof body.userId === "string" && typeof body.role === "string";
    case "instance-stats":
      return object(body) && ["cpuPercent", "memoryUsedBytes", "memoryLimitBytes", "netRxBytes", "netTxBytes", "sampledAt"]
        .every((key) => number(body[key])) && body.cpuPercent >= 0 && body.memoryUsedBytes >= 0;
    case "bulk-rebuild":
      return object(body) && Number.isSafeInteger(body.enqueued) && body.enqueued >= 0 &&
        Number.isSafeInteger(body.skipped) && body.skipped >= 0 && Array.isArray(body.messages);
    case "prune":
      return object(body) && Number.isSafeInteger(body.removed) && body.removed >= 0 && Array.isArray(body.messages);
    case "build":
      return object(body) && Number.isSafeInteger(body.id) && Number.isSafeInteger(body.challengeId) &&
        typeof body.status === "string";
    case "repo-scan-result": return validRepoScan(body);
    case "repo-binding":
      return object(body) && Number.isSafeInteger(body.id) && typeof body.repoUrl === "string" &&
        ["Active", "Paused"].includes(body.status) && Array.isArray(body.games);
    case "ad-service":
      return object(body) && Number.isSafeInteger(body.adTeamServiceId) &&
        Number.isSafeInteger(body.participationId) && Number.isSafeInteger(body.challengeId) &&
        typeof body.host === "string" && Number.isSafeInteger(body.port);
    case "error": return object(body) && typeof body.title === "string" && body.status === response.status;
    case "workers": return Array.isArray(body) && body.every(validWorker);
    case "worker": return validWorker(body);
    case "worker-created": return object(body) && validWorker(body.worker) && object(body.enrollment) &&
      typeof body.enrollment.token === "string" && body.enrollment.workerId === body.worker.id &&
      privateNoStore(headers);
    case "enrollment-token":
      return object(body) && typeof body.workerId === "string" && typeof body.token === "string" &&
        number(body.expiresAt) && privateNoStore(headers);
    case "enrollment":
      return object(body) && typeof body.workerId === "string" && typeof body.controlAddress === "string" &&
        typeof body.dataAddress === "string" && typeof body.serverName === "string" &&
        typeof body.certificatePem === "string" && body.certificatePem.length > 0 &&
        typeof body.caPem === "string" && body.caPem.length > 0 && privateNoStore(headers);
    case "object": return object(body);
    default: return false;
  }
}

const VOLATILE_KEYS = new Set([
  "currentActivity",
  "heartbeatAt",
  "leaseExpiresAt",
  "sampledAt",
  "updatedAt",
]);

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (!object(value)) return value;
  const out = {};
  for (const key of Object.keys(value).sort()) {
    if (!VOLATILE_KEYS.has(key)) out[key] = canonical(value[key]);
  }
  return out;
}

export function stableReplicaProjection(operationId, body) {
  if (!operationById.has(operationId)) throw new Error(`unknown admin operation ${operationId}`);
  if (operationId === "admin_my_ip_get") {
    return canonical({
      proxyTrusted: body?.proxyTrusted,
      trustedNetworks: Array.isArray(body?.trustedNetworks) ? [...body.trustedNetworks].sort() : body?.trustedNetworks,
      detectedIpPresent: typeof body?.detectedIp === "string" && body.detectedIp.length > 0,
      rawConnectionIpPresent: typeof body?.rawConnectionIp === "string" && body.rawConnectionIp.length > 0,
    });
  }
  if (operationId === "admin_instance_stats_get") {
    return canonical({
      hasCpu: number(body?.cpuPercent),
      hasMemoryUsed: number(body?.memoryUsedBytes),
      memoryLimitBytes: body?.memoryLimitBytes,
      hasNetRx: number(body?.netRxBytes),
      hasNetTx: number(body?.netTxBytes),
    });
  }
  return canonical(body);
}

function rustRawStringEnd(source, start) {
  let marker = start;
  if (source[marker] === "b") marker += 1;
  if (source[marker] !== "r") return null;
  marker += 1;
  let hashes = 0;
  while (source[marker] === "#") { hashes += 1; marker += 1; }
  if (source[marker] !== '"') return null;
  const terminator = `"${"#".repeat(hashes)}`;
  const end = source.indexOf(terminator, marker + 1);
  if (end < 0) throw new Error("unterminated Rust raw string while scanning admin routes");
  return end + terminator.length - 1;
}

function routeCalls(source) {
  const calls = [];
  let outerQuote = false;
  let outerEscaped = false;
  let outerLineComment = false;
  let outerBlockDepth = 0;
  for (let cursor = 0; cursor < source.length; cursor += 1) {
    const outerChar = source[cursor];
    const outerNext = source[cursor + 1];
    if (outerLineComment) {
      if (outerChar === "\n") outerLineComment = false;
      continue;
    }
    if (outerBlockDepth > 0) {
      if (outerChar === "/" && outerNext === "*") { outerBlockDepth += 1; cursor += 1; }
      else if (outerChar === "*" && outerNext === "/") { outerBlockDepth -= 1; cursor += 1; }
      continue;
    }
    if (outerQuote) {
      if (outerEscaped) outerEscaped = false;
      else if (outerChar === "\\") outerEscaped = true;
      else if (outerChar === '"') outerQuote = false;
      continue;
    }
    if (outerChar === "/" && outerNext === "/") { outerLineComment = true; cursor += 1; continue; }
    if (outerChar === "/" && outerNext === "*") { outerBlockDepth = 1; cursor += 1; continue; }
    const rawStringEnd = rustRawStringEnd(source, cursor);
    if (rawStringEnd !== null) { cursor = rawStringEnd; continue; }
    if (outerChar === '"') { outerQuote = true; continue; }
    if (!source.startsWith(".route", cursor)) continue;
    let open = cursor + ".route".length;
    while (/\s/.test(source[open] || "")) open += 1;
    if (source[open] !== "(") continue;

    let depth = 0;
    let quote = null;
    let escaped = false;
    let lineComment = false;
    let blockDepth = 0;
    let end = -1;
    for (let index = open; index < source.length; index += 1) {
      const char = source[index];
      const next = source[index + 1];
      if (lineComment) {
        if (char === "\n") lineComment = false;
        continue;
      }
      if (blockDepth > 0) {
        if (char === "/" && next === "*") { blockDepth += 1; index += 1; }
        else if (char === "*" && next === "/") { blockDepth -= 1; index += 1; }
        continue;
      }
      if (quote) {
        if (escaped) escaped = false;
        else if (char === "\\") escaped = true;
        else if (char === quote) quote = null;
        continue;
      }
      if (char === "/" && next === "/") { lineComment = true; index += 1; continue; }
      if (char === "/" && next === "*") { blockDepth = 1; index += 1; continue; }
      if (char === '"') { quote = char; continue; }
      if (char === "(") depth += 1;
      else if (char === ")") {
        depth -= 1;
        if (depth === 0) { end = index; break; }
      }
    }
    if (end < 0) throw new Error("unterminated Axum .route(...) call");
    calls.push(source.slice(open + 1, end));
    cursor = end;
  }
  return calls;
}

function parsedRoutes(source) {
  const routes = [];
  for (const call of routeCalls(source)) {
    const pathMatch = call.match(/^\s*"((?:\\.|[^"\\])*)"\s*,/);
    if (!pathMatch) throw new Error(`could not parse Axum route path from: ${call.slice(0, 80)}`);
    const path = pathMatch[1].replaceAll('\\"', '"').replaceAll('\\\\', '\\');
    const methods = [...call.matchAll(/\b(get|post|put|delete|patch)\s*\(/g)].map((entry) => entry[1].toUpperCase());
    if (methods.length === 0) throw new Error(`could not parse HTTP method for ${path}`);
    for (const method of new Set(methods)) routes.push({ method, path });
  }
  return routes;
}

// Shared by the separate organizer `/api/edit` acceptance catalog. Keep the
// parser here because the admin catalog already owns the source-drift contract;
// consumers apply their own exact namespace filter to the returned routes.
export function parseAxumRouterOperations(source) {
  if (typeof source !== "string") throw new TypeError("Axum router source must be a string");
  return Object.freeze(parsedRoutes(source).map(Object.freeze));
}

export function parseAdminRouterOperations(sources) {
  if (!sources || typeof sources !== "object") throw new TypeError("router sources must be an object");
  const operations = [];
  for (const key of ["admin", "ad", "workers"]) {
    const chunks = typeof sources[key] === "string" ? [sources[key]] : sources[key];
    if (!Array.isArray(chunks) || chunks.length === 0 || chunks.some((chunk) => typeof chunk !== "string")) {
      throw new Error(`missing ${key} router source`);
    }
    for (const source of chunks) {
      for (const route of parsedRoutes(source)) {
        if (route.path.startsWith("/api/admin/") || route.path.startsWith("/api/ad/admin/") || route.path === "/api/workers/enroll") {
          operations.push({ ...route, source: key });
        }
      }
    }
  }
  const hubChunks = typeof sources.adminHub === "string" ? [sources.adminHub] : sources.adminHub;
  const signalR = Array.isArray(hubChunks)
    ? hubChunks.flatMap((source) => parsedRoutes(source)).filter(({ path }) =>
      path === "/hub/admin" || path.startsWith("/hub/admin/"))
    : [];
  return Object.freeze({
    operations: Object.freeze(operations.map(Object.freeze)),
    signalR: Object.freeze(signalR.map(Object.freeze)),
  });
}

function routeKey({ method, path }) {
  return `${method} ${path}`;
}

function exactSetDiff(expected, actual) {
  return {
    missing: [...expected].filter((key) => !actual.has(key)),
    extra: [...actual].filter((key) => !expected.has(key)),
  };
}

export function assertRouterCoverage(sources) {
  const parsed = parseAdminRouterOperations(sources);
  const expected = new Set(ADMIN_OPERATIONS.map(routeKey));
  const actual = new Set(parsed.operations.map(routeKey));
  const routes = exactSetDiff(expected, actual);
  if (routes.missing.length || routes.extra.length || actual.size !== parsed.operations.length) {
    throw new Error(
      `admin router catalog drift (missing: ${routes.missing.join(", ") || "none"}; ` +
        `uncovered: ${routes.extra.join(", ") || "none"}; parsed=${parsed.operations.length}, unique=${actual.size})`,
    );
  }
  if (typeof sources.adminHub === "string" || Array.isArray(sources.adminHub)) {
    const expectedHub = new Set(ADMIN_SIGNALR_SURFACES.map(routeKey));
    const actualHub = new Set(parsed.signalR.map(routeKey));
    const hubs = exactSetDiff(expectedHub, actualHub);
    if (hubs.missing.length || hubs.extra.length || actualHub.size !== parsed.signalR.length) {
      throw new Error(
        `admin SignalR catalog drift (missing: ${hubs.missing.join(", ") || "none"}; ` +
          `uncovered: ${hubs.extra.join(", ") || "none"})`,
      );
    }
  }
  return Object.freeze({ operations: actual.size, signalR: parsed.signalR.length });
}
