import http from "node:http";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { fixture as commonFixture } from "./admin-operations-fixtures.mjs";

export const now = Date.now();
const challenges = [
  "Parcel Panic",
  "Yani-netlink",
  "A deliberately long challenge title for a disabled service",
].map((title, i) => ({
  challengeId: 74 + i,
  title,
  isEnabled: i !== 2,
  controlRevision: 1,
  tickSeconds: 60,
  flagLifetimeTicks: 5,
  teamsWithLiveContainer: 4,
}));
export const teams = [
  "Stargazers",
  "Packet Rangers",
  "A long team name that needs to remain readable",
  "Offline team",
  "Waiting team",
  "No service yet",
].map((teamName, i) => ({
  participationId: i + 1,
  teamName,
  services:
    i === 5
      ? []
      : challenges.map((c, j) => ({
          adTeamServiceId: i * 10 + j + 1,
          challengeId: c.challengeId,
          containerIp: "10.13.41." + (i + 2),
          containerPort: 1337,
          containerGuid:
            j === 0 ? null : "11111111-1111-4111-8111-111111111111",
          selfHosted: j === 0,
          lastCheckId: i === 4 ? null : i * 10 + j + 1,
          lastCheckStatus: ["Ok", "Mumble", "InternalError", "Offline", null][
            i
          ],
          currentFlag: "FIXTURE{not-a-real-flag}",
          snapshotAvailable: false,
          changedFileCount: j === 1 ? 2 : null,
        })),
}));
export const hills = ["Tower of Babel", "Minions in 32K", "Rythme"].map(
  (title, i) => ({
    challengeId: 90 + i,
    title,
    isEnabled: i !== 2,
    controlRevision: 1,
    containerGuid: "11111111-1111-4111-8111-111111111111",
    containerIp: "10.13.41." + (20 + i),
    containerPort: 1337,
    lastCheckStatus: i === 1 ? "Mumble" : "Ok",
    currentHolderTeamName: i === 0 ? "Stargazers" : null,
    currentHolderParticipationId: i === 0 ? 1 : null,
    resetPhase: i === 2 ? "Finalizing" : "Active",
    durablePhase: i === 2 ? "ReadinessPending" : "Active",
    cycleNumber: 5,
    cycleTick: 2,
    nextResetTicks: i === 0 ? 1 : null,
    cooldownParticipants: [],
    cycleChampions: [],
    oldContainerId: null,
    replacementContainerId: null,
    resetAttempt: i === 2 ? 2 : 0,
    readinessFailureCount: i === 2 ? 1 : 0,
    lastReadinessError:
      i === 2 ? "Fixture readiness check is still pending" : null,
    canRetry: i === 2,
    resetReceiptId: 42,
    scoringReceiptId: 43,
    claimSource: i === 0 ? "Marker" : "Api",
    apiObserverConfigured: i !== 0,
    apiObserverSecretHint: null,
    apiLastObservationAt: now - 1000,
  }),
);

export function fixture(
  path,
  method = "GET",
  scenario = "normal",
  role = "Admin",
) {
  const p = new URL(path, "http://localhost").pathname.toLowerCase();
  if (!["GET", "HEAD"].includes(method))
    return { status: 405, body: { title: "Fixture write blocked" } };
  if (p.startsWith("/api/edit/")) {
    if (role !== "Admin") return { status: 403, body: { title: "forbidden" } };
    if (scenario === "loading") return { hold: true };
    if (
      scenario === "error" ||
      (scenario === "stale" && !p.endsWith("/engines"))
    )
      return { status: 503, body: { title: "Fixture unavailable" } };
    if (p.endsWith("/ad/engines"))
      return {
        body: {
          hasAttackDefense: scenario !== "no-engines",
          hasKoth: scenario !== "no-engines",
          start: scenario === "upcoming" ? now + 3600000 : now - 3600000,
          end: scenario === "ended" ? now - 1000 : now + 3600000,
          serverTime: Date.now(),
        },
      };
    const timing = {
      currentRound: 178,
      roundStartedAt: now - 20000,
      roundEndsAt: now + 40000,
      scoringPaused: scenario === "paused",
      scoringPausedAt: scenario === "paused" ? now : null,
      controlRevision: 1,
    };
    if (p.endsWith("/ad/state"))
      return {
        body: {
          ...timing,
          challenges,
          teams: scenario === "empty" ? [] : teams,
        },
      };
    if (p.endsWith("/ad/live")) return { body: { ...timing, services: [] } };
    if (p.endsWith("/ad/koth/state"))
      return {
        body: {
          epochTicks: 8,
          cycleTicks: 3,
          championCooldownTicks: 1,
          claimConfirmationTicks: 1,
          tickSeconds: 60,
          scoringGeneratedAt: now,
          latestRound: 178,
          currentRoundEndsAt: now + 40000,
          scoringPaused: scenario === "paused",
          scoringPausedAt: null,
          controlRevision: 1,
          hills,
          teams: teams.map((team, i) => ({
            ...team,
            rank: i + 1,
            settledTotal: 120 - i * 10,
            projectedTotal: 125 - i * 10,
            hills: hills.map((h) => ({
              challengeId: h.challengeId,
              settledPoints: 40,
              isCurrentHolder: i === 0,
            })),
          })),
        },
      };
    if (p.endsWith("/snapshot/changes"))
      return {
        body: {
          snapshotAvailable: false,
          live: true,
          changes: [
            { path: "/app/config.json", kind: 0 },
            { path: "/app/readme.txt", kind: 1 },
          ],
          observedChanges: 2,
          truncated: false,
          filteredCategories: [],
        },
      };
    if (p.endsWith("/receipts"))
      return {
        body: {
          challengeId: 90,
          cycleNumber: 5,
          receipts: [
            {
              id: 42,
              phase: "Active",
              attempt: 1,
              receipt: { status: "ready" },
              filesystemDiff: null,
              createdAt: now,
            },
          ],
        },
      };
  }
  return commonFixture(path, method, "normal", role);
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url) &&
  process.argv.includes("--serve")
) {
  http
    .createServer(async (req, res) => {
      try {
        if (!["GET", "HEAD"].includes(req.method)) {
          res.writeHead(405);
          return res.end();
        }
        if (req.url.toLowerCase().startsWith("/api/")) {
          const r = fixture(req.url, req.method);
          res.writeHead(r.status || 200, {
            "Content-Type": "application/json",
          });
          return res.end(JSON.stringify(r.body));
        }
        const r = await fetch("http://127.0.0.1:63017" + req.url);
        res.writeHead(r.status, {
          "Content-Type":
            r.headers.get("Content-Type") || "application/octet-stream",
        });
        res.end(Buffer.from(await r.arrayBuffer()));
      } catch {
        res.writeHead(502);
        res.end("Fixture proxy failed");
      }
    })
    .listen(63018, "127.0.0.1");
}
