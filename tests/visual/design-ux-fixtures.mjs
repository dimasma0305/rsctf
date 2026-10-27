// Invented browser data only. Every API request is answered inside Chromium.
import { fixture as overview, guideSetup } from "./overview-pages-fixtures.mjs";
import { createChallengeLinksFixture } from "./challenge-links-fixtures.mjs";
import { fixture as adminNavigation } from "./admin-navigation-fixtures.mjs";

export { guideSetup };

export function createDesignFixture(options = {}) {
  const player = createChallengeLinksFixture();
  player.game.writeupRequired = true;
  Object.assign(player.responses["/api/game/901/details"], {
    writeupRequired: true,
    writeupDeadline: Date.now() + 86400000,
  });
  player.responses["/api/game/901/writeup"] = {
    submitted: false,
    note: "Combine your solutions into one PDF.",
  };
  const profile = overview("/api/account/profile").body;
  let settings = structuredClone(overview("/api/admin/config").body);
  settings.revision ??= 0;
  let users = Array.from({ length: 3 }, (_, index) => ({
    id:
      index === 0
        ? profile.userId
        : `22222222-2222-4222-8222-${String(index).padStart(12, "0")}`,
    userName: index === 0 ? profile.userName : `Player ${index}`,
    email: index === 0 ? profile.email : `player${index}@example.invalid`,
    role: index === 0 ? "Admin" : "User",
    emailConfirmed: true,
    registrationTimeUtc: Date.now() - 86400000,
  }));
  const state = {
    ...options,
    player,
    profile,
    users,
    failed: new Set(options.failed || []),
    requests: [],
    writes: [],
    unknown: [],
    allowWrites: false,
    mutationStatus: 200,
    holdWrites: false,
    releases: new Set(),
  };
  state.release = () => {
    for (const done of state.releases) done();
    state.releases.clear();
  };
  state.fixture = async (request) => {
    const url = new URL(request.url),
      path = url.pathname.toLowerCase(),
      method = request.method;
    state.requests.push({ method, path, search: url.search });
    if (path.startsWith("/hub"))
      return { body: { error: "Read-only fixture live updates" } };
    if (state.failed.has(path))
      return {
        status: 503,
        body: { title: "Fixture unavailable", status: 503 },
      };
    if (path === "/api/account/profile" && state.role === "Manager")
      return adminNavigation(path, method, "Manager");
    if (path === "/api/admin/users/search" && method === "POST") {
      const hint = url.searchParams.get("hint")?.toLowerCase() || "";
      const data = users.filter((user) =>
        (user.userName + user.email).toLowerCase().includes(hint),
      );
      return { body: { data, total: data.length, length: data.length } };
    }
    if (!["GET", "HEAD"].includes(method)) {
      const body = request.postData ? JSON.parse(request.postData) : undefined;
      const write = {
        path,
        method,
        body,
        search: url.search,
        idempotencyKey: Object.entries(request.headers).find(
          ([key]) => key.toLowerCase() === "idempotency-key",
        )?.[1],
      };
      state.writes.push(write);
      if (
        state.allowWrites &&
        path === "/api/admin/config" &&
        method === "PUT"
      ) {
        if (body.expectedRevision !== settings.revision)
          return {
            status: 409,
            body: { title: "Configuration changed", status: 409 },
          };
        settings = { ...settings, ...body, revision: settings.revision + 1 };
        if (state.failAfterSave) state.failed.add(path);
        state.settings = settings;
        return {
          body: { operationId: body.operationId, revision: settings.revision },
        };
      }
      if (
        !state.allowWrites ||
        !/^\/api\/admin\/users\/[\da-f-]+(?:\/password(?:-email)?)?$/.test(path)
      ) {
        state.unknown.push(write);
        return { status: 405, body: { title: "Unexpected fixture write" } };
      }
      if (state.holdWrites)
        await new Promise((done) => state.releases.add(done));
      if (state.mutationStatus !== 200)
        return {
          status: state.mutationStatus,
          body: { title: "Fixture unavailable", status: state.mutationStatus },
        };
      if (method === "PUT")
        users = users.map((user) =>
          path.endsWith(user.id) ? { ...user, ...body } : user,
        );
      if (method === "DELETE" && !path.endsWith("/password"))
        users = users.filter((user) => !path.endsWith(user.id));
      return {
        body: path.endsWith("/password") ? "fixture-only-password" : {},
      };
    }
    if (path === "/api/admin/users")
      return {
        body: { data: users, total: users.length, length: users.length },
      };
    if (path === "/api/admin/config") return { body: settings };
    if (path === "/api/game/901/details/participant")
      return player.fixture(url.pathname + url.search, method);
    if (
      path.startsWith("/api/admin/config/operations/") &&
      state.writes.some(
        (write) => write.body?.operationId === path.split("/").at(-1),
      )
    )
      return {
        status: 404,
        body: { title: "Operation not committed", status: 404 },
      };
    const response = state.playerMode
      ? player.fixture(url.pathname + url.search, method)
      : overview(
          url.pathname + url.search,
          method,
          "normal",
          state.role || "Admin",
        );
    if (response.status === 404) state.unknown.push({ method, path });
    if (state.tallPoster && path === "/api/game") {
      const poster =
        "data:image/svg+xml," +
        encodeURIComponent(
          '<svg xmlns="http://www.w3.org/2000/svg" width="40" height="4000"><rect width="40" height="4000" fill="#14618a"/></svg>',
        );
      return {
        ...response,
        body: {
          ...response.body,
          data: response.body.data.map((event) => ({ ...event, poster })),
        },
      };
    }
    return response;
  };
  return state;
}
