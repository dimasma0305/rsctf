import { ActionIcon, Alert, Badge, Button, Card, Group, Select, Stack, Text, Title, Tooltip } from '@mantine/core'
import {
  mdiAccountGroupOutline,
  mdiAlertCircleOutline,
  mdiArrowLeftBold,
  mdiArrowRightBold,
  mdiCodeBraces,
  mdiDownload,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { Empty } from '@Components/Empty'
import { WithGameMonitor } from '@Components/WithGameMonitor'
import { aiChatDelayLabel } from '@Components/monitor/AiChatHistoryModal'
import { downloadBlob } from '@Utils/ApiHelper'
import { useLanguage } from '@Utils/I18n'
import { HunamizeSize, tryGetErrorMsg } from '@Utils/Shared'
import { useGameScoreboardRead } from '@Hooks/useGame'
import api, { ChallengeType, SolverUploadRecord, SolverUploadVersion } from '@Api'

const ITEM_COUNT_PER_PAGE = 50

const JEOPARDY_TYPES: ReadonlySet<ChallengeType> = new Set([
  ChallengeType.StaticAttachment,
  ChallengeType.StaticContainer,
  ChallengeType.DynamicAttachment,
  ChallengeType.DynamicContainer,
])

const Solvers: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const { t } = useTranslation()
  const { locale } = useLanguage()

  const [activePage, setPage] = useState(1)
  const [challengeFilter, setChallengeFilter] = useState<string | null>(null)
  const challengeId = challengeFilter ? Number(challengeFilter) : undefined
  const [downloading, setDownloading] = useState(false)

  const { data, error, isLoading, mutate } = api.game.useGameListSolverUploads(
    numId,
    { count: ITEM_COUNT_PER_PAGE, skip: (activePage - 1) * ITEM_COUNT_PER_PAGE, challengeId },
    { refreshInterval: 0, revalidateOnFocus: false, keepPreviousData: true },
    numId > 0
  )
  const { scoreboard } = useGameScoreboardRead(numId)

  const challengeOptions = useMemo(() => {
    const options = new Map<string, string>()
    for (const [category, challenges] of Object.entries(scoreboard?.challenges ?? {})) {
      for (const challenge of challenges) {
        if (JEOPARDY_TYPES.has(challenge.type)) options.set(String(challenge.id), `${challenge.title} · ${category}`)
      }
    }
    for (const item of data?.items ?? []) {
      options.set(String(item.challengeId), `${item.challengeTitle} · ${item.category}`)
    }
    if (challengeFilter && !options.has(challengeFilter)) options.set(challengeFilter, `#${challengeFilter}`)
    return [...options.entries()]
      .map(([value, label]) => ({ value, label }))
      .sort((a, b) => a.label.localeCompare(b.label, locale))
  }, [challengeFilter, data?.items, locale, scoreboard?.challenges])

  const total = data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / ITEM_COUNT_PER_PAGE))
  const formatTime = (time: number) => dayjs(time).locale(locale).format('SL LTS')

  const onDownload = (record: SolverUploadRecord, version: SolverUploadVersion) =>
    downloadBlob(
      `monitor:solver:${version.id}`,
      () => api.game.gameDownloadSolverUpload(numId, version.id, { format: 'blob' }),
      setDownloading,
      t,
      `${record.teamName}-${record.challengeTitle}-v${version.version}-${version.fileName}`
    )

  const versionRow = (record: SolverUploadRecord, version: SolverUploadVersion) => (
    <Group key={version.id} component="li" gap="xs" wrap="nowrap" justify="space-between" align="flex-start">
      <Stack gap={2} style={{ minWidth: 0, flex: 1 }}>
        <Group gap={6} wrap="wrap">
          <Badge size="sm" variant="light" tt="none">
            {t('game.solver.version', 'v{{version}}', { version: version.version })}
          </Badge>
          <Text size="sm" fw={600} style={{ overflowWrap: 'anywhere', minWidth: 0 }}>
            {version.fileName}
          </Text>
          <Text size="xs" c="dimmed">
            {HunamizeSize(version.sizeBytes)}
          </Text>
        </Group>
        <Text size="xs" c="dimmed">
          <time dateTime={new Date(version.uploadedAt).toISOString()}>{formatTime(version.uploadedAt)}</time>
          {version.uploadedBy && ` · ${t('game.solver.uploaded_by', 'by {{user}}', { user: version.uploadedBy })}`}
          {version.secondsSinceSolve !== null && ` · ${aiChatDelayLabel(t, version.secondsSinceSolve)}`}
        </Text>
        <Text size="xs" c="dimmed" ff="monospace" style={{ overflowWrap: 'anywhere' }}>
          SHA-256 {version.sha256}
        </Text>
      </Stack>
      <Tooltip label={t('game.solver.download', 'Download')}>
        <ActionIcon
          variant="subtle"
          color="gray"
          size="lg"
          disabled={downloading}
          aria-label={t('game.solver.download_label', 'Download {{file}} version {{version}}', {
            file: version.fileName,
            version: version.version,
          })}
          onClick={() => void onDownload(record, version)}
        >
          <Icon path={mdiDownload} size={0.75} aria-hidden="true" />
        </ActionIcon>
      </Tooltip>
    </Group>
  )

  const recordCard = (record: SolverUploadRecord) => (
    <Card key={`${record.participationId}:${record.challengeId}`} shadow="sm" p="sm" component="li" data-solver-record>
      <Stack gap="xs">
        <Group justify="space-between" gap="xs" wrap="wrap" align="flex-start">
          <Stack gap={2} style={{ minWidth: 0, flex: '1 1 14rem' }}>
            <Text fw={650} style={{ overflowWrap: 'anywhere' }}>
              {record.challengeTitle}
            </Text>
            <Group gap={6} wrap="wrap">
              <Badge size="sm" variant="light" tt="none">
                {record.category}
              </Badge>
              <Group gap={4} wrap="nowrap" style={{ minWidth: 0 }}>
                <Icon path={mdiAccountGroupOutline} size={0.7} aria-hidden="true" />
                <Text size="sm" fw={600} style={{ overflowWrap: 'anywhere' }}>
                  <Text span size="sm" c="dimmed">
                    {t('game.solver.team', 'Team')}:{' '}
                  </Text>
                  {record.teamName}
                </Text>
              </Group>
            </Group>
          </Stack>
          <Stack gap={2} align="flex-end">
            <Badge size="sm" variant="light" color="teal" tt="none">
              {t('game.solver.version_count', '{{count}} versions', { count: record.versions.length })}
            </Badge>
            {record.solvedAt !== null && (
              <Text size="xs" c="dimmed">
                {t('game.solver.solved_at', 'Solved {{time}}', { time: formatTime(record.solvedAt) })}
              </Text>
            )}
          </Stack>
        </Group>
        <Stack gap={8} component="ul" m={0} p={0} style={{ listStyle: 'none' }}>
          {record.versions.map((version) => versionRow(record, version))}
        </Stack>
      </Stack>
    </Card>
  )

  return (
    <WithGameMonitor isLoading={isLoading && !data}>
      <Stack gap="md" w="100%">
        <Group justify="space-between" align="flex-end" gap="sm" wrap="wrap">
          <Stack gap={2} style={{ minWidth: 0, flex: '1 1 16rem' }}>
            <Title order={2}>{t('game.solver.title', 'Solver uploads')}</Title>
            <Text size="xs" c="dimmed">
              {t(
                'game.solver.subtitle',
                'Solvers teams uploaded after solving, most recent first. Every version is kept; files download as attachments and are never opened in the browser.'
              )}
            </Text>
          </Stack>
          <Select
            label={t('game.solver.challenge_filter', 'Challenge')}
            placeholder={t('game.solver.all_challenges', 'All challenges')}
            data={challengeOptions}
            value={challengeFilter}
            searchable
            clearable
            nothingFoundMessage={t('game.solver.no_challenge_match', 'No matching challenge')}
            onChange={(value) => {
              setChallengeFilter(value)
              setPage(1)
            }}
            w={{ base: '100%', xs: 280 }}
          />
        </Group>

        {error && (
          <Alert color="red" icon={<Icon path={mdiAlertCircleOutline} size={0.9} aria-hidden="true" />}>
            <Group justify="space-between" gap="sm">
              <Text size="sm">{tryGetErrorMsg(error, t)}</Text>
              <Button size="compact-sm" variant="default" onClick={() => void mutate()}>
                {t('common.button.retry', 'Retry')}
              </Button>
            </Group>
          </Alert>
        )}

        {data && data.items.length === 0 ? (
          <Empty
            bordered
            mdiPath={mdiCodeBraces}
            title={t('game.solver.empty_title', 'No solvers uploaded yet')}
            description={
              challengeFilter
                ? t('game.solver.empty_filtered', 'No records match this filter.')
                : t(
                    'game.solver.empty_description',
                    'Solvers appear here after teams upload them to solved challenges.'
                  )
            }
          />
        ) : (
          <Stack
            gap="xs"
            component="ul"
            m={0}
            p={0}
            style={{ listStyle: 'none' }}
            aria-label={t('game.solver.list_label', 'Solver upload records')}
          >
            {(data?.items ?? []).map(recordCard)}
          </Stack>
        )}

        <Group justify="space-between" gap="sm" wrap="wrap">
          <Text size="sm" c="dimmed" role="status" aria-live="polite">
            {t('game.solver.page_status', 'Page {{page}} of {{pages}} · {{total}} records', {
              page: activePage,
              pages: totalPages,
              total,
            })}
          </Text>
          <Group gap="xs">
            <ActionIcon
              size="lg"
              aria-label={t('common.pagination.previous', 'Previous page')}
              disabled={activePage <= 1}
              onClick={() => setPage(activePage - 1)}
            >
              <Icon path={mdiArrowLeftBold} size={1} aria-hidden="true" />
            </ActionIcon>
            <ActionIcon
              size="lg"
              aria-label={t('common.pagination.next', 'Next page')}
              disabled={activePage >= totalPages}
              onClick={() => setPage(activePage + 1)}
            >
              <Icon path={mdiArrowRightBold} size={1} aria-hidden="true" />
            </ActionIcon>
          </Group>
        </Group>
      </Stack>
    </WithGameMonitor>
  )
}

export default Solvers
