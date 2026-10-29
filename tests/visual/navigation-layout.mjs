// Real SPA navigation against the production build; no API writes or external data.
import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { launchBrowser, isRetiredInterception } from "./cdp.mjs";
import { serveAccountBuild } from "./account-access-fixtures.mjs";
import { createCompetitionFixture } from "./competition-fixtures.mjs";
import {
  createArenaFixture,
  arenaBrowserSetup,
} from "./attack-arena-fixtures.mjs";
import { fixture as overview, guideSetup } from "./overview-pages-fixtures.mjs";
import { repositoryRoot } from "./route-catalog.mjs";
import {
  installLayoutProbe,
  geometryFailures,
  sessionScore,
} from "./layout-shift-metrics.mjs";

const server = await serveAccountBuild(resolve(repositoryRoot, "web/build"));
const output = resolve(
  process.env.RSCTF_NAV_LAYOUT_OUTPUT ||
    resolve(repositoryRoot, "visual-audit-output/navigation-layout"),
);
mkdirSync(output, { recursive: true });
const player = createCompetitionFixture();
const scoreboard = {
  updateTimeUtc: Date.now(),
  bloodBonus: 0,
  challenges: player.responses["/api/game/901/details"].challenges,
  challengeCount: 100,
  items: [player.rank],
  timelines: [],
  divisions: [],
  total: 1,
  skip: 0,
  count: 30,
  snapshotVersion: "fixture",
};
player.responses["/api/game/901/scoreboard"] = scoreboard;
player.responses["/api/game/901/scoreboard/page"] = scoreboard;
const arena = createArenaFixture("jeopardy");
player.responses["/api/game/901/ad/scoreboard"] = arena.ad;
player.responses["/api/game/901/ad/koth/scoreboard"] = arena.koth;
player.responses["/api/team/selector"] = [
  { id: player.rank.id, name: player.rank.name, captain: true },
];
player.responses["/api/admin/users"] = {
  data: [
    {
      id: player.profile.userId,
      userName: player.profile.userName,
      email: player.profile.email,
      role: "User",
      emailConfirmed: true,
      registrationTimeUtc: Date.now() - 86400000,
    },
  ],
  total: 1,
  length: 1,
};
const axeSource = readFileSync(
  resolve(repositoryRoot, "web/node_modules/axe-core/axe.min.js"),
  "utf8",
);
const { cdp, close } = await launchBrowser().catch(async (e) => {
  await server.close();
  throw e;
});
const reports = [],
  errors = [],
  unknown = [],
  pending = new Set(),
  retiredInterceptions = [];
let negativeControl;
let slow = false,
  unavailable = false,
  coldRecovery;
const evaluate = async (expression) => {
  const r = await cdp.send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (r.exceptionDetails)
    throw new Error(
      r.exceptionDetails.exception?.description || r.exceptionDetails.text,
    );
  return r.result?.value;
};
const waitFor = async (expression) => {
  for (let i = 0; i < 150; i++) {
    if (await evaluate(`Boolean(${expression})`)) return;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(
    `Timed out: ${expression}; ${await evaluate("document.body.innerText.slice(0,1500)")}`,
  );
};
try {
  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  await cdp.send("Network.enable");
  await cdp.send("Network.setCacheDisabled", { cacheDisabled: true });
  cdp.on("Runtime.exceptionThrown", (e) =>
    errors.push(
      e.exceptionDetails.exception?.description || e.exceptionDetails.text,
    ),
  );
  await cdp.send("Page.addScriptToEvaluateOnNewDocument", {
    source: `if(location.origin===${JSON.stringify(server.origin)}) {
    localStorage.clear(); ${guideSetup} ${arenaBrowserSetup}
    localStorage.setItem('language',JSON.stringify('en-US'));
    localStorage.setItem('challenge-explorer-view',JSON.stringify('list'));
    Object.defineProperty(navigator,'connection',{value:{saveData:true,effectiveType:'3g'},configurable:true});
    (${installLayoutProbe.toString()})();
  }`,
  });
  await cdp.send("Fetch.enable", {
    patterns: [{ urlPattern: server.origin + "/*" }],
  });
  cdp.on("Fetch.requestPaused", (event) => {
    const task = (async () => {
      const { requestId, request, resourceType } = event;
      const url = new URL(request.url),
        path = url.pathname.toLowerCase();
      if (!/^\/(api\/|hub)/.test(path)) {
        if (slow && ["Script", "Font", "Image"].includes(resourceType))
          await new Promise((r) => setTimeout(r, 800));
        await cdp.send("Fetch.continueRequest", { requestId });
        return;
      }
      if (slow) await new Promise((r) => setTimeout(r, 1200));
      let response =
        path.startsWith("/api/game/901") ||
        ["/api/team/selector", "/api/admin/users"].includes(path)
          ? player.fixture(url.pathname + url.search, request.method)
          : overview(url.pathname + url.search, request.method);
      if (path.startsWith("/hub"))
        response = { body: { error: "Read-only fixture" } };
      if (
        unavailable &&
        ["/api/game/901", "/api/game/901/ad/scoreboard"].includes(path)
      )
        response = { status: 503, body: { title: "Temporary outage" } };
      if (response.status === 404) unknown.push(path);
      await cdp.send("Fetch.fulfillRequest", {
        requestId,
        responseCode: response.status || 200,
        responseHeaders: [{ name: "Content-Type", value: "application/json" }],
        body: Buffer.from(JSON.stringify(response.body)).toString("base64"),
      });
    })().catch((error) => {
      if (isRetiredInterception(error)) {
        retiredInterceptions.push({
          url: event.request.url,
          networkId: event.networkId,
        });
        return;
      }
      errors.push(String(error));
    });
    pending.add(task);
    task.finally(() => pending.delete(task));
  });
  for (const width of (
    process.env.RSCTF_NAV_LAYOUT_WIDTHS || "3440,1920,1600,1024,390,320"
  )
    .split(",")
    .map(Number)) {
    slow = false;
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      width,
      height: 900,
      deviceScaleFactor: 1,
      mobile: false,
    });
    await cdp.send("Page.navigate", { url: server.origin + "/games" });
    await waitFor(
      `document.querySelector('a[href="/games/901"]') && window.__layoutProbe`,
    );
    await evaluate(axeSource);
    await evaluate("document.fonts.ready");
    await new Promise((r) => setTimeout(r, 300));
    if (!negativeControl) {
      // A genuine input followed by a known shift must fail diagnostics even
      // though standard CLS deliberately excludes it for 500ms.
      await evaluate(`(() => {
        const box = document.createElement('div'); box.id='layout-negative-control';
        box.style.cssText='position:fixed;inset:0;z-index:99999;background:white;color:black';
        box.innerHTML='<button style="width:180px;height:44px">Shift test</button><div id="probe-spacer"></div><p style="height:200px;margin:0;background:#ddd">Layout measurement negative control</p>';
        document.body.append(box);
        box.querySelector('button').onclick=()=>document.getElementById('probe-spacer').style.height='200px';
      })()`);
      await evaluate(
        "new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))",
      );
      const recording = evaluate("window.__layoutProbe.measure(() => {}, 800)");
      await cdp.send("Input.dispatchMouseEvent", {
        type: "mousePressed",
        x: 80,
        y: 20,
        button: "left",
        clickCount: 1,
      });
      await cdp.send("Input.dispatchMouseEvent", {
        type: "mouseReleased",
        x: 80,
        y: 20,
        button: "left",
        clickCount: 1,
      });
      const measured = await recording;
      negativeControl = {
        cls: sessionScore(measured.entries),
        diagnostic: sessionScore(measured.entries, true),
        entries: measured.entries,
      };
      assert.equal(negativeControl.cls, 0);
      assert.ok(
        negativeControl.diagnostic > 0.02,
        "the browser observer must detect the intentional input-adjacent shift",
      );
      await evaluate(
        `document.getElementById('layout-negative-control').remove()`,
      );
      await new Promise((r) => setTimeout(r, 600));
    }
    slow = true;
    const journeys = [
      ["/games/901", false],
      ["/games/901/challenges", false],
      ["/games/901/scoreboard", true],
      ["back", true],
      ["forward", true],
      ["/games/901/attack", true],
      ["back", true, "/games/901/scoreboard"],
      ["forward", true, "/games/901/attack"],
      ["/games/901/scoreboard", true],
      ["/games", false],
      ...(width >= 1024
        ? [
            ["/admin/games", false],
            ["/admin/settings", false],
            ["/admin/users", false],
          ]
        : []),
    ];
    for (const [target, event, historyDestination] of journeys) {
      const action =
        target === "back" || target === "forward"
          ? `history.${target}()`
          : `document.querySelector('a[href="${target}"]').click()`;
      if (!["back", "forward"].includes(target))
        await waitFor(`document.querySelector('a[href="${target}"]')`);
      const result = await evaluate(
        `window.__layoutProbe.measure(() => { ${action} }, ${target.endsWith("/attack") ? 8000 : 4000})`,
      );
      const cls = sessionScore(result.entries),
        diagnostic = sessionScore(result.entries, true);
      const failures = geometryFailures(result.frames, { event });
      const expected =
        historyDestination ??
        (target === "back"
          ? "/games/901/challenges"
          : target === "forward"
            ? "/games/901/scoreboard"
            : target);
      if (
        !result.frames.some((frame) => frame.destination && frame.main) ||
        result.frames.at(-1).path !== expected
      )
        failures.push(
          "destination did not render within the observation window",
        );
      const loadedSelector = {
        "/games": "[data-event-rows] article",
        "/games/901": '[data-guide="event-briefing"]',
        "/games/901/challenges": "[data-challenge-list]",
        "/games/901/scoreboard": '[data-team-name="TCP1P"]',
        "/games/901/attack": '[data-arena-theme="globe"]',
        "/admin/games": "tbody tr",
        "/admin/settings": "input",
        "/admin/users": "tbody tr",
      }[expected];
      if (
        !(await evaluate(
          `Boolean(document.querySelector('#main-content ' + ${JSON.stringify(loadedSelector)}))`,
        ))
      )
        failures.push(
          "destination data did not render within the observation window",
        );
      if (cls > 0.02)
        failures.push(`navigation CLS ${cls.toFixed(4)} exceeds 0.02`);
      if (diagnostic > 0.02)
        failures.push(
          `input-inclusive shift score ${diagnostic.toFixed(4)} exceeds 0.02`,
        );
      const violations = await evaluate(
        `(async () => (await axe.run(document, {runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v => ({id:v.id,targets:v.nodes.map(n=>n.target)})))()`,
      );
      if (violations.length) failures.push("accessibility violations");
      const name = `${width}-${reports.length}-${target.replaceAll("/", "_")}`;
      reports.push({
        name,
        target,
        cls,
        diagnostic,
        failures,
        violations,
        ...result,
      });
      writeFileSync(
        `${output}/${name}.png`,
        Buffer.from(
          (await cdp.send("Page.captureScreenshot", { format: "png" })).data,
          "base64",
        ),
      );
      console.log(name, JSON.stringify({ cls, diagnostic, failures }));
    }
  }
  // A cold-load outage must expose the arena's existing automatic-recovery
  // notice, even when no event DTO can make the shared header ready yet.
  slow = false;
  unavailable = true;
  await cdp.send("Page.navigate", { url: "about:blank" });
  await cdp.send("Page.navigate", { url: server.origin + "/games/901/attack" });
  const arenaHost = `document.querySelector('[data-arena-theme="globe"]')`;
  await waitFor(
    `${arenaHost}?.shadowRoot?.getElementById('connectionStatus')?.textContent === 'Waiting for data'`,
  );
  await waitFor(`getComputedStyle(${arenaHost}).visibility === 'visible'`);
  assert.equal(
    await evaluate(`getComputedStyle(${arenaHost}).visibility`),
    "visible",
    "cold arena outage must leave the recovery notice visible",
  );
  unavailable = false;
  await evaluate(`window.dispatchEvent(new Event('online'))`);
  await waitFor(
    `${arenaHost}?.shadowRoot?.getElementById('connectionStatus')?.textContent === 'Connected' && getComputedStyle(document.querySelector('[data-event-workspace-header]')).visibility === 'visible'`,
  );
  coldRecovery = { noticeVisible: true, recovered: true };
  console.log("PASS: cold arena outage stays visible and recovers");
} finally {
  await Promise.allSettled([...pending]);
  writeFileSync(
    `${output}/report.json`,
    JSON.stringify(
      {
        reports,
        negativeControl,
        errors,
        unknown,
        retiredInterceptions,
        coldRecovery,
      },
      null,
      2,
    ),
  );
  await close();
  await server.close();
}
assert.deepEqual(errors, []);
assert.deepEqual(unknown, []);
assert.deepEqual(
  reports
    .filter((r) => r.failures.length)
    .map(({ name, failures }) => ({ name, failures })),
  [],
);
console.log(`PASS: ${reports.length} slow navigation layout regressions`);
