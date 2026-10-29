import test from "node:test";
import assert from "node:assert/strict";
import { scanSource, sourceMap } from "./interaction-source-map.mjs";

test("source scan maps native, library, custom handlers and navigation candidates with locations", () => {
  const source = `import { Button } from '@mantine/core'
const view = <><Button>Save</Button><a href="/games">Games</a>
<div onClick={open}>Custom</div><Box role="button" tabIndex={0} />
<Menu.Item>Nested</Menu.Item><PlainText>Not interactive</PlainText></>`;
  const result = scanSource(source, "web/src/example.tsx");
  assert.deepEqual(
    result.candidates.map((c) => c.tag),
    ["Button", "a", "div", "Box", "Menu.Item"],
  );
  assert.deepEqual(result.imports, ["@mantine/core"]);
  assert.equal(result.candidates[2].line, 3);
  assert.ok(result.candidates.every((c) => c.status === "source-candidate"));
});

test("comments and text that resemble JSX cannot inflate coverage", () => {
  assert.equal(
    scanSource('// <button onClick={x} />\nconst text = "<a href=link>"')
      .candidates.length,
    0,
  );
});

test("imperative arena listener assignments remain unverified source candidates", () => {
  const result =
    scanSource(`tab.addEventListener('click', open); button.onclick = show;
    button.onclick = null; document.addEventListener('mousemove', hover);`);
  assert.deepEqual(
    result.candidates.map((c) => c.signals),
    [["addEventListener:click"], ["onclick"]],
  );
  assert.ok(result.candidates.every((c) => c.status === "source-candidate"));
});

test("source map covers the route catalog and follows aliases into shared owners", () => {
  const map = sourceMap();
  assert.ok(map.routes.length >= 56);
  assert.ok(map.candidates.length > 100);
  const route = map.routes.find((r) => r.path === "/games/901/scoreboard");
  assert.ok(route.sources.includes("web/src/components/WithNavbar.tsx"));
  assert.ok(route.sources.includes("web/src/pages/games/[id]/Scoreboard.tsx"));
  assert.ok(map.candidates.every((c) => c.line > 0 && c.column > 0));
});
