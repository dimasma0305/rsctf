import {
  Alert,
  Badge,
  Button,
  Combobox,
  Group,
  Paper,
  ScrollArea,
  Table,
  Text,
  Title,
  Card,
  ThemeIcon,
  UnstyledButton,
  Center,
  Modal,
  Stack,
  TextInput,
  Menu,
  Select,
  Loader,
  Tabs,
  Tooltip,
  Box,
  Divider,
  ActionIcon,
  Progress,
  Popover,
  Pagination,
  VisuallyHidden,
  useCombobox,
} from '@mantine/core'
import type { StyleProp } from '@mantine/core'
import { useClipboard, useDebouncedValue, useReducedMotion } from '@mantine/hooks'
import { useDisclosure } from '@mantine/hooks'
import { modals } from '@mantine/modals'
import { showNotification } from '@mantine/notifications'
import {
  mdiCheckCircle,
  mdiGhost,
  mdiIpNetwork,
  mdiArrowUp,
  mdiArrowDown,
  mdiUnfoldMoreHorizontal,
  mdiInformation,
  mdiMagnify,
  mdiShieldAlert,
  mdiChevronDown,
  mdiAccountGroup,
  mdiAlertCircle,
  mdiClockOutline,
  mdiOpenInNew,
  mdiDownload,
  mdiCubeOutline,
  mdiContentCopy,
  mdiCheck,
  mdiFingerprint,
  mdiSwapHorizontal,
  mdiRefresh,
  mdiLockAlert,
  mdiChevronRight,
  mdiClose,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import relativeTime from 'dayjs/plugin/relativeTime'
import type { TFunction } from 'i18next'
import * as React from 'react'
import { FC, useState, useMemo, useCallback, useRef, useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { ScrollingText } from '@Components/ScrollingText'
import { evidenceContribution } from '@Utils/AntiCheat'
import { useLanguage } from '@Utils/I18n'
import { showErrorMsg, tryGetErrorMsg, useParticipationStatusMap } from '@Utils/Shared'
import type {
  AbnormalSolveResult,
  CheatReport,
  CollusionCompareResult,
  SequenceSuspectDetail,
  SuspicionRecordResult,
  CollusionGroupResult,
  CollusionTeamInfo,
  IdentityOverlapResult,
  IpAnalysisResult,
} from '@Api'
import api, { ParticipationStatus } from '@Api'
import tableClasses from '@Styles/Table.module.css'
import classes from './CheatInfo.module.css'
import { FusedEvidencePanel } from './FusedEvidencePanel'
import { SuspicionEvidenceReviewPanel } from './SuspicionEvidenceReview'

dayjs.extend(relativeTime)

interface CheatInfoProps {
  report: CheatReport | null
  mutate?: () => Promise<CheatReport | undefined>
  canManageParticipations?: boolean
}

interface SortConfig<T> {
  key: keyof T | null
  direction: 'asc' | 'desc'
}

function sortData<T>(data: T[], { key, direction }: SortConfig<T>) {
  if (!key) return data

  return [...data].sort((a, b) => {
    const valueA = a[key]
    const valueB = b[key]

    if (valueA === valueB) return 0
    let compare: number
    if (valueA == null) compare = 1
    else if (valueB == null) compare = -1
    else if (typeof valueA === 'number' && typeof valueB === 'number') compare = valueA - valueB
    else compare = String(valueA).localeCompare(String(valueB))
    return direction === 'asc' ? compare : -compare
  })
}

// ... ThSort ...
interface ThSortProps {
  children: React.ReactNode
  reversed: boolean
  sorted: boolean
  onSort(): void
  w?: string | number
  miw?: string | number
}

function ThSort({ children, reversed, sorted, onSort, w, miw }: ThSortProps) {
  const IconPath = sorted ? (reversed ? mdiArrowUp : mdiArrowDown) : mdiUnfoldMoreHorizontal
  return (
    <Table.Th w={w} miw={miw ?? w} scope="col" aria-sort={sorted ? (reversed ? 'descending' : 'ascending') : 'none'}>
      <UnstyledButton onClick={onSort} className={classes.control}>
        <Group justify="space-between">
          <Text fw={700} fz="sm">
            {children}
          </Text>
          <Center className={classes.icon}>
            <Icon path={IconPath} size={0.7} aria-hidden />
          </Center>
        </Group>
      </UnstyledButton>
    </Table.Th>
  )
}

const AccessibleTableCaption: FC<{ children: React.ReactNode }> = ({ children }) => (
  <Table.Caption>
    <VisuallyHidden>{children}</VisuallyHidden>
  </Table.Caption>
)

const AnalysisEmptyState: FC<{
  filtered: boolean
  description: string
  onClearFilters: () => void
}> = ({ filtered, description, onClearFilters }) => {
  const { t } = useTranslation()

  return (
    <Center className={classes.emptyState} py="xl" role="status" aria-live="polite">
      <Stack align="center" gap="xs">
        <ThemeIcon size={48} radius="xl" color="gray" variant="light">
          <Icon path={filtered ? mdiMagnify : mdiInformation} size={1.4} aria-hidden />
        </ThemeIcon>
        <Text fw={600} size="md">
          {filtered
            ? t('game.cheat_analysis.no_filter_matches', 'No results match these filters')
            : t('game.cheat_analysis.no_signals_snapshot', 'No signals in this report')}
        </Text>
        <Text size="sm" c="dimmed" ta="center">
          {filtered
            ? t('game.cheat_analysis.try_different_filters', 'Try a different search or clear the filters.')
            : description}
        </Text>
        {filtered && (
          <Button size="xs" variant="outline" color="gray" onClick={onClearFilters}>
            {t('game.cheat_analysis.clear_filters', 'Clear filters')}
          </Button>
        )}
      </Stack>
    </Center>
  )
}

interface DetailLine {
  label?: string
  value: string
}

// ── Risk band & evidence tier presentation ──────────────────────────────────
// The band — derived server-side from the highest evidence tier that fired — is
// the headline. Network/identity signals land in "context" (gray) and never
// push a team into a high band, no matter how many fire.
// One discrete risk ladder, reused everywhere: alert(red) → orange → yellow → gray.
// 'alert' is the theme's registered red scale (ThemeOverride.ts) — token-correct
// vs raw 'red', and visually reserved for data-driven danger only.
const BAND_META: Record<string, { label: string; color: string; desc: string }> = {
  evidenced: { label: 'Evidenced', color: 'alert', desc: 'Hard cross-team evidence (flag/session movement)' },
  investigate: { label: 'Investigate', color: 'orange', desc: 'Strong automation / scanner evidence' },
  watch: { label: 'Watch', color: 'yellow', desc: 'Low-confidence behavioral heuristics' },
  context: { label: 'Context', color: 'gray', desc: 'Network / identity correlation only — not suspicion' },
  clean: { label: 'No scored signals', color: 'gray', desc: 'No scored signals in this report snapshot' },
}
const bandMeta = (band?: string) => BAND_META[band ?? 'clean'] ?? BAND_META.clean

const TIER_META: Record<string, { label: string; color: string }> = {
  hard: { label: 'Hard', color: 'alert' },
  strong: { label: 'Strong', color: 'orange' },
  behavioral: { label: 'Behavioral', color: 'yellow' },
  context: { label: 'Context', color: 'gray' },
}
const tierMeta = (tier?: string) => TIER_META[tier ?? 'behavioral'] ?? TIER_META.behavioral

// A timestamp is "real" only if it parses AND isn't the 0001-01-01 / epoch default
// that some backend rows fall back to when a Time field is left unset — which
// dayjs().fromNow() would otherwise render as the absurd "2025 years ago". Route
// every relative/absolute time render through these guards.
const isRealTime = (t?: string | number | null): boolean => {
  if (t === null || t === undefined || t === '' || t === 0) return false
  const d = dayjs(t)
  return d.isValid() && d.year() > 2000
}
const fmtRelTime = (t?: string | number | null) => (isRealTime(t) ? dayjs(t).fromNow() : '—')
const fmtAbsTime = (t: string | number | null | undefined, locale?: string | null, fmt = 'YYYY-MM-DD HH:mm:ss') =>
  isRealTime(t)
    ? dayjs(t)
        .locale(locale || 'en')
        .format(fmt)
    : '—'

// Horizontal stacked bar showing the score COMPOSITION (hard / corroboration /
// strong / behavioral) at absolute scale, so a 2000-point team no longer renders
// the same as a 100-point one (unlike the old min(score,100) ring).
const RiskCompositionBar: FC<{ hard?: number; corroboration?: number; strong?: number; behavioral?: number }> = ({
  hard = 0,
  corroboration = 0,
  strong = 0,
  behavioral = 0,
}) => {
  const SCALE = 200 // px-equivalent reference; segments clamp to the track
  const seg = (v: number, color: string, key: string) =>
    v > 0 ? (
      <Box
        key={key}
        style={{ flexBasis: `${Math.min((v / SCALE) * 100, 100)}%`, backgroundColor: `var(--mantine-color-${color})` }}
      />
    ) : null
  return (
    <Box
      role="img"
      aria-label={`Risk composition: ${hard} hard, ${corroboration} corroborating, ${strong} strong, ${behavioral} behavioral points`}
      style={{
        display: 'flex',
        width: '100%',
        height: 12,
        borderRadius: 6,
        overflow: 'hidden',
        backgroundColor: 'var(--mantine-color-default-border)',
      }}
    >
      {seg(hard, 'alert-6', 'h')}
      {seg(corroboration, 'alert-3', 'c')}
      {seg(strong, 'orange-5', 's')}
      {seg(behavioral, 'yellow-5', 'b')}
      <Box style={{ flexGrow: 1 }} />
    </Box>
  )
}

// Clickable summary stat card — one flat semantic icon, no gradient, no fake bar.
// `accent` colors the number to draw the eye (used for Hard Evidence).
const SummaryCard: FC<{
  label: string
  value: number
  sub?: string
  icon: string
  color: string
  accent?: boolean
  active: boolean
  onClick: () => void
}> = ({ label, value, sub, icon, color, accent, active, onClick }) => (
  <UnstyledButton
    onClick={onClick}
    style={{ height: '100%' }}
    aria-label={`${label}: ${value}${sub ? `. ${sub}` : ''}`}
    aria-pressed={active}
  >
    <Card
      shadow="sm"
      padding="md"
      radius="md"
      withBorder
      className={`${classes.summaryCard} ${active ? classes.summaryCardActive : ''}`}
    >
      <Group justify="space-between" mb={6} wrap="nowrap">
        <Text fw={600} size="sm" c="dimmed" tt="uppercase" style={{ letterSpacing: '0.04em' }}>
          {label}
        </Text>
        <ThemeIcon size="md" radius="sm" variant="light" color={color}>
          <Icon path={icon} size={0.7} />
        </ThemeIcon>
      </Group>
      <Text
        component="p"
        size="xl"
        fw={720}
        lh={1}
        c={accent && value > 0 ? color : undefined}
        style={{ fontVariantNumeric: 'tabular-nums' }}
      >
        {value}
      </Text>
      {sub && (
        <Text size="xs" c="dimmed" mt={4}>
          {sub}
        </Text>
      )}
    </Card>
  </UnstyledButton>
)

const IP_TYPE_META: Record<string, { label: string; color: string; icon: string }> = {
  SharedIP: { label: 'Shared IP', color: 'blue', icon: mdiIpNetwork },
  SharedFingerprint: { label: 'Shared Fingerprint', color: 'violet', icon: mdiFingerprint },
  FingerprintChurn: { label: 'FP Churn', color: 'yellow', icon: mdiRefresh },
  IpChurn: { label: 'IP Churn', color: 'yellow', icon: mdiRefresh },
  CrossTeamIP: { label: 'Cross-Team IP', color: 'blue', icon: mdiSwapHorizontal },
  TokenAbuse: { label: 'Token Abuse', color: 'alert', icon: mdiLockAlert },
}

const parseDetailLines = (details?: string | null): DetailLine[] => {
  if (!details) return []

  const rawLines = details.includes('\n')
    ? details.split('\n')
    : details.includes(':')
      ? details.split(/[;|]/)
      : details.split(/\. (?=[A-Z])/)

  return rawLines
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      if (/^target\s+/i.test(line)) {
        return { label: 'Target', value: line.replace(/^target\s+/i, '').trim() }
      }

      const idx = line.indexOf(':')
      if (idx > 0) {
        return {
          label: line.slice(0, idx).trim(),
          value: line.slice(idx + 1).trim(),
        }
      }

      return { value: line }
    })
}

const CopyButton: FC<{ value: string }> = ({ value }) => {
  const { t } = useTranslation()
  const clipboard = useClipboard({ timeout: 1500 })
  return (
    <Tooltip
      label={clipboard.copied ? t('common.tab.copied', 'Copied!') : t('common.tab.copy', 'Copy')}
      withArrow
      position="top"
    >
      <ActionIcon
        size={14}
        variant="subtle"
        color={clipboard.copied ? 'green' : 'gray'}
        onClick={(e) => {
          e.stopPropagation()
          clipboard.copy(value)
        }}
        style={{ flexShrink: 0 }}
        aria-label={clipboard.copied ? t('common.tab.copied', 'Copied!') : t('common.tab.copy_value', 'Copy value')}
      >
        <Icon path={clipboard.copied ? mdiCheck : mdiContentCopy} size={0.5} aria-hidden />
      </ActionIcon>
    </Tooltip>
  )
}

const MemoizedCopyButton = React.memo(CopyButton)

const ReadableDetails: FC<{ details?: string | null; maxRows?: number }> = ({ details, maxRows = 3 }) => {
  const { t } = useTranslation()
  const lines = useMemo(() => parseDetailLines(details), [details])
  if (lines.length === 0)
    return (
      <Text size="xs" c="dimmed">
        —
      </Text>
    )

  // Find summary line (first unlabeled or label='Summary')
  const summaryLine = lines.find((l) => !l.label || l.label.toLowerCase() === 'summary')
  const kvLines = lines.filter((l) => l !== summaryLine && l.label)
  const visibleKv = kvLines.slice(0, maxRows)
  const hiddenKv = kvLines.slice(maxRows)
  const hasMore = hiddenKv.length > 0

  const renderKvLine = (line: DetailLine, idx: number) => (
    <Group key={idx} gap={4} align="flex-start" wrap="nowrap" style={{ minWidth: 0 }}>
      <Text size="xs" fw={600} c="dimmed" style={{ minWidth: 68, flexShrink: 0, lineHeight: 1.4 }}>
        {line.label}
      </Text>
      <Tooltip label={line.value} withArrow multiline maw={340} disabled={line.value.length <= 40}>
        <Text
          size="xs"
          c="dimmed"
          style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flex: 1, lineHeight: 1.4 }}
        >
          {line.value}
        </Text>
      </Tooltip>
      {line.value.length > 20 && <MemoizedCopyButton value={line.value} />}
    </Group>
  )

  return (
    <Stack gap={2} className={classes.detailsBox} style={{ maxWidth: '100%', overflow: 'hidden' }}>
      {/* Summary line — prominent */}
      {summaryLine && (
        <Text size="xs" fw={700} c="brand.5" style={{ lineHeight: 1.4 }}>
          {summaryLine.value}
        </Text>
      )}
      {/* Key-value lines */}
      {visibleKv.map(renderKvLine)}
      {/* Expand popover for hidden lines */}
      {hasMore && (
        <Popover width={400} position="bottom-end" withArrow shadow="lg" withinPortal>
          <Popover.Target>
            <UnstyledButton
              aria-label={t('game.cheat_analysis.show_more_fields', 'Show {{count}} more evidence fields', {
                count: hiddenKv.length,
              })}
            >
              <Group gap={3} style={{ userSelect: 'none' }} align="center">
                <Icon path={mdiChevronRight} size={0.55} color="var(--mantine-color-brand-5)" aria-hidden />
                <Text size="xs" c="brand" fw={600}>
                  {t('game.cheat_analysis.more_fields', '+{{count}} more fields', { count: hiddenKv.length })}
                </Text>
              </Group>
            </UnstyledButton>
          </Popover.Target>
          <Popover.Dropdown p="sm">
            <Stack gap={6}>
              <Group
                justify="space-between"
                pb={4}
                mb={2}
                style={{ borderBottom: '1px solid var(--mantine-color-dark-4)' }}
              >
                <Text size="xs" fw={700} c="dimmed">
                  {t('game.cheat_analysis.all_fields', 'All Fields')}
                </Text>
                {summaryLine && (
                  <Text size="xs" c="brand.5" fw={600}>
                    {summaryLine.value}
                  </Text>
                )}
              </Group>
              {kvLines.map(renderKvLine)}
            </Stack>
          </Popover.Dropdown>
        </Popover>
      )}
    </Stack>
  )
}

const MemoizedReadableDetails = React.memo(ReadableDetails)

const UsersCell: FC<{ users?: string[]; relatedUsers?: string[] }> = ({ users, relatedUsers }) => {
  const { t } = useTranslation()
  const currentUsers = (users ?? []).filter(Boolean)
  const others = (relatedUsers ?? []).filter(Boolean)

  if (currentUsers.length === 0 && others.length === 0) {
    return (
      <Text size="xs" c="dimmed">
        -
      </Text>
    )
  }

  const visible = [...currentUsers, ...others].slice(0, 3)
  const hidden = [...currentUsers, ...others].slice(3)

  return (
    <Group gap={4} wrap="wrap" className={classes.userWrap}>
      {visible.map((user, i) => (
        <Badge
          key={i}
          size="xs"
          color={currentUsers.includes(user) ? 'cyan' : 'gray'}
          variant="light"
          style={{ maxWidth: 120, overflow: 'hidden', textOverflow: 'ellipsis' }}
          title={user}
        >
          {user}
        </Badge>
      ))}
      {hidden.length > 0 && (
        <Popover width={260} position="top" withArrow shadow="md">
          <Popover.Target>
            <UnstyledButton
              aria-label={t('game.cheat_analysis.show_all_users', 'Show all users, including {{count}} more', {
                count: hidden.length,
              })}
            >
              <Badge size="xs" color="gray" variant="outline">
                {t('game.cheat_analysis.more', '+{{count}} more', { count: hidden.length })}
              </Badge>
            </UnstyledButton>
          </Popover.Target>
          <Popover.Dropdown>
            <Text size="xs" fw={700} c="dimmed" mb={4}>
              {t('game.cheat_analysis.all_users', 'All Users')}
            </Text>
            <Group gap={4} wrap="wrap">
              {[...currentUsers, ...others].map((user, i) => (
                <Badge key={i} size="xs" color={currentUsers.includes(user) ? 'cyan' : 'gray'} variant="light">
                  {user}
                </Badge>
              ))}
            </Group>
          </Popover.Dropdown>
        </Popover>
      )}
    </Group>
  )
}

const MemoizedUsersCell = React.memo(UsersCell)

// \u2500\u2500 Discord-style smart search \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

interface FilterDef {
  field: string
  description: string
  color: string
  icon: string
  example: string
}
interface ParsedFilter {
  field: string
  value: string
}
interface ParsedQuery {
  freeText: string
  filters: ParsedFilter[]
}

function parseSearchQuery(q: string): ParsedQuery {
  const filters: ParsedFilter[] = []
  const regex = /@(\w+):"([^"]*)"|@(\w+):([^\s@]+)/g
  const freeText = q
    .replace(regex, (_m, f1, v1, f2, v2) => {
      filters.push({ field: (f1 ?? f2).toLowerCase(), value: (v1 ?? v2).toLowerCase().trim() })
      return ''
    })
    .replace(/\s+/g, ' ')
    .trim()
  return { freeText, filters }
}

const IP_FILTER_DEFS: FilterDef[] = [
  { field: 'type', description: 'Anomaly type', color: 'orange', icon: mdiShieldAlert, example: '"shared ip"' },
  { field: 'ip', description: 'IP address / hash', color: 'blue', icon: mdiIpNetwork, example: '::ffff' },
  { field: 'user', description: 'Username', color: 'cyan', icon: mdiAccountGroup, example: 'dimas' },
  { field: 'time', description: 'Date or relative time', color: 'violet', icon: mdiClockOutline, example: '2025' },
  {
    field: 'details',
    description: 'Any detail field text',
    color: 'gray',
    icon: mdiInformation,
    example: 'source teams',
  },
]

const SOLVE_FILTER_DEFS: FilterDef[] = [
  { field: 'type', description: 'Solve anomaly type', color: 'orange', icon: mdiGhost, example: 'hoarding' },
  { field: 'challenge', description: 'Challenge name', color: 'teal', icon: mdiCubeOutline, example: 'web1' },
  { field: 'details', description: 'Detail field text', color: 'gray', icon: mdiInformation, example: 'seconds' },
  { field: 'time', description: 'Solve date/time', color: 'violet', icon: mdiClockOutline, example: '2025-01' },
]

const COLLUSION_FILTER_DEFS: FilterDef[] = [
  { field: 'team', description: 'Team name', color: 'blue', icon: mdiAccountGroup, example: 'aaa' },
  {
    field: 'similarity',
    description: 'Min similarity % (e.g. >80)',
    color: 'alert',
    icon: mdiAlertCircle,
    example: '>80',
  },
  { field: 'details', description: 'Detail text', color: 'gray', icon: mdiInformation, example: 'ring' },
]

const SUSPICION_FILTER_DEFS: FilterDef[] = [
  { field: 'team', description: 'Team name', color: 'blue', icon: mdiAccountGroup, example: 'ggg' },
  { field: 'band', description: 'Risk band', color: 'alert', icon: mdiAlertCircle, example: 'evidenced' },
  { field: 'score', description: 'Min risk score (e.g. >80)', color: 'orange', icon: mdiAlertCircle, example: '>80' },
  { field: 'status', description: 'Participation status', color: 'green', icon: mdiCheckCircle, example: 'approved' },
]

const GLOBAL_FILTER_DEFS: FilterDef[] = [
  { field: 'team', description: 'Team name (All tabs)', color: 'blue', icon: mdiAccountGroup, example: 'aaa' },
  { field: 'user', description: 'Username (IP)', color: 'cyan', icon: mdiAccountGroup, example: 'dimas' },
  { field: 'ip', description: 'IP address (IP)', color: 'blue', icon: mdiIpNetwork, example: '192.168' },
  {
    field: 'type',
    description: 'Anomaly type (IP, Solves)',
    color: 'orange',
    icon: mdiShieldAlert,
    example: 'hoarding',
  },
  { field: 'challenge', description: 'Challenge name (Solves)', color: 'teal', icon: mdiCubeOutline, example: 'web1' },
  { field: 'band', description: 'Risk band (Suspicion)', color: 'alert', icon: mdiAlertCircle, example: 'evidenced' },
  { field: 'score', description: 'Min risk score (Suspicion)', color: 'orange', icon: mdiAlertCircle, example: '>80' },
  { field: 'status', description: 'Status (Suspicion)', color: 'green', icon: mdiCheckCircle, example: 'approved' },
  {
    field: 'similarity',
    description: 'Similarity % (Collusion)',
    color: 'alert',
    icon: mdiAlertCircle,
    example: '>80',
  },
  { field: 'time', description: 'Date or time (IP, Solves)', color: 'violet', icon: mdiClockOutline, example: '2025' },
  { field: 'details', description: 'Detail text (Various)', color: 'gray', icon: mdiInformation, example: 'ring' },
]

const SmartSearch: FC<{
  value: string
  onChange: (v: string) => void
  placeholder?: string
  filterDefs: FilterDef[]
  w?: StyleProp<React.CSSProperties['width']>
}> = ({ value, onChange, placeholder, filterDefs, w = 300 }) => {
  const { t } = useTranslation()
  const inputRef = useRef<HTMLInputElement>(null)
  const [atQuery, setAtQuery] = useState('')
  const combobox = useCombobox({
    onDropdownClose: () => combobox.resetSelectedOption(),
  })

  const matchingDefs = useMemo(
    () => filterDefs.filter((f) => atQuery === '' || f.field.startsWith(atQuery.toLowerCase())),
    [filterDefs, atQuery]
  )

  const handleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const v = e.currentTarget.value
    onChange(v)
    const cursor = e.currentTarget.selectionStart ?? v.length
    const atMatch = v.slice(0, cursor).match(/@(\w*)$/)
    if (atMatch) {
      setAtQuery(atMatch[1])
      combobox.openDropdown()
    } else {
      combobox.closeDropdown()
    }
  }

  const selectFilter = (field: string) => {
    const cursor = inputRef.current?.selectionStart ?? value.length
    const before = value.slice(0, cursor)
    const after = value.slice(cursor)
    const atIdx = before.lastIndexOf('@')
    if (atIdx < 0) return
    const newVal = before.slice(0, atIdx) + `@${field}:"` + after
    onChange(newVal)
    combobox.closeDropdown()
    setTimeout(() => {
      if (inputRef.current) {
        const pos = atIdx + field.length + 3
        inputRef.current.focus()
        inputRef.current.setSelectionRange(pos, pos)
      }
    }, 0)
  }

  const removeFilter = (field: string, val: string) => {
    const normalizedField = field.toLowerCase()
    const normalizedValue = val.toLowerCase().trim()
    const withoutFilter = value.replace(
      /@(\w+):"([^"]*)"|@(\w+):([^\s@]+)/g,
      (match, quotedField, quotedValue, plainField, plainValue) => {
        const matchField = (quotedField ?? plainField).toLowerCase()
        const matchValue = (quotedValue ?? plainValue).toLowerCase().trim()
        return matchField === normalizedField && matchValue === normalizedValue ? '' : match
      }
    )
    onChange(withoutFilter.replace(/\s+/g, ' ').trim())
  }

  const parsed = useMemo(() => parseSearchQuery(value), [value])

  return (
    <Stack gap={4} w={w}>
      <Combobox store={combobox} onOptionSubmit={selectFilter} position="bottom-start" shadow="md" withinPortal>
        <Combobox.Target>
          <TextInput
            ref={inputRef}
            type="search"
            value={value}
            onChange={handleChange}
            placeholder={placeholder}
            aria-label={t('game.cheat_analysis.search_label', 'Search and filter analysis results')}
            size="xs"
            leftSection={<Icon path={mdiMagnify} size={0.8} aria-hidden />}
            rightSection={
              value ? (
                <ActionIcon
                  size={14}
                  variant="subtle"
                  color="gray"
                  onClick={() => {
                    onChange('')
                    combobox.closeDropdown()
                  }}
                  aria-label={t('game.cheat_analysis.clear_search', 'Clear search')}
                >
                  <Icon path={mdiClose} size={0.6} aria-hidden />
                </ActionIcon>
              ) : undefined
            }
            rightSectionPointerEvents="all"
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                combobox.closeDropdown()
              }
            }}
            onBlur={() => combobox.closeDropdown()}
            style={{ width: '100%' }}
          />
        </Combobox.Target>
        <Combobox.Dropdown hidden={matchingDefs.length === 0}>
          <Combobox.Options>
            <Text
              size="xs"
              c="dimmed"
              px={4}
              pb={4}
              mb={2}
              style={{ borderBottom: '1px solid var(--mantine-color-dark-5)' }}
            >
              {t('game.cheat_analysis.filter_hint_before', 'Type')}{' '}
              <Text span ff="monospace" c="brand.4" fw={700}>
                @field:"value"
              </Text>{' '}
              {t('game.cheat_analysis.filter_hint_after', 'to filter')}
            </Text>
            {matchingDefs.map((f) => (
              <Combobox.Option key={f.field} value={f.field} className={classes.filterOption}>
                <Group gap={8} px={4} py={3}>
                  <Badge
                    size="xs"
                    color={f.color}
                    variant="light"
                    leftSection={<Icon path={f.icon} size={0.4} aria-hidden />}
                    style={{ minWidth: 90, textAlign: 'center' }}
                  >
                    @{f.field}
                  </Badge>
                  <Text size="xs" c="dimmed" style={{ flex: 1 }}>
                    {f.description}
                  </Text>
                  <Text size="xs" c="dark.2" ff="monospace" fs="italic">
                    {f.example}
                  </Text>
                </Group>
              </Combobox.Option>
            ))}
          </Combobox.Options>
        </Combobox.Dropdown>
      </Combobox>
      {parsed.filters.length > 0 && (
        <Group gap={4} wrap="wrap" px={2}>
          {parsed.filters.map((f, i) => {
            const def = filterDefs.find((d) => d.field === f.field)
            return (
              <Badge
                key={`${f.field}:${f.value}:${i}`}
                size="xs"
                color={def?.color ?? 'gray'}
                variant="filled"
                rightSection={
                  <ActionIcon
                    size={10}
                    variant="transparent"
                    c="white"
                    onClick={() => removeFilter(f.field, f.value)}
                    style={{ marginLeft: 1 }}
                    aria-label={t('game.cheat_analysis.remove_filter', 'Remove {{field}} filter', { field: f.field })}
                  >
                    <Icon path={mdiClose} size={0.35} aria-hidden />
                  </ActionIcon>
                }
                style={{ cursor: 'default', paddingRight: 4 }}
              >
                @{f.field}:{f.value || '\u2026'}
              </Badge>
            )
          })}
          <Button variant="subtle" size="compact-xs" color="gray" onClick={() => onChange('')}>
            <Text size="xs" c="dimmed">
              {t('game.cheat_analysis.clear_all', 'Clear all')}
            </Text>
          </Button>
        </Group>
      )}
    </Stack>
  )
}

// \u2500\u2500 Filter field definitions per tab \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

// \u2500\u2500 Memoized Row Components \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

const SuspicionRow = React.memo<{
  item: SuspicionRecordResult
  index: number
  statusMap: ReturnType<typeof useParticipationStatusMap>
  canManageParticipations: boolean
  isChanging: boolean
  onStatusChange: (participationId: number, status: ParticipationStatus) => void
  onView: (item: SuspicionRecordResult) => void
}>(({ item, index, statusMap, canManageParticipations, isChanging, onStatusChange, onView }) => {
  const { t } = useTranslation()
  const score = item.score ?? 0
  const currentStatus = item.status ?? ParticipationStatus.Pending
  const statusMeta = statusMap.get(currentStatus)
  const band = bandMeta(item.band)

  const strongBand = item.band === 'evidenced' || item.band === 'investigate'
  const teamName = item.teamName || t('common.label.unknown', 'Unknown')
  const openDetails = () => onView(item)
  return (
    <Table.Tr
      style={{ cursor: 'pointer' }}
      onClick={openDetails}
      tabIndex={0}
      aria-label={t('game.cheat_analysis.open_team_suspicion', 'Open suspicion details for {{team}}', {
        team: teamName,
      })}
      onKeyDown={(event) => {
        if (event.currentTarget !== event.target) return
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault()
          openDetails()
        }
      }}
    >
      <Table.Td style={{ textAlign: 'center' }}>
        <Text size="xs" c="dimmed" fw={600}>
          #{index + 1}
        </Text>
      </Table.Td>
      <Table.Td miw="14rem" style={{ maxWidth: '18rem', overflow: 'hidden' }}>
        <Tooltip
          label={item.teamName || t('common.label.unknown', 'Unknown')}
          withArrow
          disabled={(item.teamName || '').length <= 24}
          multiline
          maw={280}
        >
          <Text size="sm" fw={700} className={classes.truncate} title={teamName}>
            {item.teamName || t('common.label.unknown', 'Unknown')}
          </Text>
        </Tooltip>
      </Table.Td>
      <Table.Td miw="11rem">
        <Tooltip
          label={t(`game.cheat_analysis.band_desc.${item.band ?? 'clean'}`, band.desc)}
          withArrow
          multiline
          maw={280}
        >
          <Group gap={6} wrap="nowrap">
            <Badge
              color={band.color}
              size="md"
              variant={item.band === 'context' || item.band === 'clean' ? 'light' : 'filled'}
              leftSection={<Icon path={mdiAlertCircle} size={0.5} />}
              style={{ minWidth: '6.5rem', textAlign: 'center' }}
            >
              {t(`game.cheat_analysis.band.${item.band ?? 'clean'}`, band.label)}
            </Badge>
            <Text
              size="xs"
              c={strongBand ? band.color : 'dimmed'}
              fw={700}
              ff="monospace"
              style={{ fontVariantNumeric: 'tabular-nums' }}
            >
              {score.toLocaleString()}
            </Text>
          </Group>
        </Tooltip>
      </Table.Td>
      <Table.Td miw="11rem" onClick={(e) => e.stopPropagation()}>
        {canManageParticipations && item.participationId !== undefined ? (
          <Menu shadow="md" width={200}>
            <Menu.Target>
              <UnstyledButton
                style={{ cursor: isChanging ? 'wait' : 'pointer' }}
                aria-label={t('game.cheat_analysis.change_team_status', 'Change status for {{team}}', {
                  team: teamName,
                })}
                disabled={isChanging}
              >
                <Badge
                  size="sm"
                  color={statusMeta?.color || 'gray'}
                  variant="light"
                  rightSection={
                    isChanging ? <Loader size={10} /> : <Icon path={mdiChevronDown} size={0.55} aria-hidden />
                  }
                >
                  {statusMeta?.title || t('common.label.unknown', 'Unknown')}
                </Badge>
              </UnstyledButton>
            </Menu.Target>
            <Menu.Dropdown>
              <Menu.Label>{t('admin.label.participation_status', 'Status')}</Menu.Label>
              {Array.from(statusMap.entries())
                .filter(
                  ([status]) => status === ParticipationStatus.Accepted || status === ParticipationStatus.Suspended
                )
                .map(([status, meta]) => (
                  <Menu.Item
                    key={status}
                    leftSection={
                      <Icon
                        path={meta.iconPath}
                        size={0.8}
                        color={meta.color === 'alert' ? 'red' : meta.color}
                        aria-hidden
                      />
                    }
                    onClick={() => onStatusChange(item.participationId!, status)}
                    disabled={currentStatus === status || isChanging}
                  >
                    {meta.title}
                  </Menu.Item>
                ))}
            </Menu.Dropdown>
          </Menu>
        ) : (
          <Badge size="sm" color={statusMeta?.color || 'gray'} variant="light">
            {statusMeta?.title || t('common.label.unknown', 'Unknown')}
          </Badge>
        )}
      </Table.Td>
      <Table.Td style={{ textAlign: 'center' }}>
        <Tooltip label={t('game.cheat_analysis.view_suspicion', 'View suspicion details')} withArrow>
          <ActionIcon
            variant="subtle"
            color="brand"
            size="sm"
            onClick={(event) => {
              event.stopPropagation()
              openDetails()
            }}
            aria-label={t('game.cheat_analysis.open_team_suspicion', 'Open suspicion details for {{team}}', {
              team: teamName,
            })}
          >
            <Icon path={mdiOpenInNew} size={0.7} aria-hidden />
          </ActionIcon>
        </Tooltip>
      </Table.Td>
    </Table.Tr>
  )
})

const IpAnalysisRow = React.memo<{
  item: IpAnalysisResult
  locale: string | null
}>(({ item, locale }) => {
  const { t } = useTranslation()
  const meta = (item.type ? IP_TYPE_META[item.type] : undefined) ?? {
    label: t('common.label.unknown', 'Unknown'),
    color: 'gray',
    icon: mdiInformation,
  }
  const absTime = useMemo(() => fmtAbsTime(item.time, locale), [item.time, locale])
  const relTime = useMemo(() => fmtRelTime(item.time), [item.time])

  return (
    <Table.Tr>
      <Table.Td miw="10rem" style={{ maxWidth: '14rem', overflow: 'hidden' }}>
        <Tooltip
          label={item.teamName || t('common.label.unknown', 'Unknown')}
          withArrow
          disabled={(item.teamName || '').length <= 20}
          multiline
          maw={240}
        >
          <Text
            size="sm"
            fw={700}
            title={item.teamName || t('common.label.unknown', 'Unknown')}
            style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
          >
            {item.teamName || t('common.label.unknown', 'Unknown')}
          </Text>
        </Tooltip>
      </Table.Td>
      <Table.Td w="10rem" miw="10rem">
        <Badge
          color={meta.color}
          size="xs"
          variant="light"
          leftSection={meta.icon ? <Icon path={meta.icon} size={0.45} /> : undefined}
        >
          {t(`game.cheat_analysis.ip_type.${item.type}`, meta.label)}
        </Badge>
      </Table.Td>
      <Table.Td miw="12rem">
        <MemoizedUsersCell users={item.userNames} relatedUsers={item.relatedUsers} />
      </Table.Td>
      <Table.Td miw="9rem" style={{ maxWidth: '14rem', overflow: 'hidden' }}>
        <Group gap={4} wrap="nowrap">
          <Tooltip label={item.ip || '-'} withArrow disabled={!item.ip || item.ip.length <= 20} multiline maw={360}>
            <Text
              ff="monospace"
              fz="xs"
              style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flex: 1 }}
            >
              {item.ip || '-'}
            </Text>
          </Tooltip>
          {item.ip && <MemoizedCopyButton value={item.ip} />}
        </Group>
      </Table.Td>
      <Table.Td miw="9rem">
        <Tooltip label={absTime} withArrow>
          <Group gap={4} wrap="nowrap" style={{ cursor: 'default' }}>
            <Icon path={mdiClockOutline} size={0.6} color="var(--mantine-color-dimmed)" />
            <Text fz="xs" c="dimmed">
              {relTime}
            </Text>
          </Group>
        </Tooltip>
      </Table.Td>
      <Table.Td style={{ maxWidth: '28rem', overflow: 'hidden' }}>
        <MemoizedReadableDetails details={item.details} maxRows={3} />
      </Table.Td>
    </Table.Tr>
  )
})

const AbnormalSolveRow = React.memo<{
  item: AbnormalSolveResult
  locale: string | null
  t: TFunction
}>(({ item, locale, t }) => {
  const typeColor =
    item.type === 'Hoarding'
      ? 'cyan'
      : item.type === 'NoDownload'
        ? 'violet'
        : item.type === 'NoContainer'
          ? 'indigo'
          : 'orange'
  const typeIcon = item.type === 'NoDownload' ? mdiDownload : item.type === 'NoContainer' ? mdiCubeOutline : mdiGhost
  const typeLabel =
    item.type === 'NoDownload'
      ? t('game.cheat_analysis.solve_type.NoDownload', 'No Download')
      : item.type === 'NoContainer'
        ? t('game.cheat_analysis.solve_type.NoContainer', 'No Container')
        : item.type
  const absTime = useMemo(() => fmtAbsTime(item.solveTime, locale), [item.solveTime, locale])
  const relTime = useMemo(() => fmtRelTime(item.solveTime), [item.solveTime])

  return (
    <Table.Tr>
      <Table.Td miw="10rem" style={{ maxWidth: '14rem', overflow: 'hidden' }}>
        <Tooltip
          label={item.teamName || t('common.label.unknown', 'Unknown')}
          withArrow
          disabled={(item.teamName || '').length <= 20}
          multiline
          maw={240}
        >
          <Text
            size="sm"
            fw={700}
            title={item.teamName || t('common.label.unknown', 'Unknown')}
            style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
          >
            {item.teamName || t('common.label.unknown', 'Unknown')}
          </Text>
        </Tooltip>
      </Table.Td>
      <Table.Td miw="12rem" style={{ maxWidth: '16rem', overflow: 'hidden' }}>
        <Tooltip
          label={item.challengeName || t('common.label.unknown', 'Unknown')}
          withArrow
          disabled={(item.challengeName || '').length <= 22}
          multiline
          maw={240}
        >
          <Text size="sm" style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
            {item.challengeName || t('common.label.unknown', 'Unknown')}
          </Text>
        </Tooltip>
      </Table.Td>
      <Table.Td miw="9rem">
        <Badge color={typeColor} size="xs" variant="light" leftSection={<Icon path={typeIcon} size={0.5} />}>
          {typeLabel}
        </Badge>
      </Table.Td>
      <Table.Td style={{ maxWidth: '28rem', overflow: 'hidden' }}>
        <MemoizedReadableDetails details={item.details} maxRows={3} />
      </Table.Td>
      <Table.Td miw="9rem">
        <Tooltip label={absTime} withArrow>
          <Group gap={4} wrap="nowrap" style={{ cursor: 'default' }}>
            <Icon path={mdiClockOutline} size={0.6} color="var(--mantine-color-dimmed)" />
            <Text fz="xs" c="dimmed">
              {relTime}
            </Text>
          </Group>
        </Tooltip>
      </Table.Td>
    </Table.Tr>
  )
})

const CollusionGroupRow = React.memo<{
  item: CollusionGroupResult
  onView: (item: CollusionGroupResult) => void
}>(({ item, onView }) => {
  const { t } = useTranslation()
  const rsi = item.averageRsi ?? 0
  const rsiPct = +(rsi * 100).toFixed(1)
  const rsiColor = rsi > 0.9 ? 'alert' : rsi > 0.8 ? 'orange' : 'yellow'
  const commonCount = item.commonSolves?.length ?? 0

  return (
    <Table.Tr>
      <Table.Td miw="16rem" style={{ maxWidth: '20rem', overflow: 'hidden' }}>
        <Stack gap={3}>
          {item.teams?.map((team: CollusionTeamInfo, idx: number) => (
            <Group key={team.participationId ?? team.id} gap={6} wrap="nowrap" style={{ minWidth: 0 }}>
              <Badge size="xs" variant="dot" color={idx === 0 ? 'brand' : 'violet'} />
              <Tooltip label={team.name} withArrow disabled={(team.name || '').length <= 24} multiline maw={280}>
                <Text
                  size="sm"
                  fw={600}
                  title={team.name}
                  style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
                >
                  {team.name}
                </Text>
              </Tooltip>
            </Group>
          ))}
        </Stack>
      </Table.Td>
      <Table.Td miw="12rem">
        <Stack gap={3}>
          <Group gap={6} justify="space-between">
            <Text fz="xs" fw={700} c={rsiColor}>
              {rsiPct}%
            </Text>
            <Badge size="xs" color={rsiColor} variant="light">
              {rsi > 0.9
                ? t('game.cheat_analysis.overlap.very_high', 'Very high overlap')
                : rsi > 0.8
                  ? t('game.cheat_analysis.overlap.high', 'High overlap')
                  : t('game.cheat_analysis.overlap.moderate', 'Moderate overlap')}
            </Badge>
          </Group>
          <Progress
            value={rsiPct}
            color={rsiColor}
            size="xs"
            radius="xs"
            aria-label={t('game.cheat_analysis.similarity_value', 'Similarity: {{value}}%', { value: rsiPct })}
          />
        </Stack>
      </Table.Td>
      <Table.Td miw="14rem">
        {commonCount === 0 ? (
          <Text fz="xs" c="dimmed">
            \u2014
          </Text>
        ) : (
          <Group gap={4} wrap="wrap">
            {item.commonSolves?.slice(0, 3).map((s: string, i: number) => (
              <Badge key={i} size="xs" variant="light" color="violet">
                {s}
              </Badge>
            ))}
            {commonCount > 3 && (
              <Popover width={300} position="top" withArrow shadow="md" withinPortal>
                <Popover.Target>
                  <UnstyledButton
                    aria-label={t(
                      'game.cheat_analysis.show_all_common_challenges',
                      'Show all {{count}} common challenges',
                      { count: commonCount }
                    )}
                  >
                    <Badge size="xs" variant="outline" color="violet">
                      {t('game.cheat_analysis.more', '+{{count}} more', { count: commonCount - 3 })}
                    </Badge>
                  </UnstyledButton>
                </Popover.Target>
                <Popover.Dropdown>
                  <Text size="xs" fw={700} c="dimmed" mb={6}>
                    {t('game.cheat_analysis.all_common_challenges', 'All {{count}} Common Challenges', {
                      count: commonCount,
                    })}
                  </Text>
                  <Group gap={4} wrap="wrap">
                    {item.commonSolves?.map((s: string, i: number) => (
                      <Badge key={i} size="xs" variant="light" color="violet">
                        {s}
                      </Badge>
                    ))}
                  </Group>
                </Popover.Dropdown>
              </Popover>
            )}
          </Group>
        )}
      </Table.Td>
      <Table.Td style={{ maxWidth: '28rem', overflow: 'hidden' }}>
        <MemoizedReadableDetails details={item.details} maxRows={3} />
      </Table.Td>
      <Table.Td style={{ textAlign: 'center' }}>
        <Tooltip label={t('game.cheat_analysis.view_collusion', 'View similarity details')} withArrow>
          <ActionIcon
            variant="subtle"
            color="violet"
            size="sm"
            onClick={() => onView(item)}
            aria-label={t('game.cheat_analysis.view_collusion', 'View similarity details')}
          >
            <Icon path={mdiOpenInNew} size={0.7} aria-hidden />
          </ActionIcon>
        </Tooltip>
      </Table.Td>
    </Table.Tr>
  )
})

export const CheatInfo: FC<CheatInfoProps> = ({ report, mutate, canManageParticipations = false }) => {
  const { t } = useTranslation()
  const { locale } = useLanguage()
  const statusMap = useParticipationStatusMap()
  const params = useParams()
  const gameId = parseInt(params.id || '0')

  // 1. IP Analysis Sort State
  const [ipSort, setIpSort] = useState<SortConfig<IpAnalysisResult>>({ key: null, direction: 'asc' })

  // 2. Abnormal Solves Sort State
  const [solveSort, setSolveSort] = useState<SortConfig<AbnormalSolveResult>>({ key: null, direction: 'asc' })

  // 3. Suspicion Sort State
  // Default to the server's band-first order (hard evidence on top); clicking the
  // Score header switches to an explicit raw-total sort.
  const [suspSort, setSuspSort] = useState<SortConfig<SuspicionRecordResult>>({ key: 'band', direction: 'desc' })

  const [opened, { open, close }] = useDisclosure(false)
  const [selectedGroup, setSelectedGroup] = useState<CollusionGroupResult | null>(null)

  // Suspicion Modal
  const [susOpened, { open: openSus, close: closeSus }] = useDisclosure(false)
  const [selectedSuspicion, setSelectedSuspicion] = useState<SuspicionRecordResult | null>(null)
  const [reviewEventId, setReviewEventId] = useState<number | null>(null)
  const [changingParticipationId, setChangingParticipationId] = useState<number | null>(null)

  const closeSuspicionDetails = useCallback(() => {
    setReviewEventId(null)
    closeSus()
  }, [closeSus])

  // Search states
  const [globalSearch, setGlobalSearch] = useState('')
  const [ipSearch, setIpSearch] = useState('')
  const [solveSearch, setSolveSearch] = useState('')
  const [collusionSearch, setCollusionSearch] = useState('')
  const [suspSearch, setSuspSearch] = useState('')

  const [debouncedGlobalSearch] = useDebouncedValue(globalSearch, 300)
  const [debouncedIpSearch] = useDebouncedValue(ipSearch, 300)
  const [debouncedSolveSearch] = useDebouncedValue(solveSearch, 300)
  const [debouncedCollusionSearch] = useDebouncedValue(collusionSearch, 300)
  const [debouncedSuspSearch] = useDebouncedValue(suspSearch, 300)

  const globalParsed = useMemo(() => parseSearchQuery(debouncedGlobalSearch), [debouncedGlobalSearch])

  // Pagination states
  const [ipPage, setIpPage] = useState(1)
  const [solvePage, setSolvePage] = useState(1)
  const [collusionPage, setCollusionPage] = useState(1)
  const [suspPage, setSuspPage] = useState(1)
  const ITEMS_PER_PAGE = 50

  // Pair selection for drill-down
  const [teamAId, setTeamAId] = useState<number | null>(null)
  const [teamBId, setTeamBId] = useState<number | null>(null)
  const [activeTab, setActiveTab] = useState<string | null>('suspicion')
  const evidenceTabsRef = useRef<HTMLDivElement>(null)
  const reducedMotion = useReducedMotion()

  useEffect(() => {
    const scroller = evidenceTabsRef.current
    const activeItem = scroller?.querySelector<HTMLElement>('[role="tab"][data-active]')
    if (!scroller || !activeItem) return

    const target = activeItem.offsetLeft - (scroller.clientWidth - activeItem.offsetWidth) / 2
    scroller.scrollTo({ left: Math.max(0, target), behavior: reducedMotion ? 'auto' : 'smooth' })
  }, [activeTab, reducedMotion])

  useEffect(() => {
    if (!susOpened || !selectedSuspicion) return

    const refreshed = report?.suspicionList?.find(
      (item) =>
        (selectedSuspicion.participationId !== undefined &&
          item.participationId === selectedSuspicion.participationId) ||
        (selectedSuspicion.teamId !== undefined && item.teamId === selectedSuspicion.teamId)
    )
    if (refreshed) {
      setSelectedSuspicion(refreshed)
    } else {
      setSelectedSuspicion(null)
      closeSuspicionDetails()
    }
  }, [closeSuspicionDetails, report?.suspicionList, selectedSuspicion, susOpened])

  // 5. Collusion Group Sort State
  const [collusionSort, setCollusionSort] = useState<SortConfig<CollusionGroupResult>>({
    key: 'averageRsi',
    direction: 'desc',
  })

  // Reset pagination on search or sort change
  useEffect(() => {
    setIpPage(1)
    setSolvePage(1)
    setCollusionPage(1)
    setSuspPage(1)
  }, [debouncedGlobalSearch])

  useEffect(() => {
    setIpPage(1)
  }, [debouncedIpSearch, ipSort])
  useEffect(() => {
    setSolvePage(1)
  }, [debouncedSolveSearch, solveSort])
  useEffect(() => {
    setCollusionPage(1)
  }, [debouncedCollusionSearch, collusionSort])
  useEffect(() => {
    setSuspPage(1)
  }, [debouncedSuspSearch, suspSort])

  const {
    data: drilledSolves,
    error: drillError,
    isLoading: isDrilling,
    isValidating: isDrillRefreshing,
    mutate: retryDrill,
  } = api.cheatReport.useCheatReportCompare(gameId, teamAId, teamBId, {
    keepPreviousData: false,
    shouldRetryOnError: false,
  })

  const defaultTeamAId = selectedGroup?.teams?.[0]?.participationId
  const defaultTeamBId = selectedGroup?.teams?.[1]?.participationId
  const embeddedComparison = useMemo<CollusionCompareResult | undefined>(() => {
    if (
      !selectedGroup?.detailedSolves ||
      teamAId === null ||
      teamBId === null ||
      teamAId !== defaultTeamAId ||
      teamBId !== defaultTeamBId
    ) {
      return undefined
    }

    return {
      rsi: selectedGroup.averageRsi ?? 0,
      details: selectedGroup.detailedSolves,
    }
  }, [defaultTeamAId, defaultTeamBId, selectedGroup, teamAId, teamBId])
  const activeComparison = drilledSolves ?? embeddedComparison

  const handleViewDetails = (item: CollusionGroupResult) => {
    setSelectedGroup(item)
    if (item.teams && item.teams.length >= 2) {
      setTeamAId(item.teams[0].participationId ?? null)
      setTeamBId(item.teams[1].participationId ?? null)
    } else {
      setTeamAId(null)
      setTeamBId(null)
    }
    open()
  }

  const closeCollusionDetails = () => {
    close()
    setSelectedGroup(null)
    setTeamAId(null)
    setTeamBId(null)
  }

  const handleViewSuspicion = useCallback(
    (item: SuspicionRecordResult) => {
      setReviewEventId(null)
      setSelectedSuspicion(item)
      openSus()
    },
    [openSus]
  )

  const updateParticipationStatus = useCallback(
    async (participationId: number, status: ParticipationStatus) => {
      setChangingParticipationId(participationId)
      try {
        await api.admin.adminParticipation(participationId, { status })
        await mutate?.()
        showNotification({ title: t('common.notify.success'), message: t('common.notify.updated'), color: 'green' })
      } catch (e) {
        showErrorMsg(e, t)
      } finally {
        setChangingParticipationId(null)
      }
    },
    [t, mutate]
  )

  const handleStatusChange = useCallback(
    (participationId: number, status: ParticipationStatus) => {
      if (!canManageParticipations) return
      const suspending = status === ParticipationStatus.Suspended
      modals.openConfirmModal({
        title: suspending
          ? t('game.cheat_analysis.confirm_suspend_title', 'Suspend this participation?')
          : t('game.cheat_analysis.confirm_accept_title', 'Accept this participation?'),
        children: (
          <Text size="sm">
            {suspending
              ? t(
                  'game.cheat_analysis.confirm_suspend_body',
                  'This affects the team’s ability to participate. Review the evidence before continuing.'
                )
              : t(
                  'game.cheat_analysis.confirm_accept_body',
                  'This restores the participation status. Existing evidence remains available for audit.'
                )}
          </Text>
        ),
        labels: {
          confirm: suspending ? t('game.cheat_analysis.suspend', 'Suspend') : t('game.cheat_analysis.accept', 'Accept'),
          cancel: t('common.button.cancel', 'Cancel'),
        },
        confirmProps: { color: suspending ? 'red' : 'green' },
        onConfirm: () => void updateParticipationStatus(participationId, status),
      })
    },
    [canManageParticipations, t, updateParticipationStatus]
  )

  const summaryStats = useMemo(() => {
    const totalTeams = report?.suspicionList?.length ?? 0
    // "High risk" now means hard evidence (EVIDENCED band), not a raw number.
    const highRiskTeams = report?.suspicionList?.filter((x) => x.band === 'evidenced').length ?? 0
    const highRiskPct = totalTeams ? (highRiskTeams / totalTeams) * 100 : 0
    const automationFlagged = report?.suspicionList?.filter((x) => x.band === 'investigate').length ?? 0

    const ipAnomalies = report?.ipAnalysis?.length ?? 0
    const abnormalSolves = report?.abnormalSolves?.length ?? 0
    const collusionGroups = report?.collusionGroups?.length ?? 0
    const identityOverlaps = report?.identityOverlaps?.length ?? 0

    return {
      totalTeams,
      highRiskTeams,
      highRiskPct,
      automationFlagged,
      ipAnomalies,
      abnormalSolves,
      collusionGroups,
      identityOverlaps,
    }
  }, [
    report?.suspicionList,
    report?.ipAnalysis,
    report?.abnormalSolves,
    report?.collusionGroups,
    report?.identityOverlaps,
  ])

  const sortedIpAnalysis = useMemo(() => {
    if (!report?.ipAnalysis) return []
    let data = report.ipAnalysis

    const localParsed = debouncedIpSearch ? parseSearchQuery(debouncedIpSearch) : { freeText: '', filters: [] }
    const combinedFilters = [...globalParsed.filters, ...localParsed.filters]

    if (globalParsed.freeText || localParsed.freeText || combinedFilters.length > 0) {
      data = data.filter((item) => {
        for (const f of combinedFilters) {
          switch (f.field) {
            case 'team':
              if (!item.teamName?.toLowerCase().includes(f.value)) return false
              break
            case 'type': {
              const label = ((item.type ? IP_TYPE_META[item.type]?.label : '') ?? '').toLowerCase()
              if (!item.type?.toLowerCase().includes(f.value) && !label.includes(f.value)) return false
              break
            }
            case 'ip':
              if (!item.ip?.toLowerCase().includes(f.value)) return false
              break
            case 'user':
              if (
                !item.userNames?.some((u: string) => u.toLowerCase().includes(f.value)) &&
                !item.relatedUsers?.some((u: string) => u.toLowerCase().includes(f.value))
              )
                return false
              break
            case 'details':
              if (!item.details?.toLowerCase().includes(f.value)) return false
              break
            case 'time':
              const abs = item.time ? dayjs(item.time).format('YYYY-MM-DD HH:mm:ss') : ''
              const rel = item.time ? dayjs(item.time).fromNow() : ''
              if (!abs.includes(f.value) && !rel.toLowerCase().includes(f.value)) return false
              break
            default:
              return false // Hide row if filter field is unsupported
          }
        }

        const checkFreeText = (q: string) =>
          item.teamName?.toLowerCase().includes(q) ||
          item.type?.toLowerCase().includes(q) ||
          item.ip?.toLowerCase().includes(q) ||
          item.details?.toLowerCase().includes(q) ||
          item.userNames?.some((u: string) => u.toLowerCase().includes(q)) ||
          item.relatedUsers?.some((u: string) => u.toLowerCase().includes(q))
        if (globalParsed.freeText && !checkFreeText(globalParsed.freeText.toLowerCase())) return false
        if (localParsed.freeText && !checkFreeText(localParsed.freeText.toLowerCase())) return false

        return true
      })
    }
    return sortData(data, ipSort)
  }, [report?.ipAnalysis, debouncedIpSearch, globalParsed, ipSort])

  const paginatedIpAnalysis = useMemo(() => {
    const start = (ipPage - 1) * ITEMS_PER_PAGE
    return sortedIpAnalysis.slice(start, start + ITEMS_PER_PAGE)
  }, [sortedIpAnalysis, ipPage])

  const sortedAbnormalSolves = useMemo(() => {
    if (!report?.abnormalSolves) return []
    let data = report.abnormalSolves

    const localParsed = debouncedSolveSearch ? parseSearchQuery(debouncedSolveSearch) : { freeText: '', filters: [] }
    const combinedFilters = [...globalParsed.filters, ...localParsed.filters]

    if (globalParsed.freeText || localParsed.freeText || combinedFilters.length > 0) {
      data = data.filter((item) => {
        for (const f of combinedFilters) {
          switch (f.field) {
            case 'team':
              if (!item.teamName?.toLowerCase().includes(f.value)) return false
              break
            case 'type':
              if (!item.type?.toLowerCase().includes(f.value)) return false
              break
            case 'challenge':
              if (!item.challengeName?.toLowerCase().includes(f.value)) return false
              break
            case 'details':
              if (!item.details?.toLowerCase().includes(f.value)) return false
              break
            case 'time':
              const abs = dayjs(item.solveTime).format('YYYY-MM-DD HH:mm:ss')
              const rel = dayjs(item.solveTime).fromNow()
              if (!abs.includes(f.value) && !rel.toLowerCase().includes(f.value)) return false
              break
            default:
              return false
          }
        }

        const checkFreeText = (q: string) =>
          item.teamName?.toLowerCase().includes(q) ||
          item.challengeName?.toLowerCase().includes(q) ||
          item.type?.toLowerCase().includes(q) ||
          item.details?.toLowerCase().includes(q)
        if (globalParsed.freeText && !checkFreeText(globalParsed.freeText.toLowerCase())) return false
        if (localParsed.freeText && !checkFreeText(localParsed.freeText.toLowerCase())) return false

        return true
      })
    }
    return sortData(data, solveSort)
  }, [report?.abnormalSolves, debouncedSolveSearch, globalParsed, solveSort])

  const paginatedAbnormalSolves = useMemo(() => {
    const start = (solvePage - 1) * ITEMS_PER_PAGE
    return sortedAbnormalSolves.slice(start, start + ITEMS_PER_PAGE)
  }, [sortedAbnormalSolves, solvePage])

  const sortedCollusionGroups = useMemo(() => {
    if (!report?.collusionGroups) return []
    let data = report.collusionGroups

    const localParsed = debouncedCollusionSearch
      ? parseSearchQuery(debouncedCollusionSearch)
      : { freeText: '', filters: [] }
    const combinedFilters = [...globalParsed.filters, ...localParsed.filters]

    if (globalParsed.freeText || localParsed.freeText || combinedFilters.length > 0) {
      data = data.filter((item) => {
        for (const f of combinedFilters) {
          switch (f.field) {
            case 'team':
              if (!item.teams?.some((t: CollusionTeamInfo) => t.name?.toLowerCase().includes(f.value))) return false
              break
            case 'similarity':
              const m = f.value.match(/^>?(\d+)$/)
              if (m && (item.averageRsi ?? 0) * 100 < parseInt(m[1])) return false
              break
            case 'details':
              if (!item.details?.toLowerCase().includes(f.value)) return false
              break
            default:
              return false
          }
        }

        const checkFreeText = (q: string) =>
          item.teams?.some((t: CollusionTeamInfo) => t.name?.toLowerCase().includes(q)) ||
          item.details?.toLowerCase().includes(q) ||
          item.commonSolves?.some((c: string) => c.toLowerCase().includes(q))
        if (globalParsed.freeText && !checkFreeText(globalParsed.freeText.toLowerCase())) return false
        if (localParsed.freeText && !checkFreeText(localParsed.freeText.toLowerCase())) return false

        return true
      })
    }
    return sortData(data, collusionSort)
  }, [report?.collusionGroups, debouncedCollusionSearch, globalParsed, collusionSort])

  const paginatedCollusionGroups = useMemo(() => {
    const start = (collusionPage - 1) * ITEMS_PER_PAGE
    return sortedCollusionGroups.slice(start, start + ITEMS_PER_PAGE)
  }, [sortedCollusionGroups, collusionPage])

  const sortedSuspicionList = useMemo(() => {
    if (!report?.suspicionList) return []
    let data = report.suspicionList

    const localParsed = debouncedSuspSearch ? parseSearchQuery(debouncedSuspSearch) : { freeText: '', filters: [] }
    const combinedFilters = [...globalParsed.filters, ...localParsed.filters]

    if (globalParsed.freeText || localParsed.freeText || combinedFilters.length > 0) {
      data = data.filter((item) => {
        for (const f of combinedFilters) {
          switch (f.field) {
            case 'team':
              if (!item.teamName?.toLowerCase().includes(f.value)) return false
              break
            case 'band':
              if (!(item.band ?? 'clean').toLowerCase().includes(f.value)) return false
              break
            case 'score':
              const m = f.value.match(/^>?(\d+)$/)
              if (m && (item.score ?? 0) < parseInt(m[1])) return false
              break
            case 'status':
              if (!item.status?.toLowerCase().includes(f.value)) return false
              break
            default:
              return false
          }
        }

        const checkFreeText = (q: string) => item.teamName?.toLowerCase().includes(q)
        if (globalParsed.freeText && !checkFreeText(globalParsed.freeText.toLowerCase())) return false
        if (localParsed.freeText && !checkFreeText(localParsed.freeText.toLowerCase())) return false

        return true
      })
    }
    const asc = suspSort.direction === 'asc'
    if (suspSort.key === 'teamName') {
      return [...data].sort((a, b) => {
        const cmp = (a.teamName || '').localeCompare(b.teamName || '')
        return asc ? cmp : -cmp
      })
    }
    if (suspSort.key === 'score') {
      // Explicit user intent: sort by RAW total across bands, so an admin can
      // pull a high-scoring automation (Investigate) case above a marginal
      // hard one if they choose. (The default keeps band-first ordering.)
      return [...data].sort((a, b) => (asc ? (a.score ?? 0) - (b.score ?? 0) : (b.score ?? 0) - (a.score ?? 0)))
    }
    // Default: trust the server's band-first, deterministically tie-broken order
    // (hard evidence always on top); reverse for ascending.
    return asc ? [...data].reverse() : data
  }, [report?.suspicionList, debouncedSuspSearch, globalParsed, suspSort])

  const paginatedSuspicionList = useMemo(() => {
    const start = (suspPage - 1) * ITEMS_PER_PAGE
    return sortedSuspicionList.slice(start, start + ITEMS_PER_PAGE)
  }, [sortedSuspicionList, suspPage])

  useEffect(() => {
    setIpPage((page) => Math.min(page, Math.max(1, Math.ceil(sortedIpAnalysis.length / ITEMS_PER_PAGE))))
  }, [sortedIpAnalysis.length])
  useEffect(() => {
    setSolvePage((page) => Math.min(page, Math.max(1, Math.ceil(sortedAbnormalSolves.length / ITEMS_PER_PAGE))))
  }, [sortedAbnormalSolves.length])
  useEffect(() => {
    setCollusionPage((page) => Math.min(page, Math.max(1, Math.ceil(sortedCollusionGroups.length / ITEMS_PER_PAGE))))
  }, [sortedCollusionGroups.length])
  useEffect(() => {
    setSuspPage((page) => Math.min(page, Math.max(1, Math.ceil(sortedSuspicionList.length / ITEMS_PER_PAGE))))
  }, [sortedSuspicionList.length])

  const filteredIdentityOverlaps = useMemo(() => {
    const data = report?.identityOverlaps ?? []
    if (!globalParsed.freeText && globalParsed.filters.length === 0) return data

    return data.filter((item: IdentityOverlapResult) => {
      for (const filter of globalParsed.filters) {
        switch (filter.field) {
          case 'team':
            if (!item.teamNames?.some((name) => name.toLowerCase().includes(filter.value))) return false
            break
          case 'user':
            if (!item.userNames?.some((name) => name.toLowerCase().includes(filter.value))) return false
            break
          case 'ip':
            if (item.kind !== 'ip' || !item.value?.toLowerCase().includes(filter.value)) return false
            break
          case 'type':
            if (!item.kind?.toLowerCase().includes(filter.value)) return false
            break
          default:
            return false
        }
      }

      if (!globalParsed.freeText) return true
      const query = globalParsed.freeText.toLowerCase()
      return Boolean(
        item.kind?.toLowerCase().includes(query) ||
        item.value?.toLowerCase().includes(query) ||
        item.teamNames?.some((name) => name.toLowerCase().includes(query)) ||
        item.userNames?.some((name) => name.toLowerCase().includes(query))
      )
    })
  }, [globalParsed, report?.identityOverlaps])

  const handleSort = <T,>(
    setSort: React.Dispatch<React.SetStateAction<SortConfig<T>>>,
    currentSort: SortConfig<T>,
    key: keyof T
  ) => {
    const direction = currentSort.key === key && currentSort.direction === 'asc' ? 'desc' : 'asc'
    setSort({ key, direction })
  }

  const compactHeight = 'clamp(320px, calc(100vh - 24rem), 72vh)'
  const roomyHeight = 'clamp(420px, calc(100vh - 20rem), 82vh)'

  return (
    <>
      <Modal
        opened={opened}
        onClose={closeCollusionDetails}
        title={
          <Group gap="xs">
            <ThemeIcon size="sm" color="violet" variant="light" radius="sm">
              <Icon path={mdiAccountGroup} size={0.7} />
            </ThemeIcon>
            <Text fw={700}>{t('game.cheat_analysis.collusion_details', 'Solve Similarity Details')}</Text>
          </Group>
        }
        size="xl"
        centered
      >
        {selectedGroup && (
          <Stack>
            <Group grow align="flex-end">
              <Select
                label={t('game.cheat_analysis.team_a', 'Team A')}
                data={selectedGroup.teams
                  ?.filter((team) => team.participationId !== teamBId)
                  .map((team) => ({ value: team.participationId?.toString() || '', label: team.name }))}
                value={teamAId?.toString()}
                onChange={(val) => setTeamAId(val ? parseInt(val) : null)}
                searchable
              />
              <Center h={60}>
                <Stack align="center" gap={0}>
                  <Text size="xl" fw={900} c={(activeComparison?.rsi ?? 0) > 0.9 ? 'alert' : 'yellow'}>
                    {activeComparison
                      ? `${(activeComparison.rsi * 100).toFixed(1)}%`
                      : t('game.cheat_analysis.not_available_short', '—')}
                  </Text>
                  <Text size="xs" c="dimmed">
                    {t('game.cheat_analysis.similarity', 'Similarity')}
                  </Text>
                  {isDrillRefreshing && activeComparison && (
                    <Text size="xs" c="dimmed" role="status" aria-live="polite">
                      {t('game.cheat_analysis.updating_comparison', 'Updating…')}
                    </Text>
                  )}
                </Stack>
              </Center>
              <Select
                label={t('game.cheat_analysis.team_b', 'Team B')}
                data={selectedGroup.teams
                  ?.filter((team) => team.participationId !== teamAId)
                  .map((team) => ({ value: team.participationId?.toString() || '', label: team.name }))}
                value={teamBId?.toString()}
                onChange={(val) => setTeamBId(val ? parseInt(val) : null)}
                searchable
              />
            </Group>

            {drillError && (
              <Alert
                color="red"
                role="alert"
                icon={<Icon path={mdiAlertCircle} size={0.9} aria-hidden />}
                title={t('game.cheat_analysis.comparison_failed', 'Could not load this pair comparison')}
              >
                <Stack gap="xs" align="flex-start">
                  <Text size="sm">{tryGetErrorMsg(drillError, t)}</Text>
                  <Button
                    size="xs"
                    variant="outline"
                    color="red"
                    leftSection={<Icon path={mdiRefresh} size={0.65} aria-hidden />}
                    onClick={() => void retryDrill()}
                    loading={isDrillRefreshing}
                  >
                    {t('common.button.retry', 'Retry')}
                  </Button>
                </Stack>
              </Alert>
            )}

            {teamAId === null || teamBId === null ? (
              <Alert color="yellow" icon={<Icon path={mdiInformation} size={0.9} aria-hidden />}>
                {t(
                  'game.cheat_analysis.comparison_unavailable',
                  'Choose two teams with valid participation records to compare solve timing.'
                )}
              </Alert>
            ) : isDrilling && !activeComparison ? (
              <Center h={400}>
                <Loader aria-label={t('game.cheat_analysis.loading_comparison', 'Loading pair comparison')} />
              </Center>
            ) : !drillError && activeComparison?.details && activeComparison.details.length > 0 ? (
              <ScrollArea
                h={400}
                viewportProps={{
                  role: 'region',
                  tabIndex: 0,
                  'aria-label': t('game.cheat_analysis.comparison_scroll_region', 'Pair comparison results'),
                }}
              >
                <Table striped highlightOnHover miw="46rem">
                  <AccessibleTableCaption>
                    {t('game.cheat_analysis.pair_comparison_caption', 'Solve timing comparison between two teams')}
                  </AccessibleTableCaption>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th scope="col" w="14rem" miw="14rem">
                        {t('common.label.challenge', 'Challenge')}
                      </Table.Th>
                      <Table.Th scope="col" w="11rem" miw="11rem">
                        {t('game.cheat_analysis.time_a', 'Time A')}
                      </Table.Th>
                      <Table.Th scope="col" w="11rem" miw="11rem">
                        {t('game.cheat_analysis.time_b', 'Time B')}
                      </Table.Th>
                      <Table.Th scope="col" w="8rem" miw="8rem">
                        {t('game.cheat_analysis.time_diff', 'Diff')}
                      </Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {activeComparison.details.map((solve: SequenceSuspectDetail, idx: number) => (
                      <Table.Tr
                        key={`${solve.challengeName ?? 'challenge'}:${solve.timeA ?? 'a'}:${solve.timeB ?? 'b'}:${idx}`}
                      >
                        <Table.Td fw={500} miw="14rem">
                          <ScrollingText
                            text={solve.challengeName || t('common.label.unknown', 'Unknown')}
                            size="sm"
                            maw={200}
                          />
                        </Table.Td>
                        <Table.Td ff="monospace" fz="sm" miw="11rem">
                          {fmtAbsTime(solve.timeA, locale, 'MM-DD HH:mm:ss')}
                        </Table.Td>
                        <Table.Td ff="monospace" fz="sm" miw="11rem">
                          {fmtAbsTime(solve.timeB, locale, 'MM-DD HH:mm:ss')}
                        </Table.Td>
                        <Table.Td miw="8rem">
                          <Badge
                            color={
                              (solve.timeDiff ?? 0) < 60 ? 'alert' : (solve.timeDiff ?? 0) < 300 ? 'yellow' : 'gray'
                            }
                          >
                            {(solve.timeDiff ?? 0).toFixed(0)}s
                          </Badge>
                        </Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </ScrollArea>
            ) : !drillError ? (
              <Card withBorder padding="sm">
                <ReadableDetails details={selectedGroup.details} />
                <Text size="xs" c="dimmed" mt="xs">
                  {t('game.cheat_analysis.common_solves', 'Common Solves')}: {selectedGroup.commonSolves?.join(', ')}
                </Text>
              </Card>
            ) : null}
          </Stack>
        )}
      </Modal>

      <Modal
        opened={susOpened}
        onClose={closeSuspicionDetails}
        title={
          <Group gap="xs">
            <ThemeIcon size="sm" color="alert" variant="light" radius="sm">
              <Icon path={mdiShieldAlert} size={0.7} />
            </ThemeIcon>
            <Text fw={700}>{t('game.cheat_analysis.suspicion_details', 'Suspicion Details')}</Text>
          </Group>
        }
        size="lg"
        centered
      >
        {selectedSuspicion && (
          <Stack>
            <Group justify="space-between" align="flex-start">
              <Box>
                <Text fw={700} size="lg">
                  {selectedSuspicion.teamName}
                </Text>
                <Text size="xs" c="dimmed">
                  {t('game.cheat_analysis.score_breakdown', 'Suspicion score breakdown')}
                </Text>
              </Box>
              <Group gap="sm" align="center">
                <Badge
                  color={bandMeta(selectedSuspicion.band).color}
                  size="lg"
                  variant={
                    selectedSuspicion.band === 'context' || selectedSuspicion.band === 'clean' ? 'light' : 'filled'
                  }
                >
                  {t(
                    `game.cheat_analysis.band.${selectedSuspicion.band ?? 'clean'}`,
                    bandMeta(selectedSuspicion.band).label
                  )}
                </Badge>
                <Text fw={900} size="xl" c={bandMeta(selectedSuspicion.band).color}>
                  {selectedSuspicion.score}
                </Text>
              </Group>
            </Group>
            <Box>
              <RiskCompositionBar
                hard={selectedSuspicion.hard}
                corroboration={selectedSuspicion.corroboration}
                strong={selectedSuspicion.strong}
                behavioral={selectedSuspicion.behavioral}
              />
              <Group gap="md" mt={6}>
                <Text size="xs" c="dimmed">
                  {t('game.cheat_analysis.tier.hard', 'Hard')}: <b>{selectedSuspicion.hard ?? 0}</b>
                </Text>
                <Text size="xs" c="dimmed">
                  {t('game.cheat_analysis.corroboration', 'Corroboration')}:{' '}
                  <b>{selectedSuspicion.corroboration ?? 0}</b>
                </Text>
                <Text size="xs" c="dimmed">
                  {t('game.cheat_analysis.tier.strong', 'Strong')}: <b>{selectedSuspicion.strong ?? 0}</b>
                </Text>
                <Text size="xs" c="dimmed">
                  {t('game.cheat_analysis.tier.behavioral', 'Behavioral')}: <b>{selectedSuspicion.behavioral ?? 0}</b>
                </Text>
              </Group>
              <Text size="xs" c="dimmed" mt={4}>
                {t(
                  'game.cheat_analysis.context_note',
                  'Network / identity (context) signals are shown below but never score on their own.'
                )}
              </Text>
            </Box>
            <Alert
              color="blue"
              title={t('game.cheat_analysis.triage_not_verdict', 'This score is triage, not a verdict')}
            >
              <Text size="sm">
                {t(
                  'game.cheat_analysis.triage_not_verdict_detail',
                  'Open Review evidence for each incident and verify its source records. Only a verified direct-evidence source can stand as proof; indicators and context require independent corroboration.'
                )}
              </Text>
            </Alert>
            {canManageParticipations && selectedSuspicion.participationId !== undefined && (
              <>
                <Divider />
                <FusedEvidencePanel gameId={gameId} participationId={selectedSuspicion.participationId} />
              </>
            )}
            <Divider />
            <ScrollArea
              h={380}
              viewportProps={{
                role: 'region',
                tabIndex: 0,
                'aria-label': t('game.cheat_analysis.evidence_scroll_region', 'Evidence events'),
              }}
            >
              <Table striped miw="76rem">
                <AccessibleTableCaption>
                  {t('game.cheat_analysis.evidence_events_caption', 'Evidence events for the selected team')}
                </AccessibleTableCaption>
                <Table.Thead>
                  <Table.Tr>
                    <Table.Th scope="col" w="6rem" miw="6rem">
                      {t('game.cheat_analysis.event_id', 'Event ID')}
                    </Table.Th>
                    <Table.Th scope="col" w="10rem" miw="10rem">
                      {t('game.cheat_analysis.type', 'Type')}
                    </Table.Th>
                    <Table.Th scope="col" w="9rem" miw="9rem">
                      {t('game.cheat_analysis.tier_label', 'Tier')}
                    </Table.Th>
                    <Table.Th scope="col" w="10rem" miw="10rem">
                      {t('game.cheat_analysis.score_contribution', 'Score contribution')}
                    </Table.Th>
                    <Table.Th scope="col" w="11rem" miw="11rem">
                      {t('common.label.time', 'Time')}
                    </Table.Th>
                    <Table.Th scope="col" w="23rem" miw="23rem">
                      {t('game.cheat_analysis.details', 'Details')}
                    </Table.Th>
                    <Table.Th scope="col" w="9rem" miw="9rem">
                      {t('game.cheat_analysis.evidence_review', 'Evidence review')}
                    </Table.Th>
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {selectedSuspicion.events?.map((evt, idx) => {
                    const tm = tierMeta(evt.tier)
                    const contribution = evidenceContribution(evt)
                    const counted = evt.counted ?? contribution !== 0
                    const rawDelta = evt.scoreDelta ?? 0
                    const rowKey = evt.eventId ?? `${evt.type ?? 'event'}:${evt.time ?? 'time'}:${idx}`
                    return (
                      <React.Fragment key={rowKey}>
                        <Table.Tr style={{ opacity: counted ? 1 : 0.65 }}>
                          <Table.Td miw="6rem">
                            <Text size="xs" ff="monospace" c="dimmed">
                              {evt.eventId == null ? '—' : `#${evt.eventId}`}
                            </Text>
                          </Table.Td>
                          <Table.Td miw="10rem">
                            <Text size="sm" fw={600}>
                              {evt.type ?? t('common.label.unknown', 'Unknown')}
                            </Text>
                          </Table.Td>
                          <Table.Td miw="9rem">
                            <Group gap={4} wrap="nowrap">
                              <Badge color={tm.color} size="sm" variant={counted ? 'filled' : 'outline'}>
                                {t(`game.cheat_analysis.tier.${evt.tier ?? 'behavioral'}`, tm.label)}
                              </Badge>
                              {!counted && (
                                <Text size="xs" c="dimmed">
                                  {evt.tier === 'context'
                                    ? t('game.cheat_analysis.not_scored', 'context')
                                    : t('game.cheat_analysis.capped', 'capped')}
                                </Text>
                              )}
                            </Group>
                          </Table.Td>
                          <Table.Td miw="10rem">
                            <Tooltip
                              withArrow
                              label={
                                counted
                                  ? t(
                                      'game.cheat_analysis.score_applied_detail',
                                      'Raw rule weight: {{raw}}. Applied after caps: {{applied}}.',
                                      { raw: rawDelta, applied: contribution }
                                    )
                                  : t(
                                      'game.cheat_analysis.score_not_applied_detail',
                                      'Raw rule weight: {{raw}}. This event did not add points.',
                                      { raw: rawDelta }
                                    )
                              }
                            >
                              <Badge
                                size="sm"
                                variant={counted ? 'light' : 'outline'}
                                color={contribution > 0 ? 'orange' : contribution < 0 ? 'teal' : 'gray'}
                              >
                                {counted
                                  ? `${contribution > 0 ? '+' : ''}${contribution}`
                                  : t('game.cheat_analysis.not_counted_score', '0 (not counted)')}
                              </Badge>
                            </Tooltip>
                          </Table.Td>
                          <Table.Td fz="xs" ff="monospace" miw="11rem">
                            {fmtAbsTime(evt.time, locale, 'MM-DD HH:mm:ss')}
                          </Table.Td>
                          <Table.Td miw="23rem">
                            <ReadableDetails details={evt.details} />
                          </Table.Td>
                          <Table.Td miw="9rem">
                            <Button
                              size="compact-xs"
                              variant={reviewEventId === evt.eventId ? 'filled' : 'light'}
                              color={reviewEventId === evt.eventId ? 'blue' : 'gray'}
                              disabled={evt.eventId == null}
                              onClick={() =>
                                evt.eventId != null &&
                                setReviewEventId((current) => (current === evt.eventId ? null : (evt.eventId ?? null)))
                              }
                              aria-expanded={reviewEventId === evt.eventId}
                            >
                              {reviewEventId === evt.eventId
                                ? t('common.action.hide', 'Hide')
                                : t('game.cheat_analysis.review_evidence', 'Review evidence')}
                            </Button>
                          </Table.Td>
                        </Table.Tr>
                        {evt.eventId != null && reviewEventId === evt.eventId && (
                          <Table.Tr>
                            <Table.Td colSpan={7} p="sm">
                              <SuspicionEvidenceReviewPanel gameId={gameId} eventId={evt.eventId} />
                            </Table.Td>
                          </Table.Tr>
                        )}
                      </React.Fragment>
                    )
                  })}
                  {!selectedSuspicion.events?.length && (
                    <Table.Tr>
                      <Table.Td colSpan={7}>
                        <Text size="sm" c="dimmed" ta="center" py="md">
                          {t('game.cheat_analysis.no_evidence_events', 'No event details are available.')}
                        </Text>
                      </Table.Td>
                    </Table.Tr>
                  )}
                </Table.Tbody>
              </Table>
            </ScrollArea>
          </Stack>
        )}
      </Modal>

      {/* Global search toolbar (page title is owned by the CheatCheck banner) */}
      <Group justify="flex-end" mb="md" align="center">
        <SmartSearch
          value={globalSearch}
          onChange={setGlobalSearch}
          placeholder={t(
            'game.cheat_analysis.global_search_placeholder',
            'Global search across all tabs... (type @ for filters)'
          )}
          filterDefs={GLOBAL_FILTER_DEFS}
          w={{ base: '100%', sm: 400 }}
        />
      </Group>

      <Box className={classes.summaryGrid} data-layout-section="cheat-summary" data-min-block-gap="8">
        <SummaryCard
          label={t('game.cheat_analysis.card.hard_evidence', 'Hard Evidence')}
          value={summaryStats.highRiskTeams}
          sub={t('game.cheat_analysis.card.evidenced_sub', '{{auto}} more flagged for automation', {
            auto: summaryStats.automationFlagged,
          })}
          icon={mdiShieldAlert}
          color="alert"
          accent
          active={activeTab === 'suspicion'}
          onClick={() => setActiveTab('suspicion')}
        />
        <SummaryCard
          label={t('game.cheat_analysis.card.ip_anomalies', 'Network / Device Signals')}
          value={summaryStats.ipAnomalies}
          sub={t('game.cheat_analysis.card.ip_anomalies_sub', 'Probabilistic review context')}
          icon={mdiIpNetwork}
          color="cyan"
          active={activeTab === 'ip'}
          onClick={() => setActiveTab('ip')}
        />
        <SummaryCard
          label={t('game.cheat_analysis.card.abnormal_solves', 'Abnormal Solves')}
          value={summaryStats.abnormalSolves}
          sub={t('game.cheat_analysis.card.abnormal_solves_sub', 'Solve-prerequisite signals')}
          icon={mdiGhost}
          color="orange"
          active={activeTab === 'solve'}
          onClick={() => setActiveTab('solve')}
        />
        <SummaryCard
          label={t('game.cheat_analysis.card.collusion_groups', 'Similarity Groups')}
          value={summaryStats.collusionGroups}
          sub={t('game.cheat_analysis.card.collusion_groups_sub', 'Solve-timing similarity for review')}
          icon={mdiAccountGroup}
          color="violet"
          active={activeTab === 'collusion'}
          onClick={() => setActiveTab('collusion')}
        />
        <SummaryCard
          label={t('game.cheat_analysis.tab.identity', 'Identity Overlap')}
          value={summaryStats.identityOverlaps}
          sub={t('game.cheat_analysis.card.identity_sub', 'Probabilistic correlation only')}
          icon={mdiFingerprint}
          color="gray"
          active={activeTab === 'identity'}
          onClick={() => setActiveTab('identity')}
        />
      </Box>

      <Paper shadow="md" p="md" radius="md">
        <Tabs value={activeTab} onChange={setActiveTab} variant="pills" radius="sm">
          <Tabs.List
            ref={evidenceTabsRef}
            grow
            className={classes.innerTabList}
            pb="xs"
            mb="xs"
            aria-label={t('game.cheat_analysis.evidence_categories', 'Evidence categories')}
            data-layout-section="cheat-evidence-tabs"
            data-max-layout-rows="1"
          >
            <Tabs.Tab
              value="suspicion"
              rightSection={
                <Badge
                  size="xs"
                  variant="filled"
                  color={
                    (report?.suspicionList?.filter((x) => x.band === 'evidenced' || x.band === 'investigate').length ??
                      0) > 0
                      ? 'alert'
                      : 'gray'
                  }
                  circle
                >
                  {report?.suspicionList?.length ?? 0}
                </Badge>
              }
            >
              {t('game.cheat_analysis.tab.suspicion', 'Suspicion')}
            </Tabs.Tab>
            <Tabs.Tab
              value="ip"
              rightSection={
                <Badge
                  size="xs"
                  variant="filled"
                  color={(report?.ipAnalysis?.length ?? 0) > 0 ? 'blue' : 'gray'}
                  circle
                >
                  {report?.ipAnalysis?.length ?? 0}
                </Badge>
              }
            >
              {t('game.cheat_analysis.tab.ip_analysis', 'Network / Device')}
            </Tabs.Tab>
            <Tabs.Tab
              value="solve"
              rightSection={
                <Badge
                  size="xs"
                  variant="filled"
                  color={(report?.abnormalSolves?.length ?? 0) > 0 ? 'orange' : 'gray'}
                  circle
                >
                  {report?.abnormalSolves?.length ?? 0}
                </Badge>
              }
            >
              {t('game.cheat_analysis.tab.abnormal_solves', 'Abnormal Solves')}
            </Tabs.Tab>
            <Tabs.Tab
              value="collusion"
              rightSection={
                <Badge
                  size="xs"
                  variant="filled"
                  color={(report?.collusionGroups?.length ?? 0) > 0 ? 'violet' : 'gray'}
                  circle
                >
                  {report?.collusionGroups?.length ?? 0}
                </Badge>
              }
            >
              {t('game.cheat_analysis.tab.collusion', 'Similarity')}
            </Tabs.Tab>
            <Tabs.Tab
              value="identity"
              rightSection={
                <Badge
                  size="xs"
                  variant="filled"
                  color={(report?.identityOverlaps?.length ?? 0) > 0 ? 'violet' : 'gray'}
                  circle
                >
                  {report?.identityOverlaps?.length ?? 0}
                </Badge>
              }
            >
              {t('game.cheat_analysis.tab.identity', 'Identity Overlap')}
            </Tabs.Tab>
          </Tabs.List>

          <Tabs.Panel value="suspicion" pt="md">
            <Group justify="space-between" mb="md">
              <Group gap="xs">
                <Title order={3}>{t('game.cheat_analysis.suspicion_rankings', 'Suspicion Rankings')}</Title>
                <Badge variant="light" color="alert" role="status" aria-live="polite">
                  {sortedSuspicionList.length}
                  {(globalSearch.trim() || suspSearch.trim()) &&
                    report?.suspicionList?.length !== sortedSuspicionList.length && (
                      <> / {report?.suspicionList?.length ?? 0}</>
                    )}
                </Badge>
              </Group>
              <SmartSearch
                value={suspSearch}
                onChange={setSuspSearch}
                placeholder={t('game.cheat_analysis.search_placeholder', 'Search or type @ for filters...')}
                filterDefs={SUSPICION_FILTER_DEFS}
                w={320}
              />
            </Group>
            {sortedSuspicionList.length > 0 ? (
              <>
                <ScrollArea
                  offsetScrollbars
                  h={compactHeight}
                  viewportProps={{
                    role: 'region',
                    tabIndex: 0,
                    'aria-label': t('game.cheat_analysis.suspicion_scroll_region', 'Suspicion rankings'),
                  }}
                >
                  <Table
                    className={tableClasses.table}
                    horizontalSpacing="md"
                    verticalSpacing="xs"
                    striped
                    highlightOnHover
                    withTableBorder
                    stickyHeader
                    miw="46rem"
                  >
                    <AccessibleTableCaption>
                      {t('game.cheat_analysis.suspicion_table_caption', 'Team suspicion and participation status')}
                    </AccessibleTableCaption>
                    <Table.Thead>
                      <Table.Tr>
                        <Table.Th scope="col" w="3rem" miw="3rem" style={{ textAlign: 'center' }}>
                          #
                        </Table.Th>
                        <ThSort
                          sorted={suspSort.key === 'teamName'}
                          reversed={suspSort.direction === 'desc'}
                          onSort={() => handleSort(setSuspSort, suspSort, 'teamName')}
                          w="16rem"
                        >
                          {t('common.label.team', 'Team')}
                        </ThSort>
                        <ThSort
                          sorted={suspSort.key === 'score'}
                          reversed={suspSort.direction === 'desc'}
                          onSort={() => handleSort(setSuspSort, suspSort, 'score')}
                          w="11rem"
                        >
                          {t('game.cheat_analysis.risk', 'Risk')}
                        </ThSort>
                        <Table.Th scope="col" w="11rem" miw="11rem">
                          {t('admin.label.participation_status', 'Status')}
                        </Table.Th>
                        <Table.Th scope="col" w="4rem" miw="4rem" style={{ textAlign: 'center' }}>
                          {t('game.cheat_analysis.view', 'View')}
                        </Table.Th>
                      </Table.Tr>
                    </Table.Thead>
                    <Table.Tbody>
                      {paginatedSuspicionList.map((item, index) => (
                        <SuspicionRow
                          key={item.participationId ?? `${item.teamId ?? 'team'}:${item.teamName ?? index}`}
                          item={item}
                          index={(suspPage - 1) * ITEMS_PER_PAGE + index}
                          statusMap={statusMap}
                          canManageParticipations={canManageParticipations}
                          isChanging={changingParticipationId === item.participationId}
                          onStatusChange={handleStatusChange}
                          onView={handleViewSuspicion}
                        />
                      ))}
                    </Table.Tbody>
                  </Table>
                </ScrollArea>
                {sortedSuspicionList.length > ITEMS_PER_PAGE && (
                  <Group justify="center" mt="md">
                    <Pagination
                      total={Math.ceil(sortedSuspicionList.length / ITEMS_PER_PAGE)}
                      value={suspPage}
                      onChange={setSuspPage}
                      size="sm"
                      radius="xl"
                    />
                  </Group>
                )}
              </>
            ) : (
              <AnalysisEmptyState
                filtered={Boolean(globalSearch.trim() || suspSearch.trim())}
                description={t(
                  'game.cheat_analysis.no_suspicion',
                  'No suspicion signals were recorded in this report snapshot.'
                )}
                onClearFilters={() => {
                  setGlobalSearch('')
                  setSuspSearch('')
                }}
              />
            )}
          </Tabs.Panel>

          <Tabs.Panel value="ip" pt="md">
            <Group justify="space-between" mb="md">
              <Group gap="xs">
                <Title order={3}>{t('game.cheat_analysis.tab.ip_analysis', 'Network / Device Signals')}</Title>
                <Badge variant="light" color="cyan" role="status" aria-live="polite">
                  {sortedIpAnalysis.length}
                  {(globalSearch.trim() || ipSearch.trim()) &&
                    sortedIpAnalysis.length !== report?.ipAnalysis?.length && <> / {report?.ipAnalysis?.length ?? 0}</>}
                </Badge>
              </Group>
              <SmartSearch
                value={ipSearch}
                onChange={setIpSearch}
                placeholder={t('game.cheat_analysis.search_placeholder', 'Search or type @ for filters...')}
                filterDefs={IP_FILTER_DEFS}
                w={320}
              />
            </Group>
            {sortedIpAnalysis.length > 0 ? (
              <>
                <ScrollArea
                  offsetScrollbars
                  h={roomyHeight}
                  viewportProps={{
                    role: 'region',
                    tabIndex: 0,
                    'aria-label': t('game.cheat_analysis.ip_scroll_region', 'Network and device signals'),
                  }}
                >
                  <Table
                    className={tableClasses.table}
                    horizontalSpacing="md"
                    verticalSpacing="xs"
                    striped
                    highlightOnHover
                    withTableBorder
                    stickyHeader
                    miw="76rem"
                  >
                    <AccessibleTableCaption>
                      {t('game.cheat_analysis.ip_table_caption', 'IP and identity anomalies by team')}
                    </AccessibleTableCaption>
                    <Table.Thead>
                      <Table.Tr>
                        <ThSort
                          sorted={ipSort.key === 'teamName'}
                          reversed={ipSort.direction === 'desc'}
                          onSort={() => handleSort(setIpSort, ipSort, 'teamName')}
                          w="11rem"
                        >
                          {t('common.label.team', 'Team')}
                        </ThSort>
                        <ThSort
                          sorted={ipSort.key === 'type'}
                          reversed={ipSort.direction === 'desc'}
                          onSort={() => handleSort(setIpSort, ipSort, 'type')}
                          w="11rem"
                        >
                          {t('game.cheat_analysis.type', 'Type')}
                        </ThSort>
                        <Table.Th scope="col" w="14rem" miw="14rem">
                          {t('game.cheat_analysis.users', 'Users')}
                        </Table.Th>
                        <ThSort
                          sorted={ipSort.key === 'ip'}
                          reversed={ipSort.direction === 'desc'}
                          onSort={() => handleSort(setIpSort, ipSort, 'ip')}
                          w="9rem"
                        >
                          {t('game.cheat_analysis.ip', 'IP')}
                        </ThSort>
                        <ThSort
                          sorted={ipSort.key === 'time'}
                          reversed={ipSort.direction === 'desc'}
                          onSort={() => handleSort(setIpSort, ipSort, 'time')}
                          w="9rem"
                        >
                          {t('common.label.time', 'Time')}
                        </ThSort>
                        <Table.Th scope="col">{t('game.cheat_analysis.details', 'Details')}</Table.Th>
                      </Table.Tr>
                    </Table.Thead>
                    <Table.Tbody>
                      {paginatedIpAnalysis.map((item, index) => (
                        <IpAnalysisRow
                          key={`${item.teamId ?? 'team'}:${item.type ?? 'type'}:${item.time ?? 'time'}:${item.ip ?? index}`}
                          item={item}
                          locale={locale}
                        />
                      ))}
                    </Table.Tbody>
                  </Table>
                </ScrollArea>
                {sortedIpAnalysis.length > ITEMS_PER_PAGE && (
                  <Group justify="center" mt="md">
                    <Pagination
                      total={Math.ceil(sortedIpAnalysis.length / ITEMS_PER_PAGE)}
                      value={ipPage}
                      onChange={setIpPage}
                      size="sm"
                      radius="xl"
                    />
                  </Group>
                )}
              </>
            ) : (
              <AnalysisEmptyState
                filtered={Boolean(globalSearch.trim() || ipSearch.trim())}
                description={t(
                  'game.content.no_cheat.title',
                  'No network or device correlation signals were recorded in this report snapshot.'
                )}
                onClearFilters={() => {
                  setGlobalSearch('')
                  setIpSearch('')
                }}
              />
            )}
          </Tabs.Panel>

          <Tabs.Panel value="solve" pt="md">
            <Group justify="space-between" mb="md">
              <Group gap="xs">
                <Title order={3}>{t('game.cheat_analysis.tab.abnormal_solves', 'Abnormal Solves')}</Title>
                <Badge variant="light" color="orange" role="status" aria-live="polite">
                  {sortedAbnormalSolves.length}
                  {(globalSearch.trim() || solveSearch.trim()) &&
                    sortedAbnormalSolves.length !== report?.abnormalSolves?.length && (
                      <> / {report?.abnormalSolves?.length ?? 0}</>
                    )}
                </Badge>
              </Group>
              <SmartSearch
                value={solveSearch}
                onChange={setSolveSearch}
                placeholder={t('game.cheat_analysis.search_placeholder', 'Search or type @ for filters...')}
                filterDefs={SOLVE_FILTER_DEFS}
                w={320}
              />
            </Group>
            {sortedAbnormalSolves.length > 0 ? (
              <>
                <ScrollArea
                  offsetScrollbars
                  h={roomyHeight}
                  viewportProps={{
                    role: 'region',
                    tabIndex: 0,
                    'aria-label': t('game.cheat_analysis.solve_scroll_region', 'Abnormal solve signals'),
                  }}
                >
                  <Table
                    className={tableClasses.table}
                    horizontalSpacing="md"
                    verticalSpacing="xs"
                    striped
                    highlightOnHover
                    withTableBorder
                    stickyHeader
                    miw="68rem"
                  >
                    <AccessibleTableCaption>
                      {t('game.cheat_analysis.solve_table_caption', 'Abnormal challenge solve activity')}
                    </AccessibleTableCaption>
                    <Table.Thead>
                      <Table.Tr>
                        <ThSort
                          sorted={solveSort.key === 'teamName'}
                          reversed={solveSort.direction === 'desc'}
                          onSort={() => handleSort(setSolveSort, solveSort, 'teamName')}
                          w="11rem"
                        >
                          {t('common.label.team', 'Team')}
                        </ThSort>
                        <ThSort
                          sorted={solveSort.key === 'challengeName'}
                          reversed={solveSort.direction === 'desc'}
                          onSort={() => handleSort(setSolveSort, solveSort, 'challengeName')}
                          w="13rem"
                        >
                          {t('common.label.challenge', 'Challenge')}
                        </ThSort>
                        <ThSort
                          sorted={solveSort.key === 'type'}
                          reversed={solveSort.direction === 'desc'}
                          onSort={() => handleSort(setSolveSort, solveSort, 'type')}
                          w="9rem"
                        >
                          {t('game.cheat_analysis.type', 'Type')}
                        </ThSort>
                        <Table.Th scope="col">{t('game.cheat_analysis.details', 'Details')}</Table.Th>
                        <ThSort
                          sorted={solveSort.key === 'solveTime'}
                          reversed={solveSort.direction === 'desc'}
                          onSort={() => handleSort(setSolveSort, solveSort, 'solveTime')}
                          w="9rem"
                        >
                          {t('common.label.time', 'Time')}
                        </ThSort>
                      </Table.Tr>
                    </Table.Thead>
                    <Table.Tbody>
                      {paginatedAbnormalSolves.map((item, index) => (
                        <AbnormalSolveRow
                          key={`${item.teamId ?? 'team'}:${item.challengeId ?? 'challenge'}:${item.type ?? 'type'}:${item.solveTime ?? index}`}
                          item={item}
                          locale={locale}
                          t={t}
                        />
                      ))}
                    </Table.Tbody>
                  </Table>
                </ScrollArea>
                {sortedAbnormalSolves.length > ITEMS_PER_PAGE && (
                  <Group justify="center" mt="md">
                    <Pagination
                      total={Math.ceil(sortedAbnormalSolves.length / ITEMS_PER_PAGE)}
                      value={solvePage}
                      onChange={setSolvePage}
                      size="sm"
                      radius="xl"
                    />
                  </Group>
                )}
              </>
            ) : (
              <AnalysisEmptyState
                filtered={Boolean(globalSearch.trim() || solveSearch.trim())}
                description={t(
                  'game.content.no_cheat.comment',
                  'No abnormal-solve signals were recorded in this report snapshot.'
                )}
                onClearFilters={() => {
                  setGlobalSearch('')
                  setSolveSearch('')
                }}
              />
            )}
          </Tabs.Panel>

          <Tabs.Panel value="collusion" pt="md">
            <Group justify="space-between" mb="md">
              <Group gap="xs">
                <Title order={3}>{t('game.cheat_analysis.card.collusion_groups', 'Similarity Groups')}</Title>
                <Badge variant="light" color="violet" role="status" aria-live="polite">
                  {sortedCollusionGroups.length}
                  {(globalSearch.trim() || collusionSearch.trim()) &&
                    sortedCollusionGroups.length !== report?.collusionGroups?.length && (
                      <> / {report?.collusionGroups?.length ?? 0}</>
                    )}
                </Badge>
              </Group>
              <SmartSearch
                value={collusionSearch}
                onChange={setCollusionSearch}
                placeholder={t('game.cheat_analysis.search_placeholder', 'Search or type @ for filters...')}
                filterDefs={COLLUSION_FILTER_DEFS}
                w={320}
              />
            </Group>
            {sortedCollusionGroups.length > 0 ? (
              <>
                <ScrollArea
                  offsetScrollbars
                  h={roomyHeight}
                  viewportProps={{
                    role: 'region',
                    tabIndex: 0,
                    'aria-label': t('game.cheat_analysis.collusion_scroll_region', 'Collusion similarity groups'),
                  }}
                >
                  <Table
                    className={tableClasses.table}
                    horizontalSpacing="md"
                    verticalSpacing="xs"
                    striped
                    highlightOnHover
                    withTableBorder
                    stickyHeader
                    miw="72rem"
                  >
                    <AccessibleTableCaption>
                      {t('game.cheat_analysis.collusion_table_caption', 'Team solve-timing similarity groups')}
                    </AccessibleTableCaption>
                    <Table.Thead>
                      <Table.Tr>
                        <Table.Th scope="col" w="18rem">
                          {t('game.cheat_analysis.teams', 'Teams')}
                        </Table.Th>
                        <ThSort
                          sorted={collusionSort.key === 'averageRsi'}
                          reversed={collusionSort.direction === 'desc'}
                          onSort={() => handleSort(setCollusionSort, collusionSort, 'averageRsi')}
                          w="12rem"
                        >
                          {t('game.cheat_analysis.similarity', 'Similarity')}
                        </ThSort>
                        <Table.Th scope="col" w="14rem" miw="14rem">
                          {t('game.cheat_analysis.common_solves', 'Common Solves')}
                        </Table.Th>
                        <Table.Th scope="col">{t('game.cheat_analysis.details', 'Details')}</Table.Th>
                        <Table.Th scope="col" w="4rem" miw="4rem" style={{ textAlign: 'center' }}>
                          {t('game.cheat_analysis.view', 'View')}
                        </Table.Th>
                      </Table.Tr>
                    </Table.Thead>
                    <Table.Tbody>
                      {paginatedCollusionGroups.map((item, index) => (
                        <CollusionGroupRow
                          key={`${item.teams?.map((team) => team.participationId ?? team.id).join(':') ?? 'group'}:${item.commonSolves?.join(':') ?? item.averageRsi ?? index}`}
                          item={item}
                          onView={handleViewDetails}
                        />
                      ))}
                    </Table.Tbody>
                  </Table>
                </ScrollArea>
                {sortedCollusionGroups.length > ITEMS_PER_PAGE && (
                  <Group justify="center" mt="md">
                    <Pagination
                      total={Math.ceil(sortedCollusionGroups.length / ITEMS_PER_PAGE)}
                      value={collusionPage}
                      onChange={setCollusionPage}
                      size="sm"
                      radius="xl"
                    />
                  </Group>
                )}
              </>
            ) : (
              <AnalysisEmptyState
                filtered={Boolean(globalSearch.trim() || collusionSearch.trim())}
                description={t(
                  'game.cheat_analysis.no_collusion',
                  'No solve-timing similarity groups were recorded in this report snapshot.'
                )}
                onClearFilters={() => {
                  setGlobalSearch('')
                  setCollusionSearch('')
                }}
              />
            )}
          </Tabs.Panel>

          <Tabs.Panel value="identity" pt="md">
            <Group justify="space-between" mb="xs">
              <Group gap="xs">
                <Title order={3}>{t('game.cheat_analysis.tab.identity', 'Identity Overlap')}</Title>
                <Badge variant="light" color="violet" role="status" aria-live="polite">
                  {filteredIdentityOverlaps.length}
                  {globalSearch.trim() && filteredIdentityOverlaps.length !== report?.identityOverlaps?.length && (
                    <> / {report?.identityOverlaps?.length ?? 0}</>
                  )}
                </Badge>
              </Group>
            </Group>
            <Text size="xs" c="dimmed" mb="md">
              {t(
                'game.cheat_analysis.identity_note',
                'A masked fingerprint or IP correlated with multiple teams. These non-scoring signals are probabilistic: shared devices, browser privacy settings, proxies, campus networks, and carrier NAT can all create overlap. Review alongside other evidence.'
              )}
            </Text>
            {filteredIdentityOverlaps.length > 0 ? (
              <ScrollArea
                offsetScrollbars
                h={roomyHeight}
                viewportProps={{
                  role: 'region',
                  tabIndex: 0,
                  'aria-label': t('game.cheat_analysis.identity_scroll_region', 'Identity overlap signals'),
                }}
              >
                <Table
                  className={tableClasses.table}
                  horizontalSpacing="md"
                  verticalSpacing="xs"
                  striped
                  highlightOnHover
                  withTableBorder
                  stickyHeader
                  miw="46rem"
                >
                  <AccessibleTableCaption>
                    {t('game.cheat_analysis.identity_table_caption', 'Cross-team browser fingerprint and IP overlap')}
                  </AccessibleTableCaption>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th scope="col" w="8rem" miw="8rem">
                        {t('game.cheat_analysis.identity_kind', 'Kind')}
                      </Table.Th>
                      <Table.Th scope="col" w="14rem" miw="14rem">
                        {t('game.cheat_analysis.identity_value', 'Masked value')}
                      </Table.Th>
                      <Table.Th scope="col" w="5rem" miw="5rem" style={{ textAlign: 'center' }}>
                        {t('game.cheat_analysis.identity_teams', 'Teams')}
                      </Table.Th>
                      <Table.Th scope="col" miw="16rem">
                        {t('common.label.team', 'Team')}
                      </Table.Th>
                      <Table.Th scope="col" miw="14rem">
                        {t('game.cheat_analysis.identity_users', 'Users')}
                      </Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {filteredIdentityOverlaps.map((ov: IdentityOverlapResult, idx: number) => (
                      <Table.Tr key={`${ov.kind ?? 'kind'}:${ov.value ?? 'value'}:${ov.teamNames?.join(':') ?? idx}`}>
                        <Table.Td>
                          <Badge
                            size="sm"
                            variant="light"
                            color={ov.kind === 'fingerprint' ? 'violet' : 'blue'}
                            leftSection={
                              <Icon path={ov.kind === 'fingerprint' ? mdiFingerprint : mdiIpNetwork} size={0.45} />
                            }
                          >
                            {ov.kind === 'fingerprint'
                              ? t('game.cheat_analysis.identity_fingerprint', 'Fingerprint')
                              : t('game.cheat_analysis.identity_ip', 'IP')}
                          </Badge>
                        </Table.Td>
                        <Table.Td>
                          <Text ff="monospace" fz="xs" style={{ wordBreak: 'break-all' }}>
                            {ov.value}
                          </Text>
                        </Table.Td>
                        <Table.Td style={{ textAlign: 'center' }}>
                          <Badge
                            size="sm"
                            variant="filled"
                            color={
                              ov.kind === 'fingerprint'
                                ? 'violet'
                                : (ov.teamCount ?? 0) <= 2
                                  ? 'orange'
                                  : (ov.teamCount ?? 0) <= 4
                                    ? 'yellow'
                                    : 'gray'
                            }
                          >
                            {ov.teamCount}
                          </Badge>
                        </Table.Td>
                        <Table.Td>
                          <Text size="xs">{(ov.teamNames ?? []).join(', ')}</Text>
                        </Table.Td>
                        <Table.Td>
                          <Text size="xs" c="dimmed">
                            {(ov.userNames ?? []).join(', ')}
                          </Text>
                        </Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </ScrollArea>
            ) : (
              <AnalysisEmptyState
                filtered={Boolean(globalSearch.trim())}
                description={t(
                  'game.cheat_analysis.no_identity_overlap',
                  'No cross-team identity overlap signals were recorded in this report snapshot.'
                )}
                onClearFilters={() => setGlobalSearch('')}
              />
            )}
          </Tabs.Panel>
        </Tabs>
      </Paper>
    </>
  )
}
