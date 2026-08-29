import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const accountStyles = readFileSync('src/styles/components/AccountView.module.css', 'utf8')

test('account cards can shrink to a compact viewport', () => {
  const cardRule = accountStyles.match(/\.card\s*\{([\s\S]*?)\n\}/)?.[1]

  assert.ok(cardRule, 'account card rule exists')
  assert.match(cardRule, /min-width:\s*0;/)
})
