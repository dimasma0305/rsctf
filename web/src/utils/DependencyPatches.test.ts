import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { resolve } from 'node:path'
import test from 'node:test'

const webRequire = createRequire(resolve('package.json'))

test('Pixi resolves the patched XML parser and preserves ordinary SVG data', () => {
  const pixiRequire = createRequire(webRequire.resolve('pixi.js'))
  assert.equal(pixiRequire('@xmldom/xmldom/package.json').version, '0.8.15')
  const { DOMParser, XMLSerializer } = pixiRequire('@xmldom/xmldom')
  const source = '<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0L10 10"/></svg>'
  const document = new DOMParser().parseFromString(source, 'image/svg+xml')
  assert.equal(document.documentElement.localName, 'svg')
  assert.equal(new XMLSerializer().serializeToString(document), source)
})

test('OpenAPI tooling resolves the patched YAML parser and reads a normal schema', () => {
  const generatorRequire = createRequire(webRequire.resolve('swagger-typescript-api'))
  const swaggerRequire = createRequire(generatorRequire.resolve('@apidevtools/swagger-parser'))
  const parserRequire = createRequire(swaggerRequire.resolve('@apidevtools/json-schema-ref-parser'))
  assert.equal(parserRequire('js-yaml/package.json').version, '4.3.2')
  assert.deepEqual(
    parserRequire('js-yaml').load('openapi: 3.0.3\ninfo:\n  title: Fixture\n  version: 1.0.0\npaths: {}'),
    {
      openapi: '3.0.3',
      info: { title: 'Fixture', version: '1.0.0' },
      paths: {},
    }
  )
})
