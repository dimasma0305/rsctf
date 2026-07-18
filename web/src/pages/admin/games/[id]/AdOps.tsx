import {
  ActionIcon,
  Alert,
  Badge,
  Button,
  Center,
  CopyButton,
  Divider,
  Group,
  HoverCard,
  Indicator,
  List as MList,
  Loader,
  Menu,
  Modal,
  Paper,
  RingProgress,
  ScrollArea,
  SegmentedControl,
  Select,
  Stack,
  Table,
  Text,
  TextInput,
  ThemeIcon,
  Title,
  Tooltip,
  UnstyledButton,
} from '@mantine/core'
import { useDebouncedValue } from '@mantine/hooks'
import { useModals } from '@mantine/modals'
import { showNotification } from '@mantine/notifications'
import {
  mdiAlertCircle,
  mdiAlertCircleOutline,
  mdiArrowLeft,
  mdiCheck,
  mdiCheckCircle,
  mdiChevronDown,
  mdiChevronRight,
  mdiClose,
  mdiCloseCircle,
  mdiConsole,
  mdiDownload,
  mdiFileOutline,
  mdiFileTree,
  mdiFolderOutline,
  mdiHelpCircle,
  mdiInformationOutline,
  mdiMagnify,
  mdiPauseCircleOutline,
  mdiPlayCircle,
  mdiRefresh,
  mdiRestart,
  mdiSwordCross,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate, useParams } from 'react-router'
import { ContainerExecModal } from '@Components/admin/ContainerExecModal'
import { KothOpsPanel } from '@Components/admin/KothOpsPanel'
import { WithGameEditTab } from '@Components/admin/WithGameEditTab'
import { showErrorMsg } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { highlight } from '@Utils/marked/ShikiExtension'
import { sanitizeMarkdownHtml } from '@Utils/sanitize'
import { useAdminAdState, useAdminKothState, type AdminKothHill } from '@Hooks/useGame'
import { useTicker } from '@Hooks/useTicker'
import api, {
  AdCheckStatus,
  AdFileBlob,
  AdFileViewModel,
  AdSnapshotChange,
  AdSnapshotPointModel,
  AdSnapshotTimeDiffModel,
  AdTeamCellModel,
} from '@Api'
import tableClasses from '@Styles/AdOpsTable.module.css'
import misc from '@Styles/Misc.module.css'

// Maps an A&D check status to its color, icon and short label — the single
// source of truth for how a service's health reads across the whole console.
const statusMeta = (s?: string | null): { color: string; icon: string; label: string } => {
  switch (s) {
    case 'Ok':
      return { color: 'teal', icon: mdiCheckCircle, label: 'Ok' }
    case 'Mumble':
      return { color: 'yellow', icon: mdiAlertCircle, label: 'Mumble' }
    case 'Offline':
      return { color: 'red', icon: mdiCloseCircle, label: 'Offline' }
    case 'InternalError':
      return { color: 'gray', icon: mdiHelpCircle, label: 'Error' }
    default:
      return { color: 'gray', icon: mdiHelpCircle, label: '—' }
  }
}

// docker diff kind: 0 = modified (C), 1 = added (A), 2 = deleted (D)
const kindMeta = (kind: number): { color: string; label: string } => {
  switch (kind) {
    case 1:
      return { color: 'teal', label: 'A' }
    case 2:
      return { color: 'red', label: 'D' }
    default:
      return { color: 'yellow', label: 'M' }
  }
}

// Statuses an operator can manually override a check verdict to.
const OVERRIDE_STATUSES = [
  { value: AdCheckStatus.Ok, icon: mdiCheckCircle, color: 'teal' },
  { value: AdCheckStatus.Mumble, icon: mdiAlertCircle, color: 'yellow' },
  { value: AdCheckStatus.Offline, icon: mdiCloseCircle, color: 'red' },
  { value: AdCheckStatus.InternalError, icon: mdiHelpCircle, color: 'gray' },
] as const

interface SnapTarget {
  cell: AdTeamCellModel
  teamName: string
  challengeTitle: string
}

// Guess a Shiki language from the file path so content highlights sensibly.
const langFromPath = (p: string): string => {
  const f = p.toLowerCase()
  if (f.endsWith('dockerfile') || f.includes('/dockerfile')) return 'docker'
  const ext = f.includes('.') ? f.slice(f.lastIndexOf('.') + 1) : ''
  const map: Record<string, string> = {
    sh: 'bash',
    bash: 'bash',
    py: 'python',
    js: 'typescript',
    ts: 'typescript',
    json: 'json',
    yml: 'yaml',
    yaml: 'yaml',
    c: 'c',
    h: 'c',
    cpp: 'cpp',
    cc: 'cpp',
    go: 'go',
    rs: 'rust',
    html: 'html',
    htm: 'html',
    css: 'css',
    md: 'markdown',
    sql: 'sql',
    ini: 'ini',
    conf: 'ini',
    cfg: 'ini',
    env: 'dotenv',
    toml: 'toml',
    xml: 'xml',
    java: 'java',
  }
  return map[ext] ?? 'text'
}

// Shiki-highlighted code block. Shiki escapes the code, so the rendered HTML
// is safe even though the content comes from a team's container.
const ShikiBlock: FC<{ code: string; lang: string }> = ({ code, lang }) => (
  <ScrollArea h={400} type="auto">
    <div
      style={{ fontSize: 12 }}
      // Shiki escapes source text; DOMPurify remains the final sink boundary.
      // eslint-disable-next-line react/no-danger
      dangerouslySetInnerHTML={{ __html: sanitizeMarkdownHtml(highlight(code, lang)) }}
    />
  </ScrollArea>
)

// ---- changed-files folder tree ----
interface TreeNode {
  name: string
  path: string
  kind?: number // set on leaf files (A/M/D)
  children: Map<string, TreeNode>
}

const buildTree = (changes: AdSnapshotChange[]): TreeNode => {
  const root: TreeNode = { name: '', path: '', children: new Map() }
  for (const ch of changes) {
    const parts = ch.path.split('/').filter(Boolean)
    let node = root
    let acc = ''
    parts.forEach((part, i) => {
      acc += `/${part}`
      let child = node.children.get(part)
      if (!child) {
        child = { name: part, path: acc, children: new Map() }
        node.children.set(part, child)
      }
      if (i === parts.length - 1) child.kind = ch.kind
      node = child
    })
  }
  return root
}

const countFiles = (node: TreeNode): number =>
  node.children.size === 0 ? 1 : [...node.children.values()].reduce((n, c) => n + countFiles(c), 0)

const FileTreeNode: FC<{
  node: TreeNode
  depth: number
  forceOpen: boolean
  collapsed: Set<string>
  onToggle: (path: string) => void
  onSelect: (path: string) => void
}> = ({ node, depth, forceOpen, collapsed, onToggle, onSelect }) => {
  const entries = [...node.children.values()].sort((a, b) => {
    const af = a.children.size === 0
    const bf = b.children.size === 0
    if (af !== bf) return af ? 1 : -1 // folders first
    return a.name.localeCompare(b.name)
  })

  return (
    <>
      {entries.map((child) => {
        const isFile = child.children.size === 0
        if (isFile) {
          const m = kindMeta(child.kind ?? 0)
          return (
            <UnstyledButton
              key={child.path}
              onClick={() => onSelect(child.path)}
              className={tableClasses.touchRow}
              style={{ width: '100%', borderRadius: 4, padding: '1px 4px' }}
            >
              <Group gap={6} wrap="nowrap" style={{ paddingLeft: depth * 14 + 16 }}>
                <Badge size="xs" color={m.color} variant="filled" w={20} p={0}>
                  {m.label}
                </Badge>
                <Icon path={mdiFileOutline} size={0.6} />
                <Text className={misc.ffmono} size="xs" style={{ wordBreak: 'break-all' }}>
                  {child.name}
                </Text>
              </Group>
            </UnstyledButton>
          )
        }
        const open = forceOpen || !collapsed.has(child.path)
        return (
          <div key={child.path}>
            <UnstyledButton
              onClick={() => onToggle(child.path)}
              className={tableClasses.touchRow}
              style={{ width: '100%', borderRadius: 4, padding: '1px 4px' }}
            >
              <Group gap={4} wrap="nowrap" style={{ paddingLeft: depth * 14 }}>
                <Icon path={open ? mdiChevronDown : mdiChevronRight} size={0.7} />
                <Icon path={mdiFolderOutline} size={0.7} />
                <Text size="xs" fw={500}>
                  {child.name}
                </Text>
                <Text size="xs" c="dimmed">
                  ({countFiles(child)})
                </Text>
              </Group>
            </UnstyledButton>
            {open && (
              <FileTreeNode
                node={child}
                depth={depth + 1}
                forceOpen={forceOpen}
                collapsed={collapsed}
                onToggle={onToggle}
                onSelect={onSelect}
              />
            )}
          </div>
        )
      })}
    </>
  )
}

// Drill-down into one changed file: current content (running container),
// baseline content (challenge image), and the unified diff between them.
const FileDetail: FC<{ gameId: number; sid: number; path: string; onBack: () => void }> = ({
  gameId,
  sid,
  path,
  onBack,
}) => {
  const { t } = useTranslation()
  const [loading, setLoading] = useState(true)
  const [data, setData] = useState<AdFileViewModel | null>(null)
  const [view, setView] = useState<'diff' | 'current' | 'original'>('diff')

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    setData(null)
    api.edit
      .editAdFile(gameId, sid, { path })
      .then(({ data }) => {
        if (cancelled) return
        setData(data)
        setView(data.unifiedDiff ? 'diff' : data.current ? 'current' : 'original')
      })
      .catch(() => undefined)
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [gameId, sid, path])

  const lang = langFromPath(path)

  const renderBlob = (blob: AdFileBlob | null | undefined, emptyMsg: string) => {
    if (!blob)
      return (
        <Text size="sm" c="dimmed">
          {emptyMsg}
        </Text>
      )
    if (blob.binary)
      return (
        <Text size="sm" c="dimmed">
          {t('admin.content.ad_ops.file.binary', {
            n: blob.size,
            defaultValue: 'Binary file — {{n}} bytes (not shown).',
          })}
        </Text>
      )
    return (
      <Stack gap={4}>
        {blob.truncated && (
          <Text size="xs" c="orange">
            {t('admin.content.ad_ops.file.truncated', 'Showing the first 256 KiB (truncated).')}
          </Text>
        )}
        <ShikiBlock code={blob.text ?? ''} lang={lang} />
      </Stack>
    )
  }

  const tabs: { value: string; label: string }[] = []
  if (data?.unifiedDiff) tabs.push({ value: 'diff', label: t('admin.content.ad_ops.file.tab_diff', 'Diff') })
  if (data?.current) tabs.push({ value: 'current', label: t('admin.content.ad_ops.file.tab_current', 'Current') })
  if (data?.baseline) tabs.push({ value: 'original', label: t('admin.content.ad_ops.file.tab_original', 'Original') })

  return (
    <Stack gap="sm">
      <Group justify="space-between" wrap="nowrap" gap="sm">
        <Group gap="xs" wrap="nowrap" style={{ minWidth: 0 }}>
          <ActionIcon
            size={44}
            variant="subtle"
            color="gray"
            aria-label={t('common.button.back', 'Back to file list')}
            onClick={onBack}
          >
            <Icon path={mdiArrowLeft} size={0.9} />
          </ActionIcon>
          <Text className={misc.ffmono} size="sm" fw="bold" style={{ wordBreak: 'break-all' }}>
            {path}
          </Text>
        </Group>
        {tabs.length > 1 && (
          <SegmentedControl
            size="xs"
            aria-label={t('admin.label.ad_ops.file_view', 'File view')}
            data={tabs}
            value={view}
            onChange={(v) => setView(v as 'diff' | 'current' | 'original')}
          />
        )}
      </Group>

      {loading ? (
        <Center h={200}>
          <Loader size="sm" />
        </Center>
      ) : !data ? (
        <Text size="sm" c="dimmed">
          {t('admin.content.ad_ops.file.load_failed', 'Could not read this file from the container.')}
        </Text>
      ) : view === 'diff' ? (
        data.unifiedDiff ? (
          <ShikiBlock code={data.unifiedDiff} lang="diff" />
        ) : (
          <Text size="sm" c="dimmed">
            {t(
              'admin.content.ad_ops.file.no_diff',
              'No line diff (file too large, binary, or one side missing) — use Current / Original.'
            )}
          </Text>
        )
      ) : view === 'current' ? (
        renderBlob(
          data.current,
          t('admin.content.ad_ops.file.no_current', 'Container not running — current content unavailable.')
        )
      ) : (
        renderBlob(
          data.baseline,
          t('admin.content.ad_ops.file.no_baseline', 'Not in the baseline image (team-added file).')
        )
      )}
    </Stack>
  )
}

// "History" tab: pick two capture points and see which files the team touched
// between them. Capture points accrue (deduped) as AdSnapshotService runs.
const SnapshotHistory: FC<{ gameId: number; sid: number; onSelect: (path: string) => void }> = ({
  gameId,
  sid,
  onSelect,
}) => {
  const { t } = useTranslation()
  const [points, setPoints] = useState<AdSnapshotPointModel[]>([])
  const [loading, setLoading] = useState(true)
  const [fromId, setFromId] = useState<string | null>(null)
  const [toId, setToId] = useState<string | null>(null)
  const [diff, setDiff] = useState<AdSnapshotTimeDiffModel | null>(null)
  const [diffLoading, setDiffLoading] = useState(false)

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    api.edit
      .editAdServiceSnapshots(gameId, sid)
      .then(({ data }) => {
        if (cancelled) return
        setPoints(data)
        if (data.length >= 2) {
          setFromId(String(data[0].id))
          setToId(String(data[data.length - 1].id))
        }
      })
      .catch(() => undefined)
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [gameId, sid])

  useEffect(() => {
    if (!fromId || !toId) return
    let cancelled = false
    setDiffLoading(true)
    api.edit
      .editAdSnapshotTimeDiff(gameId, sid, { fromId: Number(fromId), toId: Number(toId) })
      .then(({ data }) => {
        if (!cancelled) setDiff(data)
      })
      .catch(() => undefined)
      .finally(() => {
        if (!cancelled) setDiffLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [gameId, sid, fromId, toId])

  if (loading)
    return (
      <Center h={120}>
        <Loader size="sm" />
      </Center>
    )
  if (points.length < 2)
    return (
      <Text size="sm" c="dimmed">
        {t(
          'admin.content.ad_ops.history.too_few',
          'Not enough capture points yet — snapshots accrue as the team changes files over the game.'
        )}
      </Text>
    )

  const opts = points.map((p) => ({
    value: String(p.id),
    label: `#${p.round} · ${dayjs(p.capturedAt).format('HH:mm:ss')} (${p.fileCount})`,
  }))

  const row = (ch: AdSnapshotChange, color: string, label: string) => (
    <UnstyledButton
      key={`${label}-${ch.path}`}
      onClick={() => onSelect(ch.path)}
      className={tableClasses.touchRow}
      style={{ width: '100%', borderRadius: 4, padding: '1px 4px' }}
    >
      <Group gap={6} wrap="nowrap">
        <Badge size="xs" color={color} variant="filled" w={20} p={0}>
          {label}
        </Badge>
        <Text className={misc.ffmono} size="xs" style={{ wordBreak: 'break-all' }}>
          {ch.path}
        </Text>
      </Group>
    </UnstyledButton>
  )

  return (
    <Stack gap="sm">
      <Group grow wrap="nowrap">
        <Select
          size="xs"
          label={t('admin.content.ad_ops.history.from', 'From')}
          data={opts}
          value={fromId}
          onChange={setFromId}
        />
        <Select
          size="xs"
          label={t('admin.content.ad_ops.history.to', 'To')}
          data={opts}
          value={toId}
          onChange={setToId}
        />
      </Group>
      {diffLoading ? (
        <Center h={120}>
          <Loader size="sm" />
        </Center>
      ) : diff && (diff.added.length > 0 || diff.removed.length > 0) ? (
        <ScrollArea h={280} type="auto">
          <Stack gap={2}>
            {diff.added.map((ch) => row(ch, 'teal', '+'))}
            {diff.removed.map((ch) => row(ch, 'red', '−'))}
          </Stack>
        </ScrollArea>
      ) : (
        <Text size="sm" c="dimmed">
          {t('admin.content.ad_ops.history.no_change', 'No file changes between these two points.')}
        </Text>
      )}
    </Stack>
  )
}

const SnapshotModal: FC<{
  gameId: number
  target: SnapTarget | null
  selectedPath: string | null
  onSelectPath: (path: string | null) => void
  onOpenShell: (guid: string, title: string, inspectorSid?: number) => void
  onClose: () => void
}> = ({ gameId, target, selectedPath, onSelectPath, onOpenShell, onClose }) => {
  const { t } = useTranslation()
  const [loading, setLoading] = useState(false)
  const [changes, setChanges] = useState<AdSnapshotChange[]>([])
  const [live, setLive] = useState(false)
  const [filteredCats, setFilteredCats] = useState<string[]>([])
  const [fileSearch, setFileSearch] = useState('')
  const [debouncedFileSearch] = useDebouncedValue(fileSearch, 200)
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set())
  const [spawning, setSpawning] = useState(false)
  const [tab, setTab] = useState<'changes' | 'history'>('changes')
  const sid = target?.cell.adTeamServiceId
  const hasSnapshot = !!target?.cell.snapshotAvailable
  const containerGuid = target?.cell.containerGuid

  const toggleFolder = (path: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev)
      if (next.has(path)) next.delete(path)
      else next.add(path)
      return next
    })

  const openShell = async () => {
    if (!target) return
    const title = `${target.teamName} · ${target.challengeTitle}`
    if (containerGuid) {
      onOpenShell(containerGuid, title) // shell into the live team container
      return
    }
    // No live container → spawn a throwaway inspector from the image.
    setSpawning(true)
    try {
      const { data } = await api.edit.editAdSpawnInspector(gameId, sid!)
      onOpenShell(data.containerGuid, `${title} (inspector)`, sid!)
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setSpawning(false)
    }
  }

  const filtered = debouncedFileSearch
    ? changes.filter((c) => c.path.toLowerCase().includes(debouncedFileSearch.toLowerCase()))
    : changes
  const tree = buildTree(filtered)

  useEffect(() => {
    if (sid === undefined) return
    let cancelled = false
    setLoading(true)
    setChanges([])
    setLive(false)
    api.edit
      .editAdSnapshotChanges(gameId, sid)
      .then(({ data }) => {
        if (!cancelled) {
          setChanges(data.changes ?? [])
          setLive(!!data.live)
          setFilteredCats(data.filteredCategories ?? [])
        }
      })
      .catch(() => {
        // changes are best-effort; the tarball download is the source of truth
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [gameId, sid])

  const downloadUrl = target ? api.edit.editAdSnapshotUrl(gameId, target.cell.adTeamServiceId) : '#'
  const filename = target
    ? `ad-snapshot-team${target.cell.adTeamServiceId}-challenge${target.cell.challengeId}.tar.gz`
    : 'snapshot.tar.gz'

  return (
    <Modal
      opened={target !== null}
      onClose={onClose}
      size="xl"
      title={
        target
          ? t('admin.content.ad_ops.snapshot.title', {
              team: target.teamName,
              challenge: target.challengeTitle,
              defaultValue: 'Snapshot — {{team}} · {{challenge}}',
            })
          : ''
      }
    >
      <Stack gap="md">
        <Group justify="space-between" wrap="wrap" gap="sm">
          <Group gap="sm" wrap="nowrap">
            {hasSnapshot && (
              <Button
                component="a"
                href={downloadUrl}
                download={filename}
                variant="default"
                leftSection={<Icon path={mdiDownload} size={0.9} />}
              >
                {t('admin.button.ad_ops.snapshot.download', 'Download .tar.gz')}
              </Button>
            )}
            <Tooltip
              label={
                containerGuid
                  ? t('admin.tooltip.ad_ops.shell_live', 'Shell into the running container (their files)')
                  : t(
                      'admin.tooltip.ad_ops.shell_spawn',
                      'No running container — spawn a throwaway inspector from the image'
                    )
              }
              withArrow
              multiline
              w={240}
            >
              <Button
                color="grape"
                loading={spawning}
                leftSection={<Icon path={mdiConsole} size={0.9} />}
                onClick={openShell}
              >
                {containerGuid
                  ? t('admin.button.ad_ops.shell', 'Open shell')
                  : t('admin.button.ad_ops.shell_spawn', 'Spawn inspector')}
              </Button>
            </Tooltip>
          </Group>
          <Group gap={6} wrap="nowrap">
            <Text size="sm" c="dimmed">
              {t('admin.content.ad_ops.snapshot.changed_count', {
                count: changes.length,
                defaultValue: '{{count}} file(s) changed vs original image',
              })}
            </Text>
            <HoverCard width={360} shadow="md" withArrow position="bottom-end" openDelay={100}>
              <HoverCard.Target>
                <ActionIcon
                  size={44}
                  variant="subtle"
                  color="gray"
                  aria-label={t('admin.tooltip.ad_ops.filter_info', 'File filter help')}
                >
                  <Icon path={mdiInformationOutline} size={0.75} />
                </ActionIcon>
              </HoverCard.Target>
              <HoverCard.Dropdown>
                <Stack gap={6}>
                  <Text size="xs" fw={700}>
                    {t('admin.content.ad_ops.snapshot.filter_title', 'What this view shows')}
                  </Text>
                  <Text size="xs" c="teal">
                    {t(
                      'admin.content.ad_ops.snapshot.filter_shown',
                      'Shown (whitelist): files the team added / modified / deleted vs the base image.'
                    )}
                  </Text>
                  <Text size="xs" c="dimmed">
                    {t(
                      'admin.content.ad_ops.snapshot.filter_hidden',
                      'Hidden (blacklist) — runtime/churn paths, so only deliberate changes show:'
                    )}
                  </Text>
                  <MList size="xs" spacing={2}>
                    {(filteredCats.length
                      ? filteredCats
                      : [
                          'flag mount',
                          '/tmp, /run',
                          'logs & caches',
                          '/proc, /sys, /dev',
                          '__pycache__',
                          'ancestor dirs',
                        ]
                    ).map((c) => (
                      <MList.Item key={c}>{c}</MList.Item>
                    ))}
                  </MList>
                  <Text size="xs" c="orange">
                    {t(
                      'admin.content.ad_ops.snapshot.filter_warn',
                      'A foothold dropped into a hidden path won’t appear here — use the shell to inspect.'
                    )}
                  </Text>
                </Stack>
              </HoverCard.Dropdown>
            </HoverCard>
          </Group>
        </Group>

        {live && (
          <Text size="xs" c="dimmed">
            {t(
              'admin.content.ad_ops.snapshot.live_note',
              'Live diff: files modified since the container started (mtime-based; additions/deletions not distinguished).'
            )}
          </Text>
        )}

        <Divider
          label={
            live
              ? t('admin.content.ad_ops.snapshot.diff_label_live', 'Filesystem changes (live)')
              : t('admin.content.ad_ops.snapshot.diff_label', 'Filesystem changes (docker diff)')
          }
          labelPosition="left"
        />
        {selectedPath ? (
          <FileDetail gameId={gameId} sid={sid!} path={selectedPath} onBack={() => onSelectPath(null)} />
        ) : (
          <>
            <SegmentedControl
              size="xs"
              aria-label={t('admin.label.ad_ops.snapshot_view', 'Snapshot view')}
              value={tab}
              onChange={(v) => setTab(v as 'changes' | 'history')}
              data={[
                { value: 'changes', label: t('admin.content.ad_ops.snapshot.tab_changes', 'Current changes') },
                {
                  value: 'history',
                  label: t('admin.content.ad_ops.snapshot.tab_history', 'History (compare over time)'),
                },
              ]}
            />
            {tab === 'history' ? (
              sid != null ? (
                <SnapshotHistory gameId={gameId} sid={sid} onSelect={onSelectPath} />
              ) : null
            ) : loading ? (
              <Center h={120}>
                <Loader size="sm" />
              </Center>
            ) : changes.length === 0 ? (
              <Text size="sm" c="dimmed">
                {t(
                  'admin.content.ad_ops.snapshot.no_changes',
                  'No filesystem changes were recorded (snapshot may predate diff capture, or nothing changed).'
                )}
              </Text>
            ) : (
              <>
                <TextInput
                  size="xs"
                  aria-label={t('admin.placeholder.ad_ops.search_file', 'Filter files')}
                  leftSection={<Icon path={mdiMagnify} size={0.8} />}
                  placeholder={t('admin.placeholder.ad_ops.search_file', 'Filter files…')}
                  value={fileSearch}
                  onChange={(e) => setFileSearch(e.currentTarget.value)}
                />
                <ScrollArea h={320} type="auto">
                  {filtered.length === 0 ? (
                    <Text size="sm" c="dimmed">
                      {t('admin.content.ad_ops.snapshot.no_match', 'No files match the filter.')}
                    </Text>
                  ) : (
                    <FileTreeNode
                      node={tree}
                      depth={0}
                      forceOpen={debouncedFileSearch !== ''}
                      collapsed={collapsed}
                      onToggle={toggleFolder}
                      onSelect={onSelectPath}
                    />
                  )}
                </ScrollArea>
                <Text size="xs" c="dimmed">
                  {t(
                    'admin.content.ad_ops.snapshot.click_hint',
                    'Click a file to view its content and diff vs the original image.'
                  )}
                </Text>
              </>
            )}
          </>
        )}
      </Stack>
    </Modal>
  )
}

// One health-summary chip: an icon + count for a single status, dimmed when zero.
const HealthChip: FC<{ icon: string; color: string; count: number; label: string }> = ({
  icon,
  color,
  count,
  label,
}) => (
  <Tooltip label={label} withArrow>
    <Group gap={4} align="center" wrap="nowrap" style={{ opacity: count ? 1 : 0.45 }}>
      <ThemeIcon size="sm" radius="xl" variant="light" color={color}>
        <Icon path={icon} size={0.62} />
      </ThemeIcon>
      <Text fw={700} size="sm">
        {count}
      </Text>
    </Group>
  </Tooltip>
)

const AdOps: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1', 10)
  const { t } = useTranslation()
  const modals = useModals()
  const { adminAdState: state, error, mutate } = useAdminAdState(numId)
  const { adminKothState: koth, error: kothError, mutate: mutateKoth } = useAdminKothState(numId)
  const [busy, setBusy] = useState(false)
  const [busyHill, setBusyHill] = useState<number | null>(null)
  // Which side of the console is showing. A&D vs KotH challenges are disjoint
  // sets in a game; the switch only appears when both exist (see showViewSwitch).
  const [view, setView] = useState<'ad' | 'koth'>('ad')
  // inspectorSid set ⇒ a throwaway inspector container we must destroy on close.
  const [execTarget, setExecTarget] = useState<{
    guid: string
    title: string
    inspectorSid?: number
  } | null>(null)

  const openShell = (guid: string, title: string, inspectorSid?: number) => setExecTarget({ guid, title, inspectorSid })

  const closeShell = () => {
    if (execTarget?.inspectorSid != null)
      api.edit.editAdDestroyInspector(numId, execTarget.inspectorSid, execTarget.guid).catch(() => undefined)
    setExecTarget(null)
  }

  // The inspect-snapshot modal + selected file live in the URL hash, so they're
  // deep-linkable and the browser Back button steps file → snapshot → closed:
  //   #snapshot=<adTeamServiceId>            → modal open for that service
  //   #snapshot=<id>&file=<urlencoded path>  → + that file's content/diff
  const location = useLocation()
  const navigate = useNavigate()
  const hashParams = new URLSearchParams(location.hash.replace(/^#/, ''))
  const rawSnap = hashParams.get('snapshot')
  const snapSid = rawSnap !== null && rawSnap !== '' ? parseInt(rawSnap, 10) : null
  const selectedPath = hashParams.get('file') // URLSearchParams already decodes it

  const setHash = (frag: string) => navigate(`${location.pathname}${location.search}${frag ? `#${frag}` : ''}`)
  const openSnapshot = (cell: AdTeamCellModel) => setHash(`snapshot=${cell.adTeamServiceId}`)
  const closeSnapshot = () => setHash('')
  const selectFile = (path: string | null) =>
    setHash(snapSid == null ? '' : `snapshot=${snapSid}${path ? `&file=${encodeURIComponent(path)}` : ''}`)

  // Rebuild the modal's target from the service id in the hash.
  const snapTarget = useMemo<SnapTarget | null>(() => {
    if (snapSid == null || Number.isNaN(snapSid) || !state) return null
    for (const team of state.teams ?? []) {
      const cell = (team.services ?? []).find((s) => s.adTeamServiceId === snapSid)
      if (cell) {
        const title = (state.challenges ?? []).find((c) => c.challengeId === cell.challengeId)?.title ?? ''
        return { cell, teamName: team.teamName, challengeTitle: title }
      }
    }
    return null
  }, [snapSid, state])
  const [search, setSearch] = useState('')
  const [debouncedSearch] = useDebouncedValue(search, 200)
  const now = useTicker()
  const isMobile = useIsMobile(1080)

  // Wait for BOTH consoles' first load — the A&D state always resolves (even
  // empty) for any game, and the KotH state resolves to an object too, so a
  // KotH-only game doesn't flash the "no A&D challenges" empty state.
  const isLoading = (!state && !error) || (koth === undefined && !kothError)

  // A&D and KotH challenges are disjoint within a game. Derive which sides
  // exist + which to render. showKoth is flash-free (doesn't wait for an
  // effect): a KotH-only game renders KotH immediately even before any toggle.
  const hasAd = (state?.challenges.length ?? 0) > 0
  const hasKoth = (koth?.hills.length ?? 0) > 0
  const showViewSwitch = hasAd && hasKoth
  const showKoth = hasKoth && (view === 'koth' || !hasAd)

  const toggleHill = async (hill: AdminKothHill) => {
    setBusyHill(hill.challengeId)
    try {
      await api.edit.editAdToggleChallenge(numId, hill.challengeId)
      await mutateKoth()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setBusyHill(null)
    }
  }

  const ensureContainers = async () => {
    setBusy(true)
    try {
      await api.edit.editAdEnsureContainers(numId)
      showNotification({
        color: 'teal',
        icon: <Icon path={mdiCheck} size={1} />,
        title: t('admin.notification.ad_ops.ensure_queued.title', 'Container reconcile queued'),
        message: t('admin.notification.ad_ops.ensure_queued.message', 'Missing A&D containers will spin up shortly.'),
      })
      setTimeout(() => {
        mutate()
        mutateKoth()
      }, 3_000)
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setBusy(false)
    }
  }

  // Reset = destroy the running container and recreate it from the challenge
  // base image. This WIPES the team's filesystem changes (including any patches)
  // — there is no in-place restart on Kubernetes — so confirm before firing.
  const resetCell = (cell: AdTeamCellModel) => {
    modals.openConfirmModal({
      title: t('admin.content.ad_ops.reset_confirm.title', 'Reset container to base image?'),
      children: (
        <Text size="sm">
          {t(
            'admin.content.ad_ops.reset_confirm.message',
            "Destroys the running container and recreates it from the challenge image. The team's current " +
              "filesystem changes — including any patches — are wiped, and a fresh flag is delivered. This can't be undone."
          )}
        </Text>
      ),
      labels: {
        confirm: t('admin.content.ad_ops.reset_confirm.ok', 'Reset to base image'),
        cancel: t('admin.content.ad_ops.reset_confirm.cancel', 'Cancel'),
      },
      confirmProps: { color: 'red' },
      onConfirm: async () => {
        try {
          await api.edit.editAdForceRestart(numId, cell.adTeamServiceId)
          showNotification({
            color: 'teal',
            icon: <Icon path={mdiRestart} size={1} />,
            title: t('admin.notification.ad_ops.restart_queued.title', 'Reset queued'),
            message: t(
              'admin.notification.ad_ops.restart_queued.message',
              'Container will be recreated from the base image in seconds.'
            ),
          })
          setTimeout(() => mutate(), 3_000)
        } catch (e) {
          showErrorMsg(e, t)
        }
      },
    })
  }

  const toggleScoringPause = async () => {
    setBusy(true)
    try {
      const { data } = await api.edit.editAdToggleScoringPause(numId)
      showNotification({
        color: data.scoringPaused ? 'orange' : 'teal',
        icon: <Icon path={data.scoringPaused ? mdiPauseCircleOutline : mdiCheck} size={1} />,
        message: data.scoringPaused
          ? t('admin.notification.ad_ops.scoring_paused', 'Scoring paused — rounds + checks frozen.')
          : t('admin.notification.ad_ops.scoring_resumed', 'Scoring resumed.'),
      })
      mutate()
      mutateKoth()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setBusy(false)
    }
  }

  const overrideCheck = async (checkId: number, newStatus: AdCheckStatus) => {
    try {
      await api.edit.editAdOverrideCheck(numId, checkId, { newStatus })
      showNotification({
        color: 'teal',
        icon: <Icon path={mdiCheck} size={1} />,
        message: t('admin.notification.ad_ops.check_overridden', {
          status: newStatus,
          defaultValue: 'Check overridden to {{status}}.',
        }),
      })
      mutate()
    } catch (e) {
      showErrorMsg(e, t)
    }
  }

  if (isLoading) {
    return (
      <WithGameEditTab isLoading>
        <Center h="40vh">
          <Loader />
        </Center>
      </WithGameEditTab>
    )
  }

  // A transient fetch error must not masquerade as "no challenges". The A&D /state
  // is required for the shared header; if it failed — or both states are empty only
  // because the KotH fetch failed — show an error + retry rather than the empty card.
  if (!state || (!hasAd && !hasKoth && (error || kothError))) {
    return (
      <WithGameEditTab>
        <Center h="40vh">
          <Stack align="center" gap="sm">
            <Icon path={mdiAlertCircleOutline} size={2.5} color="var(--mantine-color-red-6)" />
            <Text fw="bold" c="dimmed">
              {t('admin.content.ad_ops.load_error', 'Could not load the operator console.')}
            </Text>
            <Button
              variant="default"
              leftSection={<Icon path={mdiRefresh} size={0.9} />}
              onClick={() => {
                mutate()
                mutateKoth()
              }}
            >
              {t('admin.button.ad_ops.retry', 'Retry')}
            </Button>
          </Stack>
        </Center>
      </WithGameEditTab>
    )
  }

  if (!hasAd && !hasKoth) {
    return (
      <WithGameEditTab>
        <Center h="40vh">
          <Stack align="center" gap="xs">
            <Icon path={mdiSwordCross} size={2.5} color="var(--mantine-color-dimmed)" />
            <Text fw="bold" c="dimmed">
              {t('admin.content.ad_ops.empty.title', 'No A&D or KotH challenges in this game')}
            </Text>
            <Text size="sm" c="dimmed">
              {t(
                'admin.content.ad_ops.empty.description',
                'Add a challenge with type Attack & Defense or King of the Hill to use this console.'
              )}
            </Text>
          </Stack>
        </Center>
      </WithGameEditTab>
    )
  }

  // While paused, freeze the countdown at the pause instant — the round isn't
  // burning time (resume shifts its end forward), so the timer must stop too.
  const timerRef = state.scoringPaused && state.scoringPausedAt ? dayjs(state.scoringPausedAt) : now
  const roundEndsIn = state.roundEndsAt ? Math.max(0, dayjs(state.roundEndsAt).diff(timerRef, 'second')) : null
  const roundTotal =
    state.roundStartedAt && state.roundEndsAt
      ? Math.max(1, dayjs(state.roundEndsAt).diff(state.roundStartedAt, 'second'))
      : null
  const roundPct =
    roundTotal && roundEndsIn !== null ? Math.min(100, Math.max(3, ((roundTotal - roundEndsIn) / roundTotal) * 100)) : 0
  const ringColor =
    roundEndsIn === 0
      ? 'red'
      : roundTotal && roundEndsIn !== null && roundEndsIn / roundTotal < 0.25
        ? 'orange'
        : 'teal'
  const ringLabel =
    state.currentRound == null
      ? '—'
      : roundEndsIn === null
        ? '∞'
        : roundEndsIn === 0
          ? t('admin.content.ad_ops.round_ended_short', 'end')
          : `${roundEndsIn}s`

  // Aggregate every (team × challenge) cell into a fleet-wide health summary.
  const counts = { Ok: 0, Mumble: 0, Offline: 0, InternalError: 0, unchecked: 0 }
  state.teams.forEach((r) =>
    r.services.forEach((c) => {
      const k = c.lastCheckStatus
      if (k === 'Ok' || k === 'Mumble' || k === 'Offline' || k === 'InternalError') counts[k]++
      else counts.unchecked++
    })
  )

  const enabledChallenges = state.challenges.filter((c) => c.isEnabled).length
  const visibleTeams = state.teams.filter(
    (r) => debouncedSearch === '' || r.teamName.toLowerCase().includes(debouncedSearch.toLowerCase())
  )

  // KotH equivalents for the header stats when the KotH view is active. Health
  // is per-hill (one shared box), not per-(team × challenge).
  const kothHills = koth?.hills ?? []
  const kothEnabledHills = kothHills.filter((h) => h.isEnabled).length
  const kothCounts = { Ok: 0, Mumble: 0, Offline: 0, InternalError: 0, unchecked: 0 }
  kothHills.forEach((h) => {
    const k = h.lastCheckStatus
    if (k === 'Ok' || k === 'Mumble' || k === 'Offline' || k === 'InternalError') kothCounts[k]++
    else kothCounts.unchecked++
  })
  // View-aware header values (A&D grid vs KotH hills).
  const headerCounts = showKoth ? kothCounts : counts
  const headerEnabled = showKoth ? kothEnabledHills : enabledChallenges
  const headerTotal = showKoth ? kothHills.length : state.challenges.length
  const tickSeconds = state.challenges[0]?.tickSeconds ?? koth?.tickSeconds ?? 60

  return (
    <WithGameEditTab>
      <SnapshotModal
        gameId={numId}
        target={snapTarget}
        selectedPath={selectedPath}
        onSelectPath={selectFile}
        onOpenShell={openShell}
        onClose={closeSnapshot}
      />
      <ContainerExecModal
        containerGuid={execTarget?.guid ?? null}
        containerTitle={execTarget?.title}
        opened={execTarget != null}
        onClose={closeShell}
      />
      <Stack gap="md">
        {/* Mission-control bar: round timing, scoring state, fleet health, actions */}
        <Paper p="md" withBorder radius="md">
          <Group justify="space-between" align="center" wrap="wrap" gap="lg">
            <Group gap="xl" wrap="wrap" align="center">
              {/* Round progress ring + number */}
              <Group gap="sm" wrap="nowrap" align="center">
                <RingProgress
                  size={76}
                  thickness={8}
                  roundCaps
                  sections={[{ value: roundPct, color: ringColor }]}
                  label={
                    <Text ta="center" fw={700} size="sm" c={ringColor === 'teal' ? undefined : ringColor}>
                      {ringLabel}
                    </Text>
                  }
                />
                <Stack gap={2}>
                  <Text size="xs" c="dimmed" tt="uppercase" fw={600}>
                    {t('admin.content.ad_ops.current_round', 'Round')}
                  </Text>
                  <Group gap={6} align="center" wrap="nowrap">
                    <Text fw="bold" size="xl" lh={1}>
                      {state.currentRound ?? '—'}
                    </Text>
                    {state.scoringPaused ? (
                      <Badge
                        color="orange"
                        variant="light"
                        leftSection={<Icon path={mdiPauseCircleOutline} size={0.6} />}
                      >
                        {t('admin.content.ad_ops.scoring_paused', 'Scoring paused')}
                      </Badge>
                    ) : (
                      // Stay "Live" between ticks too (countdown hitting 0 is a
                      // ~5s gap before the scheduler advances) — toggling the
                      // badge there reflowed the whole header row.
                      state.currentRound != null && (
                        <Badge color="teal" variant="dot">
                          {t('admin.content.ad_ops.live', 'Live')}
                        </Badge>
                      )
                    )}
                  </Group>
                </Stack>
              </Group>

              {/* Challenges / hills enabled */}
              <Stack gap={2}>
                <Text size="xs" c="dimmed" tt="uppercase" fw={600}>
                  {showKoth
                    ? t('admin.content.ad_ops.hills_active', 'Hills')
                    : t('admin.content.ad_ops.challenges_active', 'Challenges')}
                </Text>
                <Text fw="bold" size="xl" lh={1}>
                  {headerEnabled}/{headerTotal}
                </Text>
              </Stack>

              {/* Flag cycle (A&D) / pristine crown-cycle reset (KotH) — game-global tick */}
              <Stack gap={2}>
                <Text size="xs" c="dimmed" tt="uppercase" fw={600}>
                  {showKoth
                    ? t('admin.content.ad_ops.hill_cycle', 'Hill cycle')
                    : t('admin.content.ad_ops.flag_cycle', 'Flag cycle')}
                </Text>
                <Text fw={600} size="sm" lh={1.3}>
                  {showKoth
                    ? t('admin.content.ad_ops.koth.tick_summary', {
                        tick: tickSeconds,
                        cycle: koth?.cycleTicks ?? 3,
                        defaultValue: 'tick {{tick}}s · pristine reset every {{cycle}} ticks',
                      })
                    : t('admin.content.ad_ops.tick_summary', {
                        tick: tickSeconds,
                        lifetime: state.challenges[0]?.flagLifetimeTicks ?? 5,
                        defaultValue: 'tick {{tick}}s · lifetime {{lifetime}} ticks',
                      })}
                </Text>
              </Stack>

              {/* Fleet-wide health — A&D services or KotH hills */}
              <Stack gap={4}>
                <Text size="xs" c="dimmed" tt="uppercase" fw={600}>
                  {showKoth
                    ? t('admin.content.ad_ops.hill_health', 'Hill health')
                    : t('admin.content.ad_ops.service_health', 'Service health')}
                </Text>
                <Group gap="md" wrap="nowrap">
                  <HealthChip icon={mdiCheckCircle} color="teal" count={headerCounts.Ok} label="Ok" />
                  <HealthChip icon={mdiAlertCircle} color="yellow" count={headerCounts.Mumble} label="Mumble" />
                  <HealthChip icon={mdiCloseCircle} color="red" count={headerCounts.Offline} label="Offline" />
                  <HealthChip icon={mdiHelpCircle} color="gray" count={headerCounts.InternalError} label="Error" />
                  {headerCounts.unchecked > 0 && (
                    <HealthChip
                      icon={mdiHelpCircle}
                      color="dark"
                      count={headerCounts.unchecked}
                      label={t('admin.content.ad_ops.health_unchecked', 'Unchecked')}
                    />
                  )}
                </Group>
              </Stack>
            </Group>

            <Group gap="sm" wrap="wrap" justify={isMobile ? 'flex-end' : undefined}>
              <Button
                leftSection={<Icon path={mdiRefresh} size={0.9} />}
                variant="default"
                size={isMobile ? 'xs' : 'sm'}
                disabled={busy}
                onClick={() => {
                  mutate()
                  mutateKoth()
                }}
              >
                {t('admin.button.ad_ops.refresh', 'Refresh')}
              </Button>
              <Button
                leftSection={<Icon path={mdiPlayCircle} size={0.9} />}
                variant="default"
                size={isMobile ? 'xs' : 'sm'}
                disabled={busy}
                onClick={ensureContainers}
              >
                {t('admin.button.ad_ops.ensure_containers', 'Ensure containers')}
              </Button>
              <Button
                leftSection={<Icon path={state.scoringPaused ? mdiPlayCircle : mdiPauseCircleOutline} size={0.9} />}
                variant="default"
                color={state.scoringPaused ? 'teal' : 'orange'}
                size={isMobile ? 'xs' : 'sm'}
                disabled={busy}
                onClick={toggleScoringPause}
              >
                {state.scoringPaused
                  ? t('admin.button.ad_ops.resume_scoring', 'Resume scoring')
                  : t('admin.button.ad_ops.pause_scoring', 'Pause scoring')}
              </Button>
            </Group>
          </Group>
          <Alert mt="md" color="cyan" variant="light" icon={<Icon path={mdiInformationOutline} size={0.9} />}>
            <Text size="sm">
              {t(
                'admin.content.ad_ops.automatic_scoring_rounds',
                'Official epoch rounds advance automatically through flag delivery and the checker pipeline. Manual advance is disabled so every scored round has complete evidence.'
              )}
            </Text>
          </Alert>
        </Paper>

        {/* Team × challenge grid */}
        <Paper p="md" withBorder radius="md">
          <Group justify="space-between" mb="sm" wrap="wrap" gap="sm">
            <Group gap="sm" align="center">
              {showViewSwitch && (
                <SegmentedControl
                  size="xs"
                  aria-label={t('admin.label.ad_ops.game_mode', 'Game mode')}
                  value={showKoth ? 'koth' : 'ad'}
                  onChange={(v) => setView(v as 'ad' | 'koth')}
                  data={[
                    { value: 'ad', label: t('admin.content.ad_ops.view_ad', 'A&D') },
                    { value: 'koth', label: t('admin.content.ad_ops.view_koth', 'KotH') },
                  ]}
                />
              )}
              <Title order={4}>
                {showKoth
                  ? t('admin.content.ad_ops.koth.grid_title', 'Hills')
                  : t('admin.content.ad_ops.grid_title', 'Team status')}
              </Title>
              <Badge variant="light" color="gray">
                {showKoth
                  ? t('admin.content.ad_ops.koth.hills_count', {
                      count: kothHills.length,
                      defaultValue_one: '{{count}} hill',
                      defaultValue_other: '{{count}} hills',
                    })
                  : t('admin.content.ad_ops.teams_count', {
                      count: visibleTeams.length,
                      defaultValue: '{{count}} teams',
                    })}
              </Badge>
            </Group>
            {!showKoth && (
              <TextInput
                size="xs"
                w={260}
                maw="100%"
                aria-label={t('admin.placeholder.ad_ops.search_team', 'Filter teams')}
                leftSection={<Icon path={mdiMagnify} size={0.8} />}
                placeholder={t('admin.placeholder.ad_ops.search_team', 'Filter teams…')}
                value={search}
                onChange={(e) => setSearch(e.currentTarget.value)}
              />
            )}
          </Group>

          {showKoth && koth ? (
            <KothOpsPanel
              gameId={numId}
              koth={koth}
              onShell={openShell}
              onToggleHill={toggleHill}
              busyHill={busyHill}
              onMutate={() => mutateKoth()}
            />
          ) : state.teams.length === 0 ? (
            <Alert color="orange" icon={<Icon path={mdiAlertCircleOutline} size={1} />}>
              {t(
                'admin.content.ad_ops.no_teams',
                'No accepted teams yet. Once you accept teams from the participations page, their containers will spin up automatically.'
              )}
            </Alert>
          ) : (
            <ScrollArea.Autosize mah="55vh" type="auto">
              <Table verticalSpacing="xs" striped highlightOnHover withColumnBorders>
                <Table.Caption>
                  {t('admin.content.ad_ops.table_caption', 'Attack-defense team service status')}
                </Table.Caption>
                <Table.Thead className={tableClasses.thead}>
                  <Table.Tr>
                    <Table.Th scope="col" className={tableClasses.corner}>
                      {t('admin.content.ad_ops.column_team', 'Team')}
                    </Table.Th>
                    {state.challenges.map((c) => (
                      <Table.Th scope="col" key={c.challengeId}>
                        <Group gap={6} wrap="nowrap" justify="space-between">
                          <Text
                            truncate
                            fw="bold"
                            size="sm"
                            c={c.isEnabled ? undefined : 'dimmed'}
                            style={{ flex: 1, minWidth: 0 }}
                          >
                            {c.title}
                          </Text>
                          {c.isEnabled ? (
                            <Tooltip
                              label={t('admin.content.ad_ops.teams_with_container', {
                                count: c.teamsWithLiveContainer ?? 0,
                                defaultValue: '{{count}} live',
                              })}
                              withArrow
                            >
                              <Badge size="xs" variant="light" color={c.teamsWithLiveContainer ? 'blue' : 'gray'}>
                                {c.teamsWithLiveContainer ?? 0}
                              </Badge>
                            </Tooltip>
                          ) : (
                            <Tooltip
                              label={t(
                                'admin.tooltip.ad_ops.challenge_off',
                                'Disabled on the Challenges page — no scoring or flag rotation'
                              )}
                              withArrow
                            >
                              <Badge size="xs" variant="light" color="gray">
                                {t('admin.content.ad_ops.challenge_off', 'off')}
                              </Badge>
                            </Tooltip>
                          )}
                        </Group>
                      </Table.Th>
                    ))}
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {visibleTeams.map((row) => (
                    <Table.Tr key={row.participationId}>
                      <Table.Td className={tableClasses.left}>
                        <Text truncate fw="bold" size="sm" maw="12rem">
                          {row.teamName}
                        </Text>
                      </Table.Td>
                      {state.challenges.map((c) => {
                        const cell = row.services.find((s) => s.challengeId === c.challengeId)
                        const sm = statusMeta(cell?.lastCheckStatus)
                        return (
                          <Table.Td key={c.challengeId}>
                            {cell ? (
                              <Stack gap={6}>
                                <Group justify="space-between" wrap="nowrap" gap={4}>
                                  <Menu
                                    shadow="md"
                                    position="bottom-start"
                                    withinPortal
                                    disabled={cell.lastCheckId == null}
                                  >
                                    <Menu.Target>
                                      <UnstyledButton
                                        disabled={cell.lastCheckId == null}
                                        className={tableClasses.statusControl}
                                        aria-label={t('admin.tooltip.ad_ops.override_status', {
                                          defaultValue: 'Override SLA verdict. Current status: {{status}}',
                                          status: cell.lastCheckStatus ?? '—',
                                        })}
                                      >
                                        <Badge
                                          size="sm"
                                          color={sm.color}
                                          variant={cell.lastCheckStatus ? 'light' : 'outline'}
                                          leftSection={<Icon path={sm.icon} size={0.55} />}
                                        >
                                          {cell.lastCheckStatus ?? '—'}
                                        </Badge>
                                      </UnstyledButton>
                                    </Menu.Target>
                                    <Menu.Dropdown>
                                      <Menu.Label>
                                        {t('admin.content.ad_ops.override_label', 'Override SLA verdict')}
                                      </Menu.Label>
                                      {OVERRIDE_STATUSES.map((s) => (
                                        <Menu.Item
                                          key={s.value}
                                          leftSection={
                                            <Icon
                                              path={s.icon}
                                              size={0.7}
                                              color={`var(--mantine-color-${s.color}-6)`}
                                            />
                                          }
                                          onClick={() =>
                                            cell.lastCheckId != null && overrideCheck(cell.lastCheckId, s.value)
                                          }
                                        >
                                          {s.value}
                                        </Menu.Item>
                                      ))}
                                    </Menu.Dropdown>
                                  </Menu>
                                  <Group gap={2} wrap="nowrap">
                                    {cell.containerGuid && (
                                      <Tooltip
                                        label={t('admin.tooltip.ad_ops.shell', 'Open a shell in this container')}
                                        withArrow
                                      >
                                        <ActionIcon
                                          size={44}
                                          variant="subtle"
                                          color="blue"
                                          aria-label={t('admin.tooltip.ad_ops.shell', 'Open a shell in this container')}
                                          onClick={() =>
                                            setExecTarget({
                                              guid: cell.containerGuid!,
                                              title: `${row.teamName} · ${c.title}`,
                                            })
                                          }
                                        >
                                          <Icon path={mdiConsole} size={0.7} />
                                        </ActionIcon>
                                      </Tooltip>
                                    )}
                                    <Tooltip
                                      label={t(
                                        'admin.tooltip.ad_ops.restart',
                                        'Reset to base image — wipes the team’s changes (bypasses player cooldown)'
                                      )}
                                      withArrow
                                    >
                                      <ActionIcon
                                        size={44}
                                        variant="subtle"
                                        color="gray"
                                        aria-label={t(
                                          'admin.tooltip.ad_ops.reset',
                                          'Reset container to its base image'
                                        )}
                                        onClick={() => resetCell(cell)}
                                      >
                                        <Icon path={mdiRestart} size={0.7} />
                                      </ActionIcon>
                                    </Tooltip>
                                    {(cell.snapshotAvailable || cell.containerGuid) && (
                                      <Tooltip
                                        label={
                                          cell.snapshotAvailable
                                            ? t('admin.tooltip.ad_ops.snapshot', 'Inspect post-game snapshot')
                                            : t(
                                                'admin.tooltip.ad_ops.snapshot_live',
                                                'Inspect filesystem changes (live)'
                                              )
                                        }
                                        withArrow
                                      >
                                        <Indicator
                                          disabled={cell.changedFileCount == null}
                                          label={cell.changedFileCount}
                                          size={15}
                                          color="grape"
                                          offset={3}
                                        >
                                          <ActionIcon
                                            size={44}
                                            variant="subtle"
                                            color="grape"
                                            aria-label={t('admin.tooltip.ad_ops.snapshot', 'Open file snapshot')}
                                            onClick={() => openSnapshot(cell)}
                                          >
                                            <Icon path={mdiFileTree} size={0.7} />
                                          </ActionIcon>
                                        </Indicator>
                                      </Tooltip>
                                    )}
                                  </Group>
                                </Group>
                                {cell.selfHosted && (
                                  <Tooltip
                                    label={t(
                                      'admin.tooltip.ad_ops.self_hosted',
                                      'Self-hosted / Bring Your Own Container (BYOC): the team runs the service on their own machine. There is no RSCTF-hosted container to shell into, snapshot, or read files from — only the SLA status is meaningful.'
                                    )}
                                  >
                                    <Badge size="xs" color="grape" variant="light" style={{ width: 'fit-content' }}>
                                      {t('admin.content.ad_ops.self_hosted', 'self-hosted')}
                                    </Badge>
                                  </Tooltip>
                                )}
                                {cell.containerIp && (
                                  <CopyButton value={`${cell.containerIp}:${cell.containerPort ?? ''}`}>
                                    {({ copied, copy }) => (
                                      <Tooltip
                                        label={
                                          copied
                                            ? t('game.tooltip.copy.copied', 'Copied')
                                            : t('game.tooltip.copy.ip_port', 'Copy IP:port')
                                        }
                                      >
                                        <UnstyledButton
                                          onClick={copy}
                                          aria-label={t('game.tooltip.copy.ip_port', 'Copy IP:port')}
                                          className={tableClasses.copyControl}
                                        >
                                          <Text className={misc.ffmono} size="xs">
                                            {cell.containerIp}:{cell.containerPort}
                                          </Text>
                                        </UnstyledButton>
                                      </Tooltip>
                                    )}
                                  </CopyButton>
                                )}
                                {cell.currentFlag && (
                                  <CopyButton value={cell.currentFlag}>
                                    {({ copied, copy }) => (
                                      <Tooltip
                                        label={
                                          copied
                                            ? t('game.tooltip.copy.copied', 'Copied')
                                            : t('admin.tooltip.ad_ops.copy_flag', 'Copy current flag')
                                        }
                                      >
                                        <UnstyledButton
                                          onClick={copy}
                                          aria-label={t('admin.tooltip.ad_ops.copy_flag', 'Copy current flag')}
                                          className={tableClasses.copyControl}
                                        >
                                          <Text className={misc.ffmono} size="xs" c="dimmed" truncate maw="14rem">
                                            {cell.currentFlag}
                                          </Text>
                                        </UnstyledButton>
                                      </Tooltip>
                                    )}
                                  </CopyButton>
                                )}
                              </Stack>
                            ) : (
                              <Center>
                                <Icon path={mdiClose} size={0.8} color="var(--mantine-color-dimmed)" />
                              </Center>
                            )}
                          </Table.Td>
                        )
                      })}
                    </Table.Tr>
                  ))}
                  {visibleTeams.length === 0 && (
                    <Table.Tr>
                      <Table.Td colSpan={state.challenges.length + 1}>
                        <Text ta="center" c="dimmed" py="md" size="sm">
                          {t('admin.content.ad_ops.no_team_match', 'No teams match the filter.')}
                        </Text>
                      </Table.Td>
                    </Table.Tr>
                  )}
                </Table.Tbody>
              </Table>
            </ScrollArea.Autosize>
          )}
        </Paper>
      </Stack>
    </WithGameEditTab>
  )
}

export default AdOps
