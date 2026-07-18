import { Badge, Box, Group, ScrollArea, Stack, Text, Title } from '@mantine/core'
import { mdiCalendarBlankOutline, mdiGestureSwipeHorizontal } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs, { Dayjs } from 'dayjs'
import { CSSProperties, FC, ReactNode, useEffect, useMemo, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { useLanguage } from '@Utils/I18n'
import classes from '@Styles/GanttTimeline.module.css'

interface GanttTimeLineProps {
  items: GanttItem[]
}

export interface GanttItem {
  id: number
  color?: string
  textTitle: string
  statusLabel?: string
  title: ReactNode
  start: Dayjs
  end: Dayjs
}

interface TimelineDay {
  time: Dayjs
  isToday: boolean
  isWeekend: boolean
}

interface TimelineMonth {
  label: string
  position: number
}

interface DateData {
  start: Dayjs
  end: Dayjs
  durationSeconds: number
  nowPosition: number
  days: TimelineDay[]
  months: TimelineMonth[]
}

const DAY_COUNT = 49

const clamp = (value: number, minimum: number, maximum: number) => Math.min(maximum, Math.max(minimum, value))

export const GanttTimeLine: FC<GanttTimeLineProps> = ({ items }) => {
  const viewport = useRef<HTMLDivElement>(null)
  const todayMarker = useRef<HTMLSpanElement>(null)
  const { t } = useTranslation()
  const { locale } = useLanguage()

  const dateData = useMemo<DateData>(() => {
    const now = dayjs()
    const start = now.startOf('week').subtract(3, 'week').startOf('day')
    const end = start.add(DAY_COUNT, 'day')
    const durationSeconds = end.diff(start, 'second')
    const days: TimelineDay[] = []
    const months: TimelineMonth[] = []
    let previousMonth = ''

    for (let index = 0; index < DAY_COUNT; index++) {
      const current = start.add(index, 'day').locale(locale)
      const monthKey = current.format('YYYY-MM')

      days.push({
        time: current,
        isToday: current.isSame(now, 'day'),
        isWeekend: current.day() === 0 || current.day() === 6,
      })

      if (monthKey !== previousMonth) {
        months.push({
          label: current.format('MMMM YYYY'),
          position: (index / DAY_COUNT) * 100,
        })
        previousMonth = monthKey
      }
    }

    return {
      start,
      end,
      durationSeconds,
      nowPosition: clamp((now.diff(start, 'second') / durationSeconds) * 100, 0, 100),
      days,
      months,
    }
  }, [locale])

  useEffect(() => {
    const element = viewport.current
    const marker = todayMarker.current
    if (!element || !marker) return

    const frame = window.requestAnimationFrame(() => {
      const viewportBox = element.getBoundingClientRect()
      const markerBox = marker.getBoundingClientRect()
      const markerPosition = element.scrollLeft + markerBox.left - viewportBox.left

      element.scrollTo({ left: Math.max(0, markerPosition - element.clientWidth * 0.62) })
    })

    return () => window.cancelAnimationFrame(frame)
  }, [items.length, locale])

  const positionOf = (time: Dayjs) => (time.diff(dateData.start, 'second') / dateData.durationSeconds) * 100

  return (
    <Box component="section" className={classes.root} aria-labelledby="event-schedule-title">
      <Group
        component="header"
        justify="space-between"
        align="flex-end"
        gap="md"
        wrap="wrap"
        className={classes.header}
      >
        <Stack gap={2}>
          <Text className={classes.eyebrow}>{t('game.content.schedule_window', 'Schedule window')}</Text>
          <Title order={2} size="h3" id="event-schedule-title" className={classes.heading}>
            {t('game.content.schedule_title', 'Competition schedule')}
          </Title>
        </Stack>
        <Group gap="xs" wrap="wrap" className={classes.range}>
          <Icon path={mdiCalendarBlankOutline} size={0.78} aria-hidden="true" />
          <Text size="sm">
            <time dateTime={dateData.start.toISOString()}>{dateData.start.locale(locale).format('SLL')}</time>
            <span aria-hidden="true"> — </span>
            <time dateTime={dateData.end.toISOString()}>{dateData.end.locale(locale).format('SLL')}</time>
          </Text>
          <Badge color="gray" variant="light" size="sm">
            {t('game.content.schedule_window_count', '{{count}} in this window', { count: items.length })}
          </Badge>
        </Group>
      </Group>

      {items.length === 0 ? (
        <Text className={classes.empty}>
          {t('game.content.no_recent_games', 'No events fall within this schedule yet.')}
        </Text>
      ) : (
        <>
          <ScrollArea
            className={classes.scrollArea}
            type="auto"
            offsetScrollbars="x"
            scrollbarSize={9}
            viewportRef={viewport}
            viewportProps={{
              tabIndex: 0,
              'aria-label': t('game.content.schedule_scroll_label', 'Scrollable seven-week event schedule'),
            }}
          >
            <div className={classes.canvas} style={{ '--today-position': `${dateData.nowPosition}%` } as CSSProperties}>
              <div className={classes.calendarHeader}>
                <div className={classes.cornerLabel}>
                  <Text fw={720} size="sm">
                    {t('game.content.event', 'Event')}
                  </Text>
                  <Text size="xs" c="dimmed">
                    {t('game.content.status_and_format', 'Status · format')}
                  </Text>
                </div>
                <div className={classes.calendarScale} aria-hidden="true">
                  <div className={classes.monthRow}>
                    {dateData.months.map((month) => (
                      <span key={`${month.label}-${month.position}`} style={{ left: `${month.position}%` }}>
                        {month.label}
                      </span>
                    ))}
                  </div>
                  <div className={classes.dayRow}>
                    {dateData.days.map((day) => (
                      <span
                        key={day.time.valueOf()}
                        className={classes.dayCell}
                        data-today={day.isToday || undefined}
                        data-weekend={day.isWeekend || undefined}
                      >
                        <small>{day.time.format('dd')}</small>
                        <strong>{day.time.format('D')}</strong>
                      </span>
                    ))}
                  </div>
                  <span ref={todayMarker} className={classes.todayHeaderMarker} />
                </div>
              </div>

              <div className={classes.rows} role="list">
                {items.map((item) => {
                  const isVisible = !item.end.isBefore(dateData.start) && !item.start.isAfter(dateData.end)
                  const left = clamp(positionOf(item.start), 0, 100)
                  const right = clamp(positionOf(item.end), 0, 100)
                  const width = Math.max(0, right - left)
                  const rangeLabel = t(
                    'game.content.event_schedule_range',
                    '{{title}}, {{status}}, {{start}} to {{end}}',
                    {
                      title: item.textTitle,
                      status: item.statusLabel ?? t('game.content.scheduled', 'Scheduled'),
                      start: item.start.locale(locale).format('L LTS'),
                      end: item.end.locale(locale).format('L LTS'),
                    }
                  )

                  return (
                    <div key={item.id} className={classes.row} role="listitem">
                      <div className={classes.eventCell}>{item.title}</div>
                      <div className={classes.track}>
                        <span className={classes.todayLine} aria-hidden="true" />
                        {isVisible ? (
                          <Link
                            to={`/games/${item.id}`}
                            className={classes.eventBar}
                            aria-label={rangeLabel}
                            title={rangeLabel}
                            style={
                              {
                                '--event-left': `${left}%`,
                                '--event-width': `${width}%`,
                                '--event-color': item.color ?? 'var(--mantine-primary-color-6)',
                              } as CSSProperties
                            }
                          >
                            <span className={classes.eventBarTitle}>{item.textTitle}</span>
                            <span className={classes.eventBarStatus}>{item.statusLabel}</span>
                          </Link>
                        ) : (
                          <Text size="xs" className={classes.outsideRange}>
                            {t('game.content.outside_schedule', 'Outside this 7-week window')}
                          </Text>
                        )}
                      </div>
                    </div>
                  )
                })}
              </div>
            </div>
          </ScrollArea>
          <Text size="xs" className={classes.mobileHint}>
            <Icon path={mdiGestureSwipeHorizontal} size={0.7} aria-hidden="true" />
            {t('game.content.schedule_swipe_hint', 'Swipe horizontally to explore the full schedule.')}
          </Text>
        </>
      )}
    </Box>
  )
}
