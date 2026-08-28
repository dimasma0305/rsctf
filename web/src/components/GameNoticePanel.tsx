import { Card, Center, List, ScrollArea, SegmentedControl, Stack, Text, useMantineTheme } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { TFunction } from 'i18next'
import { FC, useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { Empty } from '@Components/Empty'
import { InlineMarkdown } from '@Components/MarkdownRenderer'
import { reconcileLiveRows } from '@Utils/FeedReconciliation'
import { useLanguage } from '@Utils/I18n'
import { currentListSnapshotRows, LatestListRequest, type ListSnapshot } from '@Utils/LatestRequest'
import { MAX_GAME_NOTICE_ROWS, mergeGameNotices, receiveGameNotice } from '@Utils/NoticeFeed'
import { NoticTypeIconMap } from '@Utils/Shared'
import { NOTICE_FALLBACK_POLL_MS } from '@Utils/SignalRRecovery'
import { useViewerIdentity } from '@Utils/ViewerIdentity'
import { useRecoveringHub } from '@Hooks/useRecoveringHub'
import api, { GameNotice, NoticeType } from '@Api'
import misc from '@Styles/Misc.module.css'
import typoClasses from '@Styles/Typography.module.css'

enum NoticeFilter {
  All = 'all',
  Challenge = 'challenge',
  Events = 'events',
  Game = 'game',
}

const ApplyFilter = (notices: readonly GameNotice[], filter: NoticeFilter) => {
  switch (filter) {
    case NoticeFilter.All:
      return [...notices]
    case NoticeFilter.Challenge:
      return notices.filter((notice) => notice.type === NoticeType.NewChallenge || notice.type === NoticeType.NewHint)
    case NoticeFilter.Events:
      return notices.filter(
        (notice) =>
          notice.type === NoticeType.FirstBlood ||
          notice.type === NoticeType.SecondBlood ||
          notice.type === NoticeType.ThirdBlood
      )
    case NoticeFilter.Game:
      return notices.filter((notice) => notice.type === NoticeType.Normal)
    default:
      return [...notices]
  }
}

const formatNotice = (t: TFunction, notice: GameNotice) => {
  switch (notice.type) {
    case NoticeType.Normal:
      return notice.values.at(-1) || ''
    case NoticeType.NewChallenge:
      return t('game.notice.new_challenge', {
        title: notice.values.at(0),
      })
    case NoticeType.NewHint:
      return t('game.notice.new_hint', {
        title: notice.values.at(0),
      })
    case NoticeType.FirstBlood:
      return t('game.notice.blood', {
        team: notice.values.at(0),
        chal: notice.values.at(1),
        blood: t('challenge.bonus.first_blood'),
      })
    case NoticeType.SecondBlood:
      return t('game.notice.blood', {
        team: notice.values.at(0),
        chal: notice.values.at(1),
        blood: t('challenge.bonus.second_blood'),
      })
    case NoticeType.ThirdBlood:
      return t('game.notice.blood', {
        team: notice.values.at(0),
        chal: notice.values.at(1),
        blood: t('challenge.bonus.third_blood'),
      })
    default:
      return notice.values.at(-1) || ''
  }
}

const PANEL_HEIGHT = 'clamp(12rem, calc(100dvh - 25rem), 48rem)'

export const GameNoticePanel: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const feedActive = Boolean(id) && Number.isInteger(numId) && numId > 0
  const { scope: viewerScope } = useViewerIdentity()
  const noticeScope = JSON.stringify([viewerScope, numId])

  const [, update] = useState(0)
  const newNotices = useRef<ListSnapshot<GameNotice>>({ scope: noticeScope, rows: [] })
  const noticeSnapshotRows = useRef<ListSnapshot<GameNotice>>({ scope: noticeScope, rows: [] })
  const noticeRequest = useRef(new LatestListRequest<GameNotice>())
  const [noticeSnapshot, setNoticeSnapshot] = useState<ListSnapshot<GameNotice>>()
  const [filter, setFilter] = useState<NoticeFilter>(NoticeFilter.All)
  const iconMap = NoticTypeIconMap(0.8)

  const { t } = useTranslation()
  const { locale } = useLanguage()
  const theme = useMantineTheme()
  const notices = currentListSnapshotRows(noticeScope, noticeSnapshot)

  const fetchNotices = useCallback(async () => {
    if (!feedActive) return
    const snapshot = await noticeRequest.current.run(noticeScope, async (signal) => {
      const response = await api.game.gameNotices(numId, { count: MAX_GAME_NOTICE_ROWS }, { signal })
      return response.data.slice(0, MAX_GAME_NOTICE_ROWS)
    })
    if (!snapshot) return

    const liveRows = currentListSnapshotRows(noticeScope, newNotices.current) ?? []
    noticeSnapshotRows.current = snapshot
    newNotices.current = {
      scope: noticeScope,
      rows: reconcileLiveRows(liveRows, snapshot.rows, (notice) => notice.id).slice(0, MAX_GAME_NOTICE_ROWS),
    }
    setNoticeSnapshot(snapshot)
  }, [feedActive, noticeScope, numId])

  useEffect(() => {
    // Keep the panel's prior silent initial-read behavior; the recovery owner
    // performs reconnect and fallback-poll retries for transient failures.
    void fetchNotices().catch(() => undefined)
    return () => noticeRequest.current.cancel()
  }, [fetchNotices])

  useRecoveringHub({
    active: feedActive,
    url: `/hub/user?game=${numId}`,
    ownerKey: noticeScope,
    handlers: {
      ReceivedGameNotice: (raw) => {
        const message = raw as GameNotice
        const liveRows = currentListSnapshotRows(noticeScope, newNotices.current) ?? []
        const snapshotRows = currentListSnapshotRows(noticeScope, noticeSnapshotRows.current) ?? []
        const received = receiveGameNotice(message, liveRows, snapshotRows)
        if (!received.accepted) return
        newNotices.current = { scope: noticeScope, rows: received.rows }

        if (message.type === NoticeType.NewChallenge || message.type === NoticeType.NewHint) {
          showNotification({
            color: 'yellow',
            message: formatNotice(t, message),
            autoClose: 5000,
          })
        }

        if (message.type === NoticeType.Normal) {
          showNotification({
            color: theme.primaryColor,
            message: formatNotice(t, message),
            autoClose: 5000,
          })
        }

        update((version) => version + 1)
      },
    },
    revalidate: fetchNotices,
    pollingIntervalMs: NOTICE_FALLBACK_POLL_MS,
  })

  const liveNotices = currentListSnapshotRows(noticeScope, newNotices.current) ?? []
  const allNotices = mergeGameNotices(liveNotices, notices ?? [])
  const filteredNotices = ApplyFilter(allNotices, filter)
  const visibleNotices = filteredNotices.slice(0, MAX_GAME_NOTICE_ROWS)

  return (
    <Card shadow="sm" w="100%">
      <Stack gap="xs">
        <SegmentedControl
          value={filter}
          aria-label={t('game.label.notice_type.filter', 'Filter notices by type')}
          color={theme.primaryColor}
          fullWidth
          bg="transparent"
          fw={500}
          onChange={(value) => setFilter(value as NoticeFilter)}
          data={[
            { value: NoticeFilter.All, label: t('game.label.notice_type.all') },
            { value: NoticeFilter.Game, label: t('game.label.notice_type.game') },
            { value: NoticeFilter.Events, label: t('game.label.notice_type.events') },
            { value: NoticeFilter.Challenge, label: t('game.label.notice_type.challenge') },
          ]}
        />
        {visibleNotices.length ? (
          <ScrollArea
            offsetScrollbars
            scrollbarSize={0}
            h={PANEL_HEIGHT}
            viewportProps={{
              tabIndex: 0,
              'aria-label': t('game.label.notices', 'Game notices'),
            }}
          >
            <List size="sm" spacing={3} classNames={{ itemWrapper: misc.alignNormal }}>
              {visibleNotices.map((notice) => (
                <List.Item key={notice.id} icon={<Icon {...iconMap.get(notice.type)!} />}>
                  <Stack gap={1}>
                    <Text fz="xs" fw="bold" c="dimmed">
                      {dayjs(notice.time).locale(locale).format('SLL LTS')}
                    </Text>
                    {notice.type === NoticeType.Normal ? (
                      <InlineMarkdown fz="sm" fw={500} c="dimmed" source={formatNotice(t, notice)} />
                    ) : (
                      <Text fz="sm" fw={500} c="dimmed" className={typoClasses.inline}>
                        {formatNotice(t, notice)}
                      </Text>
                    )}
                  </Stack>
                </List.Item>
              ))}
            </List>
          </ScrollArea>
        ) : (
          <Center h={PANEL_HEIGHT}>
            <Empty description={t('game.content.no_notice')} />
          </Center>
        )}
      </Stack>
    </Card>
  )
}
