import test from "node:test";
import assert from "node:assert/strict";
import { mergedControls, renderInventory } from "./interaction-report.mjs";

test("later discovery cannot erase verified probes or conflate distinct controls", () => {
  const control = {
    selector: "button:nth-of-type(1)",
    role: "button",
    name: "Menu",
    status: "exercised",
  };
  const inventory = {
    states: [
      { name: "page", controls: [control] },
      {
        name: "closed",
        controls: [
          { ...control, status: "discovered" },
          { ...control, selector: "button:nth-of-type(2)", status: "disabled" },
        ],
      },
    ],
  };
  const merged = mergedControls(inventory);
  assert.equal(merged.length, 2);
  assert.equal(merged[0].status, "exercised");
  assert.deepEqual(merged[0].states, ["page", "closed"]);
  assert.equal(merged[1].status, "disabled");
});

test("HTML artifact escapes fixture text and destinations instead of executing them", () => {
  const unsafe = '<script>alert("fixture")</script>';
  const html = renderInventory(
    {},
    [
      {
        project: "compact",
        route: "/",
        states: [
          {
            name: "page",
            controls: [{ selector: "button", name: unsafe, href: unsafe }],
          },
        ],
      },
    ],
    { routes: [] },
  );
  assert.ok(!html.includes(unsafe));
  assert.ok(html.includes("&lt;script&gt;"));
  assert.ok(html.includes("Discovery is not behavior coverage"));
});
