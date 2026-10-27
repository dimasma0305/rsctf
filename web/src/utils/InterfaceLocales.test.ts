import assert from 'node:assert/strict'
import { readdirSync, readFileSync } from 'node:fs'
import test from 'node:test'

function strings(value: unknown, prefix = ''): Record<string, string> {
  if (typeof value === 'string') {
    assert.ok(value.trim(), `${prefix} must not be blank`)
    return { [prefix]: value }
  }
  assert.ok(value && typeof value === 'object' && !Array.isArray(value), prefix)
  return Object.assign(
    {},
    ...Object.entries(value).map(([key, child]) => strings(child, prefix ? `${prefix}.${key}` : key))
  )
}

const load = (locale: string) =>
  strings({
    interface: JSON.parse(readFileSync(`src/locales/${locale}/interface.json`, 'utf8')),
    pagination: JSON.parse(readFileSync(`src/locales/${locale}/common.json`, 'utf8')).pagination,
    settingsNavigation: JSON.parse(readFileSync(`src/locales/${locale}/admin.json`, 'utf8')).content.settings.nav,
    settingsStatus: JSON.parse(readFileSync(`src/locales/${locale}/admin.json`, 'utf8')).content.settings.status,
  })
const source = load('en-US')
const parameters = (text: string) => [...text.matchAll(/\{\{\s*([^}]+?)\s*\}\}/g)].map((match) => match[1]).sort()

for (const folder of readdirSync('src/locales', { withFileTypes: true }).filter((entry) => entry.isDirectory())) {
  test(`${folder.name} translates every interface key with the same parameters`, () => {
    const target = load(folder.name)
    assert.deepEqual(Object.keys(target).sort(), Object.keys(source).sort())
    for (const [key, value] of Object.entries(source)) assert.deepEqual(parameters(target[key]), parameters(value), key)
  })
}
