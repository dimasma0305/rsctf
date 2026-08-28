import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const source = (path: string) => readFileSync(path, 'utf8')

test('application defaults do not turn new reads into hidden pollers or retries', () => {
  const app = source('src/App.tsx')
  assert.match(app, /refreshInterval:\s*0/)
  assert.match(app, /shouldRetryOnError:\s*false/)
  assert.doesNotMatch(app, /refreshInterval:\s*60_000/)
})

test('idle stats, catalog, and review reads declare bounded one-shot ownership', () => {
  const stats = source('src/components/account/StatsPanel.tsx')
  const catalog = source('src/pages/challenges/Index.tsx')
  const pending = source('src/pages/admin/games/[id]/pending.tsx')
  assert.match(stats, /['"]\/api\/account\/stats['"], OnceSWRConfig/)
  assert.doesNotMatch(stats, /fetch\(/)
  assert.match(catalog, /OnceSWRConfig/)
  assert.doesNotMatch(catalog, /refreshInterval:\s*60_000/)
  assert.match(pending, /count:\s*PAGE_SIZE/)
  assert.match(pending, /OnceSWRConfig/)
})

test('closed engine toolkits cannot own SSH or token reads', () => {
  const ad = source('src/components/AdGuideModal.tsx')
  const koth = source('src/components/KothGuideModal.tsx')
  assert.match(ad, /useAdToken\(gameId, undefined, modalProps\.opened\)/)
  assert.match(ad, /useAdGameGetSshKey\([\s\S]*modalProps\.opened/)
  assert.match(koth, /modalProps\.opened \? `\/api\/game\/\$\{gameId\}/)
})
