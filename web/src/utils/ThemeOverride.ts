import { generateColors } from '@mantine/colors-generator'
import {
  Accordion,
  ActionIcon,
  Alert,
  Anchor,
  Avatar,
  Badge,
  Button,
  Card,
  Checkbox,
  Code,
  Drawer,
  Loader,
  MantineThemeOverride,
  Menu,
  Modal,
  MultiSelect,
  NumberInput,
  Pagination,
  Paper,
  PasswordInput,
  Popover,
  Select,
  Switch,
  Table,
  Tabs,
  Textarea,
  TextInput,
  Tooltip,
  TooltipFloating,
  createTheme,
  useMantineTheme,
} from '@mantine/core'
import { createStyles } from '@mantine/emotion'
import { useLocalStorage, useMediaQuery } from '@mantine/hooks'
import { useEffect, useState } from 'react'
import { useConfig } from '@Hooks/useConfig'
import tooltipClasses from '@Styles/Tooltip.module.css'
import { buildSemanticAccentColors } from './ThemeContrast'

const CustomTheme: MantineThemeOverride = {
  colors: {
    gray: [
      '#f8fafc',
      '#f1f5f9',
      '#e2e8f0',
      '#cbd5e1',
      '#94a3b8',
      '#64748b',
      '#475569',
      '#334155',
      '#1e293b',
      '#0f172a',
    ],
    brand: [
      '#e6fffb',
      '#ccfbf1',
      '#99f6e4',
      '#5eead4',
      '#2dd4bf',
      '#14b8a6',
      '#0d9488',
      '#0f766e',
      '#115e59',
      '#134e4a',
    ],
    semanticAccent: buildSemanticAccentColors('#0d9488'),
    alert: [
      '#FFB4B4',
      '#FFA0A0',
      '#FF8c8c',
      '#FF7878',
      '#FF6464',
      '#FE5050',
      '#FE3c3c',
      '#FE2828',
      '#FC1414',
      '#FC0000',
    ],
    light: [
      '#FFFFFF',
      '#F8F8F8',
      '#EFEFEF',
      '#E0E0E0',
      '#DFDFDF',
      '#D0D0D0',
      '#CFCFCF',
      '#C0C0C0',
      '#BFBFBF',
      '#B0B0B0',
    ],
    dark: [
      '#f8fafc',
      '#e2e8f0',
      '#cbd5e1',
      '#94a3b8',
      '#64748b',
      '#334155',
      '#1e293b',
      '#111827',
      '#0b1120',
      '#070b14',
    ],
  },
  white: '#ffffff',
  black: '#070b14',
  primaryColor: 'brand',
  primaryShade: { light: 7, dark: 7 },
  autoContrast: true,
  luminanceThreshold: 0.38,
  cursorType: 'pointer',
  respectReducedMotion: true,
  defaultRadius: 'md',
  fontFamily:
    'Lexend, -apple-system, BlinkMacSystemFont, Helvetica Neue, PingFang SC, Microsoft YaHei, Source Han Sans SC, Noto Sans CJK SC, sans-serif',
  fontFamilyMonospace:
    'JetBrains Mono, ui-monospace, SFMono-Regular, Monaco, Consolas, Courier New, monospace, sans-serif',
  headings: {
    fontFamily: 'Lexend, sans-serif',
    fontWeight: '720',
    textWrap: 'balance',
    sizes: {
      h1: { fontSize: 'clamp(1.75rem, 3vw, 2.5rem)', lineHeight: '1.15' },
      h2: { fontSize: 'clamp(1.4rem, 2vw, 1.9rem)', lineHeight: '1.2' },
      h3: { fontSize: 'clamp(1.15rem, 1.5vw, 1.45rem)', lineHeight: '1.25' },
    },
  },
  radius: {
    xs: '4px',
    sm: '7px',
    md: '10px',
    lg: '14px',
    xl: '20px',
  },
  shadows: {
    xs: '0 1px 2px rgba(8, 15, 30, 0.05)',
    sm: '0 1px 3px rgba(8, 15, 30, 0.08), 0 1px 2px rgba(8, 15, 30, 0.04)',
    md: '0 8px 24px rgba(8, 15, 30, 0.09)',
    lg: '0 18px 48px rgba(8, 15, 30, 0.13)',
    xl: '0 28px 72px rgba(8, 15, 30, 0.18)',
  },
  breakpoints: {
    xs: '36em',
    sm: '48em',
    md: '62em',
    lg: '75em',
    xl: '88em',
    w18: '1800px',
    w24: '2400px',
    w30: '3000px',
    w36: '3600px',
    w42: '4200px',
    w48: '4800px',
  },
  components: {
    Loader: Loader.extend({
      defaultProps: {
        type: 'bars',
      },
    }),
    Switch: Switch.extend({
      defaultProps: {
        size: 'md',
      },
      styles: {
        body: {
          alignItems: 'center',
        },
        labelWrapper: {
          display: 'flex',
        },
      },
    }),
    Modal: Modal.extend({
      defaultProps: {
        centered: true,
        radius: 'lg',
        overlayProps: { backgroundOpacity: 0.62, blur: 6 },
        styles: {
          title: {
            fontWeight: 'bold',
          },
        },
      },
    }),
    Drawer: Drawer.extend({
      defaultProps: {
        overlayProps: { backgroundOpacity: 0.62, blur: 6 },
      },
    }),
    Popover: Popover.extend({
      defaultProps: {
        withinPortal: true,
        shadow: 'lg',
      },
    }),
    ActionIcon: ActionIcon.extend({
      defaultProps: {
        size: 'lg',
        variant: 'subtle',
        radius: 'md',
      },
    }),
    Badge: Badge.extend({
      defaultProps: {
        variant: 'light',
        radius: 'sm',
      },
    }),
    Button: Button.extend({
      defaultProps: {
        radius: 'md',
      },
      styles: {
        root: {
          fontWeight: 680,
        },
      },
    }),
    Card: Card.extend({
      defaultProps: {
        radius: 'lg',
        withBorder: false,
      },
    }),
    Paper: Paper.extend({
      defaultProps: {
        radius: 'lg',
        withBorder: false,
      },
    }),
    Alert: Alert.extend({
      defaultProps: {
        radius: 'md',
        variant: 'light',
      },
    }),
    Accordion: Accordion.extend({
      defaultProps: {
        radius: 'md',
        variant: 'separated',
      },
    }),
    Anchor: Anchor.extend({
      defaultProps: {
        underline: 'hover',
      },
    }),
    Checkbox: Checkbox.extend({
      defaultProps: {
        radius: 'sm',
      },
    }),
    TextInput: TextInput.extend({ defaultProps: { radius: 'md' } }),
    PasswordInput: PasswordInput.extend({ defaultProps: { radius: 'md' } }),
    NumberInput: NumberInput.extend({ defaultProps: { radius: 'md' } }),
    Textarea: Textarea.extend({ defaultProps: { radius: 'md' } }),
    Select: Select.extend({ defaultProps: { radius: 'md' } }),
    MultiSelect: MultiSelect.extend({ defaultProps: { radius: 'md' } }),
    Pagination: Pagination.extend({ defaultProps: { radius: 'md' } }),
    Table: Table.extend({
      defaultProps: {
        highlightOnHover: true,
        verticalSpacing: 'xs',
      },
    }),
    Tabs: Tabs.extend({
      styles: {
        tab: {
          minHeight: 40,
          padding: 'var(--mantine-spacing-xs) var(--mantine-spacing-sm)',
          fontWeight: 650,
        },
      },
    }),
    Avatar: Avatar.extend({
      defaultProps: {
        color: 'brand',
      },
    }),
    Menu: Menu.extend({
      defaultProps: {
        radius: 'md',
        shadow: 'lg',
      },
      styles: {
        item: {
          fontWeight: 500,
        },
      },
    }),
    Code: Code.extend({
      styles: {
        root: {
          fontWeight: 500,
        },
      },
    }),
    Tooltip: Tooltip.extend({
      defaultProps: {
        withArrow: true,
      },
      classNames: tooltipClasses,
    }),
    TooltipFloating: TooltipFloating.extend({
      classNames: tooltipClasses,
    }),
  },
}

export enum ColorProvider {
  Managed = 'Managed',
  Default = 'Default',
  Custom = 'Custom',
}

export interface CustomColor {
  provider: ColorProvider
  color: string
}

export const useCustomColor = () => {
  const [customColor, setCustomColorInner] = useLocalStorage<CustomColor>({
    key: 'custom-theme',
    defaultValue: { provider: ColorProvider.Managed, color: '' } as CustomColor,
    getInitialValueInEffect: false,
    serialize: (value: CustomColor) => {
      if (value.provider === ColorProvider.Custom && /^#[0-9A-F]{6}$/i.test(value.color)) {
        return value.color
      } else if (value.provider === ColorProvider.Managed) {
        return ''
      } else {
        return 'brand'
      }
    },
    deserialize: (value?: string) => {
      if (typeof value !== 'string') return { provider: ColorProvider.Managed, color: '' }

      if (value === 'brand') {
        return { provider: ColorProvider.Default, color: '' }
      } else if (/^#[0-9A-F]{6}$/i.test(value)) {
        return { provider: ColorProvider.Custom, color: value }
      } else {
        return { provider: ColorProvider.Managed, color: '' }
      }
    },
  })

  const setCustomColor = (color: CustomColor) => {
    // validate custom color, do not save invalid values
    if (color.provider === ColorProvider.Custom && !/^#[0-9A-F]{6}$/i.test(color.color)) return

    setCustomColorInner(color)
  }

  // color: null for use platform color, 'brand' for default theme
  //        or hex color string for custom color
  return { customColor, setCustomColor }
}

export const useCustomTheme = () => {
  const { config } = useConfig()
  const { customColor } = useCustomColor()

  const resolveManaged = (color: string | null | undefined) => {
    return color && /^#[0-9A-F]{6}$/i.test(color) ? color : null
  }

  const [theme, setTheme] = useState<MantineThemeOverride>(createTheme(CustomTheme))

  useEffect(() => {
    if (customColor.provider === ColorProvider.Default) {
      setTheme(CustomTheme)
      return
    }

    const resolvedColor =
      customColor.provider === ColorProvider.Custom
        ? customColor.color
        : customColor.provider === ColorProvider.Managed
          ? resolveManaged(config.customTheme)
          : null

    if (resolvedColor) {
      setTheme({
        ...CustomTheme,
        colors: {
          ...CustomTheme.colors,
          custom: generateColors(resolvedColor),
          semanticAccent: buildSemanticAccentColors(resolvedColor),
        },
        components: {
          ...CustomTheme.components,
          Avatar: Avatar.extend({
            defaultProps: {
              color: 'custom',
            },
          }),
        },
        primaryColor: 'custom',
      })
    } else {
      setTheme(CustomTheme)
    }
  }, [customColor, config.customTheme])

  return { theme }
}

export const useIsMobile = (limit?: number) => {
  const theme = useMantineTheme()
  const isMobile = useMediaQuery(`(max-width: ${limit ? `${limit}px` : theme.breakpoints.sm})`)
  return isMobile
}

interface UseDisplayInputStylesProps {
  ff?: 'monospace' | 'text'
  fw?: React.CSSProperties['fontWeight']
  lh?: React.CSSProperties['lineHeight']
  cs?: React.CSSProperties['cursor']
}

export const useDisplayInputStyles = createStyles(
  (theme, { fw = 'normal', lh = '1.5rem', ff = 'text', cs = 'auto' }: UseDisplayInputStylesProps) => ({
    wrapper: {
      width: '100%',
    },
    input: {
      fontWeight: fw,
      fontFamily: ff === 'text' ? theme.fontFamily : theme.fontFamilyMonospace,
      height: lh,
      lineHeight: lh,
      cursor: cs,
      userSelect: 'none',
      minHeight: '1rem',
      maxHeight: '2rem',
    },
  })
)
