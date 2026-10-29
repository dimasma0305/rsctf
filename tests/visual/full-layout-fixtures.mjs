// Local read-only rendering fixtures. Unknown reads are failures, never empty successes.
import { fixture as overview, posts } from "./overview-pages-fixtures.mjs";
import { fixture as admin } from "./admin-navigation-fixtures.mjs";
import { createCompetitionFixture } from "./competition-fixtures.mjs";
import { createArenaFixture } from "./attack-arena-fixtures.mjs";
import { contractFixture } from "./layout-contract-fixtures.mjs";
import { gradingFixture } from "./writeup-grading-fixtures.mjs";

export const player = createCompetitionFixture();
const arena = createArenaFixture("jeopardy");
const scoreboard = {
  ...arena.scoreboard,
  challenges: player.responses["/api/game/901/details"].challenges,
  items: [player.rank],
  total: 1,
  skip: 0,
  count: 30,
  snapshotVersion: "fixture",
  updateTimeUtc: Date.now(),
  bloodBonus: 0,
  divisions: [{ id: 1, name: "Open" }],
  timelines: [],
};
const grading = gradingFixture();
const responses = {
  ...player.responses,
  "/api/game/901/scoreboard": scoreboard,
  "/api/game/901/scoreboard/page": scoreboard,
  "/api/game/901/ad/scoreboard": arena.ad,
  "/api/game/901/ad/koth/scoreboard": arena.koth,
  "/api/team/selector": [{ id: 7, name: "TCP1P", captain: true }],
  "/api/team": [],
  "/api/admin/workers": [],
  "/api/donations": {
    provider: "Trakteer",
    currency: "IDR",
    fetchedAt: Date.now(),
    totalAmount: 0,
    totalQuantity: 0,
    supportCount: 0,
    supporterCount: 0,
    leaderboard: [],
    messages: [],
  },
  "/api/game/games/901/captures/page": { items: [], nextCursor: null },
  "/api/account/stats": {
    totalSolves: 0,
    totalFirstBloods: 0,
    gamesParticipated: 0,
    solvesByCategory: {},
    games: [],
  },
  "/api/account/apitoken": { token: null },
  "/api/posts/page": { data: posts, total: posts.length },
  "/api/posts/00000001": {
    ...posts[0],
    content: "# Schedule\n\nRead the player guide.",
    isHidden: false,
  },
  "/api/admin/users": {
    data: [
      {
        id: player.profile.userId,
        userName: "Fixture player",
        email: "player@example.invalid",
        role: "User",
        emailConfirmed: true,
        registrationTimeUtc: Date.now(),
      },
    ],
    total: 1,
    length: 1,
  },
};
export function fixture(path, method = "GET", role = "Admin") {
  const url = new URL(path, "http://127.0.0.1"),
    p = url.pathname.toLowerCase();
  if (p.startsWith("/hub")) return { body: { error: "Read-only fixture" } };
  if (!["GET", "HEAD"].includes(method))
    return { status: 405, body: { title: "Fixture write blocked" } };
  if (p === "/api/account/profile")
    return role === "Guest"
      ? { status: 401, body: {} }
      : { body: { ...player.profile, role } };
  if (p === "/api/config")
    return {
      body: {
        ...player.config,
        allowPasswordLogin: true,
        allowPasswordRegistration: true,
        donationsEnabled: true,
      },
    };
  if (p === "/api/admin/writeups/901/grading")
    return grading("/api/admin/writeups/19/grading");
  if (p in responses) return { body: responses[p] };
  const mapped = path
    .replace("/edit/games/901", "/edit/games/19")
    .replace("/challenges/9001", "/challenges/74");
  for (const provider of [overview, admin]) {
    const result = provider(mapped, method);
    if (result.status !== 404) return result;
  }
  const generated = contractFixture(p);
  if (generated) return generated;
  return {
    status: 404,
    body: { title: "Unknown full-layout fixture", path: p },
  };
}
