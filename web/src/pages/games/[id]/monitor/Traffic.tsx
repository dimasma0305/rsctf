import {
  ActionIcon,
  Box,
  Center,
  Divider,
  Grid,
  Group,
  Pagination,
  Paper,
  rem,
  Stack,
  Text,
  Title,
  Tooltip,
  useMantineColorScheme,
  useMantineTheme,
} from '@mantine/core'
import { useModals } from '@mantine/modals'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose, mdiDeleteForeverOutline, mdiDownloadMultiple } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { CSSProperties, FC, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams, useSearchParams } from 'react-router'
import { ScrollSelect } from '@Components/ScrollSelect'
import { ChallengeItem, FileItem, TeamItem } from '@Components/TrafficItems'
import { WithGameMonitor } from '@Components/WithGameMonitor'
import { FlowInspector } from '@Components/traffic/FlowInspector'
import { useLanguage } from '@Utils/I18n'
import { currentListSnapshotRows, LatestListRequest, LatestRequest, type ListSnapshot } from '@Utils/LatestRequest'
import { showErrorMsg } from '@Utils/Shared'
import { HunamizeSize } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import api, { FileRecord } from '@Api'
import type { ChallengeTrafficModel, TeamTrafficModel, TrafficInventoryPage } from '@Api'

const TRAFFIC_PAGE_SIZE = 50

const useLatestTrafficList = <T,>(
  scope: string,
  enabled: boolean,
  request: (signal: AbortSignal) => Promise<readonly T[]>
) => {
  const owner = useRef(new LatestListRequest<T>())
  const generation = useRef(0)
  const [snapshot, setSnapshot] = useState<ListSnapshot<T>>()
  const [loading, setLoading] = useState(enabled)

  const refresh = useCallback(async () => {
    const current = ++generation.current
    if (!enabled) {
      owner.current.cancel()
      setLoading(false)
      return
    }
    setLoading(true)
    try {
      const result = await owner.current.run(scope, request)
      if (result) setSnapshot(result)
    } catch {
      // The page retains the last good snapshot; a manual refresh retries.
    } finally {
      if (generation.current === current) setLoading(false)
    }
  }, [enabled, request, scope])

  useEffect(() => {
    void refresh()
    return () => {
      generation.current += 1
      owner.current.cancel()
    }
  }, [refresh])

  return {
    data: currentListSnapshotRows(scope, snapshot),
    loading,
    mutate: refresh,
  }
}

const useLatestTrafficPage = <T,>(
  scope: string,
  enabled: boolean,
  request: (signal: AbortSignal) => Promise<TrafficInventoryPage<T>>
) => {
  const owner = useRef(new LatestRequest())
  const generation = useRef(0)
  const [snapshot, setSnapshot] = useState<{ scope: string; page: TrafficInventoryPage<T> }>()
  const [loading, setLoading] = useState(enabled)

  const refresh = useCallback(async () => {
    const current = ++generation.current
    if (!enabled) {
      owner.current.cancel()
      setLoading(false)
      return
    }
    setLoading(true)
    try {
      const page = await owner.current.run(request)
      if (page) setSnapshot({ scope, page })
    } catch {
      // Retain only a snapshot from this exact navigation scope.
    } finally {
      if (generation.current === current) setLoading(false)
    }
  }, [enabled, request, scope])

  useEffect(() => {
    void refresh()
    return () => {
      generation.current += 1
      owner.current.cancel()
    }
  }, [refresh])

  return {
    page: snapshot?.scope === scope ? snapshot.page : undefined,
    loading,
    mutate: refresh,
  }
}

interface InventoryPagerProps {
  page: number
  pageSize: number
  total: number
  loaded: number
  label: string
  onChange: (page: number) => void
}

const InventoryPager: FC<InventoryPagerProps> = ({ page, pageSize, total, loaded, label, onChange }) => {
  const { t } = useTranslation()
  const pages = Math.max(1, Math.ceil(total / pageSize))
  const first = loaded === 0 ? 0 : (page - 1) * pageSize + 1
  const last = loaded === 0 ? 0 : Math.min(total, first + loaded - 1)

  return (
    <Stack gap={2} align="center" px="xs" py={4}>
      <Text size="xs" c="dimmed" role="status" aria-live="polite" aria-atomic="true">
        {t('game.content.traffic.page_status', 'Showing {{first}}–{{last}} of {{total}}', { first, last, total })}
      </Text>
      {pages > 1 && (
        <Box component="nav" aria-label={label} maw="100%">
          <Pagination size="xs" total={pages} value={Math.min(page, pages)} onChange={onChange} withEdges />
        </Box>
      )}
    </Stack>
  )
}

const Traffic: FC = () => {
  const { id } = useParams()
  const gameId = parseInt(id ?? '-1')

  const [searchParams, setSearchParams] = useSearchParams()
  const parseInt2 = (raw: string | null): number | null => {
    if (!raw) return null
    const n = Number.parseInt(raw, 10)
    return Number.isFinite(n) ? n : null
  }
  const challengeId = parseInt2(searchParams.get('chal'))
  const participationId = parseInt2(searchParams.get('team'))
  const inspectFilename = searchParams.get('file')
  const [teamPage, setTeamPage] = useState(1)
  const [filePage, setFilePage] = useState(1)

  // Cascading URL writer for the three "navigation" slots. Cascade rules
  // match the plan: changing the upstream slot clears the downstream ones,
  // closing the modal (file → null) wipes the inner inspector state too.
  const setNav = (updates: { chal?: number | null; team?: number | null; file?: string | null }) => {
    if ('chal' in updates) {
      setTeamPage(1)
      setFilePage(1)
    } else if ('team' in updates) {
      setFilePage(1)
    }
    setSearchParams(
      (prev) => {
        const out = new URLSearchParams(prev)
        if ('chal' in updates) {
          if (updates.chal != null) out.set('chal', String(updates.chal))
          else out.delete('chal')
          out.delete('team')
          out.delete('file')
          out.delete('port')
          out.delete('flowPeer')
        }
        if ('team' in updates) {
          if (updates.team != null) out.set('team', String(updates.team))
          else out.delete('team')
          out.delete('file')
          out.delete('port')
          out.delete('flowPeer')
        }
        if ('file' in updates) {
          if (updates.file != null) out.set('file', updates.file)
          else out.delete('file')
          out.delete('port')
          out.delete('flowPeer')
          if (updates.file == null) {
            out.delete('regex')
            out.delete('ip')
            out.delete('dir')
            out.delete('flags')
            out.delete('mode')
          }
        }
        return out
      },
      { replace: true }
    )
  }

  const [disabled, setDisabled] = useState(false)
  const [downloadAllBusy, setDownloadAllBusy] = useState(false)
  const downloadAllOwner = useRef(false)
  const downloadAllRelease = useRef<ReturnType<typeof setTimeout> | null>(null)
  const theme = useMantineTheme()

  const { t } = useTranslation()
  const { locale } = useLanguage()
  const { colorScheme } = useMantineColorScheme()
  const modals = useModals()
  const isCompact = useIsMobile(1200)

  const loadChallenges = useCallback(
    (signal: AbortSignal) =>
      api.game.gameGetChallengesWithTrafficCapturing(gameId, { signal }).then((response) => response.data),
    [gameId]
  )
  const loadTeams = useCallback(
    (signal: AbortSignal) =>
      api.game
        .gameGetChallengeTrafficPage(
          challengeId ?? 0,
          { skip: (teamPage - 1) * TRAFFIC_PAGE_SIZE, count: TRAFFIC_PAGE_SIZE },
          { signal }
        )
        .then((response) => response.data),
    [challengeId, teamPage]
  )
  const loadFiles = useCallback(
    (signal: AbortSignal) =>
      api.game
        .gameGetTeamTrafficPage(
          challengeId ?? 0,
          participationId ?? 0,
          { skip: (filePage - 1) * TRAFFIC_PAGE_SIZE, count: TRAFFIC_PAGE_SIZE },
          { signal }
        )
        .then((response) => response.data),
    [challengeId, filePage, participationId]
  )
  const challengeQuery = useLatestTrafficList<ChallengeTrafficModel>(`game:${gameId}`, gameId > 0, loadChallenges)
  const teamQuery = useLatestTrafficPage<TeamTrafficModel>(
    `challenge:${challengeId ?? 0}:page:${teamPage}`,
    challengeId != null,
    loadTeams
  )
  const fileQuery = useLatestTrafficPage<FileRecord>(
    `files:${challengeId ?? 0}:${participationId ?? 0}:page:${filePage}`,
    challengeId != null && participationId != null,
    loadFiles
  )
  const challengeTraffic = useMemo(
    () =>
      challengeQuery.data && [...challengeQuery.data].sort((a, b) => a.category?.localeCompare(b.category ?? '') ?? 0),
    [challengeQuery.data]
  )
  const teamTraffic = useMemo(
    () => teamQuery.page && [...teamQuery.page.items].sort((a, b) => (a.teamId ?? 0) - (b.teamId ?? 0)),
    [teamQuery.page]
  )
  const fileRecords = fileQuery.page?.items
  const mutateChallenges = challengeQuery.mutate
  const mutateTeams = teamQuery.mutate
  const mutateTraffic = fileQuery.mutate
  const teamTotal = teamQuery.page?.total
  const fileTotal = fileQuery.page?.total

  useEffect(() => {
    if (teamTotal === undefined) return
    const pages = Math.max(1, Math.ceil(teamTotal / TRAFFIC_PAGE_SIZE))
    if (teamPage > pages) setTeamPage(pages)
  }, [teamPage, teamTotal])

  useEffect(() => {
    if (fileTotal === undefined) return
    const pages = Math.max(1, Math.ceil(fileTotal / TRAFFIC_PAGE_SIZE))
    if (filePage > pages) setFilePage(pages)
  }, [filePage, fileTotal])

  const onDownload = (item: FileRecord) => {
    if (!challengeId || !participationId || !item.fileName) return

    window.open(
      `/api/game/captures/${challengeId}/${participationId}/${item.fileName}`,
      '_blank',
      'noopener,noreferrer'
    )
  }

  const onDownloadAll = () => {
    if (!challengeId || !participationId) {
      showNotification({
        color: 'red',
        title: t('common.error.encountered'),
        message: t('game.notification.select_chal_and_part'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }
    if (downloadAllOwner.current) return
    downloadAllOwner.current = true
    setDownloadAllBusy(true)

    const link = document.createElement('a')
    link.href = `/api/game/captures/${challengeId}/${participationId}/all`
    link.download = `captures_${challengeId}_${participationId}.zip`
    link.rel = 'noopener noreferrer'
    document.body.append(link)
    link.click()
    link.remove()

    // Native downloads deliberately stream outside page memory, so the browser
    // does not expose their completion. Keep one immediate intent owner while
    // the server establishes its authoritative archive lease.
    downloadAllRelease.current = setTimeout(() => {
      downloadAllOwner.current = false
      downloadAllRelease.current = null
      setDownloadAllBusy(false)
    }, 5000)
  }

  useEffect(
    () => () => {
      if (downloadAllRelease.current) clearTimeout(downloadAllRelease.current)
    },
    []
  )

  const onDelete = async (item: FileRecord) => {
    if (!challengeId || !participationId || !item.fileName) return

    setDisabled(true)

    try {
      await api.game.gameDeleteTeamTraffic(challengeId, participationId, item.fileName)
      showNotification({
        color: 'teal',
        message: t('game.notification.traffic.deleted'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      mutateTeams()
      mutateTraffic()
      setDisabled(false)
    }
  }

  const onDeleteAll = async () => {
    if (!challengeId || !participationId) return

    setDisabled(true)

    try {
      await api.game.gameDeleteAllTeamTraffic(challengeId, participationId)
      showNotification({
        color: 'teal',
        message: t('game.notification.traffic.deleted'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      if (filePage === 1) mutateTraffic()
      else setFilePage(1)
      mutateTeams()
      mutateChallenges()
      setDisabled(false)
    }
  }

  const totalFileSize = fileRecords?.reduce((acc, cur) => acc + (cur?.size ?? 0), 0) ?? 0

  const orderedFileRecords = useMemo(
    () => [...(fileRecords ?? [])].sort((a, b) => dayjs(b.updateTime).diff(dayjs(a.updateTime))),
    [fileRecords]
  )

  const dividerColor = colorScheme === 'dark' ? theme.colors.dark[4] : theme.colors.gray[4]
  const innerStyle: CSSProperties = isCompact
    ? { borderBottom: `${rem(1)} solid ${dividerColor}`, paddingBottom: 'var(--mantine-spacing-xs)' }
    : { borderRight: `${rem(1)} solid ${dividerColor}` }

  const scrollHeight = isCompact ? 'clamp(10rem, 26vh, 15rem)' : 'calc(100vh - 174px)'
  const pagedScrollHeight = isCompact ? scrollHeight : 'calc(100vh - 224px)'
  const fileScrollHeight = isCompact ? 'clamp(14rem, 36vh, 21rem)' : pagedScrollHeight
  const headerHeight = rem(32)

  return (
    <WithGameMonitor isLoading={challengeQuery.loading && !challengeTraffic}>
      {!challengeTraffic || challengeTraffic?.length === 0 ? (
        <Center mih={isCompact ? rem(240) : 'calc(100vh - 140px)'}>
          <Stack gap={0}>
            <Title order={2}>{t('game.content.no_traffic.title')}</Title>
            <Text>{t('game.content.no_traffic.comment')}</Text>
          </Stack>
        </Center>
      ) : (
        <Paper shadow="md" p={{ base: 'xs', sm: 'md' }}>
          <Grid gap={isCompact ? 'sm' : 0} h={isCompact ? 'auto' : 'calc(100vh - 142px)'}>
            <Grid.Col span={{ base: 12, lg: 3 }} style={innerStyle}>
              <Group h={headerHeight} pb="3px" px="xs">
                <Text size="md" fw="bold">
                  {t('common.label.challenge')}
                </Text>
              </Group>
              <Divider size="sm" />
              <ScrollSelect
                itemComponent={ChallengeItem}
                items={challengeTraffic}
                selectedId={challengeId}
                onSelect={(id) => setNav({ chal: id })}
                h={scrollHeight}
              />
            </Grid.Col>
            <Grid.Col span={{ base: 12, lg: 3 }} style={innerStyle}>
              <Group h={headerHeight} pb="3px" px="xs">
                <Text size="md" fw="bold">
                  {t('common.label.team')}
                </Text>
              </Group>
              <Divider size="sm" />
              <ScrollSelect
                itemComponent={TeamItem}
                items={teamTraffic}
                selectedId={participationId}
                onSelect={(id) => setNav({ team: id })}
                h={pagedScrollHeight}
              />
              <InventoryPager
                page={teamPage}
                pageSize={TRAFFIC_PAGE_SIZE}
                total={teamTotal ?? 0}
                loaded={teamTraffic?.length ?? 0}
                label={t('game.content.traffic.team_pages', 'Captured team pages')}
                onChange={setTeamPage}
              />
            </Grid.Col>
            <Grid.Col span={{ base: 12, lg: 6 }}>
              <Group h={headerHeight} pb="3px" px="xs" justify="space-between" wrap="nowrap">
                <Text size="md" fw="bold">
                  {t('game.label.traffic')}
                  <Text span px="md" fw="bold" size="sm" c="dimmed">
                    {HunamizeSize(totalFileSize ?? 0)}
                  </Text>
                </Text>
                <Group justify="right" gap="sm" wrap="nowrap">
                  <Tooltip label={t('game.button.delete.all_traffic')} position="left">
                    <ActionIcon
                      size="md"
                      aria-label={t('game.button.delete.all_traffic', 'Delete all listed traffic')}
                      onClick={() =>
                        modals.openConfirmModal({
                          title: t('game.button.delete.all_traffic'),
                          children: <Text size="sm">{t('game.content.traffic.deleted_all_confirm')}</Text>,
                          onConfirm: onDeleteAll,
                          confirmProps: { color: 'red' },
                        })
                      }
                    >
                      <Icon path={mdiDeleteForeverOutline} size={1} />
                    </ActionIcon>
                  </Tooltip>
                  <Tooltip label={t('game.button.download.all_traffic')} position="left">
                    <ActionIcon
                      size="md"
                      loading={downloadAllBusy}
                      disabled={disabled || downloadAllBusy}
                      aria-label={t('game.button.download.all_traffic', 'Download all listed traffic')}
                      onClick={onDownloadAll}
                    >
                      <Icon path={mdiDownloadMultiple} size={1} />
                    </ActionIcon>
                  </Tooltip>
                </Group>
              </Group>
              <Divider size="sm" />
              <ScrollSelect
                itemComponent={FileItem}
                itemComponentProps={{
                  onDownload,
                  onDelete,
                  onInspect: (item: FileRecord) => item.fileName && setNav({ file: item.fileName }),
                  disabled,
                  t,
                  locale,
                }}
                items={orderedFileRecords}
                h={fileScrollHeight}
              />
              <InventoryPager
                page={filePage}
                pageSize={TRAFFIC_PAGE_SIZE}
                total={fileTotal ?? 0}
                loaded={orderedFileRecords.length}
                label={t('game.content.traffic.file_pages', 'Capture file pages')}
                onChange={setFilePage}
              />
            </Grid.Col>
          </Grid>
        </Paper>
      )}
      <FlowInspector
        challengeId={inspectFilename ? challengeId : null}
        participationId={inspectFilename ? participationId : null}
        filename={inspectFilename}
        onClose={() => setNav({ file: null })}
      />
    </WithGameMonitor>
  )
}

export default Traffic
