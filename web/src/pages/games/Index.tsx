import {
  ActionIcon,
  Anchor,
  Badge,
  Group,
  Pagination,
  SimpleGrid,
  Skeleton,
  Stack,
  Text,
  TextInput,
  Title,
  useMantineColorScheme,
  useMantineTheme,
} from '@mantine/core'
import { useDebouncedValue } from '@mantine/hooks'
import { mdiClose, mdiMagnify } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useRef, useState } from 'react'
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
  const [search, setSearch] = useState('')
  const [debouncedSearch] = useDebouncedValue(search.trim(), 300)
  const searchInput = useRef<HTMLInputElement>(null)
  const isMobile = useIsMobile()
  const theme = useMantineTheme()
  const { colorScheme } = useMantineColorScheme()

  const { data: games, isLoading } = api.game.useGameGames(
    {
      count: ITEM_PER_PAGE,
      skip: (activePage - 1) * ITEM_PER_PAGE,
      search: debouncedSearch || undefined,
    },
    {
      refreshInterval: 5 * 60 * 1000,
    }
  )

  const clearSearch = () => {
    setSearch('')
    setPage(1)
    searchInput.current?.focus()
  }

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
          <Link
            className={ganttClasses.eventLabel}
            to={`/games/${game.id}`}
            title={`${title} — ${statusLabel} · ${toLimitTag(t, game.limit)}`}
          >
            <span className={ganttClasses.title} title={title}>
              {title}
            </span>
            <span className={ganttClasses.eventMeta} title={`${statusLabel} · ${toLimitTag(t, game.limit)}`}>
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

        <form role="search" className={classes.searchForm} onSubmit={(event) => event.preventDefault()}>
          <TextInput
            ref={searchInput}
            className={classes.searchInput}
            label={t('game.content.search_label', 'Search events')}
            description={t(
              'game.content.search_description',
              'Search every event by name, summary, or exact event ID.'
            )}
            placeholder={t('game.content.search_placeholder', 'Enter an event name')}
            value={search}
            maxLength={100}
            enterKeyHint="search"
            leftSection={<Icon path={mdiMagnify} size={0.82} aria-hidden="true" />}
            rightSection={
              search ? (
                <ActionIcon
                  type="button"
                  variant="subtle"
                  color="gray"
                  aria-label={t('game.content.clear_search', 'Clear event search')}
                  onClick={clearSearch}
                >
                  <Icon path={mdiClose} size={0.82} aria-hidden="true" />
                </ActionIcon>
              ) : undefined
            }
            rightSectionPointerEvents="all"
            aria-controls="event-catalog-results"
            onChange={(event) => {
              setSearch(event.currentTarget.value)
              setPage(1)
            }}
            onKeyDown={(event) => {
              if (event.key === 'Escape' && search) clearSearch()
            }}
          />
          {debouncedSearch && games && (
            <Text role="status" aria-live="polite" size="sm" c="dimmed">
              {t('game.content.search_results', '{{count}} matching events for “{{query}}”', {
                count: games.total,
                query: debouncedSearch,
              })}
            </Text>
          )}
        </form>

        <div
          id="event-catalog-results"
          aria-busy={games === undefined || isLoading ? true : undefined}
        >
          {games === undefined ? (
            <SimpleGrid cols={{ base: 1, md: 2, xl: 3, w24: 4 }} spacing="lg" verticalSpacing="lg">
              {Array.from({ length: ITEM_PER_PAGE }).map((_, index) => (
                <Skeleton key={index} h="13.25rem" radius="lg" />
              ))}
            </SimpleGrid>
          ) : games.data.length === 0 ? (
            <Empty
              description={
                debouncedSearch
                  ? t('game.content.no_search_results', 'No events match “{{query}}”.', {
                      query: debouncedSearch,
                    })
                  : t('game.content.no_game', 'No games available')
              }
            />
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
