import {
  Alert,
  Badge,
  Box,
  Button,
  Group,
  Loader,
  ScrollArea,
  Stack,
  Table,
  Tabs,
  Text,
  ThemeIcon,
  Title,
  VisuallyHidden,
} from '@mantine/core'
import { mdiAlertCircle, mdiChartBox, mdiFlagVariant, mdiRefresh, mdiShieldSearch } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams, useSearchParams } from 'react-router'
import { WithGameMonitor } from '@Components/WithGameMonitor'
import { CheatInfo } from '@Components/monitor/CheatInfo'
import { CheatSubmissionLog } from '@Components/monitor/CheatSubmissionLog'
import { isCheatReportStale, normalizeCheatViewTab } from '@Utils/AntiCheat'
import { tryGetErrorMsg } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { useAntiCheatReport } from '@Hooks/useAntiCheatReport'
import { useUser } from '@Hooks/useUser'
import { DetectorCapability, Role } from '@Api'

const DETECTOR_STATUS_COLORS: Record<DetectorCapability['status'], string> = {
  active: 'green',
  background: 'blue',
  telemetryOnly: 'yellow',
  unimplemented: 'gray',
}

const detectorStatusFallback: Record<DetectorCapability['status'], string> = {
  active: 'Active',
  background: 'Background',
  telemetryOnly: 'Telemetry only',
  unimplemented: 'Unavailable',
}

const detectorScopeFallback: Record<DetectorCapability['scope'], string> = {
  allGames: 'All games',
  jeopardy: 'Jeopardy',
  jeopardyContainers: 'Jeopardy containers',
  platform: 'Platform',
}

const CheatCheck: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id!)
  const { t, i18n } = useTranslation()
  const { user } = useUser()
  const isMobile = useIsMobile()
  const [searchParams, setSearchParams] = useSearchParams()
  const activeTab = normalizeCheatViewTab(searchParams.get('tab'))

  const handleTabChange = (value: string | null) => {
    const next = new URLSearchParams(searchParams)
    next.set('tab', normalizeCheatViewTab(value))
    setSearchParams(next)
  }

  const { data: report, isLoading, isValidating, error, mutate } = useAntiCheatReport(numId, activeTab === 'analysis')
  const refresh = () => void mutate()
  const lastReconciledAt = report?.lastReconciledAt
  const reportIsStale = isCheatReportStale(lastReconciledAt)
  const formatReportTime = (value: number | null | undefined) =>
    value != null && Number.isFinite(value)
      ? new Intl.DateTimeFormat(i18n.resolvedLanguage, {
          dateStyle: 'medium',
          timeStyle: 'medium',
        }).format(new Date(value))
      : null
  const lastEvaluated = formatReportTime(lastReconciledAt) ?? t('game.content.cheat.not_evaluated', 'Not evaluated yet')
  const oldestPending = formatReportTime(report?.oldestPendingAt)
  const pendingJobs = report?.pendingJobs ?? 0
  const finalizing = report?.evidenceClosedAt != null && report?.sealedAt == null
  const capabilityCounts = report?.detectorCapabilities?.reduce(
    (counts, detector) => {
      counts[detector.status] += 1
      return counts
    },
    { active: 0, background: 0, telemetryOnly: 0, unimplemented: 0 }
  )

  if (isLoading && !report)
    return (
      <WithGameMonitor>
        <Stack align="center" justify="center" h="60vh" gap="md">
          <Loader size="lg" />
          <Text c="dimmed" size="sm">
            {t('game.content.cheat.loading', 'Loading cheat analysis…')}
          </Text>
        </Stack>
      </WithGameMonitor>
    )

  if (error && !report)
    return (
      <WithGameMonitor>
        <Alert
          color="alert"
          title={t('game.content.cheat.load_failed', 'Failed to load report')}
          icon={<Icon path={mdiAlertCircle} size={1} />}
        >
          <Stack gap="sm" align="flex-start">
            <Text size="sm">{tryGetErrorMsg(error, t)}</Text>
            <Button size="xs" variant="outline" color="red" onClick={refresh} loading={isValidating}>
              {t('common.button.retry', 'Retry')}
            </Button>
          </Stack>
        </Alert>
      </WithGameMonitor>
    )

  return (
    <WithGameMonitor>
      <Stack gap="md" w="100%">
        {/* ── Page header ──────────────────────── */}
        <Group justify="space-between" align="flex-start" gap="md" wrap="wrap">
          <Group gap="sm" align="center">
            <ThemeIcon size="lg" radius="md" variant="light" color="alert">
              <Icon path={mdiShieldSearch} size={0.9} aria-hidden />
            </ThemeIcon>
            <Box>
              <Title order={2}>{t('game.title.cheat_check', 'Cheat Analysis')}</Title>
              <Text size="xs" c="dimmed">
                {t(
                  'game.content.cheat.subtitle',
                  'Evidence signals for organizer review; no single heuristic proves cheating.'
                )}
              </Text>
            </Box>
          </Group>
          <Group gap="sm" align="center">
            {report?.sealedAt != null && (
              <Badge color="green" variant="light">
                {t('game.content.cheat.final_sealed', 'Final evidence sealed')}
              </Badge>
            )}
            {finalizing && (
              <Badge color="yellow" variant="light">
                {t('game.content.cheat.finalizing', 'Final reconciliation pending')}
              </Badge>
            )}
            <Text size="xs" c={reportIsStale ? 'yellow' : 'dimmed'} role="status" aria-live="polite" aria-atomic="true">
              {t('game.content.cheat.last_evaluated', 'Last evaluated: {{time}}', { time: lastEvaluated })}
            </Text>
            <Button
              size="xs"
              variant="outline"
              leftSection={<Icon path={mdiRefresh} size={0.7} aria-hidden />}
              onClick={refresh}
              loading={isValidating}
            >
              {t('common.button.refresh', 'Refresh')}
            </Button>
          </Group>
        </Group>

        {error && report && (
          <Alert
            color="yellow"
            role="alert"
            title={t('game.content.cheat.refresh_failed', 'Refresh failed — showing the last report')}
            icon={<Icon path={mdiAlertCircle} size={1} aria-hidden />}
          >
            {tryGetErrorMsg(error, t)}
          </Alert>
        )}

        {!error && report && reportIsStale && (
          <Alert
            color="yellow"
            role="alert"
            title={t('game.content.cheat.report_stale', 'This report is stale')}
            icon={<Icon path={mdiAlertCircle} size={1} aria-hidden />}
          >
            {t(
              'game.content.cheat.report_stale_detail',
              'Refresh the report before using its signals to make a participation decision.'
            )}
          </Alert>
        )}

        {report?.lastError && (
          <Alert
            color="red"
            role="alert"
            title={t('game.content.cheat.reconciliation_failed', 'Detector reconciliation failed')}
            icon={<Icon path={mdiAlertCircle} size={1} aria-hidden />}
          >
            <Text size="sm">{report.lastError}</Text>
          </Alert>
        )}

        {pendingJobs > 0 && (
          <Alert
            color="yellow"
            role="status"
            title={t('game.content.cheat.pending_jobs', '{{count}} evaluation jobs pending', {
              count: pendingJobs,
            })}
            icon={<Icon path={mdiAlertCircle} size={1} aria-hidden />}
          >
            {oldestPending
              ? t('game.content.cheat.oldest_pending', 'Oldest pending evidence: {{time}}', {
                  time: oldestPending,
                })
              : t(
                  'game.content.cheat.pending_jobs_detail',
                  'The report may change after durable evaluation catches up.'
                )}
          </Alert>
        )}

        {finalizing && pendingJobs === 0 && !report?.lastError && (
          <Alert
            color="blue"
            role="status"
            title={t('game.content.cheat.finalizing', 'Final reconciliation pending')}
            icon={<Icon path={mdiShieldSearch} size={1} aria-hidden />}
          >
            {t(
              'game.content.cheat.finalizing_detail',
              'Competitive evidence is closed while the immutable final detector snapshot completes.'
            )}
          </Alert>
        )}

        {capabilityCounts && (
          <Alert
            color={capabilityCounts.unimplemented > 0 || capabilityCounts.telemetryOnly > 0 ? 'yellow' : 'blue'}
            title={t('game.content.cheat.detector_coverage', 'Detector coverage')}
            icon={<Icon path={mdiShieldSearch} size={1} aria-hidden />}
          >
            <Group gap="xs" wrap="wrap">
              <Badge color="green" variant="light">
                {t('game.content.cheat.detectors_active', '{{count}} active', { count: capabilityCounts.active })}
              </Badge>
              <Badge color="blue" variant="light">
                {t('game.content.cheat.detectors_background', '{{count}} background', {
                  count: capabilityCounts.background,
                })}
              </Badge>
              <Badge color="yellow" variant="light">
                {t('game.content.cheat.detectors_telemetry', '{{count}} telemetry only', {
                  count: capabilityCounts.telemetryOnly,
                })}
              </Badge>
              <Badge color="gray" variant="light">
                {t('game.content.cheat.detectors_unavailable', '{{count}} unavailable', {
                  count: capabilityCounts.unimplemented,
                })}
              </Badge>
            </Group>
            <Text size="xs" mt="xs">
              {t(
                'game.content.cheat.detector_coverage_note',
                'Telemetry-only and unavailable rules cannot produce scored evidence in this report.'
              )}
            </Text>
            <details style={{ marginTop: 'var(--mantine-spacing-xs)' }}>
              <summary style={{ cursor: 'pointer' }}>
                <Text component="span" size="xs" fw={600}>
                  {t('game.content.cheat.detector_inventory_toggle', 'View detector inventory')}
                </Text>
              </summary>
              <ScrollArea
                h={260}
                mt="xs"
                viewportProps={{
                  role: 'region',
                  tabIndex: 0,
                  'aria-label': t('game.content.cheat.detector_inventory_region', 'Detector inventory'),
                }}
              >
                <Table striped withTableBorder miw="48rem">
                  <Table.Caption>
                    <VisuallyHidden>
                      {t(
                        'game.content.cheat.detector_inventory_caption',
                        'Anti-cheat detector implementation and scoring coverage'
                      )}
                    </VisuallyHidden>
                  </Table.Caption>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th scope="col">{t('game.content.cheat.detector_code', 'Detector')}</Table.Th>
                      <Table.Th scope="col">{t('game.content.cheat.detector_status', 'Status')}</Table.Th>
                      <Table.Th scope="col">{t('game.content.cheat.detector_scope', 'Scope')}</Table.Th>
                      <Table.Th scope="col">{t('game.content.cheat.detector_detail', 'Details')}</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {report?.detectorCapabilities?.map((detector) => (
                      <Table.Tr key={detector.code}>
                        <Table.Td>
                          <Text size="xs" ff="monospace">
                            {detector.code}
                          </Text>
                        </Table.Td>
                        <Table.Td>
                          <Badge size="xs" variant="light" color={DETECTOR_STATUS_COLORS[detector.status]}>
                            {t(
                              `game.content.cheat.detector_status_value.${detector.status}`,
                              detectorStatusFallback[detector.status]
                            )}
                          </Badge>
                        </Table.Td>
                        <Table.Td>
                          <Text size="xs">
                            {t(
                              `game.content.cheat.detector_scope_value.${detector.scope}`,
                              detectorScopeFallback[detector.scope]
                            )}
                          </Text>
                        </Table.Td>
                        <Table.Td>
                          <Text size="xs" c="dimmed">
                            {detector.detail ?? t('common.label.not_available', 'Not provided')}
                          </Text>
                        </Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </ScrollArea>
            </details>
          </Alert>
        )}

        {/* ── Top-level tabs ────────────────────── */}
        <Tabs value={activeTab} onChange={handleTabChange} variant="pills" radius="md" keepMounted={false}>
          <Tabs.List
            grow
            style={{
              borderBottom: '1px solid light-dark(var(--mantine-color-gray-2), var(--mantine-color-dark-5))',
              flexWrap: 'nowrap',
              paddingBottom: 4,
              marginBottom: 8,
            }}
          >
            <Tabs.Tab value="analysis" leftSection={<Icon path={mdiChartBox} size={0.85} />}>
              {isMobile
                ? t('game.tab.cheat.analysis_short', 'Analysis')
                : t('game.tab.cheat.analysis', 'Anomaly Analysis')}
            </Tabs.Tab>
            <Tabs.Tab value="submissions" leftSection={<Icon path={mdiFlagVariant} size={0.85} />}>
              {isMobile
                ? t('game.tab.cheat.submissions_short', 'Submissions')
                : t('game.tab.cheat.submissions', 'Submissions & Flags')}
            </Tabs.Tab>
          </Tabs.List>

          <Tabs.Panel value="analysis" pt="xs">
            <CheatInfo report={report || null} mutate={mutate} canManageParticipations={user?.role === Role.Admin} />
          </Tabs.Panel>

          <Tabs.Panel value="submissions" pt="xs">
            <CheatSubmissionLog gameId={numId} active={activeTab === 'submissions'} />
          </Tabs.Panel>
        </Tabs>
      </Stack>
    </WithGameMonitor>
  )
}

export default CheatCheck
