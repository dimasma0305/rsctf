import { createRequire } from "node:module";
import { readFileSync, readdirSync } from "node:fs";
import { relative, resolve } from "node:path";
import { repositoryRoot, discoverPageRoutes } from "./route-catalog.mjs";

const require = createRequire(resolve(repositoryRoot, "web/package.json"));
const ts = require("typescript");
const native = /^(a|area|button|input|select|textarea|summary|label)$/;
const component =
  /(?:Button|Anchor|Link|Input|Select|Checkbox|Radio|Switch|Slider|Menu\.Item|Tabs\.Tab|Accordion\.Control|Pagination|Dropzone)$/;
const event =
  /^on(?:Click|DoubleClick|PointerDown|MouseDown|KeyDown|Submit|Change)$/;

export function scanSource(text, file = "fixture.tsx") {
  const ast = ts.createSourceFile(
    file,
    text,
    ts.ScriptTarget.Latest,
    true,
    file.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  );
  const candidates = [],
    imports = [];
  const add = (node, tag, signals) => {
    const pos = ast.getLineAndCharacterOfPosition(node.getStart(ast));
    candidates.push({
      source: file,
      line: pos.line + 1,
      column: pos.character + 1,
      tag,
      signals,
      status: "source-candidate",
      reason: "Static candidate; rendering and behavior are not proven",
    });
  };
  const visit = (node) => {
    if (
      (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) &&
      node.moduleSpecifier &&
      ts.isStringLiteral(node.moduleSpecifier)
    )
      imports.push(node.moduleSpecifier.text);
    if (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) {
      const tag = node.tagName.getText(ast);
      const attrs = node.attributes.properties
        .filter(ts.isJsxAttribute)
        .map((a) => a.name.getText(ast));
      const signals = attrs.filter(
        (a) =>
          event.test(a) ||
          [
            "href",
            "to",
            "role",
            "tabIndex",
            "aria-haspopup",
            "aria-expanded",
          ].includes(a),
      );
      if (native.test(tag) || component.test(tag) || signals.length)
        add(node, tag, signals);
    }
    // Arena controls can be wired imperatively rather than through JSX.
    if (
      ts.isCallExpression(node) &&
      ts.isPropertyAccessExpression(node.expression) &&
      node.expression.name.text === "addEventListener" &&
      node.arguments[0] &&
      ts.isStringLiteral(node.arguments[0]) &&
      /^(click|dblclick|pointerdown|mousedown|keydown|submit|change)$/.test(
        node.arguments[0].text,
      )
    )
      add(node, node.expression.expression.getText(ast), [
        `addEventListener:${node.arguments[0].text}`,
      ]);
    if (
      ts.isBinaryExpression(node) &&
      node.operatorToken.kind === ts.SyntaxKind.EqualsToken &&
      ts.isPropertyAccessExpression(node.left) &&
      /^on(click|dblclick|pointerdown|mousedown|keydown|submit|change)$/.test(
        node.left.name.text,
      ) &&
      node.right.kind !== ts.SyntaxKind.NullKeyword
    )
      add(node, node.left.expression.getText(ast), [node.left.name.text]);
    ts.forEachChild(node, visit);
  };
  visit(ast);
  return { candidates, imports };
}

export function sourceMap(root = repositoryRoot) {
  const src = resolve(root, "web/src"),
    files = new Map();
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const file = resolve(dir, entry.name);
      if (entry.isDirectory()) walk(file);
      else if (/\.(ts|tsx)$/.test(file) && !file.endsWith(".test.ts"))
        files.set(
          file,
          scanSource(readFileSync(file, "utf8"), relative(root, file)),
        );
    }
  };
  walk(src);
  const config = ts.readConfigFile(
    resolve(root, "web/tsconfig.app.json"),
    ts.sys.readFile,
  );
  const parsed = ts.parseJsonConfigFileContent(
    config.config,
    ts.sys,
    resolve(root, "web"),
  );
  const reachable = (start) => {
    const seen = new Set();
    const visit = (file) => {
      if (!files.has(file) || seen.has(file)) return;
      seen.add(file);
      for (const specifier of files.get(file).imports) {
        const dependency = ts.resolveModuleName(
          specifier,
          file,
          parsed.options,
          ts.sys,
        ).resolvedModule?.resolvedFileName;
        if (dependency) visit(dependency);
      }
    };
    visit(start);
    // App providers include global menus/dialogs shared by every route.
    visit(resolve(src, "App.tsx"));
    return [...seen]
      .filter((f) => files.get(f).candidates.length)
      .map((f) => relative(root, f))
      .sort();
  };
  return {
    limitations: [
      "Static candidates are not rendered coverage.",
      "Dynamic imports, generated markup, third-party internals and canvas hit targets require browser or explicit scenarios.",
    ],
    candidates: [...files.values()].flatMap((f) => f.candidates),
    routes: discoverPageRoutes({
      gameId: 901,
      challengeId: 9001,
      postId: "00000001",
    }).map((r) => ({
      ...r,
      sources: reachable(resolve(src, "pages", r.sourceFile)),
    })),
  };
}
