import { Anchor, Code, Divider, List, Text } from '@mantine/core'
import { useLocalStorage } from '@mantine/hooks'
import { modals } from '@mantine/modals'
import dayjs from 'dayjs'
import 'dayjs/locale/de'
import 'dayjs/locale/fr'
import 'dayjs/locale/id'
import 'dayjs/locale/ja'
import 'dayjs/locale/ko'
import 'dayjs/locale/ru'
import 'dayjs/locale/vi'
import 'dayjs/locale/zh'
import 'dayjs/locale/zh-tw'
import localizedFormat from 'dayjs/plugin/localizedFormat'
import { PropsWithChildren, createContext, useCallback, useContext, useEffect, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { RSCTF_REPOSITORY } from '@Hooks/useConfig'

dayjs.extend(localizedFormat)

export const LanguageMap = {
  'en-US': '🇺🇸 English',
  'zh-CN': '🇨🇳 简体中文',
  'zh-TW': '🇨🇳 繁體中文',
  'ja-JP': '🇯🇵 日本語',
  'id-ID': '🇮🇩 Bahasa',
  'ko-KR': '🇰🇷 한국어',
  'ru-RU': '🇷🇺 Русский',
  'vi-VN': '🇻🇳 Tiếng việt',
  'de-DE': '🇩🇪 Deutsch (MT)',
  'fr-FR': '🇫🇷 Français (MT)',
  'es-ES': '🇪🇸 Español (MT)',
}

interface ExtraLocalFormat {
  SL: string
  SLL: string
  SMY: string
}

const shortLocalFormat = new Map<string, ExtraLocalFormat>([
  ['en', { SL: 'MM/DD', SLL: 'YY/MM/DD', SMY: 'MMMM, YYYY' }],
  ['zh', { SL: 'MM/DD', SLL: 'YY/MM/DD', SMY: 'YYYY年MMM' }],
  ['ja', { SL: 'MM/DD', SLL: 'YY/MM/DD', SMY: 'YYYY年MMM' }],
  ['ko', { SL: 'MM/DD', SLL: 'YY/MM/DD', SMY: 'YYYY년 MMMM' }],
  ['ru', { SL: 'DD.MM', SLL: 'DD.MM.YY', SMY: 'MMMM YYYY г.' }],
  ['de', { SL: 'DD.MM', SLL: 'DD.MM.YY', SMY: 'MMMM YYYY' }],
  ['id', { SL: 'DD/MM', SLL: 'DD/MM/YY', SMY: 'MMMM YYYY' }],
  ['fr', { SL: 'DD/MM', SLL: 'DD/MM/YY', SMY: 'MMMM YYYY' }],
  ['es', { SL: 'DD/MM', SLL: 'DD/MM/YY', SMY: 'MMMM [de] YYYY' }],
  ['vi', { SL: 'DD/MM', SLL: 'DD/MM/YY', SMY: 'MMMM [năm] YYYY' }],
])

dayjs.extend((_o, c, _d) => {
  const proto = c.prototype
  const oldFormat = proto.format

  proto.format = function (fmt: string) {
    const locale = this.locale().split('-')[0]
    const shortLocal = shortLocalFormat.get(locale)
    if (shortLocal) {
      fmt = fmt
        .replace(/SL{1,2}/g, (a) => {
          return shortLocal[a as keyof ExtraLocalFormat]
        })
        .replace(/SMY/g, shortLocal.SMY)
    }
    return oldFormat.call(this, fmt)
  }
})

export const defaultLanguage = 'en-US'
export let apiLanguage: string = defaultLanguage
export type SupportedLanguages = keyof typeof LanguageMap

const supportedLanguages = Object.keys(LanguageMap) as SupportedLanguages[]

interface LanguageContextValue {
  language: SupportedLanguages
  locale: string
  setLanguage: (lang: SupportedLanguages) => void
  supportedLanguages: SupportedLanguages[]
}

const LanguageContext = createContext<LanguageContextValue | undefined>(undefined)

export const LanguageProvider = ({ children }: PropsWithChildren) => {
  const { t, i18n } = useTranslation()

  const [language, setLanguageInner] = useLocalStorage<SupportedLanguages>({
    key: 'language',
    defaultValue: i18n.language as SupportedLanguages,
    getInitialValueInEffect: false,
  })

  useEffect(() => {
    i18n.changeLanguage(language)
    apiLanguage = language
    const pageLang = language.toLowerCase()
    dayjs.locale(pageLang)
    document.documentElement.setAttribute('lang', pageLang)
  }, [language])

  const setLanguage = useCallback(
    (lang: SupportedLanguages) => {
      // check if language is supported
      if (supportedLanguages.includes(lang)) {
        setLanguageInner(lang)

        const isMT = LanguageMap[lang].includes('(MT)')
        const isWIP = LanguageMap[lang].includes('(WIP)')

        if (!isMT && !isWIP) return

        modals.openConfirmModal({
          w: '30vw',
          maw: '30rem',
          title: (
            <Text fw="bold">
              {isMT
                ? t('common.content.language.mt.title', '🤖 Machine Translation')
                : t('common.content.language.wip.title', '🚀 Incomplete Translation')}
            </Text>
          ),
          children: (
            <>
              <Text>
                {isMT
                  ? t(
                      'common.content.language.mt.description',
                      'This translation is done by machine and AIs, it may not be accurate.'
                    )
                  : t(
                      'common.content.language.wip.description',
                      'This language is still in progress, some parts may not be translated.'
                    )}
              </Text>
              <Divider my={10} />
              <Text>{t('common.content.language.help', 'If you want to help with the translation:')}</Text>
              <List>
                <List.Item>
                  <Text>
                    {t('common.content.language.current', 'Current Language:')} <Code>{lang}</Code>{' '}
                    <Text span size="sm">
                      {LanguageMap[lang]}
                    </Text>
                  </Text>
                </List.Item>
                <List.Item>
                  {t('common.content.language.contact', 'Contact us on')}{' '}
                  <Anchor href={RSCTF_REPOSITORY} target="_blank" rel="noreferrer">
                    GitHub
                  </Anchor>
                </List.Item>
                <List.Item>
                  {t('common.content.language.track', 'Track the progress on')}{' '}
                  <Anchor href={`${RSCTF_REPOSITORY}/issues`} target="_blank" rel="noreferrer">
                    GitHub Issues
                  </Anchor>
                </List.Item>
              </List>
            </>
          ),
          confirmProps: { color: undefined },
          labels: {
            confirm: t('common.button.confirm', 'Confirm'),
            cancel: t('common.content.language.switch_to_english', 'Switch to English'),
          },
          onCancel: () => setLanguage('en-US'),
        })
      } else {
        console.warn(`Language ${lang} is not supported, fallback to ${defaultLanguage}`)
        setLanguageInner(defaultLanguage)
      }
    },
    [setLanguageInner, t]
  )

  const contextValue = useMemo(
    () => ({ language, locale: language.split('-')[0], setLanguage, supportedLanguages }),
    [language, setLanguage]
  )

  return <LanguageContext.Provider value={contextValue}>{children}</LanguageContext.Provider>
}

export const useLanguage = () => {
  const context = useContext(LanguageContext)
  if (!context) {
    throw new Error('useLanguage must be used within a LanguageProvider')
  }
  return context
}

export const normalizeLanguage = (language: string) => language.toUpperCase().replace(/[_-].*/, '')

export const convertLanguage = (language: string): SupportedLanguages => {
  const normalizedLanguage = normalizeLanguage(language)

  const matchedLanguage = Object.keys(LanguageMap).filter((lang) => normalizeLanguage(lang) === normalizedLanguage)
  if (matchedLanguage.length > 0) {
    return matchedLanguage.at(0) as SupportedLanguages
  }

  return defaultLanguage
}
