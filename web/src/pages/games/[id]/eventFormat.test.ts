import type { TFunction } from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { formatGameEvent } from './eventFormat'

const t = ((key: string, options?: Record<string, unknown>) => {
  if (key === 'game.event.download') return `Downloaded: ${options?.chal}`
  if (key === 'game.event.unknown_challenge') return 'Unknown challenge'
  return key
}) as TFunction

function downloadEvent(values: string[]) {
  return {
    type: 'Download',
    values,
  }
}

test('download events show the canonical challenge title without exposing the token', () => {
  const formatted = formatGameEvent(t, downloadEvent(['326', 'Twin Tokens', 'sensitive-download-token']))

  assert.equal(formatted, 'Downloaded: Twin Tokens')
  assert.doesNotMatch(formatted, /sensitive-download-token/)
})

test('download events remain identifiable when a stored title is empty', () => {
  assert.equal(formatGameEvent(t, downloadEvent(['326', '', 'sensitive-download-token'])), 'Downloaded: #326')
})
