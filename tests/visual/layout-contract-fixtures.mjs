// Generate explicit empty-state reads from the checked-in API contract. Populated
// scenarios override these in full-layout-fixtures. This is rendering coverage,
// not backend/auth validation; report every contract-generated read as such.
import ts from "../../web/node_modules/typescript/lib/typescript.js";
import { readFileSync } from "node:fs";

const source = ts.createSourceFile(
  "Api.ts",
  readFileSync(new URL("../../web/src/Api.ts", import.meta.url), "utf8"),
  ts.ScriptTarget.Latest,
  true,
);
const types = new Map(),
  endpoints = [];
for (const node of source.statements)
  if (
    node.name &&
    (ts.isInterfaceDeclaration(node) ||
      ts.isTypeAliasDeclaration(node) ||
      ts.isEnumDeclaration(node))
  )
    types.set(node.name.text, node);
function value(type, depth = 0) {
  if (!type || depth > 8) return null;
  if (ts.isArrayTypeNode(type)) return [];
  if (ts.isUnionTypeNode(type))
    return value(
      type.types.find(
        (t) =>
          t.kind !== ts.SyntaxKind.NullKeyword &&
          !(
            ts.isLiteralTypeNode(t) &&
            t.literal.kind === ts.SyntaxKind.NullKeyword
          ),
      ) || type.types[0],
      depth + 1,
    );
  if (ts.isLiteralTypeNode(type))
    return ts.isStringLiteral(type.literal)
      ? type.literal.text
      : ts.isNumericLiteral(type.literal)
        ? Number(type.literal.text)
        : type.literal.kind === ts.SyntaxKind.NullKeyword
          ? null
          : type.literal.kind === ts.SyntaxKind.TrueKeyword;
  if (type.kind === ts.SyntaxKind.StringKeyword) return "Fixture";
  if (type.kind === ts.SyntaxKind.NumberKeyword) return 0;
  if (type.kind === ts.SyntaxKind.BooleanKeyword) return false;
  if (ts.isTypeReferenceNode(type)) {
    const name = type.typeName.getText(source);
    if (name === "Array") return [];
    if (name === "Record") return {};
    const declaration = types.get(name);
    if (!declaration) return null;
    if (ts.isTypeAliasDeclaration(declaration))
      return value(declaration.type, depth + 1);
    if (ts.isEnumDeclaration(declaration)) {
      const literal = declaration.members[0]?.initializer;
      return literal && ts.isStringLiteral(literal) ? literal.text : 0;
    }
    return Object.fromEntries(
      declaration.members
        .filter((m) => ts.isPropertySignature(m) && !m.questionToken)
        .map((m) => [
          m.name.getText(source).replace(/^['"]|['"]$/g, ""),
          value(m.type, depth + 1),
        ]),
    );
  }
  if (ts.isTypeLiteralNode(type))
    return Object.fromEntries(
      type.members
        .filter(ts.isPropertySignature)
        .map((m) => [m.name.getText(source), value(m.type, depth + 1)]),
    );
  return null;
}
const escape = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
function visit(node) {
  if (
    ts.isCallExpression(node) &&
    ts.isPropertyAccessExpression(node.expression) &&
    node.expression.name.text === "request" &&
    node.typeArguments?.length &&
    ts.isObjectLiteralExpression(node.arguments[0])
  ) {
    const fields = new Map(
      node.arguments[0].properties
        .filter(ts.isPropertyAssignment)
        .map((p) => [p.name.getText(source), p.initializer]),
    );
    const path = fields.get("path"),
      method = fields.get("method");
    if (method && ts.isStringLiteral(method) && method.text === "GET" && path) {
      let pattern;
      if (ts.isTemplateExpression(path))
        pattern =
          escape(path.head.text) +
          path.templateSpans
            .map((s) => "[^/]+" + escape(s.literal.text))
            .join("");
      else if (ts.isStringLiteralLike(path)) pattern = escape(path.text);
      if (pattern)
        endpoints.push({
          pattern: new RegExp("^" + pattern + "$", "i"),
          body: value(node.typeArguments[0]),
          contract: node.typeArguments[0].getText(source),
        });
    }
  }
  ts.forEachChild(node, visit);
}
visit(source);
export function contractFixture(path) {
  const endpoint = endpoints.find((e) => e.pattern.test(path));
  return endpoint
    ? { body: structuredClone(endpoint.body), contract: endpoint.contract }
    : undefined;
}
