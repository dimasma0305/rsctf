import { Button, Stack, Text } from '@mantine/core'
import { useCallback } from 'react'
import { useTranslation } from 'react-i18next'
import { assertJsonResponse } from '@Utils/ChallengePolling'
import { httpErrorStatus } from '@Utils/ProfileRetry'
import { useChallengePolling } from '@Hooks/useChallengePolling'
import api from '@Api'

export function AdPatchActivity({ gameId, challengeId }: { gameId: number; challengeId: number }) {
  const { t } = useTranslation()
  const request = useCallback(
    async (signal: AbortSignal) =>
      assertJsonResponse(await api.game.getAdPatchActivity(gameId, challengeId, { signal })),
    [gameId, challengeId]
  )
  const { data, error, mutate, isValidating } = useChallengePolling({
    key: `/api/game/${gameId}/ad/challenges/${challengeId}/patch-activity`,
    active: true,
    request,
    refreshInterval: 0,
  })
  if (error && [401, 403, 404].includes(httpErrorStatus(error) ?? 0)) return null
  if (error)
    return (
      <Button variant="subtle" onClick={() => void mutate()}>
        {t('game.ad.patch.activity_retry', 'Reload patch activity')}
      </Button>
    )
  if (!data) return null
  return (
    <details>
      <summary>{t('game.ad.patch.activity', 'Patch activity across teams')}</summary>
      <Stack gap="xs">
        <Text size="sm">
          {t('game.ad.patch.activity_help', 'Successful ZIP deployments appear after one additional round finishes.')}
        </Text>
        {data.length ? (
          <ul>
            {data.map((row) => (
              <li key={row.round}>
                {t('game.ad.patch.activity_round', 'Round {{round}} · {{deployments}} deployed', row)}
              </li>
            ))}
          </ul>
        ) : (
          <Text size="sm">{t('game.ad.patch.activity_empty', 'No rounds are available yet.')}</Text>
        )}
        <Button size="compact-xs" variant="subtle" loading={isValidating} onClick={() => void mutate()}>
          {t('game.ad.patch.activity_refresh', 'Refresh activity')}
        </Button>
      </Stack>
    </details>
  )
}
