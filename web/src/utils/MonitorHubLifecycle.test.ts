import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'

const monitorPages = ['src/pages/games/[id]/monitor/Events.tsx', 'src/pages/games/[id]/monitor/Submissions.tsx']

test('monitor hubs survive game timing revalidation and stop at the event boundary', () => {
  for (const page of monitorPages) {
    const source = readFileSync(page, 'utf8')

    assert.match(source, /const \{ finished \} = useGameStatus\(game\)/, page)
    assert.match(source, /const monitorConnectionActive = Boolean\(game\?\.end\) && !finished/, page)
    assert.match(source, /if \(monitorConnectionActive\) \{[\s\S]*?new signalR\.HubConnectionBuilder\(\)/, page)
    assert.match(source, /\}, \[monitorConnectionActive, numId, t\]\)/, page)
    assert.doesNotMatch(source, /\}, \[game, numId, t\]\)/, page)
  }
})
