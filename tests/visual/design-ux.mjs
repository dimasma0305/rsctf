// Production client in Chromium, with all APIs handled by invented fixtures.
import assert from "node:assert/strict";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { launchBrowser } from "./cdp.mjs";
import { serveAccountBuild } from "./account-access-fixtures.mjs";
import { createDesignFixture, guideSetup } from "./design-ux-fixtures.mjs";
import { repositoryRoot } from "./route-catalog.mjs";

const output = resolve(
  process.env.RSCTF_DESIGN_OUTPUT ||
    resolve(repositoryRoot, "visual-audit-output/design-ux"),
);
mkdirSync(output, { recursive: true });
const server = await serveAccountBuild(
  process.env.RSCTF_DESIGN_BUILD_ROOT || resolve(repositoryRoot, "web/build"),
);
const browser = await launchBrowser().catch(async (error) => {
  await server.close();
  throw error;
});
const { cdp } = browser;
const reports = [],
  exceptions = [],
  interceptionErrors = [],
  owners = [],
  pending = new Set();
let state, documentScript, viewport;
const baseline = process.argv.includes("--baseline");
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
const evaluate = async (expression) => {
  const result = await cdp.send("Runtime.evaluate", {
    expression,
    awaitPromise: true,
    returnByValue: true,
  });
  if (result.exceptionDetails)
    throw new Error(
      result.exceptionDetails.exception?.description ||
        result.exceptionDetails.text,
    );
  return result.result?.value;
};
const wait = async (expression) => {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (await evaluate(`Boolean(document.body && (${expression}))`)) return;
    await pause(100);
  }
  const active = await evaluate(
    `({tag:document.activeElement?.tagName,name:document.activeElement?.textContent?.slice(0,120),owner:document.activeElement?.closest('[data-user-actions]')?.getAttribute('data-user-actions')})`,
  );
  throw new Error(
    "Timed out: " + expression + "; active=" + JSON.stringify(active),
  );
};
const visible = `element => element.getClientRects().length && getComputedStyle(element).visibility !== 'hidden'`;
const find = (selector) =>
  `[...document.querySelectorAll(${JSON.stringify(selector)})].find(${visible})`;
const click = async (selector) => {
  await wait(find(selector));
  await evaluate(`${find(selector)}.scrollIntoView({block:"nearest"})`);
  await evaluate(`${find(selector)}.focus(); ${find(selector)}.click()`);
};
const clickText = async (selector, text) => {
  const expression = `[...document.querySelectorAll(${JSON.stringify(selector)})].filter(${visible}).find(element=>element.textContent.trim()===${JSON.stringify(text)})`;
  await wait(expression);
  await evaluate(`${expression}.click()`);
};
const press = async (key) => {
  for (const type of ["keyDown", "keyUp"])
    await cdp.send("Input.dispatchKeyEvent", {
      type,
      key,
      code: key,
      windowsVirtualKeyCode: { Escape: 27, Enter: 13, Tab: 9, ArrowDown: 40 }[
        key
      ],
      ...(key === "Enter" && type === "keyDown" ? { text: "\r" } : {}),
    });
};
// Wait for the specific popup to own focus before sending keyboard events.
// DOM insertion precedes Mantine's focus trap, and an exiting menu may still
// contain matching labels while another entity's menu is opening.
const openUserMenu = async (group) => {
  const trigger = group + " button:last-child";
  await click(trigger);
  await wait(
    `(() => { const target = ${find(trigger)}; const menu = document.getElementById(target?.getAttribute('aria-controls')); return target?.getAttribute('aria-expanded') === 'true' && menu?.contains(document.activeElement); })()`,
  );
  const id = await evaluate(`${find(trigger)}.getAttribute('aria-controls')`);
  return `[id="${id}"]`;
};
const chooseUserAction = async (group, label) => {
  const menu = await openUserMenu(group);
  await clickText(menu + " [role=menuitem]", label);
};
const snapshot = async (name) => {
  const shot = await cdp.send("Page.captureScreenshot", {
    format: "png",
    captureBeyondViewport: false,
  });
  writeFileSync(
    resolve(output, name + ".png"),
    Buffer.from(shot.data, "base64"),
  );
};
const audit = async (name) => {
  await evaluate("document.fonts.ready");
  await evaluate(
    readFileSync(
      resolve(repositoryRoot, "web/node_modules/axe-core/axe.min.js"),
      "utf8",
    ),
  );
  const result =
    await evaluate(`(async()=>({overflow:document.documentElement.scrollWidth>innerWidth+1,headings:document.querySelectorAll('h1').length,
    violations:(await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,targets:v.nodes.map(n=>n.target)})),
    labels:(await axe.run(document,{runOnly:{type:'rule',values:['label-content-name-mismatch']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>({target:n.target,html:n.html}))}))}))()`);
  assert.equal(result.overflow, false, "horizontal overflow");
  assert.equal(result.headings, 1, "one page heading");
  assert.deepEqual(result.violations, [], "WCAG violations");
  assert.deepEqual(
    result.labels,
    [],
    "visible names must match accessible names",
  );
  assert.equal(
    await evaluate('document.body.innerText.includes("interface.")'),
    false,
    "untranslated interface key",
  );
  await snapshot(name);
};
const visit = async (path, options = {}) => {
  state?.release();
  state = createDesignFixture(options);
  owners.push(state);
  if (documentScript)
    await cdp.send("Page.removeScriptToEvaluateOnNewDocument", {
      identifier: documentScript,
    });
  const setup = `if(window===window.top && location.origin===${JSON.stringify(server.origin)}){${guideSetup};localStorage.setItem('language',JSON.stringify(${JSON.stringify(options.language || "en-US")}));localStorage.setItem('mantine-color-scheme-value',${JSON.stringify(options.scheme || "dark")});${options.preserveView ? "" : "localStorage.removeItem('challenge-explorer-view');"}}`;
  documentScript = (
    await cdp.send("Page.addScriptToEvaluateOnNewDocument", { source: setup })
  ).identifier;
  await cdp.send("Page.navigate", { url: "about:blank" });
  await cdp.send("Page.navigate", { url: server.origin + path });
  await wait('document.querySelector("#main-content h1")');
  await pause(600);
  await evaluate("document.fonts.ready");
};
const check = async (name, action) => {
  const entry = { name: viewport.name + "-" + name };
  if (
    process.env.RSCTF_DESIGN_CASE &&
    !entry.name.includes(process.env.RSCTF_DESIGN_CASE)
  )
    return;
  try {
    entry.measurements = await action();
    entry.status = "passed";
    console.log("PASS", entry.name);
  } catch (error) {
    entry.status = "failed";
    entry.error = error.message;
    entry.diagnostics = {
      writes: state?.writes,
      active: await evaluate(
        `({name:document.activeElement?.textContent, owner:document.activeElement?.closest('[data-user-actions]')?.getAttribute('data-user-actions'), menus:[...document.querySelectorAll('[role=menu]')].map(menu=>({id:menu.id,opacity:getComputedStyle(menu).opacity}))})`,
      ).catch(() => null),
    };
    await snapshot(entry.name + "-failed").catch(() => {});
    console.error("FAIL", entry.name, error.message);
  } finally {
    reports.push(entry);
    state?.release();
  }
};
const top = (selector) =>
  evaluate(`${find(selector)}?.getBoundingClientRect().top`);
const noLoading = async () =>
  assert.equal(
    await evaluate(`Boolean(${find(".mantine-LoadingOverlay-root")})`),
    false,
  );

try {
  await cdp.send("Page.enable");
  await cdp.send("Runtime.enable");
  await cdp.send("Emulation.setEmulatedMedia", {
    features: [{ name: "prefers-reduced-motion", value: "reduce" }],
  });
  cdp.on("Runtime.exceptionThrown", (event) =>
    exceptions.push(
      event.exceptionDetails.exception?.description ||
        event.exceptionDetails.text,
    ),
  );
  await cdp.send("Fetch.enable", {
    patterns: [
      { urlPattern: server.origin + "/api/*" },
      { urlPattern: server.origin + "/hub*" },
    ],
  });
  cdp.on("Fetch.requestPaused", (event) => {
    const owner = state;
    const task = (async () => {
      const response = await owner.fixture(event.request);
      await cdp.send("Fetch.fulfillRequest", {
        requestId: event.requestId,
        responseCode: response.status || 200,
        responseHeaders: [{ name: "Content-Type", value: "application/json" }],
        body: Buffer.from(JSON.stringify(response.body)).toString("base64"),
      });
    })()
      .catch((error) => {
        if (!/Invalid (InterceptionId|request id)/i.test(error.message))
          interceptionErrors.push(error.message);
      })
      .finally(() => pending.delete(task));
    pending.add(task);
  });
  for (viewport of [
    { name: "desktop", width: 1440, height: 1100 },
    { name: "tablet", width: 768, height: 1024 },
    { name: "mobile", width: 390, height: 844 },
    { name: "compact", width: 320, height: 568 },
  ]) {
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      ...viewport,
      name: undefined,
      deviceScaleFactor: 1,
      mobile: viewport.width <= 390,
    });
    for (const failed of [
      "/api/admin/dashboard",
      "/api/admin/submissiontrend",
      "/api/admin/reviews",
    ])
      await check("failed-" + failed.split("/").at(-1), async () => {
        await visit("/admin/dashboard", { failed: [failed] });
        assert.ok(
          await evaluate(`Boolean(${find("[data-request-error]")})`),
          "failed reads need an inline error",
        );
        await noLoading();
        if (failed.endsWith("/dashboard"))
          assert.equal(
            await evaluate(
              'document.querySelector("[data-dashboard-stats]").innerText.includes("0")',
            ),
            false,
            "unknown totals must not become zero",
          );
        if (failed.endsWith("/submissiontrend"))
          assert.equal(
            await evaluate(
              'document.querySelector("#main-content").innerText.includes("No submission activity")',
            ),
            false,
          );
        if (failed.endsWith("/reviews"))
          assert.equal(
            await evaluate(
              'document.querySelector("[role=tabpanel]").innerText.includes("No data")',
            ),
            false,
          );
        assert.equal(
          await evaluate(
            'document.querySelectorAll(".mantine-Notification-root").length',
          ),
          0,
          "one read failure must not create repeated toasts",
        );
        await audit(viewport.name + "-failed-" + failed.split("/").at(-1));
        state.failed.clear();
        await click("[data-request-error] button");
        await wait('!document.querySelector("[data-request-error]")');
        if (failed.endsWith("/dashboard"))
          await wait(
            'document.querySelector("[data-dashboard-stats]").innerText.includes("24")',
          );
      });
    await check("settings-error-and-retry", async () => {
      await visit("/admin/settings", { failed: ["/api/admin/config"] });
      await noLoading();
      assert.ok(await evaluate(`Boolean(${find("[data-request-error]")})`));
      assert.equal(
        await evaluate('Boolean(document.querySelector("#settings-panel"))'),
        false,
        "unloaded configuration must not appear as editable defaults",
      );
      await audit(viewport.name + "-settings-error");
      state.failed.clear();
      await click("[data-request-error] button");
      await wait('document.querySelector("#settings-panel input")');
      await noLoading();
    });
    await check("mobile-content-and-pinned-notice", async () => {
      await visit("/");
      if (viewport.width <= 390) {
        const pinnedTop =
          (await top("[data-home-pinned]")) ??
          (await top("[data-post-card][data-pinned]"));
        assert.ok(
          pinnedTop < 300,
          `pinned announcement begins at ${pinnedTop}`,
        );
        await audit(viewport.name + "-home");
      }
      await visit("/admin/settings");
      if (viewport.width <= 390) {
        const fieldTop = await top("#settings-panel input");
        assert.ok(
          fieldTop < viewport.height - 90,
          `first settings field begins at ${fieldTop}`,
        );
      }
      await audit(viewport.name + "-settings");
      const fieldTop = await top("#settings-panel input");
      await visit("/admin/users");
      if (viewport.width <= 390)
        assert.ok(
          (await top("main article h2")) < viewport.height - 90,
          "first user must be visible above the dock",
        );
      await audit(viewport.name + "-users");
      return {
        settingsFieldTop: fieldTop,
        firstUserTop: await top("main article h2"),
      };
    });
    if (baseline) continue;
    await check("stale-data-stays-visible", async () => {
      await visit("/admin/dashboard");
      await wait(
        'document.querySelector("[data-dashboard-stats]").innerText.includes("24")',
      );
      state.failed.add("/api/admin/dashboard");
      await click('button[aria-label="Refresh dashboard"]');
      await wait('document.querySelector("[data-request-error]")');
      assert.ok(
        await evaluate(
          'document.querySelector("[data-dashboard-stats]").innerText.includes("24")',
        ),
      );
      assert.ok(
        await evaluate(
          'document.querySelector("[data-request-error]").innerText.includes("last loaded data")',
        ),
      );
    });
    await check("settings-navigation-keeps-draft", async () => {
      await visit("/admin/settings");
      const input = '#settings-panel input[placeholder="RS"]';
      await click(input);
      await evaluate(`${find(input)}.select()`);
      await cdp.send("Input.insertText", { text: "Draft retained" });
      if (viewport.width <= 992) {
        await click("#settings-section-picker");
        await clickText("[role=option]", "Accounts");
        await wait(
          'document.querySelector("#settings-section-picker").value === "Accounts"',
        );
        await click("#settings-section-picker");
        await clickText("[role=option]", "Platform");
        await wait(
          'document.querySelector("#settings-section-picker").value === "Platform"',
        );
      } else {
        await click("#settings-tab-account");
        await wait(
          'document.querySelector("#settings-tab-account").getAttribute("aria-selected") === "true"',
        );
        await click("#settings-tab-platform");
        await wait(
          'document.querySelector("#settings-tab-platform").getAttribute("aria-selected") === "true"',
        );
      }
      await wait(
        'document.querySelector("#settings-panel input[placeholder=RS]")',
      );
      assert.equal(
        await evaluate(
          'document.querySelector("#settings-panel input[placeholder=RS]").value',
        ),
        "Draft retained",
      );
    });
    await check("settings-retry-preserves-new-draft-and-revision", async () => {
      await visit("/admin/settings", {
        scheme: viewport.name === "tablet" ? "light" : "dark",
      });
      state.allowWrites = true;
      state.failAfterSave = true;
      const input = '#settings-panel input[placeholder="RS"]';
      const edit = async (text) => {
        await click(input);
        await evaluate(`${find(input)}.select()`);
        await cdp.send("Input.insertText", { text });
      };
      await edit("Committed name");
      await clickText("button", "Save Changes");
      await wait('document.querySelector("[data-request-error]")');
      assert.equal(state.writes.length, 1);
      const committedRevision = state.settings.revision;
      await edit("New local draft");
      // Another operator commits while this browser retains its own draft.
      state.settings.globalConfig = {
        ...state.settings.globalConfig,
        title: "Other operator",
      };
      state.settings.revision += 1;
      state.failed.clear();
      await click("[data-request-error] button");
      await wait('!document.querySelector("[data-request-error]")');
      assert.equal(await evaluate(`${find(input)}.value`), "New local draft");
      await clickText("button", "Save Changes");
      await wait('document.querySelector("[data-settings-conflict]")');
      assert.equal(state.writes.length, 2);
      assert.equal(state.writes[1].body.expectedRevision, committedRevision);
      assert.equal(
        state.settings.revision,
        committedRevision + 1,
        "conflicted draft must not commit",
      );
      await wait('document.querySelector("[data-settings-conflict]")');
      await audit(viewport.name + "-settings-conflict");
      await click("[data-settings-conflict] button");
      await audit(viewport.name + "-discard-draft-confirmation");
      await clickText("[role=dialog] button", "Keep draft");
      await wait(
        '!document.querySelector("[role=dialog]") && document.activeElement === document.querySelector("[data-settings-conflict] button")',
      );
      assert.equal(await evaluate(`${find(input)}.value`), "New local draft");
      state.failed.add("/api/admin/config");
      await click("[data-settings-conflict] button");
      await clickText("[role=dialog] button", "Discard draft and reload");
      await wait(
        'document.querySelector("[data-settings-reload-error]") && !document.querySelector("[role=dialog] button[data-loading]")',
      );
      assert.equal(
        await evaluate(`${find(input)}.value`),
        "New local draft",
        "failed reload must preserve the draft",
      );
      await audit(viewport.name + "-settings-reload-error");
      assert.equal(
        state.writes.length,
        2,
        "discard/reload only reads configuration",
      );
      state.failed.clear();
      await clickText("[role=dialog] button", "Discard draft and reload");
      await wait(
        '!document.querySelector("[role=dialog]") && !document.querySelector("[data-settings-conflict]")',
      );
      assert.equal(await evaluate(`${find(input)}.value`), "Other operator");
      await wait(
        'document.activeElement === document.getElementById("settings-panel")',
      );
      state.failAfterSave = false;
      await edit("Resolved draft");
      await clickText("button", "Save Changes");
      await wait(
        '!document.querySelector("[data-settings-conflict]") && !document.querySelector("button[data-loading]")',
      );
      assert.equal(state.writes.length, 3);
      assert.equal(
        state.writes[2].body.expectedRevision,
        committedRevision + 1,
      );
      assert.equal(state.settings.globalConfig.title, "Resolved draft");
    });
    await check("users-error-and-retry", async () => {
      await visit("/admin/users", { failed: ["/api/admin/users"] });
      await noLoading();
      await wait('document.querySelector("[data-request-error]")');
      await click("[data-request-error] button");
      await pause(150);
      assert.ok(await evaluate(`Boolean(${find("[data-request-error]")})`));
      state.failed.clear();
      await click("[data-request-error] button");
      await wait(
        '!document.querySelector("[data-request-error]") && document.querySelector("[data-user-actions]")',
      );
      await audit(viewport.name + "-users-recovered");
    });
    await check("compact-event-rows-and-team-limit", async () => {
      await visit("/games");
      const width = await evaluate(
        'document.querySelector("[data-event-rows] article").getBoundingClientRect().width',
      );
      const available = await evaluate(
        'document.querySelector("#event-catalog-results").clientWidth',
      );
      assert.ok(
        width >= available * 0.9,
        "event rows must use available content width",
      );
      assert.ok(
        await evaluate(
          'document.querySelector("[data-event-rows]").innerText.includes("Up to 3 per team")',
        ),
      );
      await audit(viewport.name + "-events");
    });
    await check("tall-event-posters-stay-bounded", async () => {
      await visit("/games", { tallPoster: true });
      await wait(
        '[...document.querySelectorAll("[data-event-rows] img")].every(img=>img.complete && img.naturalHeight===4000)',
      );
      assert.ok(
        await evaluate(
          '[...document.querySelectorAll("[data-event-rows] article")].every(row=>row.getBoundingClientRect().height<420)',
        ),
        "poster aspect ratio must not inflate event rows",
      );
      await audit(viewport.name + "-event-posters");
    });
    await check("challenge-view-and-writeup-remain-reachable", async () => {
      await visit("/games/901/challenges", { playerMode: true });
      if (viewport.width <= 390) {
        await wait('document.querySelector("[data-challenge-list]")');
        const firstTop = await top("[data-challenge-list]");
        assert.ok(
          firstTop < viewport.height - 90,
          `first challenges begin at ${firstTop}`,
        );
        await click("[data-challenge-filters]");
        await wait('document.querySelector("#challenge-view-picker")');
        await audit(viewport.name + "-challenge-options");
        await clickText("[role=dialog] button", "Submit Writeup");
        await wait(
          'document.querySelector("[role=dialog]")?.innerText.includes("PDF")',
        );
        await press("Escape");
        await wait('!document.querySelector("[role=dialog]")');
        await wait(
          'document.activeElement === document.querySelector("[data-challenge-filters]")',
        );
      }
      await audit(viewport.name + "-challenges");
    });
    await check("user-actions-confirm-and-cancel", async () => {
      await visit("/admin/users");
      const id = state.users[1].id,
        group = `[data-user-actions="${id}"]`;
      assert.equal(state.writes.length, 0);
      await chooseUserAction(group, "Send password setup link");
      await wait('document.querySelector("[role=dialog]")');
      assert.equal(
        state.writes.length,
        0,
        "opening confirmation must not mutate",
      );
      await audit(viewport.name + "-account-confirmation");
      await press("Escape");
      await wait('!document.querySelector("[role=dialog]")');
      assert.equal(state.writes.length, 0);
      await wait(
        `${find(group + " button:last-child")} === document.activeElement`,
      );
      state.allowWrites = true;
      state.holdWrites = true;
      await chooseUserAction(group, "Send password setup link");
      await clickText("[role=dialog] button", "Confirm");
      await pause(200);
      assert.equal(state.writes.length, 1);
      await press("Escape");
      assert.ok(
        await evaluate('Boolean(document.querySelector("[role=dialog]"))'),
        "pending confirmation retains ownership",
      );
      state.release();
      await wait('!document.querySelector("[role=dialog]")');
      const write = state.writes[0];
      assert.equal(write.method, "POST");
      assert.equal(write.path, `/api/admin/users/${id}/password-email`);
      assert.equal(write.body.operationId, write.idempotencyKey);
      assert.ok(write.body.operationId);
    });
    await check("user-menu-guards-and-retry-owner", async () => {
      await visit("/admin/users");
      const self = `[data-user-actions="${state.users[0].id}"]`;
      const selfMenu = await openUserMenu(self);
      assert.equal(
        await evaluate(
          `${find(selfMenu + " [role=menuitem][data-disabled]")}.textContent.trim()`,
        ),
        "Delete user",
      );
      await press("Escape");
      await wait(
        `!document.querySelector(${JSON.stringify(selfMenu)}) && ${find(self + " button:last-child")}?.getAttribute('aria-expanded') === 'false' && ${find(self + " button:last-child")} === document.activeElement`,
      );
      const group = `[data-user-actions="${state.users[1].id}"]`;
      const send = async () => {
        await chooseUserAction(group, "Send password setup link");
        await clickText("[role=dialog] button", "Confirm");
        await wait('!document.querySelector("[role=dialog]")');
        await wait(
          `${find(group + " button:last-child")} === document.activeElement`,
        );
      };
      state.allowWrites = true;
      state.mutationStatus = 503;
      await send();
      assert.equal(state.writes.length, 1);
      assert.equal(
        state.writes[0].path,
        `/api/admin/users/${state.users[1].id}/password-email`,
        "the selected menu must belong to the intended account",
      );
      state.mutationStatus = 200;
      await send();
      assert.equal(state.writes.length, 2);
      assert.deepEqual(
        state.writes[0],
        state.writes[1],
        "ambiguous retry must keep its idempotency owner",
      );
    });
    await check("saved-challenge-view-is-honored", async () => {
      await visit("/games/901/challenges#view=cards", {
        playerMode: true,
        preserveView: true,
      });
      await wait('document.querySelector("[data-guide=challenge-card]")');
      if (viewport.width <= 390) {
        await click("[data-challenge-filters]");
        await evaluate(
          'document.querySelector("#challenge-view-picker").scrollIntoView({block:"nearest"}); document.querySelector("#challenge-view-picker").focus()',
        );
        // The same native selectOption contract used by browser drivers:
        // an OS picker may cancel ArrowDown on Escape instead of committing.
        await evaluate(
          `(() => { const select = document.querySelector('#challenge-view-picker'); select.value = 'list'; select.dispatchEvent(new Event('input', {bubbles:true})); select.dispatchEvent(new Event('change', {bubbles:true})); })()`,
        );
        await press("Escape");
      } else {
        await clickText(".mantine-SegmentedControl-label", "List");
      }
      await wait('document.querySelector("[data-challenge-list]")');
      await evaluate("history.replaceState(null,'',location.pathname)");
      await cdp.send("Page.reload");
      await wait('document.querySelector("[data-challenge-list]")');
      assert.equal(
        await evaluate(
          'JSON.parse(localStorage.getItem("challenge-explorer-view"))',
        ),
        "list",
      );
    });
    await check("verification-label-matches-the-write", async () => {
      await visit("/admin/users");
      state.allowWrites = true;
      const switches = "[role=switch]";
      await click(switches);
      await pause(200);
      assert.equal(state.writes.length, 1);
      assert.deepEqual(state.writes[0].body, { emailConfirmed: false });
      assert.ok(
        await evaluate(
          `${find(switches)}.getAttribute('aria-label').includes('email verification')`,
        ),
      );
    });
  }
  if (!baseline) {
    viewport = { name: "manager", width: 390, height: 844 };
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      width: 390,
      height: 844,
      deviceScaleFactor: 1,
      mobile: true,
    });
    await check("navigation-respects-manager-role", async () => {
      await visit("/admin/games", { role: "Manager" });
      assert.equal(
        await evaluate(
          `Boolean(document.querySelector('a[href="/admin/users"],a[href="/admin/settings"]'))`,
        ),
        false,
      );
      assert.equal(
        await evaluate(
          `Boolean(document.querySelector('a[href="/admin/games"]'))`,
        ),
        true,
      );
      await audit("manager-navigation");
    });
    const locales = [
      "en-US",
      "id-ID",
      "de-DE",
      "fr-FR",
      "es-ES",
      "ru-RU",
      "vi-VN",
      "ja-JP",
      "ko-KR",
      "zh-CN",
      "zh-TW",
    ];
    for (const [index, language] of locales.entries()) {
      viewport = { name: language, width: 320, height: 568 };
      await cdp.send("Emulation.setDeviceMetricsOverride", {
        width: 320,
        height: 568,
        deviceScaleFactor: 1,
        mobile: true,
      });
      await check("localized-compact-surfaces", async () => {
        const options = { language, scheme: index % 2 ? "light" : "dark" };
        await visit("/", options);
        await wait(
          `document.documentElement.lang === ${JSON.stringify(language.toLowerCase())}`,
        );
        if (language === "id-ID")
          assert.equal(
            await evaluate(
              'document.querySelector("#news-feed-title").innerText',
            ),
            "Berita & pengumuman",
          );
        await audit(language + "-home");
        await visit("/admin/settings", options);
        await click("#settings-section-picker");
        assert.equal(
          await evaluate(
            `[...document.querySelectorAll("[role=option]")].filter(${visible}).length`,
          ),
          12,
        );
        await audit(language + "-settings-options");
        await press("Escape");
        const settingsName = '#settings-panel input[placeholder="RS"]';
        await click(settingsName);
        await evaluate(`${find(settingsName)}.select()`);
        await cdp.send("Input.insertText", { text: "Locale draft" });
        state.allowWrites = true;
        state.settings.revision += 1;
        await click(".mantine-Affix-root button");
        await wait('document.querySelector("[data-settings-conflict]")');
        await audit(language + "-settings-conflict");
        await click("[data-settings-conflict] button");
        await wait('document.querySelector("[role=dialog]")');
        await audit(language + "-discard-confirmation");
        await press("Escape");
        await wait('!document.querySelector("[role=dialog]")');
        await visit("/admin/users", options);
        await openUserMenu(`[data-user-actions="${state.users[1].id}"]`);
        await audit(language + "-user-menu");
        await press("Escape");
      });
    }
  }
} finally {
  for (const owner of owners) owner.release();
  await Promise.allSettled([...pending]);
  await browser.close();
  await server.close();
  const result = {
    reports,
    exceptions,
    interceptionErrors,
    unknown: owners.flatMap((owner) => owner.unknown),
    unhandled: server.unhandled,
  };
  writeFileSync(
    resolve(output, "report.json"),
    JSON.stringify(result, null, 2),
  );
  console.log(
    JSON.stringify({
      passed: reports.filter((entry) => entry.status === "passed").length,
      failed: reports.filter((entry) => entry.status === "failed").length,
      exceptions,
      interceptionErrors,
      unknown: result.unknown,
      unhandled: result.unhandled,
    }),
  );
  if (
    reports.some((entry) => entry.status === "failed") ||
    exceptions.length ||
    interceptionErrors.length ||
    result.unknown.length ||
    result.unhandled.length
  )
    process.exitCode = 1;
}
