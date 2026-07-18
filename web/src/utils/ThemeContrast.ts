import { darken, lighten, luminance } from '@mantine/core'
import type { MantineColorsTuple } from '@mantine/core'

const LIGHT_ACCENT_BACKGROUND = '#ffffff'
const DARK_ACCENT_BACKGROUND = '#111a29'

export const contrastRatio = (foreground: string, background: string) => {
  const foregroundLuminance = luminance(foreground)
  const backgroundLuminance = luminance(background)
  const lighter = Math.max(foregroundLuminance, backgroundLuminance)
  const darker = Math.min(foregroundLuminance, backgroundLuminance)

  return (lighter + 0.05) / (darker + 0.05)
}

const resolveContrastColor = (
  color: string,
  background: string,
  minimumRatio: number,
  direction: 'darker' | 'lighter'
) => {
  if (contrastRatio(color, background) >= minimumRatio) return color

  // Keep the selected hue as long as possible while moving its luminance toward
  // the nearest contrast-safe foreground. The final black/white fallback covers
  // unusual browser-supported color formats without weakening the guarantee.
  for (let step = 1; step <= 100; step += 1) {
    const amount = step / 100
    const candidate = direction === 'darker' ? darken(color, amount) : lighten(color, amount)
    if (contrastRatio(candidate, background) >= minimumRatio) return candidate
  }

  return direction === 'darker' ? '#000000' : '#ffffff'
}

/**
 * Semantic accent values consumed by App.css.
 * 0/1 are AA text colors for light/dark surfaces; 2/3 are 3:1 component
 * boundaries. The remaining tuple entries repeat safe values because Mantine
 * color collections intentionally always contain ten shades.
 */
export const buildSemanticAccentColors = (baseColor: string): MantineColorsTuple => {
  const lightText = resolveContrastColor(baseColor, LIGHT_ACCENT_BACKGROUND, 4.5, 'darker')
  const darkText = resolveContrastColor(baseColor, DARK_ACCENT_BACKGROUND, 4.5, 'lighter')
  const lightBorder = resolveContrastColor(baseColor, LIGHT_ACCENT_BACKGROUND, 3, 'darker')
  const darkBorder = resolveContrastColor(baseColor, DARK_ACCENT_BACKGROUND, 3, 'lighter')

  return [
    lightText,
    darkText,
    lightBorder,
    darkBorder,
    lightText,
    darkText,
    lightBorder,
    darkBorder,
    lightText,
    darkText,
  ]
}
