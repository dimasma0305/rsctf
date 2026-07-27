import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'

test('the monitor event stream is a named keyboard-accessible region', () => {
  const source = readFileSync('src/pages/games/[id]/monitor/Events.tsx', 'utf8')

  assert.match(source, /viewportProps=\{\{[\s\S]*?role:\s*'region',[\s\S]*?tabIndex:\s*0,[\s\S]*?'aria-label':/)
})
