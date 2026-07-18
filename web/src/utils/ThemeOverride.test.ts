import { generateColors } from '@mantine/colors-generator'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { buildSemanticAccentColors, contrastRatio } from './ThemeContrast'

const LIGHT_SURFACE = '#ffffff'
const DARK_SURFACE = '#111a29'

const mixHex = (foreground: string, background: string, foregroundWeight: number) => {
  const channels = (color: string) => {
    const value = color.replace('#', '')
    return [0, 2, 4].map((offset) => Number.parseInt(value.slice(offset, offset + 2), 16))
  }
  const front = channels(foreground)
  const back = channels(background)
  const mixed = front.map((channel, index) =>
    Math.round(channel * foregroundWeight + back[index] * (1 - foregroundWeight))
  )

  return `#${mixed.map((channel) => channel.toString(16).padStart(2, '0')).join('')}`
}

test('semantic accents remain contrast-safe for arbitrary configured colors', () => {
  const colors = ['#0d9488', '#ffff00', '#00ff00', '#ff00ff', '#ffffff', '#000000', '#777777']

  for (const color of colors) {
    const semantic = buildSemanticAccentColors(color)

    assert.ok(contrastRatio(semantic[0], LIGHT_SURFACE) >= 4.5, `${color} light text`)
    assert.ok(contrastRatio(semantic[1], DARK_SURFACE) >= 4.5, `${color} dark text`)
    assert.ok(contrastRatio(semantic[2], LIGHT_SURFACE) >= 3, `${color} light border`)
    assert.ok(contrastRatio(semantic[3], DARK_SURFACE) >= 3, `${color} dark border`)
  }
})

test('light and dark control borders keep at least 3:1 contrast', () => {
  const css = readFileSync('src/styles/App.css', 'utf8')
  const schemes = [
    { name: 'light', surface: '#f8fafc' },
    { name: 'dark', surface: '#0e1726' },
  ]

  for (const scheme of schemes) {
    const block = css.match(new RegExp(`\\[data-mantine-color-scheme='${scheme.name}'\\] \\{([\\s\\S]*?)\\n\\}`))?.[1]
    const border = block?.match(/--app-control-border:\s*(#[0-9a-f]{6})/i)?.[1]

    assert.ok(border, `${scheme.name} control border token exists`)
    assert.ok(contrastRatio(border, scheme.surface) >= 3, `${scheme.name} control border`)
  }
})

test('active accent-surface text stays AA-safe for arbitrary configured colors', () => {
  const css = readFileSync('src/styles/App.css', 'utf8')
  const lightText = css
    .match(/\[data-mantine-color-scheme='light'\] \{([\s\S]*?)\n\}/)?.[1]
    ?.match(/--app-accent-surface-text:\s*(#[0-9a-f]{6})/i)?.[1]
  const darkText = css
    .match(/\[data-mantine-color-scheme='dark'\] \{([\s\S]*?)\n\}/)?.[1]
    ?.match(/--app-accent-surface-text:\s*(#[0-9a-f]{6})/i)?.[1]

  assert.ok(lightText, 'light active text token exists')
  assert.ok(darkText, 'dark active text token exists')

  for (const color of ['#0d9488', '#ffff00', '#00ff00', '#ff00ff', '#ffffff', '#000000', '#777777']) {
    const palette = generateColors(color)
    const lightAccentSurface = mixHex(palette[1], '#ffffff', 0.76)
    const darkAccentSurface = mixHex(palette[9], DARK_SURFACE, 0.34)

    assert.ok(contrastRatio(lightText, lightAccentSurface) >= 4.5, `${color} light active text`)
    assert.ok(contrastRatio(darkText, darkAccentSurface) >= 4.5, `${color} dark active text`)
  }
})

test('decorative surfaces are borderless and semantic accent variables resolve', () => {
  const css = readFileSync('src/styles/App.css', 'utf8')

  assert.equal((css.match(/--app-surface-border:\s*transparent;/g) ?? []).length, 2)
  assert.match(css, /\.mantine-Card-root,[\s\S]*?border-color:\s*var\(--app-surface-border\);/)
  assert.match(css, /\.mantine-Modal-content,[\s\S]*?border:\s*1px solid var\(--app-surface-border\);/)
  assert.match(css, /--app-accent-text:\s*var\(--mantine-color-semanticAccent-[01]\)/)
  assert.doesNotMatch(css, /--mantine-color-semantic-accent-/)
})
