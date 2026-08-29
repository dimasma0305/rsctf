import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

test('challenge archive instructions use the contrast-safe muted text token', () => {
  const source = readFileSync('src/pages/games/[id]/submit.tsx', 'utf8')
  const hint = source.indexOf("t('game.submit.dropzone.hint')")

  assert.notEqual(hint, -1)
  assert.match(source.slice(Math.max(0, hint - 180), hint), /color: 'var\(--app-text-muted\)'/)
})

test('disabled challenge submissions keep instructions readable and disable the controls', () => {
  const source = readFileSync('src/pages/games/[id]/submit.tsx', 'utf8')

  assert.match(source, /aria-disabled=\{disabled \|\| undefined\}/)
  assert.match(source, /<Dropzone\s+disabled=\{busy \|\| disabled\}/)
  assert.match(source, /disabled=\{!file \|\| disabled\}/)
  assert.doesNotMatch(source, /opacity:\s*0\.55/)
})
