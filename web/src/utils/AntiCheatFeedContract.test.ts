import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const api = readFileSync('src/Api.ts', 'utf8')
const log = readFileSync('src/components/monitor/CheatSubmissionLog.tsx', 'utf8')

test('anti-cheat incident log uses a bounded cursor delta and one-shot reads', () => {
  assert.match(api, /interface CheatIncidentPageModel/)
  assert.match(api, /\/api\/game\/\$\{id\}\/cheatinfo\/page/)
  assert.match(log, /useGameCheatInfoPage\(\s*gameId,\s*\{ count: 100 \}/)
  assert.match(log, /gameCheatInfoPage\(\s*gameId,\s*\{ after, count: 100 \},\s*\{ signal \}\s*\)/)
  assert.match(log, /new LatestRequest\(\)/)
  assert.match(log, /refreshInterval: 0/)
  assert.match(log, /pageIndex < 10/)
})
