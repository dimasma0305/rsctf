import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fixture } from "./ad-operations-fixtures.mjs";
import { launchBrowser } from "./cdp.mjs";

const target = process.env.RSCTF_AD_OPS_TARGET || "http://127.0.0.1:63017";
assert.ok(
  ["http://127.0.0.1:63017", "https://intechfest.1pc.tf"].includes(target),
);
const output = resolve(
  process.env.RSCTF_AD_OPS_OUTPUT || "../visual-audit-output/ad-ops-local",
);
const before = process.argv.includes("--before"),
  screensOnly = process.argv.includes("--screens-only");
mkdirSync(output, { recursive: true });
const { cdp, close } = await launchBrowser();
const reports = [],
  writes = [],
  unknown = [],
  errors = [],
  reads = [];
let scenario = "normal";
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
const wait = async (expression) => {
  for (let n = 0; n < 100; n++) {
    if (await evaluate(`Boolean(document.body && (${expression}))`)) return;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(
    "Timed out: " +
      expression +
      "; " +
      (await evaluate("document.body.innerText.slice(-1200)")),
  );
};
const click = async (text) => {
  const q = `Array.from(document.querySelectorAll('button')).find(b=>b.textContent.trim()===${JSON.stringify(text)} && b.getBoundingClientRect().height)`;
  assert.ok(await evaluate(`!!(${q})`), text);
  await evaluate(`${q}.focus();${q}.click()`);
};
const navigate = (suffix) =>
  cdp.send("Page.navigate", {
    url: target + "/admin/games/19/adops?ui-check=" + suffix,
  });
const chooseFilter = async (label, option) => {
  await evaluate(
    `document.getElementById(Array.from(document.querySelectorAll('label')).find(l=>l.textContent.trim()===${JSON.stringify(label)}).htmlFor).click()`,
  );
  await wait(
    `Array.from(document.querySelectorAll('[role=option]')).some(o=>o.textContent.trim()===${JSON.stringify(option)})`,
  );
  await evaluate(
    `Array.from(document.querySelectorAll('[role=option]')).find(o=>o.textContent.trim()===${JSON.stringify(option)}).click()`,
  );
};
const audit = async (name) => {
  await evaluate(
    "Promise.all(document.getAnimations().filter(a=>a.effect?.getComputedTiming().endTime!==Infinity).map(a=>a.finished.catch(()=>{})))",
  );
  await evaluate(readFileSync("node_modules/axe-core/axe.min.js", "utf8"));
  const r = await evaluate(
    `(async()=>({overflow:document.documentElement.scrollWidth>innerWidth+1,headings:document.querySelectorAll('h1').length,violations:(await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>({target:n.target,summary:n.failureSummary}))}))}))()`,
  );
  const shot = await cdp.send("Page.captureScreenshot", {
    format: "png",
    captureBeyondViewport: false,
  });
  writeFileSync(output + "/" + name + ".png", Buffer.from(shot.data, "base64"));
  reports.push({ name, ...r });
  console.log(name, JSON.stringify(r));
};
const chooseMode = async (mode) => {
  await evaluate(
    `document.querySelector('input[type=radio][value=${mode}]').click()`,
  );
  await wait(
    mode === "koth"
      ? `document.body.innerText.includes('Tower of Babel')`
      : `document.body.innerText.includes('Stargazers') && !document.body.innerText.includes('Tower of Babel')`,
  );
};
try {
  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  cdp.on("Runtime.exceptionThrown", ({ exceptionDetails }) =>
    errors.push(
      exceptionDetails.exception?.description || exceptionDetails.text,
    ),
  );
  await cdp.send("Fetch.enable", {
    patterns: [
      { urlPattern: target + "/api/*" },
      { urlPattern: target + "/hub*" },
    ],
  });
  cdp.on("Fetch.requestPaused", async ({ requestId, request }) => {
    const u = new URL(request.url);
    if (!["GET", "HEAD"].includes(request.method))
      writes.push({ path: u.pathname, method: request.method });
    else reads.push(u.pathname);
    const r = fixture(u.pathname + u.search, request.method, scenario);
    if (r.unknown) unknown.push(r.unknown);
    if (r.hold) return;
    await cdp.send("Fetch.fulfillRequest", {
      requestId,
      responseCode: r.status || 200,
      responseHeaders: [{ name: "Content-Type", value: "application/json" }],
      body: Buffer.from(JSON.stringify(r.body)).toString("base64"),
    });
  });
  await cdp.send("Page.addScriptToEvaluateOnNewDocument", {
    source: `for(const id of ['guest','11111111-1111-4111-8111-111111111111'])localStorage.setItem('rsctf-player-guide:'+id,JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true}));`,
  });
  for (const [name, width, height, language, scheme] of [
    ["desktop", 1440, 1100, "en-US", "dark"],
    ["wide", 1920, 1080, "en-US", "dark"],
    ["tablet", 768, 1024, "en-US", "dark"],
    ["mobile", 390, 844, "en-US", "dark"],
    ["compact", 320, 568, "en-US", "dark"],
    ["light-id", 390, 844, "id-ID", "light"],
    ["light-desktop", 1440, 1100, "en-US", "light"],
  ]) {
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      width,
      height,
      deviceScaleFactor: 1,
      mobile: false,
    });
    await cdp.send("Emulation.setEmulatedMedia", {
      features: [
        {
          name: "prefers-reduced-motion",
          value: name === "light-id" ? "reduce" : "no-preference",
        },
      ],
    });
    await cdp.send("Page.addScriptToEvaluateOnNewDocument", {
      source: `localStorage.setItem('language',JSON.stringify(${JSON.stringify(language)}));localStorage.setItem('mantine-color-scheme-value',${JSON.stringify(scheme)});`,
    });
    await navigate(name);
    await wait(`document.body.innerText.includes('Stargazers')`);
    await audit(name + "-ad");
    await chooseMode("koth");
    await audit(name + "-koth");
  }
  if (!before && !screensOnly) {
    await cdp.send("Page.addScriptToEvaluateOnNewDocument", {
      source: `localStorage.setItem('language',JSON.stringify('en-US'));localStorage.setItem('mantine-color-scheme-value','dark');`,
    });
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      width: 390,
      height: 844,
      deviceScaleFactor: 1,
      mobile: false,
    });
    await navigate("filters");
    await wait(`document.querySelector('[data-ad-team]')`);
    assert.equal(
      await evaluate(`document.body.innerText.includes('FIXTURE{')`),
      false,
    );
    const count = reads.length;
    await evaluate(`document.querySelector('input[data-ad-search]').focus()`);
    await cdp.send("Input.insertText", { text: "Stargazers" });
    await wait(`document.querySelectorAll('[data-ad-team]').length===1`);
    assert.equal(reads.length, count, "Filtering does not fetch");
    await audit("mobile-search");
    await click("Clear filters");
    await wait(`document.querySelectorAll('[data-ad-team]').length===6`);
    await chooseFilter("Challenge", "Parcel Panic");
    assert.equal(
      await evaluate(
        `document.querySelector('[data-ad-team]').children.length`,
      ),
      2,
    );
    assert.equal(
      await evaluate(
        `document.querySelectorAll('[data-ad-cell] button[aria-label="Reset container to its base image"]').length`,
      ),
      0,
    );
    await chooseFilter("Teams with", "Services needing attention");
    await wait(`document.querySelectorAll('[data-ad-team]').length===5`);
    await audit("mobile-attention-filter");
    await click("Clear filters");
    await wait(`document.querySelectorAll('[data-ad-team]').length===6`);
    await evaluate(
      `document.querySelector('[data-ad-team]').closest('.mantine-ScrollArea-viewport').scrollIntoView({block:'center'});document.querySelector('[data-ad-team]').closest('.mantine-ScrollArea-viewport').focus()`,
    );
    for (const type of ["keyDown", "keyUp"])
      await cdp.send("Input.dispatchKeyEvent", {
        type,
        key: "ArrowRight",
        windowsVirtualKeyCode: 39,
      });
    await wait(
      `document.querySelector('[data-ad-team]').closest('.mantine-ScrollArea-viewport').scrollLeft>0`,
    );
    await audit("mobile-service-grid");
    await evaluate(
      `document.querySelector('[data-ad-team] button[aria-label="Reset container to its base image"]').click()`,
    );
    await wait(`document.querySelector('[role=dialog]')`);
    await audit("mobile-reset-confirmation");
    await click("Cancel");
    await wait(`!document.querySelector('[role=dialog]')`);
    await evaluate(
      `Array.from(document.querySelectorAll('label')).find(l=>l.textContent==='Show current flags').click()`,
    );
    await wait(`document.body.innerText.includes('FIXTURE{')`);
    await evaluate(
      `Array.from(document.querySelectorAll('label')).find(l=>l.textContent==='Show current flags').click()`,
    );
    await wait(`!document.body.innerText.includes('FIXTURE{')`);
    scenario = "stale";
    await click("Refresh");
    await wait(
      `document.body.innerText.includes('Status could not be refreshed')`,
    );
    await audit("stale-status");
    scenario = "normal";
    await click("Refresh");
    await wait(
      `!document.body.innerText.includes('Status could not be refreshed')`,
    );
    await evaluate(
      `document.querySelector('[data-ad-cell="2"] button[aria-label="Inspect post-game snapshot"]').click()`,
    );
    await wait(
      `document.querySelector('[role=dialog]') && document.body.innerText.includes('config.json')`,
    );
    await audit("mobile-file-inspection");
    await evaluate(
      `document.querySelector('[role=dialog] button[aria-label="Close"]').click()`,
    );
    await wait(`!document.querySelector('[role=dialog]')`);
    await chooseMode("koth");
    await click("Receipts");
    await wait(
      `document.querySelector('[role=dialog]') && document.body.innerText.includes('receipt(s)')`,
    );
    await audit("mobile-koth-receipts");
    await evaluate(
      `document.querySelector('[role=dialog] button[aria-label="Close"]').click()`,
    );
    await wait(`!document.querySelector('[role=dialog]')`);
    for (const state of [
      "paused",
      "ended",
      "upcoming",
      "empty",
      "no-engines",
      "error",
      "loading",
    ]) {
      scenario = state;
      await navigate(state);
      await wait(
        state === "loading"
          ? `document.querySelector('[data-ad-loading]')`
          : state === "error"
            ? `document.querySelector('[data-ad-error]')`
            : state === "no-engines"
              ? `document.body.innerText.includes('No A&D or KotH')`
              : `document.body.innerText.includes(${JSON.stringify(state === "empty" ? "No accepted teams" : "Parcel Panic")})`,
      );
      if (state === "ended")
        assert.ok(
          await evaluate(`document.body.innerText.includes('Event ended')`),
        );
      if (state === "upcoming")
        assert.ok(
          await evaluate(`document.body.innerText.includes('Not started')`),
        );
      await audit(state);
      if (state === "error") {
        scenario = "normal";
        await click("Retry");
        await wait(`document.querySelector('[data-ad-team]')`);
      }
    }
  }
  assert.deepEqual(
    writes,
    [],
    "No service resets, shell sessions, scoring changes, or overrides",
  );
  assert.deepEqual(unknown, []);
  assert.deepEqual(errors, []);
  if (!before)
    assert.deepEqual(
      reports.filter(
        (r) => r.overflow || r.headings !== 1 || r.violations.length,
      ),
      [],
    );
} finally {
  writeFileSync(
    output + "/report.json",
    JSON.stringify(
      { target, fixtures: true, reports, writes, unknown, errors },
      null,
      2,
    ),
  );
  await close();
}
