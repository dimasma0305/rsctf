import { Button, Divider, Stack } from '@mantine/core'
import { mdiGoogle } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { useSearchParams } from 'react-router'
import { useConfig } from '@Hooks/useConfig'

// @mdi/js dropped third-party brand glyphs, so inline the Discord mark (24x24 viewBox,
// matching what @mdi/react's <Icon> expects for its `path` prop).
const mdiDiscordPath =
  'M20.317 4.369a19.791 19.791 0 0 0-4.885-1.515.074.074 0 0 0-.079.037c-.21.375-.444.864-.608 1.25a18.27 18.27 0 0 0-5.487 0 12.64 12.64 0 0 0-.617-1.25.077.077 0 0 0-.079-.037A19.736 19.736 0 0 0 3.677 4.37a.07.07 0 0 0-.032.027C.533 9.046-.32 13.58.099 18.057a.082.082 0 0 0 .031.057 19.9 19.9 0 0 0 5.993 3.03.078.078 0 0 0 .084-.028 14.09 14.09 0 0 0 1.226-1.994.076.076 0 0 0-.041-.106 13.107 13.107 0 0 1-1.872-.892.077.077 0 0 1-.008-.128 10.2 10.2 0 0 0 .372-.292.074.074 0 0 1 .077-.01c3.928 1.793 8.18 1.793 12.062 0a.074.074 0 0 1 .078.01c.12.098.246.198.373.291a.077.077 0 0 1-.006.127 12.299 12.299 0 0 1-1.873.893.077.077 0 0 0-.041.106c.36.698.772 1.362 1.225 1.993a.076.076 0 0 0 .084.028 19.839 19.839 0 0 0 6.002-3.03.077.077 0 0 0 .032-.054c.5-5.177-.838-9.674-3.549-13.66a.061.061 0 0 0-.031-.03zM8.02 15.33c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.956-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.333-.956 2.418-2.157 2.418zm7.975 0c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.955-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.333-.946 2.418-2.157 2.418z'

/**
 * External OAuth sign-in buttons (Google / Discord). Renders nothing unless at least one
 * provider is configured server-side (config.enableGoogleAuth / enableDiscordAuth). Each
 * button is a full-page navigation to the backend challenge endpoint — the OAuth redirect
 * dance and session cookie are handled entirely server-side, after which the user lands
 * back on `from` already signed in.
 */
export const OAuthButtons: FC = () => {
  const { config } = useConfig()
  const { t } = useTranslation()
  const params = useSearchParams()[0]

  if (!config.enableGoogleAuth && !config.enableDiscordAuth) return null

  const from = params.get('from') ?? '/'
  const go = (provider: string) => {
    window.location.href = `/api/oauth/${provider}?returnUrl=${encodeURIComponent(from)}`
  }

  return (
    <Stack gap="xs" w="100%">
      <Divider w="100%" label={t('account.oauth.divider', 'or continue with')} labelPosition="center" />
      {config.enableGoogleAuth && (
        <Button
          fullWidth
          variant="default"
          leftSection={<Icon path={mdiGoogle} size={0.9} />}
          onClick={() => go('google')}
        >
          {t('account.oauth.google', 'Continue with Google')}
        </Button>
      )}
      {config.enableDiscordAuth && (
        <Button
          fullWidth
          variant="default"
          leftSection={<Icon path={mdiDiscordPath} size={0.9} />}
          onClick={() => go('discord')}
        >
          {t('account.oauth.discord', 'Continue with Discord')}
        </Button>
      )}
    </Stack>
  )
}
