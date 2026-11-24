import { Button, Group, Stack, Text } from '@mantine/core'
import { useCallback, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { SWRConfig } from 'swr'
import api from '@Api'
import { AccessibleModal } from '@Components/AccessibleModal'
import { useChallengePolling } from '@Hooks/useChallengePolling'
import { useUser } from '@Hooks/useUser'
import { downloadBlob } from '@Utils/ApiHelper'
import { assertJsonResponse } from '@Utils/ChallengePolling'
import { useServerNow } from '@Utils/ServerClock'

type Props = { gameId: number; hostedSsh?: boolean } & ({ challengeId: number } | { serviceId: number })
const privateCache = { provider: () => new Map() }

export function ByocSourceHistory(props: Props) {
  const { t } = useTranslation()
  const [opened, setOpened] = useState(false)
  const title = props.hostedSsh ? t('game.ad.ssh.history', 'SSH edit history') : t('game.byoc.source.history', 'Source history')
  return <>
    <Button size="compact-xs" variant="subtle" onClick={() => setOpened(true)}>{title}</Button>
    {opened && <AccessibleModal opened onClose={() => setOpened(false)} title={title} size="lg">
      <PrivateHistory {...props} />
    </AccessibleModal>}
  </>
}

function PrivateHistory(props: Props) {
  const { user } = useUser()
  return <SWRConfig key={user?.userId} value={privateCache}><Pages {...props} /></SWRConfig>
}

function Pages(props: Props) {
  const [before, setBefore] = useState<number>()
  return <History key={before ?? 'latest'} {...props} before={before} setBefore={setBefore} />
}

function StaleReportWarning({ lastSeen }: { lastSeen: number }) {
  const { t } = useTranslation()
  const now = useServerNow()
  return now.valueOf() - lastSeen >= 3 * 60_000 ? <>{t('game.byoc.source.stale', 'Reports are out of date.')} </> : null
}

function RecoveryDownload({ game, challenge, revision, capture }: { game: number; challenge: number; revision: number; capture: string }) {
  const { t } = useTranslation()
  const [busy, setBusy] = useState(false)
  return <Button size="xs" variant="light" loading={busy} onClick={() => void downloadBlob(
    `ssh-recovery:${capture}`, () => api.game.downloadAdSshRecovery(game, challenge, revision, capture),
    setBusy, t, `ssh-revision-${revision}.zip`
  )}>{t('game.ad.patch.ssh_recover', 'Download starter with SSH edits (revision {{number}})', { number: revision })}</Button>
}

function History(props: Props & { before?: number, setBefore: (value?: number) => void }) {
  const { t } = useTranslation()
  const { before, setBefore } = props
  const { gameId } = props
  const admin = 'serviceId' in props
  const id = admin ? props.serviceId : props.challengeId
  const method = props.hostedSsh ? 'sshSourceHistory' : 'byocSourceHistory'
  const request = useCallback(async (signal: AbortSignal) => assertJsonResponse(await (
    admin ? api.admin[method](gameId, id, before, { signal }) : api.game[method](gameId, id, before, { signal })
  )), [admin, method, gameId, id, before])
  const { data, error, mutate } = useChallengePolling({
    key: `${method}:${admin ? 'admin' : 'team'}:${gameId}:${id}:${before ?? 'latest'}`,
    active: true, refreshInterval: before ? 0 : 60000, request,
  })
  if (error) return <Button onClick={() => void mutate().catch(() => {})}>{t('game.byoc.source.retry', 'Retry source history')}</Button>
  if (!data) return <Text role="status">{t('game.byoc.source.loading', 'Loading source history…')}</Text>
  const labels = {
    observed: props.hostedSsh ? t('game.ad.ssh.observed', 'SSH source observed') : t('game.byoc.source.observed', 'Watching challenge source'),
    unavailable: t('game.byoc.source.unavailable', 'Source could not be read'),
    notRegistered: t('game.byoc.source.unregistered', 'No source files registered'),
    legacyAgent: t('game.byoc.source.legacy', 'Agent version does not report source'),
  }
  const kinds = {
    present: t('game.byoc.source.present', 'Present'), missing: t('game.byoc.source.missing', 'Missing'),
    created: t('game.byoc.source.created', 'Created'), deleted: t('game.byoc.source.deleted', 'Deleted'),
    registered: t('game.byoc.source.registered', 'Started tracking'), unregistered: t('game.byoc.source.removed', 'Stopped tracking'),
    modified: t('game.byoc.source.modified', 'Modified'),
  }
  const latest = data.events[0]
  return <Stack gap="sm">
    {!latest && <Text>{props.hostedSsh
      ? t('game.ad.ssh.empty', 'No SSH edits have been recorded. Source files must be registered before kickoff.')
      : t('game.byoc.source.empty', 'No agent reports received yet.')}</Text>}
    {!before && latest && <Text size="sm" role="status">
      {!props.hostedSsh && <StaleReportWarning lastSeen={latest.lastSeen} />}
      {t('game.byoc.source.last', 'Last report {{time}}', { time: new Date(latest.lastSeen).toLocaleString() })}
    </Text>}
    {latest?.status === 'observed' && latest.trackedFiles === 0 && <Text size="sm">{t('game.byoc.source.waiting', 'Waiting for source files in the watched directory.')}</Text>}
    {latest?.status === 'notRegistered' && <Text size="sm">{t('game.byoc.source.setup', 'New BYOC bundles enable source history on startup. If you use .rsctf-files, list the paths you want to track.')}</Text>}
    {data.events.map(event => <div key={event.revision}>
      <Text size="sm" fw={600}>{labels[event.status]}</Text>
      <Text size="xs" c="dimmed">{new Date(event.firstSeen).toLocaleString()}</Text>
      {!!event.changes.length && <details>
        <summary>{t('game.byoc.source.files', '{{count}} file(s)', { count: event.changes.length })}</summary>
        {event.changes.map(change => <Text key={change.path} size="sm" style={{ overflowWrap: 'anywhere' }}>
          {kinds[change.kind]} <code>{change.path}</code>
        </Text>)}
      </details>}
      {props.hostedSsh && !admin && event.recoveryId && <RecoveryDownload
        game={gameId} challenge={id} revision={event.revision} capture={event.recoveryId}
      />}
    </div>)}
    <Group>
      {before !== undefined && <Button size="xs" variant="subtle" onClick={() => setBefore(undefined)}>{t('game.byoc.source.latest', 'Latest reports')}</Button>}
      {data.before !== null && <Button size="xs" variant="subtle" onClick={() => setBefore(data.before ?? undefined)}>{t('game.byoc.source.older', 'Older reports')}</Button>}
    </Group>
  </Stack>
}
