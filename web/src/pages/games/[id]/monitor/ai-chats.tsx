import {
  ActionIcon,
  Alert,
  Badge,
  Button,
  Card,
  Group,
  Select,
  Stack,
  Text,
  Title,
  Tooltip,
  useComputedColorScheme,
} from '@mantine/core'
import { useClipboard } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import {
  mdiAccountGroupOutline,
  mdiAccountOutline,
  mdiAlertCircleOutline,
  mdiArrowLeftBold,
  mdiArrowRightBold,
  mdiCheck,
  mdiContentCopy,
  mdiOpenInNew,
  mdiRobotOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { Empty } from '@Components/Empty'
import { WithGameMonitor } from '@Components/WithGameMonitor'
import { aiChatHostname, safeAiChatHref } from '@Utils/AiChatLinks'
import { useLanguage } from '@Utils/I18n'
import { tryGetErrorMsg } from '@Utils/Shared'
import { useGameScoreboardRead } from '@Hooks/useGame'
import api, { AiChatLinkRecord, ChallengeType } from '@Api'

const ITEM_COUNT_PER_PAGE = 50

const JEOPARDY_TYPES: ReadonlySet<ChallengeType> = new Set([
  ChallengeType.StaticAttachment,
  ChallengeType.StaticContainer,
  ChallengeType.DynamicAttachment,
  ChallengeType.DynamicContainer,
])

const AiChats: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const { t } = useTranslation()
  // Orange stays below 4.5:1 on white at badge sizes; red.9 reaches 5.5:1.
  const blockedColor = useComputedColorScheme('dark') === 'dark' ? 'orange' : 'red.9'
  const { locale } = useLanguage()
  const clipboard = useClipboard({ timeout: 1500 })

  const [activePage, setPage] = useState(1)
  const [challengeFilter, setChallengeFilter] = useState<string | null>(null)
  const challengeId = challengeFilter ? Number(challengeFilter) : undefined

  const { data, error, isLoading, mutate } = api.game.useGameListAiChatLinks(
    numId,
    { count: ITEM_COUNT_PER_PAGE, skip: (activePage - 1) * ITEM_COUNT_PER_PAGE, challengeId },
    { refreshInterval: 0, revalidateOnFocus: false, keepPreviousData: true },
    numId > 0
  )
  // One read of the monitor-visible scoreboard catalog supplies the filter.
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

  const onCopy = (url: string) => {
    clipboard.copy(url)
    showNotification({
      color: 'teal',
      message: t('game.ai_chat.copied', 'Link copied'),
      icon: <Icon path={mdiCheck} size={1} />,
    })
  }

  const recordCard = (record: AiChatLinkRecord) => (
    <Card key={`${record.participationId}:${record.challengeId}`} shadow="sm" p="sm" component="li">
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
                    {t('game.ai_chat.team', 'Team')}:{' '}
                  </Text>
                  {record.teamName}
                </Text>
              </Group>
            </Group>
          </Stack>
          <Stack gap={2} align="flex-end">
            <Text size="xs" c="dimmed">
              <time dateTime={new Date(record.updatedAt).toISOString()}>
                {dayjs(record.updatedAt).locale(locale).format('SL LTS')}
              </time>
            </Text>
            {record.submittedBy && (
              <Group gap={4} wrap="nowrap">
                <Icon path={mdiAccountOutline} size={0.6} aria-hidden="true" />
                <Text size="xs" c="dimmed">
                  {t('game.ai_chat.submitted_by', 'Saved by {{user}}', { user: record.submittedBy })}
                </Text>
              </Group>
            )}
          </Stack>
        </Group>
        <Stack gap={6} component="ul" m={0} p={0} style={{ listStyle: 'none' }}>
          {record.links.map((link) => {
            const href = safeAiChatHref(link.url)
            return (
              <Group key={link.url} component="li" gap="xs" wrap="nowrap" justify="space-between" align="flex-start">
                <Stack gap={2} style={{ minWidth: 0, flex: 1 }}>
                  <Group gap={6} wrap="wrap">
                    <Badge size="sm" variant="light" color={link.providerActive ? 'teal' : 'gray'} tt="none">
                      {link.providerLabel}
                    </Badge>
                    {!link.providerActive && (
                      <Badge size="sm" variant="outline" color={blockedColor} tt="none">
                        {t('game.ai_chat.provider_blocked', 'Provider now blocked')}
                      </Badge>
                    )}
                    <Text size="sm" fw={600} style={{ overflowWrap: 'anywhere' }}>
                      {aiChatHostname(link.url)}
                    </Text>
                  </Group>
                  <Text size="xs" c="dimmed" ff="monospace" style={{ overflowWrap: 'anywhere' }}>
                    {link.url}
                  </Text>
                </Stack>
                <Group gap={2} wrap="nowrap">
                  <Tooltip label={t('game.ai_chat.copy', 'Copy link')}>
                    <ActionIcon
                      variant="subtle"
                      color="gray"
                      size="lg"
                      aria-label={t('game.ai_chat.copy', 'Copy link')}
                      onClick={() => onCopy(link.url)}
                    >
                      <Icon path={mdiContentCopy} size={0.75} aria-hidden="true" />
                    </ActionIcon>
                  </Tooltip>
                  {href && (
                    <Tooltip label={t('game.ai_chat.open', 'Open link in a new tab')}>
                      <ActionIcon
                        component="a"
                        href={href}
                        target="_blank"
                        rel="noopener noreferrer nofollow"
                        variant="subtle"
                        color="gray"
                        size="lg"
                        aria-label={t('game.ai_chat.open', 'Open link in a new tab')}
                      >
                        <Icon path={mdiOpenInNew} size={0.75} aria-hidden="true" />
                      </ActionIcon>
                    </Tooltip>
                  )}
                </Group>
              </Group>
            )
          })}
        </Stack>
      </Stack>
    </Card>
  )

  return (
    <WithGameMonitor isLoading={isLoading && !data}>
      <Stack gap="md" w="100%">
        <Group justify="space-between" align="flex-end" gap="sm" wrap="wrap">
          <Stack gap={2} style={{ minWidth: 0, flex: '1 1 16rem' }}>
            <Title order={2}>{t('game.ai_chat.title', 'AI chat links')}</Title>
            <Text size="xs" c="dimmed">
              {t(
                'game.ai_chat.subtitle',
                'Share links teams attached to solved challenges, newest first. Links open in a new tab.'
              )}
            </Text>
          </Stack>
          <Select
            label={t('game.ai_chat.challenge_filter', 'Challenge')}
            placeholder={t('game.ai_chat.all_challenges', 'All challenges')}
            data={challengeOptions}
            value={challengeFilter}
            searchable
            clearable
            nothingFoundMessage={t('game.ai_chat.no_challenge_match', 'No matching challenge')}
            onChange={(value) => {
              setChallengeFilter(value)
              setPage(1)
            }}
            w={{ base: '100%', xs: 280 }}
          />
        </Group>

        {error && (
          <Alert color="red" icon={<Icon path={mdiAlertCircleOutline} size={0.9} />}>
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
            mdiPath={mdiRobotOutline}
            title={t('game.ai_chat.empty_title', 'No AI chat links yet')}
            description={
              challengeFilter
                ? t('game.ai_chat.empty_filtered', 'No team has attached links to this challenge.')
                : t('game.ai_chat.empty_description', 'Links appear here after teams attach them to solved challenges.')
            }
          />
        ) : (
          <Stack
            gap="xs"
            component="ul"
            m={0}
            p={0}
            style={{ listStyle: 'none' }}
            aria-label={t('game.ai_chat.list_label', 'AI chat link records')}
          >
            {(data?.items ?? []).map(recordCard)}
          </Stack>
        )}

        <Group justify="space-between" gap="sm" wrap="wrap">
          <Text size="sm" c="dimmed" role="status" aria-live="polite">
            {t('game.ai_chat.page_status', 'Page {{page}} of {{pages}} · {{total}} records', {
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

export default AiChats
