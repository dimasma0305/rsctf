import { Button, Group, Stack, Text } from '@mantine/core'
import { useCallback, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useChallengePolling } from '@Hooks/useChallengePolling'
import { assertJsonResponse } from '@Utils/ChallengePolling'
import { httpErrorStatus } from '@Utils/ProfileRetry'
import api from '@Api'

type Cursor = { afterTeam: number; afterRevision: number }

export function AdPatchArchive({ gameId, challengeId }: { gameId: number; challengeId: number }) {
  const { t } = useTranslation()
  const [cursor, setCursor] = useState<Cursor>()
  const request = useCallback(
    async (signal: AbortSignal) =>
      assertJsonResponse(await api.game.getAdPatchArchive(gameId, challengeId, cursor ?? {}, { signal })),
    [gameId, challengeId, cursor]
  )
  const { data, error, mutate, isValidating } = useChallengePolling({
    key: `/api/game/${gameId}/ad/challenges/${challengeId}/patch-archive/${cursor?.afterTeam ?? 0}/${cursor?.afterRevision ?? 0}`,
    request,
    active: true,
    refreshInterval: 0,
  })
  if (error && [401, 403, 404].includes(httpErrorStatus(error) ?? 0)) return null
  if (error)
    return <Button variant="subtle" onClick={() => void mutate()}>{t('game.ad.patch.archive_retry', 'Reload patch archive')}</Button>
  if (!data || (!data.revisions.length && !cursor)) return null
  const last = data.revisions.at(-1)
  return (
    <section>
      <Text fw={600}>{t('game.ad.patch.archive', 'Deployed patches')}</Text>
      <Stack gap="xs">
        <Text size="sm">{t('game.ad.patch.archive_help', 'Changes from the starter for files selected by the organizer.')}</Text>
        {data.revisions.map((row) => (
          <div key={`${row.teamId}:${row.revision}`}>
            <Text fw={600}>{t('game.ad.patch.archive_revision', '{{team}} · Revision {{revision}}', { team: row.teamName, revision: row.revision })}</Text>
            <ul>
              {row.changes.map((change) => (
                <li key={change.path}><code style={{ overflowWrap: 'anywhere' }}>{change.path}</code>{' · '}
                  {change.kind === 'Deleted' ? t('game.ad.patch.archive_deleted', 'Deleted') : t('game.ad.patch.archive_modified', 'Modified')}
                </li>
              ))}
            </ul>
          </div>
        ))}
        <Group>
          {cursor && <Button variant="subtle" onClick={() => setCursor(undefined)}>{t('game.ad.patch.archive_first', 'First page')}</Button>}
          {data.hasMore && last && <Button variant="light" disabled={isValidating} onClick={() => setCursor({ afterTeam: last.teamId, afterRevision: last.revision })}>{t('game.ad.patch.archive_next', 'Next page')}</Button>}
        </Group>
      </Stack>
    </section>
  )
}
