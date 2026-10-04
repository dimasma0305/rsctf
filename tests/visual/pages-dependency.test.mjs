import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'
import test from 'node:test'
import { pathToFileURL } from 'node:url'
import { discoverPageRoutes, repositoryRoot } from './route-catalog.mjs'

const webRoot = join(repositoryRoot, 'web')
const webRequire = createRequire(join(webRoot, 'package.json'))
const pluginEntry = webRequire.resolve('vite-plugin-pages')
const pluginRequire = createRequire(pluginEntry)
const { PageContext } = await import(pathToFileURL(join(dirname(pluginEntry), 'index.js')))
const options = {
  resolver: 'react',
  dirs: [{ dir: './src/pages', baseRoute: '', filePattern: '**/*.tsx' }],
}

test('Pages uses the dependency-free matcher only through its compatible isMatch API', () => {
  const dependency = pluginRequire('micromatch/package.json')
  assert.equal(dependency.name, 'picomatch')
  assert.deepEqual(dependency.dependencies ?? {}, {})
  const matcher = pluginRequire('micromatch')
  assert.equal(matcher.isMatch('/src/pages/Index.tsx', []), false)
  assert.equal(matcher.isMatch('/src/pages/components/Card.tsx', ['**/components/**']), true)
  assert.equal(matcher.isMatch('/src/pages/Index.tsx', ['**/components/**']), false)

  // The alias is intentionally not a general-purpose micromatch replacement.
  // An upstream change to the plugin's API use must be reviewed before release.
  const esm = readFileSync(join(dirname(pluginEntry), 'index.js'), 'utf8')
  const cjs = readFileSync(pluginEntry, 'utf8')
  assert.deepEqual(esm.match(/micromatch\.[A-Za-z]+/g), ['micromatch.isMatch'])
  assert.deepEqual(cjs.match(/import_micromatch\.default\.[A-Za-z]+/g), ['import_micromatch.default.isMatch'])
  assert.doesNotMatch(esm, /micromatch\s*\(/)
  assert.doesNotMatch(cjs, /import_micromatch\.default\s*\(/)
  const lock = readFileSync(join(webRoot, 'pnpm-lock.yaml'), 'utf8')
  assert.doesNotMatch(lock, /^  (?:braces|micromatch)@/m)
})

test('Pages preserves every public route, parameter, and page module', async () => {
  const context = new PageContext(options, webRoot)
  await context.searchGlob()
  const routes = await context.options.resolver.getComputedRoutes(context)
  const actual = []
  function walk(nodes, parent = '') {
    for (const node of nodes) {
      assert.equal(node.caseSensitive, false)
      const path = `${parent}/${node.path}`.replace(/\/+/g, '/').replace(/\/$/, '') || '/'
      if (node.element) actual.push({ path: path.toLowerCase(), source: node.element })
      if (node.children) walk(node.children, path)
    }
  }
  walk(routes)
  const expected = discoverPageRoutes({ gameId: ':id', challengeId: ':chalId', postId: ':postId' }).map(
    (route) => ({
      path: route.sourceFile === '[...all].tsx' ? '/*' : route.path.toLowerCase(),
      source: `/src/pages/${route.sourceFile}`,
    })
  )
  const bySource = (a, b) => a.source.localeCompare(b.source)
  assert.equal(context.pageRouteMap.size, 56)
  assert.deepEqual(actual.sort(bySource), expected.sort(bySource))
})

test('Pages watcher preserves extension, directory, and default exclusion filtering', async () => {
  const context = new PageContext(options, webRoot)
  const handlers = new Map()
  const watcher = { on(event, handler) { handlers.set(event, handler); return this } }
  context.setupWatcher(watcher)
  const page = join(webRoot, 'src/pages/games/[id]/WatchFixture.tsx')
  const helper = join(webRoot, 'src/pages/games/[id]/watchFixture.css')
  const component = join(webRoot, 'src/pages/__fixtures__/WatchFixture.tsx')
  const outside = join(webRoot, 'src/components/WatchFixture.tsx')
  for (const path of [page, helper, component, outside]) await handlers.get('add')(path)
  assert.deepEqual([...context.pageRouteMap.keys()], [page])
  await handlers.get('unlink')(helper)
  assert.equal(context.pageRouteMap.size, 1)
  await handlers.get('unlink')(page)
  assert.equal(context.pageRouteMap.size, 0)
})
