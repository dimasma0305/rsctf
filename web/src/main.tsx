import { App } from '@App'
import i18n from 'i18next'
import LanguageDetector from 'i18next-browser-languagedetector'
import resourcesToBackend from 'i18next-resources-to-backend'
import ReactDOM from 'react-dom/client'
import { initReactI18next } from 'react-i18next'
import { BrowserRouter } from 'react-router'
import manifest from 'virtual:i18n-manifest'
import { installClipboardPolyfill } from '@Utils/clipboardPolyfill'
import { convertLanguage, LanguageProvider } from '@Utils/I18n'

// Browsers gate navigator.clipboard behind a secure context (HTTPS or
// localhost). Plain HTTP deploys like 1pc.tf:8080 lose every Mantine
// CopyButton silently — install the execCommand fallback before any
// Mantine code runs.
installClipboardPolyfill()

i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .use(
    // implement by custom vite plugin, see plugins/vite-i18n-virtual-manifest.ts
    resourcesToBackend(async (lang: string, _: string) => {
      const file = manifest[lang.toLowerCase()]
      if (!file) return {}
      const response = await fetch(`/static/${file}`)
      return response.json()
    })
  )
  .init({
    // Fall back to en-US (the source-of-truth locale) for any key missing in the
    // active language — NOT to convertLanguage, which returns the same active code
    // and so leaves missing keys rendering as raw dotted key strings.
    fallbackLng: 'en-US',
    interpolation: {
      escapeValue: false,
    },
    detection: {
      convertDetectedLanguage: convertLanguage,
    },
  })

const app = ReactDOM.createRoot(document.getElementById('root')!)

app.render(
  <BrowserRouter>
    <LanguageProvider>
      <App />
    </LanguageProvider>
  </BrowserRouter>
)
