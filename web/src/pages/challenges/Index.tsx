import {
  ActionIcon,
  Badge,
  Button,
  Card,
  Group,
  Pagination,
  SegmentedControl,
  Select,
  SimpleGrid,
  Skeleton,
  Stack,
  Text,
  TextInput,
  Title,
  VisuallyHidden,
} from '@mantine/core'
import { useDebouncedValue } from '@mantine/hooks'
import { mdiCheckCircleOutline, mdiClose, mdiFlagOutline, mdiMagnify, mdiOpenInNew } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, Navigate } from 'react-router'
import { Empty } from '@Components/Empty'
import { PageHeader } from '@Components/PageHeader'
import { WithNavBar } from '@Components/WithNavbar'
import { ChallengeCategoryList, useChallengeCategoryLabelMap, useChallengeTypeLabelMap } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUser } from '@Hooks/useUser'
import api, { ChallengeCategory, ChallengeType } from '@Api'
import classes from '@Styles/ChallengeCatalog.module.css'

const ITEMS_PER_PAGE = 24
type SolveFilter = 'all' | 'solved' | 'unsolved'

const challengeHash = (id: number, title: string) => `#${id}-${encodeURIComponent(title.replace(/ /g, '-'))}`

const ChallengeCatalog: FC = () => {
  const { t } = useTranslation()
  const { user, error: userError } = useUser()
  const categoryMap = useChallengeCategoryLabelMap()
  const typeMap = useChallengeTypeLabelMap()
  const isMobile = useIsMobile()
  const searchInput = useRef<HTMLInputElement>(null)
  const [page, setPage] = useState(1)
  const [search, setSearch] = useState('')
  const [debouncedSearch] = useDebouncedValue(search.trim(), 300)
  const [category, setCategory] = useState<ChallengeCategory | null>(null)
  const [challengeType, setChallengeType] = useState<ChallengeType | null>(null)
  const [solveFilter, setSolveFilter] = useState<SolveFilter>('all')

  const { data: catalog, isLoading } = api.game.useGameChallengeCatalog(
    {
      count: ITEMS_PER_PAGE,
      skip: (page - 1) * ITEMS_PER_PAGE,
      search: debouncedSearch || undefined,
      category: category ?? undefined,
      type: challengeType ?? undefined,
      solved: solveFilter === 'all' ? undefined : solveFilter === 'solved',
    },
    { refreshInterval: 60_000 },
    Boolean(user)
  )

  usePageTitle(t('challenge.catalog.title', 'My challenges'))

  if (userError?.status === 401) {
    return <Navigate to="/account/login?from=%2Fchallenges" replace />
  }

  const resetPage = () => setPage(1)
  const clearFilters = () => {
    setSearch('')
    setCategory(null)
    setChallengeType(null)
    setSolveFilter('all')
    setPage(1)
    searchInput.current?.focus()
  }
  const filtersActive = Boolean(search || category || challengeType || solveFilter !== 'all')
  const pageCount = Math.ceil((catalog?.total ?? 0) / ITEMS_PER_PAGE)

  return (
    <WithNavBar withFooter withHeader stickyHeader isLoading={!user && !userError}>
      <PageHeader
        eyebrow={t('challenge.catalog.eyebrow', 'Player workspace')}
        title={t('challenge.catalog.title', 'My challenges')}
        description={t(
          'challenge.catalog.description',
          'Search challenges across events you have joined. Upcoming, hidden, and unauthorized event content stays private.'
        )}
        actions={
          catalog && (
            <Badge size="lg" variant="light">
              {t('challenge.catalog.total', '{{count}} challenges', { count: catalog.total })}
            </Badge>
          )
        }
      />

      <Stack gap="xl" className={classes.catalog}>
        <form
          role="search"
          className={classes.filters}
          data-guide="challenge-filters"
          onSubmit={(event) => event.preventDefault()}
        >
          <TextInput
            ref={searchInput}
            className={classes.search}
            label={t('challenge.catalog.search_label', 'Search challenges')}
            placeholder={t('challenge.catalog.search_placeholder', 'Challenge, event, or exact ID')}
            value={search}
            maxLength={100}
            enterKeyHint="search"
            leftSection={<Icon path={mdiMagnify} size={0.78} aria-hidden="true" />}
            rightSection={
              search ? (
                <ActionIcon
                  type="button"
                  size="sm"
                  variant="subtle"
                  aria-label={t('challenge.catalog.clear_search', 'Clear challenge search')}
                  onClick={() => {
                    setSearch('')
                    resetPage()
                    searchInput.current?.focus()
                  }}
                >
                  <Icon path={mdiClose} size={0.72} aria-hidden="true" />
                </ActionIcon>
              ) : undefined
            }
            rightSectionPointerEvents="all"
            aria-controls="challenge-catalog-results"
            onChange={(event) => {
              setSearch(event.currentTarget.value)
              resetPage()
            }}
          />
          <Select
            className={classes.select}
            label={t('challenge.catalog.category', 'Category')}
            placeholder={t('challenge.catalog.all_categories', 'All categories')}
            clearable
            searchable
            value={category}
            data={ChallengeCategoryList.map((value) => ({
              value,
              label: categoryMap.get(value)?.name ?? value,
            }))}
            onChange={(value) => {
              setCategory(value as ChallengeCategory | null)
              resetPage()
            }}
          />
          <Select
            className={classes.select}
            label={t('challenge.catalog.type', 'Type')}
            placeholder={t('challenge.catalog.all_types', 'All types')}
            clearable
            value={challengeType}
            data={Object.values(ChallengeType).map((value) => ({
              value,
              label: typeMap.get(value)?.name ?? value,
            }))}
            onChange={(value) => {
              setChallengeType(value as ChallengeType | null)
              resetPage()
            }}
          />
          <Stack gap={5} className={classes.solveFilter}>
            <Text component="span" size="sm" fw={500} id="challenge-solve-filter-label">
              {t('challenge.catalog.progress', 'Progress')}
            </Text>
            <SegmentedControl
              size="sm"
              value={solveFilter}
              aria-labelledby="challenge-solve-filter-label"
              data={[
                { value: 'all', label: t('challenge.catalog.progress_all', 'All') },
                { value: 'unsolved', label: t('challenge.catalog.progress_open', 'Open') },
                { value: 'solved', label: t('challenge.catalog.progress_solved', 'Solved') },
              ]}
              onChange={(value) => {
                setSolveFilter(value as SolveFilter)
                resetPage()
              }}
            />
          </Stack>
          {filtersActive && (
            <Button
              variant="subtle"
              color="gray"
              size="sm"
              leftSection={<Icon path={mdiClose} size={0.7} />}
              onClick={clearFilters}
            >
              {t('challenge.catalog.clear_filters', 'Clear')}
            </Button>
          )}
          <VisuallyHidden role="status" aria-live="polite" aria-atomic="true">
            {catalog &&
              t('challenge.catalog.filtered_total', '{{count}} matching challenges', { count: catalog.total })}
          </VisuallyHidden>
        </form>

        <div id="challenge-catalog-results" aria-busy={!catalog || isLoading ? true : undefined}>
          {!catalog ? (
            <SimpleGrid cols={{ base: 1, sm: 2, lg: 3, xl: 4 }} spacing="lg">
              {Array.from({ length: 8 }).map((_, index) => (
                <Skeleton key={index} h="11.5rem" radius="lg" />
              ))}
            </SimpleGrid>
          ) : catalog.data.length === 0 ? (
            <Empty
              description={
                filtersActive
                  ? t('challenge.catalog.no_match', 'No joined-event challenges match these filters.')
                  : t('challenge.catalog.empty', 'Join an event and wait for it to start to see its challenges here.')
              }
            />
          ) : (
            <SimpleGrid cols={{ base: 1, sm: 2, lg: 3, xl: 4 }} spacing="lg">
              {catalog.data.map((challenge) => {
                const category = categoryMap.get(challenge.category)
                const isLiveScoring =
                  challenge.type === ChallengeType.AttackDefense || challenge.type === ChallengeType.KingOfTheHill
                return (
                  <Card component="article" key={`${challenge.gameId}:${challenge.id}`} className={classes.card}>
                    <Link
                      className={classes.cardLink}
                      to={`/games/${challenge.gameId}/challenges${challengeHash(challenge.id, challenge.title)}`}
                      aria-label={t('challenge.catalog.open', 'Open {{challenge}} in {{event}}', {
                        challenge: challenge.title,
                        event: challenge.gameTitle,
                      })}
                    >
                      <Group justify="space-between" align="flex-start" wrap="nowrap" gap="sm">
                        <span
                          className={classes.categoryIcon}
                          data-color={category?.color ?? 'gray'}
                          aria-hidden="true"
                        >
                          <Icon path={category?.icon ?? mdiFlagOutline} size={0.92} />
                        </span>
                        {challenge.solved && (
                          <Badge
                            color="green"
                            variant="light"
                            leftSection={<Icon path={mdiCheckCircleOutline} size={0.58} />}
                          >
                            {t('challenge.catalog.solved', 'Solved')}
                          </Badge>
                        )}
                      </Group>
                      <Stack gap={5} className={classes.copy}>
                        <Title order={2} size="h4" lineClamp={2} title={challenge.title}>
                          {challenge.title}
                        </Title>
                        <Text size="sm" c="dimmed" lineClamp={1} title={challenge.gameTitle}>
                          {challenge.gameTitle}
                        </Text>
                      </Stack>
                      <Group gap={6} className={classes.metadata}>
                        <Badge color={category?.color ?? 'gray'} variant="light">
                          {category?.name ?? challenge.category}
                        </Badge>
                        <Badge color="gray" variant="light">
                          {isLiveScoring
                            ? t('challenge.catalog.live_scoring', 'Live scoring')
                            : t('challenge.catalog.points', '{{count}} pts', { count: challenge.score })}
                        </Badge>
                        <Badge color="gray" variant="outline">
                          {t('challenge.catalog.solves', '{{count}} solves', { count: challenge.solveCount })}
                        </Badge>
                      </Group>
                      <span className={classes.openHint} aria-hidden="true">
                        {t('challenge.catalog.open_short', 'Open')}
                        <Icon path={mdiOpenInNew} size={0.62} />
                      </span>
                    </Link>
                  </Card>
                )
              })}
            </SimpleGrid>
          )}
        </div>

        {pageCount > 1 && (
          <nav aria-label={t('challenge.catalog.pagination', 'Challenge result pages')} className={classes.pagination}>
            <Pagination.Root total={pageCount} siblings={isMobile ? 0 : 2} value={page} onChange={setPage}>
              <Group gap={5} justify="flex-end">
                <Pagination.Previous aria-label={t('common.pagination.previous', 'Previous page')} />
                <Pagination.Items />
                <Pagination.Next aria-label={t('common.pagination.next', 'Next page')} />
              </Group>
            </Pagination.Root>
          </nav>
        )}
      </Stack>
    </WithNavBar>
  )
}

export default ChallengeCatalog
