// Real Chromium, built client, and invented API data. No application API is contacted.
import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { launchBrowser } from "./cdp.mjs";
import { serveAccountBuild } from "./account-access-fixtures.mjs";
import { fixture, guideSetup } from "./overview-pages-fixtures.mjs";
import { repositoryRoot } from "./route-catalog.mjs";

const output = resolve(
  process.env.RSCTF_TEAM_ACCESS_OUTPUT ||
    resolve(repositoryRoot, "visual-audit-output/team-access"),
);
mkdirSync(output, { recursive: true });
const server = await serveAccountBuild(
  process.env.RSCTF_TEAM_ACCESS_BUILD_ROOT ||
    resolve(repositoryRoot, "web/build"),
);
const browser = await launchBrowser().catch(async (error) => {
  await server.close();
  throw error;
});
const { cdp } = browser;
const results = [],
  exceptions = [],
  unknown = [],
  errors = [],
  pending = new Set();
let state;
const code = "team:7:01234567890123456789012345678901";
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const evaluate = async (expression) => {
  const r = await cdp.send("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (r.exceptionDetails)
    throw new Error(
      r.exceptionDetails.exception?.description || r.exceptionDetails.text,
    );
  return r.result?.value;
};
const until = async (read, description) => {
  for (let n = 0; n < 80; n++) {
    if (await read()) return;
    await pause(100);
  }
  throw new Error("Timed out: " + description);
};
const wait = (expression) =>
  until(
    () => evaluate(`Boolean(document.body && (${expression}))`),
    expression,
  );
const click = async (selector) => {
  await wait(`document.querySelector(${JSON.stringify(selector)})`);
  await evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
};
const fill = async (selector, text) => {
  await evaluate(
    `document.querySelector(${JSON.stringify(selector)}).focus(); document.querySelector(${JSON.stringify(selector)}).select()`,
  );
  await cdp.send("Input.insertText", { text });
};
const key = async (name) => {
  for (const type of ["keyDown", "keyUp"])
    await cdp.send("Input.dispatchKeyEvent", {
      type,
      key: name,
      code: name,
      windowsVirtualKeyCode: name === "Escape" ? 27 : 13,
    });
};
const snapshot = async (name) => {
  const r = await cdp.send("Page.captureScreenshot", {
    format: "png",
    captureBeyondViewport: false,
  });
  writeFileSync(resolve(output, name + ".png"), Buffer.from(r.data, "base64"));
};
const audit = async (name) => {
  await evaluate(
    readFileSync(
      resolve(repositoryRoot, "web/node_modules/axe-core/axe.min.js"),
      "utf8",
    ),
  );
  const violations = await evaluate(
    `(async()=> (await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,targets:v.nodes.map(n=>n.target)})))()`,
  );
  assert.deepEqual(violations, [], "WCAG violations");
  const labels = await evaluate(
    `(async()=> (await axe.run(document,{runOnly:{type:'rule',values:['label-content-name-mismatch']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>({target:n.target,html:n.html,summary:n.failureSummary}))})))()`,
  );
  assert.deepEqual(labels, [], "Visible labels match accessible names");
  assert.equal(
    await evaluate(
      "document.documentElement.scrollWidth > document.documentElement.clientWidth + 1",
    ),
    false,
    "Overflow",
  );
  await snapshot(name);
};
const release = () => {
  state?.releaseJoin?.();
  state?.releaseRecovery?.();
};
const visit = async (path, options = {}) => {
  release();
  state = { options, joins: 0, recoveries: 0 };
  await cdp.send("Page.navigate", { url: "about:blank" });
  await cdp.send("Page.navigate", { url: server.origin + path });
  await wait('document.querySelector("#main-content h1")');
  await evaluate("document.fonts.ready");
};
const check = async (name, action) => {
  try {
    await action();
    results.push({ name, status: "passed" });
    console.log("PASS", name);
  } catch (error) {
    await snapshot(name + "-failed").catch(() => {});
    results.push({ name, status: "failed", error: error.message });
    console.error("FAIL", name, error.message);
  } finally {
    release();
  }
};
try {
  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  await cdp.send("Emulation.setEmulatedMedia", {
    features: [{ name: "prefers-reduced-motion", value: "reduce" }],
  });
  cdp.on("Runtime.exceptionThrown", (r) =>
    exceptions.push(
      r.exceptionDetails.exception?.description || r.exceptionDetails.text,
    ),
  );
  await cdp.send("Page.addScriptToEvaluateOnNewDocument", {
    source: `if(location.origin===${JSON.stringify(server.origin)}){${guideSetup};localStorage.setItem('i18nextLng','en-US');}`,
  });
  await cdp.send("Fetch.enable", {
    patterns: [
      { urlPattern: server.origin + "/api/*" },
      { urlPattern: server.origin + "/hub*" },
    ],
  });
  cdp.on("Fetch.requestPaused", ({ requestId, request }) => {
    const task = (async () => {
      const owner = state,
        url = new URL(request.url);
      let r;
      if (url.pathname === "/api/team" && request.method === "GET")
        r = { body: [] };
      else if (
        url.pathname === "/api/team/accept" &&
        request.method === "POST"
      ) {
        owner.joins++;
        if (owner.options.holdJoin)
          await new Promise((resolve) => {
            owner.releaseJoin = resolve;
          });
        r = owner.options.rejectJoin
          ? { status: 409, body: { title: "Invite expired", status: 409 } }
          : { body: { status: 200 } };
      } else if (
        url.pathname === "/api/account/recovery" &&
        request.method === "POST"
      ) {
        owner.recoveries++;
        await new Promise((resolve) => {
          owner.releaseRecovery = resolve;
        });
        r = { body: { status: 200 } };
      } else if (url.pathname === "/api/config")
        r = {
          body: {
            title: "Regression fixture",
            slogan: "Capture the flag",
            allowRegister: true,
            allowPasswordRegistration: true,
            allowTeamCreation: true,
            enableBrowserFingerprint: false,
            apiPublicKey: null,
            portMapping: "Default",
          },
        };
      else if (url.pathname === "/api/captcha") r = { body: { type: "None" } };
      else
        r = fixture(
          url.pathname + url.search,
          request.method,
          "normal",
          owner?.options.guest ? "Guest" : "User",
        );
      if (
        (r.status === 404 && !url.pathname.startsWith("/hub")) ||
        r.status === 405
      )
        unknown.push({ method: request.method, path: url.pathname });
      await cdp.send("Fetch.fulfillRequest", {
        requestId,
        responseCode: r.status || 200,
        responseHeaders: [{ name: "Content-Type", value: "application/json" }],
        body: Buffer.from(JSON.stringify(r.body)).toString("base64"),
      });
    })()
      .catch((error) => {
        if (!/Invalid (InterceptionId|request id)/i.test(error.message))
          errors.push(error.message);
      })
      .finally(() => pending.delete(task));
    pending.add(task);
  });
  for (const [name, width, height] of [
    ["desktop", 1440, 1100],
    ["tablet", 768, 1024],
    ["mobile", 390, 844],
    ["compact", 320, 568],
  ]) {
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      width,
      height,
      deviceScaleFactor: 1,
      mobile: width <= 390,
    });
    await check(name + "-invite-survives-until-dialog-closes", async () => {
      await visit("/teams?join=" + encodeURIComponent(code) + "&source=invite");
      await wait('document.querySelector("[role=dialog] input")');
      assert.equal(
        await evaluate('document.querySelector("[role=dialog] input").value'),
        code,
      );
      await pause(200);
      assert.equal(
        await evaluate('new URLSearchParams(location.search).get("join")'),
        code,
      );
      await audit(name + "-valid-invite");
      await key("Escape");
      await wait('!document.querySelector("[role=dialog]")');
      assert.equal(
        await evaluate('new URLSearchParams(location.search).get("source")'),
        "invite",
      );
      assert.equal(
        await evaluate('new URLSearchParams(location.search).has("join")'),
        false,
      );
      await audit(name + "-invite-closed");
    });
    await check(
      name + "-malformed-invite-is-editable-without-page-crash",
      async () => {
        await visit("/teams?join=%25");
        await wait('document.querySelector("[role=dialog] input")');
        assert.equal(
          await evaluate(
            'Boolean(document.querySelector("[data-error-fallback]"))',
          ),
          false,
        );
        assert.equal(
          await evaluate('document.querySelector("[role=dialog] input").value'),
          "%",
        );
        assert.equal(
          await evaluate(
            'document.querySelector("[data-guide=team-join-submit]").disabled',
          ),
          true,
        );
        await audit(name + "-invalid-invite");
        await fill("[role=dialog] input", code);
        await wait(
          '!document.querySelector("[data-guide=team-join-submit]").disabled',
        );
        await click("[data-guide=team-join-submit]");
        await wait('!document.querySelector("[role=dialog]")');
        assert.equal(state.joins, 1);
      },
    );
    await check(name + "-pending-join-owns-its-dialog-and-code", async () => {
      await visit("/teams?join=" + encodeURIComponent(code), {
        holdJoin: true,
      });
      await click("[data-guide=team-join-submit]");
      await until(() => state.joins === 1, "join request");
      await key("Escape");
      assert.equal(
        await evaluate('Boolean(document.querySelector("[role=dialog]"))'),
        true,
      );
      assert.equal(
        await evaluate(
          'document.querySelector("[role=dialog] input").disabled',
        ),
        true,
      );
      assert.equal(
        await evaluate(
          'Boolean(document.querySelector("[role=dialog] button[aria-label=Close]"))',
        ),
        false,
      );
      await click("[data-guide=team-join-submit]");
      assert.equal(state.joins, 1);
      state.releaseJoin();
      await wait('!document.querySelector("[role=dialog]")');
      await wait(
        'document.body.innerText.includes("Successfully joined team")',
      );
      assert.equal(
        await evaluate('new URLSearchParams(location.search).has("join")'),
        false,
      );
    });
    await check(
      name + "-leaving-pending-join-fences-late-success",
      async () => {
        await visit("/", { holdJoin: true });
        await click('#main-content a[href="/teams"]');
        await wait('location.pathname === "/teams"');
        await click("[data-guide=team-join]");
        await wait('document.querySelector("[role=dialog] input")');
        await fill("[role=dialog] input", code);
        await click("[data-guide=team-join-submit]");
        await until(() => state.joins === 1, "pending join request");
        const history = await cdp.send("Page.getNavigationHistory");
        await cdp.send("Page.navigateToHistoryEntry", {
          entryId: history.entries[history.currentIndex - 1].id,
        });
        await wait('location.pathname === "/"');
        state.releaseJoin();
        await wait('!document.querySelector("[role=dialog]")');
        await pause(300);
        assert.equal(
          await evaluate(
            'document.body.innerText.includes("Successfully joined team")',
          ),
          false,
        );
        assert.equal(await evaluate("location.pathname"), "/");
      },
    );
    await check(name + "-rejected-join-keeps-code-and-can-retry", async () => {
      await visit("/teams?join=" + encodeURIComponent(code), {
        rejectJoin: true,
      });
      await click("[data-guide=team-join-submit]");
      await wait('document.body.innerText.includes("Invite expired")');
      assert.equal(
        await evaluate('document.querySelector("[role=dialog] input").value'),
        code,
      );
      state.options.rejectJoin = false;
      await wait(
        '!document.querySelector("[data-guide=team-join-submit]").disabled',
      );
      await click("[data-guide=team-join-submit]");
      await wait('!document.querySelector("[role=dialog]")');
      assert.equal(state.joins, 2);
    });
    for (const unmounted of [false, true])
      await check(
        name + "-abandoned-recovery-clears-status-" + unmounted,
        async () => {
          await visit("/account/recovery", { guest: true });
          await wait('document.querySelector("input[type=email]")');
          await fill("input[type=email]", "fixture@example.invalid");
          await click("button[type=submit]");
          await until(() => state.recoveries === 1, "recovery request");
          await click('#main-content a[href="/"]');
          await wait('location.pathname === "/"');
          if (unmounted) {
            await wait('!document.querySelector("input[type=email]")');
            await pause(200);
          }
          state.releaseRecovery();
          await wait('!document.querySelector("input[type=email]")');
          await pause(250);
          assert.equal(
            await evaluate(
              'document.querySelectorAll(".mantine-Notification-root").length',
            ),
            0,
          );
          await audit(name + "-recovery-retired-" + unmounted);
        },
      );
  }
} finally {
  release();
  await Promise.allSettled([...pending]);
  await browser.close();
  await server.close();
  const report = {
    results,
    exceptions,
    unknown,
    errors,
    unhandled: server.unhandled,
  };
  writeFileSync(
    resolve(output, "report.json"),
    JSON.stringify(report, null, 2),
  );
  console.log(
    JSON.stringify({
      passed: results.filter((r) => r.status === "passed").length,
      failed: results.filter((r) => r.status === "failed").length,
      exceptions,
      unknown,
      errors,
      unhandled: server.unhandled,
    }),
  );
  if (
    results.some((r) => r.status === "failed") ||
    exceptions.length ||
    unknown.length ||
    errors.length ||
    server.unhandled.length
  )
    process.exitCode = 1;
}
