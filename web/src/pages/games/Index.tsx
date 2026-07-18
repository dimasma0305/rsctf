import {
  Anchor,
  Badge,
  Group,
  Pagination,
  SimpleGrid,
  Skeleton,
  Stack,
  Text,
  Title,
  useMantineColorScheme,
  useMantineTheme,
} from '@mantine/core'
import { FC, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { Empty } from '@Components/Empty'
import { GameCard, GameColorMap, GameStatus, getGameStatusLabel } from '@Components/GameCard'
import { PageHeader } from '@Components/PageHeader'
import { WithNavBar } from '@Components/WithNavbar'
import { GanttTimeLine } from '@Components/charts/GanttTimeline'
import { useIsMobile } from '@Utils/ThemeOverride'
import { getGameStatus, toLimitTag, useRecentGames } from '@Hooks/useGame'
import { usePageTitle } from '@Hooks/usePageTitle'
import api from '@Api'
import classes from '@Styles/GamesIndex.module.css'
import ganttClasses from '@Styles/GanttTimeline.module.css'

const ITEM_PER_PAGE = 12

const Games: FC = () => {
  const { t } = useTranslation()
  const { recentGames } = useRecentGames()
  const [activePage, setPage] = useState(1)
  const isMobile = useIsMobile()
  const theme = useMantineTheme()
  const { colorScheme } = useMantineColorScheme()

  const { data: games } = api.game.useGameGames(
    { count: ITEM_PER_PAGE, skip: (activePage - 1) * ITEM_PER_PAGE },
    {
      refreshInterval: 5 * 60 * 1000,
    }
  )

  usePageTitle(t('game.title.index'))

  const recents =
    recentGames?.map((game) => {
      const { startTime, endTime, status } = getGameStatus(game)
      const color = GameColorMap.get(status) ?? 'gray'
      const colorHex = theme.colors[color][colorScheme === 'dark' ? 5 : 6]
      const title = game.title || t('game.content.untitled', 'Untitled event')
      const statusLabel = getGameStatusLabel(t, status)

      return {
        id: game.id,
        textTitle: title,
        statusLabel,
        color: colorHex,
        title: (
          <Link className={ganttClasses.eventLabel} to={`/games/${game.id}`}>
            <span className={ganttClasses.title}>{title}</span>
            <span className={ganttClasses.eventMeta}>
              {statusLabel} · {toLimitTag(t, game.limit)}
            </span>
          </Link>
        ),
        start: startTime,
        end: endTime,
      }
    }) ?? []

  const lifecycleSections = [
    {
      status: GameStatus.OnGoing,
      title: getGameStatusLabel(t, GameStatus.OnGoing),
      description: t('game.content.lifecycle.live_description', 'Open now — jump in while scoring is active.'),
    },
    {
      status: GameStatus.Coming,
      title: getGameStatusLabel(t, GameStatus.Coming),
      description: t('game.content.lifecycle.upcoming_description', 'Plan ahead and get your team ready.'),
    },
    {
      status: GameStatus.Ended,
      title: getGameStatusLabel(t, GameStatus.Ended),
      description: t('game.content.lifecycle.past_description', 'Revisit completed events and their results.'),
    },
  ].map((section) => ({
    ...section,
    events: games?.data.filter((game) => getGameStatus(game).status === section.status) ?? [],
  }))

  const pageCount = Math.ceil((games?.total ?? 0) / ITEM_PER_PAGE)

  return (
    <WithNavBar withFooter withHeader stickyHeader>
      <PageHeader
        eyebrow={t('game.content.workspace', 'Competition')}
        title={t('game.title.index')}
        description={t('game.content.index_description', 'Browse upcoming, live, and completed competitions.')}
        actions={
          games && (
            <Badge size="lg" variant="light" className={classes.totalBadge}>
              {t('game.content.events_total', '{{count}} events', { count: games.total })}
            </Badge>
          )
        }
      />

      <Stack gap="xl" className={classes.catalog}>
        <Group component="header" justify="space-between" align="flex-end" gap="lg" wrap="wrap">
          <Stack gap={3}>
            <Text className={classes.eyebrow}>{t('game.content.event_discovery', 'Event discovery')}</Text>
            <Title order={2} size="h3" className={classes.catalogTitle}>
              {t('game.content.choose_event', 'Choose your next challenge')}
            </Title>
            <Text size="sm" c="dimmed">
              {t(
                'game.content.page_grouping_hint',
                'Events on this page are organized by where they are in their lifecycle.'
              )}
            </Text>
          </Stack>

          {games && games.data.length > 0 && (
            <nav
              className={classes.lifecycleOverview}
              aria-label={t('game.content.lifecycle.summary', 'Events by status on this page')}
            >
              {lifecycleSections
                .filter((section) => section.events.length > 0)
                .map((section) => (
                  <Anchor
                    key={section.status}
                    href={`#lifecycle-${section.status}`}
                    className={classes.lifecycleCount}
                    data-status={section.status}
                  >
                    <span className={classes.lifecycleDot} aria-hidden="true" />
                    <span>{section.title}</span>
                    <strong>{section.events.length}</strong>
                  </Anchor>
                ))}
            </nav>
          )}
        </Group>

        <div aria-live="polite" aria-busy={games === undefined || undefined}>
          {games === undefined ? (
            <SimpleGrid cols={{ base: 1, md: 2, xl: 3, w24: 4 }} spacing="lg" verticalSpacing="lg">
              {Array.from({ length: ITEM_PER_PAGE }).map((_, index) => (
                <Skeleton key={index} h="13.25rem" radius="lg" />
              ))}
            </SimpleGrid>
          ) : games.data.length === 0 ? (
            <Empty description={t('game.content.no_game', 'No games available')} />
          ) : (
            <Stack gap="xl">
              {lifecycleSections
                .filter((section) => section.events.length > 0)
                .map((section) => (
                  <section
                    key={section.status}
                    aria-labelledby={`lifecycle-${section.status}`}
                    className={classes.lifecycleSection}
                  >
                    <Group justify="space-between" align="center" gap="md" className={classes.sectionHeader}>
                      <Group wrap="nowrap" gap="sm">
                        <span className={classes.sectionMarker} data-status={section.status} aria-hidden="true">
                          <span />
                        </span>
                        <div>
                          <Title
                            order={3}
                            size="h4"
                            id={`lifecycle-${section.status}`}
                            className={classes.sectionTitle}
                          >
                            {section.title}
                          </Title>
                          <Text size="sm" c="dimmed">
                            {section.description}
                          </Text>
                        </div>
                      </Group>
                      <Badge color={GameColorMap.get(section.status)} variant="light" size="lg">
                        {section.events.length}
                      </Badge>
                    </Group>

                    <SimpleGrid cols={{ base: 1, md: 2, xl: 3, w24: 4 }} spacing="lg" verticalSpacing="lg">
                      {section.events.map((game) => (
                        <GameCard key={game.id} game={game} />
                      ))}
                    </SimpleGrid>
                  </section>
                ))}
            </Stack>
          )}
        </div>

        {pageCount > 1 && (
          <nav aria-label={t('game.content.pagination_label', 'Event result pages')} className={classes.paginationNav}>
            <Pagination.Root total={pageCount} siblings={isMobile ? 0 : 2} value={activePage} onChange={setPage}>
              <Group gap={5} justify={isMobile ? 'center' : 'flex-end'}>
                {!isMobile && <Pagination.First aria-label={t('common.pagination.first', 'First page')} />}
                <Pagination.Previous aria-label={t('common.pagination.previous', 'Previous page')} />
                <Pagination.Items />
                <Pagination.Next aria-label={t('common.pagination.next', 'Next page')} />
                {!isMobile && <Pagination.Last aria-label={t('common.pagination.last', 'Last page')} />}
              </Group>
            </Pagination.Root>
          </nav>
        )}
      </Stack>

      <GanttTimeLine items={recents} />
    </WithNavBar>
  )
}

export default Games
