import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { launchBrowser, isRetiredInterception } from "./cdp.mjs";
import { serveAccountBuild } from "./account-access-fixtures.mjs";
import {
  discoverPageRoutes,
  repositoryRoot,
  viewportCatalog,
} from "./route-catalog.mjs";
import { fixture, player } from "./full-layout-fixtures.mjs";
import { arenaBrowserSetup } from "./attack-arena-fixtures.mjs";
import { pdfFixture } from "./writeup-grading-fixtures.mjs";
import {
  installSurfaceProbe,
  surfaceFailures,
} from "./surface-layout-metrics.mjs";
import { installLayoutProbe, sessionScore } from "./layout-shift-metrics.mjs";

const output = resolve(
  process.env.RSCTF_FULL_LAYOUT_OUTPUT || "visual-audit-output/full-layout",
);
mkdirSync(output, { recursive: true });
const routes = discoverPageRoutes({
  gameId: 901,
  challengeId: 9001,
  postId: "00000001",
});
const selected = routes.filter(
  (r) =>
    !process.env.RSCTF_FULL_LAYOUT_ROUTE ||
    r.path.includes(process.env.RSCTF_FULL_LAYOUT_ROUTE),
);
const views = (
  process.env.RSCTF_FULL_LAYOUT_VIEWPORTS || "wide,desktop,mobile,compact"
).split(",");
assert(selected.length, "route filter must match at least one route");
assert(
  views.every((view) => viewportCatalog[view]),
  "unknown viewport",
);
const server = await serveAccountBuild(
  process.env.RSCTF_FULL_LAYOUT_BUILD_ROOT ||
    resolve(repositoryRoot, "web/build"),
);
const { cdp, close } = await launchBrowser().catch(async (e) => {
  await server.close();
  throw e;
});
const reports = [],
  pending = new Set();
let role = "Admin",
  active,
  script;
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
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
const key = async () => {
  for (const type of ["keyDown", "keyUp"])
    await cdp.send("Input.dispatchKeyEvent", {
      type,
      key: "Escape",
      code: "Escape",
      windowsVirtualKeyCode: 27,
    });
};
const visible = `e=>e.getClientRects().length && getComputedStyle(e).visibility!=='hidden'`;
const axeSource = readFileSync(
  resolve(repositoryRoot, "web/node_modules/axe-core/axe.min.js"),
  "utf8",
);
// Explicit open-only buttons supplement semantic popup triggers. No submit,
// download, import-file picker, destructive action, or menu item is activated.
const dialogButtons = {
  "/account/profile": ["Change Email", "Change Password"],
  "/admin/games": ["New Game"],
  "/admin/repo-bindings": ["Add repository"],
};
async function record(name, result, options = {}) {
  const failures = surfaceFailures(result.frames, options);
  const score = sessionScore(result.entries, true);
  if (score > 0.02)
    failures.push(`input-inclusive shift score ${score.toFixed(5)}`);
  const item = { name, score, failures, ...result };
  active.interactions.push(item);
  writeFileSync(
    `${output}/${active.name}-${name}.png`,
    Buffer.from(
      (await cdp.send("Page.captureScreenshot", { format: "png" })).data,
      "base64",
    ),
  );
  console.log(active.name, name, JSON.stringify({ score, failures }));
}
try {
  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  await cdp.send("Network.enable");
  await cdp.send("Network.setCacheDisabled", { cacheDisabled: true });
  cdp.on("Runtime.exceptionThrown", (e) =>
    active?.errors.push(
      e.exceptionDetails.exception?.description || e.exceptionDetails.text,
    ),
  );
  await cdp.send("Fetch.enable", {
    patterns: [{ urlPattern: server.origin + "/*" }],
  });
  cdp.on("Fetch.requestPaused", (event) => {
    const owner = active;
    const task = (async () => {
      const { requestId, request, resourceType } = event,
        url = new URL(request.url);
      if (url.pathname === "/assets/writeup-fixture.pdf")
        return cdp.send("Fetch.fulfillRequest", {
          requestId,
          responseCode: 200,
          responseHeaders: [{ name: "Content-Type", value: "application/pdf" }],
          body: pdfFixture().toString("base64"),
        });
      if (!/^\/(api\/|hub)/i.test(url.pathname)) {
        if (["Script", "Font", "Image"].includes(resourceType))
          await pause(300);
        return cdp.send("Fetch.continueRequest", { requestId });
      }
      await pause(700);
      const response = fixture(url.pathname + url.search, request.method, role);
      if (response.contract)
        owner?.emptyReads.push({
          path: url.pathname,
          contract: response.contract,
        });
      if (response.status === 404) owner?.unknown.push(url.pathname);
      if (response.status === 405) owner?.writes.push(url.pathname);
      return cdp.send("Fetch.fulfillRequest", {
        requestId,
        responseCode: response.status || 200,
        responseHeaders: [{ name: "Content-Type", value: "application/json" }],
        body: Buffer.from(JSON.stringify(response.body)).toString("base64"),
      });
    })().catch((e) => {
      if (isRetiredInterception(e)) {
        owner?.retiredInterceptions.push({
          url: event.request.url,
          networkId: event.networkId,
        });
        return;
      }
      owner?.errors.push(String(e));
    });
    pending.add(task);
    task.finally(() => pending.delete(task));
  });
  for (const view of views)
    for (const route of selected) {
      await Promise.allSettled([...pending]);
      active = {
        name: `${view}-${route.name}`,
        route: route.path,
        view,
        errors: [],
        retiredInterceptions: [],
        unknown: [],
        emptyReads: [],
        writes: [],
        interactions: [],
        failures: [],
      };
      reports.push(active);
      role =
        route.auth === "anonymous" && route.path.startsWith("/account/")
          ? "Guest"
          : "Admin";
      await cdp.send("Emulation.setDeviceMetricsOverride", {
        ...viewportCatalog[view],
        deviceScaleFactor: 1,
        mobile: false,
      });
      if (script)
        await cdp.send("Page.removeScriptToEvaluateOnNewDocument", {
          identifier: script,
        });
      script = (
        await cdp.send("Page.addScriptToEvaluateOnNewDocument", {
          source: `if(location.origin===${JSON.stringify(server.origin)}) {localStorage.clear(); ${arenaBrowserSetup}; localStorage.setItem('rsctf-player-guide:${player.profile.userId}',JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[]})); localStorage.setItem('challenge-explorer-view',JSON.stringify('list')); Object.defineProperty(navigator,'connection',{value:{saveData:true,effectiveType:'3g'}}); (${installLayoutProbe.toString()})(); (${installSurfaceProbe.toString()})();}`,
        })
      ).identifier;
      try {
        await cdp.send("Page.navigate", { url: server.origin + route.urlPath });
        for (let n = 0; n < 100; n++) {
          if (
            await evaluate(
              'Boolean(window.__layoutProbe && document.querySelector("#main-content h1"))',
            )
          )
            break;
          await pause(100);
        }
        const loading = await evaluate(
          "window.__layoutProbe.measure(()=>{},3000)",
        );
        loading.entries = await evaluate("window.__layoutProbe.entries");
        active.loading = loading;
        active.score = sessionScore(loading.entries, true);
        if (active.score > 0.02)
          active.failures.push(
            `loading shift score ${active.score.toFixed(5)}`,
          );
        if ((await evaluate("location.pathname")) !== route.expectedPath)
          active.failures.push("unexpected redirect");
        if (
          !(await evaluate(
            'Boolean(document.querySelector("#main-content h1"))',
          ))
        )
          active.failures.push("missing page content");
        if (
          await evaluate(
            'Boolean(document.querySelector("[data-request-error], [data-error-fallback]"))',
          )
        )
          active.failures.push("rendered request error");
        await evaluate("document.fonts.ready");
        await evaluate(axeSource);
        writeFileSync(
          `${output}/${active.name}.png`,
          Buffer.from(
            (await cdp.send("Page.captureScreenshot", { format: "png" })).data,
            "base64",
          ),
        );
        // Scan every rendered popup trigger in page content. Open only; never choose
        // an option, submit a form, or confirm a mutation. Escape closes each surface.
        const selectors = await evaluate(
          `(() => { const seen=new Set(), labels=${JSON.stringify(dialogButtons[route.path] || [])}; const all=[...document.querySelectorAll('[data-page-content] [aria-haspopup], [data-workspace-bar] button, #main-content button')].filter(${visible}); return all.filter(e=>!e.disabled && (e.hasAttribute('aria-haspopup') || /search pages/i.test(e.getAttribute('aria-label')||e.textContent) || labels.includes(e.textContent.trim()) || ['team-join','team-create'].includes(e.getAttribute('data-guide')))).filter(e=>{const key=[e.tagName,e.getAttribute('aria-haspopup'),e.getAttribute('aria-label')||e.textContent.trim()].join(':');if(seen.has(key))return false;seen.add(key);return true}).map((e,i)=>{e.setAttribute('data-layout-scan-trigger',String(i)); return '[data-layout-scan-trigger="'+i+'"]'}) })()`,
        );
        active.discoveredPopups = selectors.length;
        active.triggers = await evaluate(
          `Array.from(document.querySelectorAll('[data-layout-scan-trigger]'), e=>({label:e.getAttribute('aria-label')||e.getAttribute('placeholder')||e.textContent.trim(),html:e.outerHTML}))`,
        );
        active.excludedSurfaces = [];
        for (
          let i = 0;
          process.env.RSCTF_FULL_LAYOUT_POPUPS !== "0" && i < selectors.length;
          i++
        ) {
          const selector = selectors[i];
          if (
            route.path === "/posts/00000001/edit" &&
            (await evaluate(
              `document.querySelector(${JSON.stringify(selector)}).classList.contains('mantine-TagsInput-inputField')`,
            ))
          ) {
            active.excludedSurfaces.push({
              selector,
              reason:
                "Tag editor has no unselected suggestions in this fixture; text editing is covered by post tests.",
            });
            continue;
          }
          await evaluate(
            `document.querySelector(${JSON.stringify(selector)}).scrollIntoView({block:'center'})`,
          );
          await pause(100);
          const opened = await evaluate(
            `__surfaceProbe(()=>{const e=document.querySelector(${JSON.stringify(selector)});e.focus({preventScroll:true});e.click();if(location.pathname.endsWith('/monitor/cheatcheck') && e instanceof HTMLInputElement) {Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(e,'@');e.dispatchEvent(new Event('input',{bubbles:true}));}})`,
          );
          await record(`popup-${i}-open`, opened, { popup: true });
          const datePickerOverflow = await evaluate(
            `(() => { const e=[...document.querySelectorAll('[data-dates-dropdown]')].find(${visible}); if(!e)return false; const r=e.getBoundingClientRect(); return r.top < 0 || r.bottom > innerHeight || r.left < 0 || r.right > innerWidth; })()`,
          );
          if (datePickerOverflow)
            active.failures.push(`popup ${i} date picker extends outside viewport`);
          const dialog = await evaluate(
            `Boolean([...document.querySelectorAll('[role="dialog"]')].find(${visible}))`,
          );
          const modal = await evaluate(
            `Boolean([...document.querySelectorAll('[role="dialog"][aria-modal="true"]')].find(${visible}))`,
          );
          if (dialog) {
            const violations = await evaluate(
              `(async()=> (await axe.run(document.querySelector('[role="dialog"]'),{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,targets:v.nodes.map(n=>n.target)})))()`,
            );
            if (violations.length)
              active.failures.push(
                `popup ${i} accessibility: ${JSON.stringify(violations)}`,
              );
            if (
              modal &&
              !(await evaluate(
                `Boolean(document.activeElement?.closest('[role="dialog"]'))`,
              ))
            )
              active.failures.push(`popup ${i} did not receive focus`);
            if (modal) {
              const beforeScroll = await evaluate(
                '[scrollY,document.querySelector("#main-content")?.scrollTop||0]',
              );
              await cdp.send("Input.dispatchMouseEvent", {
                type: "mouseWheel",
                x: 5,
                y: 100,
                deltaX: 0,
                deltaY: 300,
              });
              await pause(100);
              if (
                modal &&
                JSON.stringify(beforeScroll) !==
                  JSON.stringify(
                    await evaluate(
                      '[scrollY,document.querySelector("#main-content")?.scrollTop||0]',
                    ),
                  )
              )
                active.failures.push(
                  `popup ${i} did not lock background scrolling`,
                );
            }
          }
          const closing = evaluate("__surfaceProbe(()=>{})");
          await key();
          await record(`popup-${i}-close`, await closing, { closing: true });
          if (
            modal &&
            !(await evaluate(
              `document.activeElement===document.querySelector(${JSON.stringify(selector)})`,
            ))
          )
            active.failures.push(`popup ${i} did not restore focus`);
        }
        if (
          process.env.RSCTF_FULL_LAYOUT_POPUPS !== "0" &&
          route.path === "/games/901/scoreboard" &&
          viewportCatalog[view].width >= 1024
        ) {
          // The bonus legend is a representative Mantine tooltip; hover only.
          const point = await evaluate(
            `(() => {const e=[...document.querySelectorAll('#main-content p')].find(e=>e.textContent.trim()==='+0%');if(!e)throw new Error('Missing bonus tooltip fixture');e.scrollIntoView({block:'center'});const r=e.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`,
          );
          await pause(100);
          const opening = evaluate("__surfaceProbe(()=>{})");
          await cdp.send("Input.dispatchMouseEvent", {
            type: "mouseMoved",
            ...point,
          });
          await record("bonus-tooltip-open", await opening, { popup: true });
          const closing = evaluate("__surfaceProbe(()=>{})");
          await cdp.send("Input.dispatchMouseEvent", {
            type: "mouseMoved",
            x: 0,
            y: 0,
          });
          await record("bonus-tooltip-close", await closing, { closing: true });
        }
      } catch (e) {
        active.failures.push(String(e));
        if (String(e).includes("CDP timed out")) throw e;
      }
      writeFileSync(
        `${output}/${active.name}.json`,
        JSON.stringify(active, null, 2),
      );
      console.log(
        active.name,
        JSON.stringify({
          score: active.score,
          failures: active.failures,
          unknown: active.unknown,
          errors: active.errors,
        }),
      );
    }
} finally {
  await Promise.allSettled([...pending]);
  await close();
  await server.close();
  writeFileSync(
    `${output}/report.json`,
    JSON.stringify(
      {
        inventory: routes.map((r) => ({ source: r.sourceFile, path: r.path })),
        selected: selected.length,
        viewports: views,
        popupsEnabled: process.env.RSCTF_FULL_LAYOUT_POPUPS !== "0",
        reports,
      },
      null,
      2,
    ),
  );
}
const failures = reports.filter(
  (r) =>
    r.failures.length ||
    r.errors.length ||
    r.unknown.length ||
    r.writes.length ||
    r.interactions.some((i) => i.failures.length),
);
console.log(`${reports.length} route scans; ${failures.length} failed`);
assert.equal(
  failures.length,
  0,
  "see full-layout report for route, popup and fixture gaps",
);
