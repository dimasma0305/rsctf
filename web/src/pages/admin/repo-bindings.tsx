import {
  ActionIcon,
  Skeleton,
  Alert,
  Anchor,
  Badge,
  Box,
  Button,
  Center,
  Code,
  Container,
  Group,
  Loader,
  NumberInput,
  Pagination,
  Paper,
  SimpleGrid,
  Stack,
  Switch,
  Text,
  TextInput,
  Title,
  Tooltip,
} from '@mantine/core'
import { useMediaQuery } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import {
  mdiAlertCircleOutline,
  mdiCheck,
  mdiClockOutline,
  mdiDeleteOutline,
  mdiPause,
  mdiPlay,
  mdiPlus,
  mdiRefresh,
  mdiSourceBranch,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import relativeTime from 'dayjs/plugin/relativeTime'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import useSWR from 'swr'
import { AccessibleModal } from '@Components/AccessibleModal'
import { AdminPage } from '@Components/admin/AdminPage'
import { apiCollectionPageCount, apiCollectionView, decodeApiCollection } from '@Utils/ApiCollection'
import { showErrorMsg } from '@Utils/Shared'
import api, { RepoBindingInfoModel, RepoBindingScanHistoryModel, RepoBindingScanResultModel } from '@Api'
import classes from '@Styles/AdminOperations.module.css'

dayjs.extend(relativeTime)

const BINDING_PAGE_SIZE = 20
const HISTORY_PAGE_SIZE = 20

interface ResponsivePaginationProps {
  value: number
  onChange: (page: number) => void
  total: number
  label: string
}

const ResponsivePagination: FC<ResponsivePaginationProps> = ({ value, onChange, total, label }) => {
  const { t } = useTranslation()
  const compact = useMediaQuery('(max-width: 35.99em)', false, { getInitialValueInEffect: false })

  return (
    <Box component="nav" aria-label={label} w="100%" maw="100%">
      <Pagination.Root
        value={value}
        onChange={onChange}
        total={total}
        siblings={compact ? 0 : 1}
        boundaries={compact ? 0 : 1}
        size="sm"
        getItemProps={(page) => ({
          'aria-label': t('common.pagination.page', {
            defaultValue: 'Page {{page}}',
            page,
          }),
        })}
      >
        <Group justify={compact ? 'center' : 'flex-end'} gap="xs" wrap="nowrap">
          <Pagination.Previous
            aria-label={t('common.pagination.previous', 'Previous page')}
            title={t('common.pagination.previous', 'Previous page')}
          />
          {compact ? (
            <Text size="sm" fw={700} miw="5.75rem" ta="center" aria-live="polite">
              {t('common.pagination.page_of', {
                defaultValue: 'Page {{page}} of {{total}}',
                page: value,
                total,
              })}
            </Text>
          ) : (
            <Pagination.Items />
          )}
          <Pagination.Next
            aria-label={t('common.pagination.next', 'Next page')}
            title={t('common.pagination.next', 'Next page')}
          />
        </Group>
      </Pagination.Root>
    </Box>
  )
}

const RepoBindings: FC = () => {
  const { t } = useTranslation()
  const [bindingPage, setBindingPage] = useState(1)
  const [bindingKnownPageCount, setBindingKnownPageCount] = useState<number>()
  const bindingQuery = { count: BINDING_PAGE_SIZE, skip: (bindingPage - 1) * BINDING_PAGE_SIZE }
  // Idle pages never poll. A claimed scan is the sole signal that enables the
  // short activity refresh; completion clears it durably on every replica.
  const {
    data: bindingPayload,
    error: bindingRequestError,
    mutate,
  } = useSWR<unknown>(['/api/admin/repobindings', bindingQuery], {
    keepPreviousData: false,
    refreshInterval: (latest) => {
      const collection = decodeApiCollection<RepoBindingInfoModel>(latest)
      return collection.status === 'ready' && collection.items.some((binding) => binding.currentActivity) ? 3000 : 0
    },
  })
  const bindingCollection = decodeApiCollection<RepoBindingInfoModel>(bindingPayload)
  const bindings = bindingCollection.status === 'ready' ? bindingCollection.items : undefined
  const bindingView = apiCollectionView(bindingCollection, bindingRequestError)
  const bindingPageCount = apiCollectionPageCount(bindingCollection, BINDING_PAGE_SIZE)

  useEffect(() => {
    if (bindingPageCount === undefined || bindingPage <= bindingPageCount) return
    setBindingPage(bindingPageCount)
  }, [bindingPage, bindingPageCount])

  useEffect(() => {
    if (bindingCollection.status !== 'ready') return
    setBindingKnownPageCount(bindingPageCount)
  }, [bindingCollection.status, bindingPageCount])

  const [addOpened, setAddOpened] = useState(false)
  const addInFlight = useRef(false)
  const [repoUrl, setRepoUrl] = useState('')
  const [refValue, setRefValue] = useState('')
  const [githubToken, setGithubToken] = useState('')
  const [intervalSeconds, setIntervalSeconds] = useState<number | string>(60)
  const [runImmediately, setRunImmediately] = useState(true)
  const [busy, setBusy] = useState(false)
  const [lastResult, setLastResult] = useState<RepoBindingScanResultModel | null>(null)
  const [historyTarget, setHistoryTarget] = useState<RepoBindingInfoModel | null>(null)
  const [history, setHistory] = useState<RepoBindingScanHistoryModel[] | null>(null)
  const [historyLoading, setHistoryLoading] = useState(false)
  const [historyLoadFailed, setHistoryLoadFailed] = useState(false)
  const [historyPage, setHistoryPage] = useState(1)
  const [historyRequestedPage, setHistoryRequestedPage] = useState(1)
  const [historyTotal, setHistoryTotal] = useState(0)
  const [historyPaginated, setHistoryPaginated] = useState(false)
  const historyOwner = useRef<{ generation: number; controller: AbortController | null }>({
    generation: 0,
    controller: null,
  })

  useEffect(
    () => () => {
      historyOwner.current.generation += 1
      historyOwner.current.controller?.abort()
      historyOwner.current.controller = null
    },
    []
  )

  const flash = (r: RepoBindingScanResultModel) => {
    setLastResult(r)
    showNotification({
      color: r.failures === 0 ? 'teal' : 'orange',
      title: t('admin.notification.repo_binding.scanned'),
      message: t('admin.notification.repo_binding.summary', {
        games: r.gamesCreated + r.gamesUpdated,
        challenges: r.challengesImported + r.challengesUpdated,
        failures: r.failures,
      }),
      icon: <Icon path={mdiCheck} size={1} />,
    })
  }

  const onAdd = async () => {
    if (!repoUrl.trim() || busy || addInFlight.current) return
    addInFlight.current = true
    setBusy(true)
    setLastResult(null)
    try {
      const resp = await api.admin.adminCreateRepoBinding({
        repoUrl,
        ref: refValue || null,
        githubToken: githubToken || null,
        intervalSeconds: Number(intervalSeconds) || 60,
        runImmediately,
      })
      flash(resp.data)
      setRepoUrl('')
      setRefValue('')
      setGithubToken('')
      setAddOpened(false)
      mutate()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      addInFlight.current = false
      setBusy(false)
    }
  }

  const onScan = async (b: RepoBindingInfoModel) => {
    setBusy(true)
    try {
      const resp = await api.admin.adminScanRepoBinding(b.id)
      flash(resp.data)
      mutate()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setBusy(false)
    }
  }

  const loadHistory = async (b: RepoBindingInfoModel, page: number) => {
    const generation = historyOwner.current.generation + 1
    historyOwner.current.generation = generation
    historyOwner.current.controller?.abort()
    const controller = new AbortController()
    historyOwner.current.controller = controller
    setHistoryRequestedPage(page)
    setHistoryLoading(true)
    setHistoryLoadFailed(false)
    try {
      const resp = await api.request<unknown>({
        path: `/api/admin/repobindings/${b.id}/scans`,
        method: 'GET',
        query: { count: HISTORY_PAGE_SIZE, skip: (page - 1) * HISTORY_PAGE_SIZE },
        format: 'json',
        signal: controller.signal,
      })
      if (controller.signal.aborted || historyOwner.current.generation !== generation) return
      const result = decodeApiCollection<RepoBindingScanHistoryModel>(resp.data)
      if (result.status !== 'ready') {
        setHistory((current) => current ?? [])
        setHistoryLoadFailed(true)
        return
      }
      setHistory(result.items)
      setHistoryPage(page)
      setHistoryTotal(result.total)
      setHistoryPaginated(result.paginated)
    } catch (e) {
      if (controller.signal.aborted || historyOwner.current.generation !== generation) return
      setHistory((current) => current ?? [])
      setHistoryLoadFailed(true)
      showErrorMsg(e, t)
    } finally {
      if (historyOwner.current.generation === generation) {
        historyOwner.current.controller = null
        setHistoryLoading(false)
      }
    }
  }

  const onOpenHistory = (b: RepoBindingInfoModel) => {
    setHistoryTarget(b)
    setHistory(null)
    setHistoryLoading(true)
    setHistoryLoadFailed(false)
    setHistoryPage(1)
    setHistoryRequestedPage(1)
    setHistoryTotal(0)
    setHistoryPaginated(false)
    void loadHistory(b, 1)
  }

  const onTogglePause = async (b: RepoBindingInfoModel) => {
    setBusy(true)
    try {
      await api.admin.adminUpdateRepoBinding(b.id, {
        status: b.status === 'Active' ? 'Paused' : 'Active',
      })
      mutate()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setBusy(false)
    }
  }

  const onTogglePushOnEdit = async (b: RepoBindingInfoModel) => {
    setBusy(true)
    try {
      await api.admin.adminUpdateRepoBinding(b.id, { pushOnEdit: !b.pushOnEdit })
      mutate()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setBusy(false)
    }
  }

  const [deleteTarget, setDeleteTarget] = useState<RepoBindingInfoModel | null>(null)

  const onDelete = (b: RepoBindingInfoModel) => {
    setDeleteTarget(b)
  }

  const confirmDelete = async () => {
    const b = deleteTarget
    if (!b) return
    setBusy(true)
    try {
      await api.admin.adminDeleteRepoBinding(b.id)
      mutate()
      setDeleteTarget(null)
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setBusy(false)
    }
  }

  return (
    <AdminPage>
      <Container fluid px={0} w="100%" className={classes.workspace} data-repo-workspace>
        <Stack gap="lg" pb={32}>
          <Group justify="space-between" align="center" className={classes.toolbar}>
            <Text className={classes.scopeNote}>{t('admin.operations.repo_scope')}</Text>
            <Group gap="sm">
              <Button component={Link} to="/admin/builds" variant="default">
                {t('admin.content.builds.title')}
              </Button>
              <Button
                variant="default"
                disabled={busy}
                onClick={() => void mutate()}
                leftSection={<Icon path={mdiRefresh} size={0.8} aria-hidden="true" />}
              >
                {t('admin.operations.refresh')}
              </Button>
              <Button
                onClick={() => setAddOpened(true)}
                leftSection={<Icon path={mdiPlus} size={0.8} aria-hidden="true" />}
              >
                {t('admin.content.repo_binding.add')}
              </Button>
            </Group>
          </Group>
          {bindings && (
            <div className={classes.overview} aria-label={t('admin.operations.repo_overview')}>
              {[
                [t('admin.operations.repositories'), bindings.length],
                [t('admin.operations.active'), bindings.filter((b) => b.status === 'Active').length],
                [t('admin.operations.paused'), bindings.filter((b) => b.status === 'Paused').length],
                [t('admin.operations.scanning'), bindings.filter((b) => b.currentActivity).length],
              ].map(([label, count]) => (
                <div key={label} className={classes.metric}>
                  <Text className={classes.metricLabel}>{label}</Text>
                  <Text className={classes.metricValue}>{count}</Text>
                </div>
              ))}
            </div>
          )}
          <Title order={2} size="h4">
            {t('admin.operations.connected_repositories')}
          </Title>

          {lastResult && (
            <Paper p="md" withBorder className={classes.result} role="status">
              <Stack gap="xs">
                <Text fw={650}>{t('admin.operations.latest_result')}</Text>
                <Group gap="md">
                  <Badge color="teal" variant="light">
                    {t('admin.content.repo_binding.summary.games_created', {
                      count: lastResult.gamesCreated,
                      defaultValue: 'games +{{count}}',
                    })}
                  </Badge>
                  <Badge color="blue" variant="light">
                    {t('admin.content.repo_binding.summary.games_updated', {
                      count: lastResult.gamesUpdated,
                      defaultValue: 'games ~{{count}}',
                    })}
                  </Badge>
                  <Badge color="teal" variant="light">
                    {t('admin.content.repo_binding.summary.challenges_created', {
                      count: lastResult.challengesImported,
                      defaultValue: 'challenges +{{count}}',
                    })}
                  </Badge>
                  <Badge color="blue" variant="light">
                    {t('admin.content.repo_binding.summary.challenges_updated', {
                      count: lastResult.challengesUpdated,
                      defaultValue: 'challenges ~{{count}}',
                    })}
                  </Badge>
                  <Badge color={lastResult.failures > 0 ? 'red' : 'gray'} variant="light">
                    {t('admin.content.repo_binding.summary.failures', {
                      count: lastResult.failures,
                      defaultValue: 'failures {{count}}',
                    })}
                  </Badge>
                </Group>
                {lastResult.messages.length > 0 && (
                  <Stack gap={2}>
                    {lastResult.messages.slice(0, 12).map((m, i) => (
                      <Code key={i} block style={{ whiteSpace: 'pre-wrap', fontSize: 11 }}>
                        {m}
                      </Code>
                    ))}
                    {lastResult.messages.length > 12 && (
                      <Text size="xs" c="dimmed">
                        {t('admin.content.repo_binding.summary.more_messages', {
                          count: lastResult.messages.length - 12,
                          defaultValue: '…and {{count}} more',
                        })}
                      </Text>
                    )}
                  </Stack>
                )}
              </Stack>
            </Paper>
          )}

          {bindingView === 'stale' && (
            <Alert
              color="yellow"
              icon={<Icon path={mdiAlertCircleOutline} size={1} />}
              title={t(
                'admin.content.repo_binding.refresh_failed_title',
                'Refresh failed — showing the last repository bindings'
              )}
              role="status"
            >
              <Group justify="space-between" align="center" wrap="wrap">
                <Text size="sm">
                  {t(
                    'admin.content.repo_binding.refresh_failed',
                    'The latest refresh failed. Existing binding actions remain available.'
                  )}
                </Text>
                <Button size="xs" variant="light" onClick={() => void mutate()}>
                  {t('admin.button.repo_binding.retry', 'Retry')}
                </Button>
              </Group>
            </Alert>
          )}

          {bindingView === 'loading' ? (
            <Stack role="status" aria-label={t('admin.operations.loading')} data-repo-loading>
              <Skeleton h={200} animate={false} radius="md" />
              <Skeleton h={200} animate={false} radius="md" />
            </Stack>
          ) : bindingView === 'failed' ? (
            <Alert
              color="red"
              icon={<Icon path={mdiAlertCircleOutline} size={1} />}
              title={t('admin.content.repo_binding.load_failed_title', 'Repository bindings could not be loaded')}
              role="alert"
            >
              <Stack gap="sm" align="flex-start">
                <Text size="sm">
                  {t(
                    'admin.content.repo_binding.load_failed',
                    'The server returned an invalid response or the request failed.'
                  )}
                </Text>
                <Button size="xs" variant="light" onClick={() => void mutate()}>
                  {t('admin.button.repo_binding.retry', 'Retry')}
                </Button>
              </Stack>
            </Alert>
          ) : !bindings || bindings.length === 0 ? (
            <Center className={classes.empty}>
              <Stack gap={0} align="center">
                <Title order={3} size="h4">
                  {t('admin.content.repo_binding.empty_title')}
                </Title>
                <Text c="dimmed">{t('admin.content.repo_binding.empty')}</Text>
              </Stack>
            </Center>
          ) : (
            <Stack gap="md">
              {bindings.map((b) => (
                <Paper key={b.id} p="lg" withBorder className={classes.repoCard} data-repo-binding={b.id}>
                  <Stack gap="sm">
                    {/* Header: repo URL + PAT chip on the left; status + interval + actions on the right */}
                    <Group justify="space-between" wrap="wrap" align="flex-start">
                      <Group gap="xs" wrap="wrap" miw={0} style={{ flex: '1 1 20rem' }}>
                        <Icon path={mdiSourceBranch} size={1} />
                        <Tooltip label={b.repoUrl}>
                          <Title order={3} size="h4" className={classes.repoName}>
                            {b.repoUrl.replace('https://github.com/', '')}
                          </Title>
                        </Tooltip>
                        {b.hasGitHubToken &&
                          (b.tokenStatus === 'DecryptFailed' ? (
                            <Tooltip label={t('admin.content.repo_binding.token_decrypt_failed')}>
                              <Badge size="xs" color="red" variant="light">
                                {t('admin.content.repo_binding.summary.pat_failed', 'PAT ✗')}
                              </Badge>
                            </Tooltip>
                          ) : (
                            <Tooltip label={t('admin.content.repo_binding.has_token')}>
                              <Badge size="xs" color="gray" variant="light">
                                {t('admin.content.repo_binding.summary.pat', 'PAT')}
                              </Badge>
                            </Tooltip>
                          ))}
                      </Group>
                      <Group gap="xs" wrap="wrap">
                        <Badge color={b.status === 'Active' ? 'teal' : 'gray'} variant="light">
                          {b.status}
                        </Badge>
                        <Badge color="gray" variant="light">
                          {b.intervalSeconds}s
                        </Badge>
                        <Tooltip label={t('admin.button.repo_binding.scan')}>
                          <Button
                            variant="default"
                            size="sm"
                            disabled={busy}
                            onClick={() => onScan(b)}
                            leftSection={<Icon path={mdiRefresh} size={0.8} aria-hidden="true" />}
                          >
                            {t('admin.button.repo_binding.scan')}
                          </Button>
                        </Tooltip>
                        <Tooltip label={t('admin.button.repo_binding.history')}>
                          <Button
                            variant="default"
                            size="sm"
                            onClick={() => onOpenHistory(b)}
                            leftSection={<Icon path={mdiClockOutline} size={0.8} aria-hidden="true" />}
                          >
                            {t('admin.button.repo_binding.history')}
                          </Button>
                        </Tooltip>
                        <Tooltip
                          label={t(
                            b.status === 'Active'
                              ? 'admin.button.repo_binding.pause'
                              : 'admin.button.repo_binding.resume'
                          )}
                        >
                          <ActionIcon
                            variant="subtle"
                            disabled={busy}
                            aria-label={t(
                              b.status === 'Active'
                                ? 'admin.button.repo_binding.pause'
                                : 'admin.button.repo_binding.resume'
                            )}
                            onClick={() => onTogglePause(b)}
                          >
                            <Icon path={b.status === 'Active' ? mdiPause : mdiPlay} size={1} />
                          </ActionIcon>
                        </Tooltip>
                        <Tooltip label={t('admin.button.repo_binding.delete')}>
                          <ActionIcon
                            variant="subtle"
                            color="red"
                            disabled={busy}
                            aria-label={t('admin.button.repo_binding.delete')}
                            onClick={() => onDelete(b)}
                          >
                            <Icon path={mdiDeleteOutline} size={1} />
                          </ActionIcon>
                        </Tooltip>
                      </Group>
                    </Group>

                    {/* Subheader: ref + event count */}
                    <Group justify="space-between" wrap="wrap" align="center">
                      <Text size="xs" c="dimmed">
                        {t('admin.content.repo_binding.card.ref_label')}: {b.ref ?? 'default'}
                        {' · '}
                        {t('admin.content.repo_binding.card.events_count', { count: b.games.length })}
                      </Text>
                      <Tooltip
                        label={t('admin.content.repo_binding.push_on_edit_help')}
                        multiline
                        w="min(280px, calc(100vw - 2rem))"
                        position="left"
                      >
                        <Switch
                          size="xs"
                          checked={b.pushOnEdit ?? false}
                          disabled={busy || !b.hasGitHubToken}
                          onChange={() => onTogglePushOnEdit(b)}
                          label={t('admin.content.repo_binding.push_on_edit_label')}
                        />
                      </Tooltip>
                    </Group>

                    {/* Child games */}
                    {b.games.length === 0 ? (
                      <Text size="xs" c="dimmed">
                        {t('admin.content.repo_binding.no_games')}
                      </Text>
                    ) : (
                      <Stack gap={8} className={classes.linkedEvents}>
                        {b.games.map((g) => (
                          <Group key={g.id} gap="xs" wrap="wrap">
                            <Anchor component={Link} to={`/admin/games/${g.id}/challenges`} size="sm">
                              {g.title}
                            </Anchor>
                            {g.eventManifestPath && <Code className={classes.wrapCode}>{g.eventManifestPath}</Code>}
                          </Group>
                        ))}
                      </Stack>
                    )}

                    {/* Footer: timing + commit */}
                    <Group gap="md" wrap="wrap" className={classes.repoFooter}>
                      <Text size="xs" c="dimmed">
                        {b.lastScanUtc
                          ? `${t('admin.content.repo_binding.card.last_scan')} ${dayjs(b.lastScanUtc).fromNow()}`
                          : t('admin.content.repo_binding.card.never_scanned')}
                      </Text>
                      <Text size="xs" c="dimmed">
                        {b.status === 'Paused'
                          ? t('admin.content.repo_binding.paused_short')
                          : b.nextScanUtc
                            ? `${t('admin.content.repo_binding.card.next_scan')} ${dayjs(b.nextScanUtc).fromNow()}`
                            : t('admin.content.repo_binding.due_now')}
                      </Text>
                      {b.lastCommitSha && (
                        <Text size="xs" c="dimmed">
                          {t('admin.content.repo_binding.card.commit')}: <Code>{b.lastCommitSha.substring(0, 7)}</Code>
                        </Text>
                      )}
                    </Group>

                    {b.currentActivity && (
                      <Group gap="xs" wrap="nowrap">
                        <Loader size="xs" />
                        <Text size="xs" c="blue" ff="monospace" lineClamp={1} title={b.currentActivity}>
                          {b.currentActivity}
                        </Text>
                      </Group>
                    )}

                    {(b.pushBacklog ?? 0) > 0 && (
                      <Group gap="xs" wrap="wrap" role="status">
                        <Badge size="xs" color={b.pushLastError ? 'orange' : 'blue'} variant="light">
                          {t('admin.content.repo_binding.push_backlog', {
                            defaultValue: '{{count}} upstream edit(s) queued',
                            count: b.pushBacklog ?? 0,
                          })}
                        </Badge>
                        {b.pushLastError && (
                          <Text size="xs" className={classes.warningText} lineClamp={2} title={b.pushLastError}>
                            {b.pushLastError}
                          </Text>
                        )}
                      </Group>
                    )}

                    {b.lastScanMessage && (
                      <Text
                        size="xs"
                        c="dimmed"
                        lineClamp={2}
                        ff="monospace"
                        title={b.lastScanMessage}
                        className={classes.scanMessage}
                      >
                        {b.lastScanMessage}
                      </Text>
                    )}
                  </Stack>
                </Paper>
              ))}
            </Stack>
          )}
          {bindingKnownPageCount !== undefined && bindingKnownPageCount > 1 && (
            <ResponsivePagination
              value={bindingPage}
              onChange={setBindingPage}
              total={bindingKnownPageCount}
              label={t('common.pagination.label', 'Repository binding pages')}
            />
          )}
        </Stack>
      </Container>

      <AccessibleModal
        opened={addOpened}
        onClose={() => {
          if (!busy) {
            setAddOpened(false)
            setGithubToken('')
          }
        }}
        title={t('admin.content.repo_binding.add')}
        size="lg"
        closeOnEscape={!busy}
        closeOnClickOutside={!busy}
        withCloseButton={!busy}
      >
        <Text size="sm" c="dimmed" mb="lg">
          {t('admin.content.repo_binding.subtitle')}
        </Text>
        <form
          onSubmit={(event) => {
            event.preventDefault()
            void onAdd()
          }}
        >
          <Stack gap="sm">
            <TextInput
              label={t('admin.content.repo_binding.repo_url')}
              placeholder="https://github.com/your-team/challenges"
              required
              disabled={busy}
              autoComplete="off"
              value={repoUrl}
              onChange={(e) => setRepoUrl(e.currentTarget.value)}
            />
            <SimpleGrid cols={{ base: 1, sm: 2 }}>
              <TextInput
                label={t('admin.content.repo_binding.ref')}
                disabled={busy}
                placeholder="main"
                value={refValue}
                onChange={(e) => setRefValue(e.currentTarget.value)}
              />
              <TextInput
                label={t('admin.content.repo_binding.token')}
                disabled={busy}
                description={t('admin.content.repo_binding.token_help')}
                type="password"
                autoComplete="new-password"
                placeholder="github_pat_…"
                value={githubToken}
                onChange={(e) => setGithubToken(e.currentTarget.value)}
              />
            </SimpleGrid>
            <SimpleGrid cols={{ base: 1, sm: 2 }}>
              <NumberInput
                label={t('admin.content.repo_binding.interval')}
                disabled={busy}
                description={t('admin.content.repo_binding.interval_help')}
                min={60}
                max={86400}
                step={60}
                value={intervalSeconds}
                onChange={setIntervalSeconds}
              />
              <Switch
                label={t('admin.content.repo_binding.run_immediately')}
                disabled={busy}
                checked={runImmediately}
                onChange={(e) => setRunImmediately(e.currentTarget.checked)}
              />
            </SimpleGrid>
            <Group justify="flex-end">
              <Button
                leftSection={<Icon path={mdiPlus} size={1} />}
                loading={busy}
                disabled={busy || !repoUrl.trim()}
                type="submit"
              >
                {t('admin.button.repo_binding.add')}
              </Button>
            </Group>
          </Stack>
        </form>
      </AccessibleModal>

      <AccessibleModal
        size="min(64rem, calc(100vw - 2rem))"
        opened={historyTarget != null}
        onClose={() => {
          historyOwner.current.generation += 1
          historyOwner.current.controller?.abort()
          historyOwner.current.controller = null
          setHistoryTarget(null)
          setHistory(null)
          setHistoryLoading(false)
          setHistoryLoadFailed(false)
          setHistoryPage(1)
          setHistoryRequestedPage(1)
          setHistoryTotal(0)
          setHistoryPaginated(false)
        }}
        title={
          <Stack gap={0}>
            <Text fw={700}>{t('admin.content.repo_binding.history_title')}</Text>
            {historyTarget && (
              <Text size="xs" c="dimmed" ff="monospace">
                {historyTarget.repoUrl.replace('https://github.com/', '')}
              </Text>
            )}
          </Stack>
        }
      >
        {history === null ? (
          <Center py="xl">
            <Text c="dimmed" role="status">
              {t('admin.content.repo_binding.history_loading')}
            </Text>
          </Center>
        ) : (
          <Stack gap="sm" aria-busy={historyLoading}>
            {historyLoadFailed && (
              <Alert
                color="red"
                icon={<Icon path={mdiAlertCircleOutline} size={1} />}
                title={t('admin.content.repo_binding.history_load_failed_title', 'Scan history could not be loaded')}
                role="alert"
              >
                <Stack gap="sm" align="flex-start">
                  <Text size="sm">
                    {t(
                      'admin.content.repo_binding.history_load_failed',
                      'The server returned an invalid response or the request failed.'
                    )}
                  </Text>
                  {historyTarget && (
                    <Button
                      size="xs"
                      variant="light"
                      onClick={() => void loadHistory(historyTarget, historyRequestedPage)}
                    >
                      {t('admin.button.repo_binding.retry', 'Retry')}
                    </Button>
                  )}
                </Stack>
              </Alert>
            )}
            {historyLoading && (
              <Text size="sm" c="dimmed" role="status">
                {t('admin.content.repo_binding.history_loading')}
              </Text>
            )}
            {history.length === 0 && !historyLoadFailed && !historyLoading ? (
              <Center py="xl">
                <Text c="dimmed">{t('admin.content.repo_binding.history_empty')}</Text>
              </Center>
            ) : (
              history.map((row) => (
                <Paper key={row.id} p="sm" withBorder>
                  <Stack gap={6}>
                    <Group justify="space-between" wrap="wrap">
                      <Group gap="xs" wrap="wrap">
                        <Text size="sm" fw="bold">
                          {dayjs(row.ranAtUtc).fromNow()}
                        </Text>
                        <Text size="xs" c="dimmed" ff="monospace">
                          {dayjs(row.ranAtUtc).format('YYYY-MM-DD HH:mm:ss')}
                        </Text>
                      </Group>
                      {row.commitSha && <Code>{row.commitSha.substring(0, 7)}</Code>}
                    </Group>
                    <Group gap="md" wrap="wrap">
                      <Badge size="xs" color="teal" variant="light">
                        {t('admin.content.repo_binding.summary.games_created', {
                          count: row.gamesCreated,
                          defaultValue: 'games +{{count}}',
                        })}
                      </Badge>
                      <Badge size="xs" color="blue" variant="light">
                        {t('admin.content.repo_binding.summary.games_updated', {
                          count: row.gamesUpdated,
                          defaultValue: 'games ~{{count}}',
                        })}
                      </Badge>
                      <Badge size="xs" color="teal" variant="light">
                        {t('admin.content.repo_binding.summary.challenges_created_short', {
                          count: row.challengesImported,
                          defaultValue: 'chal +{{count}}',
                        })}
                      </Badge>
                      <Badge size="xs" color="blue" variant="light">
                        {t('admin.content.repo_binding.summary.challenges_updated_short', {
                          count: row.challengesUpdated,
                          defaultValue: 'chal ~{{count}}',
                        })}
                      </Badge>
                      <Badge size="xs" color={row.failures > 0 ? 'red' : 'gray'} variant="light">
                        {t('admin.content.repo_binding.summary.failures', {
                          count: row.failures,
                          defaultValue: 'failures {{count}}',
                        })}
                      </Badge>
                    </Group>
                    {row.messages && (
                      <Code
                        block
                        role="region"
                        tabIndex={0}
                        aria-label={t('admin.content.repo_binding.history_title')}
                        className={classes.wrapCode}
                        style={{ whiteSpace: 'pre-wrap', fontSize: 11, maxHeight: '20vh', overflowY: 'auto' }}
                      >
                        {row.messages}
                      </Code>
                    )}
                  </Stack>
                </Paper>
              ))
            )}
            {historyPaginated && historyTotal > HISTORY_PAGE_SIZE && historyTarget && (
              <ResponsivePagination
                value={historyPage}
                onChange={(page) => void loadHistory(historyTarget, page)}
                total={Math.max(1, Math.ceil(historyTotal / HISTORY_PAGE_SIZE))}
                label={t('common.pagination.label', 'Repository scan history pages')}
              />
            )}
          </Stack>
        )}
      </AccessibleModal>

      <AccessibleModal
        size="min(36rem, calc(100vw - 2rem))"
        opened={deleteTarget !== null}
        onClose={() => setDeleteTarget(null)}
        title={deleteTarget ? t('admin.content.repo_binding.delete_title', { repo: deleteTarget.repoUrl }) : ''}
        centered
      >
        <Stack gap="md">
          <Text size="sm">{t('admin.content.repo_binding.delete_warning')}</Text>
          <Group justify="flex-end" gap="xs" wrap="wrap">
            <Button variant="default" onClick={() => setDeleteTarget(null)} disabled={busy}>
              {t('common.button.cancel')}
            </Button>
            <Button color="red" onClick={confirmDelete} disabled={busy} loading={busy}>
              {t('admin.button.repo_binding.delete')}
            </Button>
          </Group>
        </Stack>
      </AccessibleModal>
    </AdminPage>
  )
}

export default RepoBindings
