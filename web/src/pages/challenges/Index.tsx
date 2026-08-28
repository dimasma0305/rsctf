import {
  ActionIcon,
  Alert,
  Badge,
  Button,
  Group,
  Pagination,
  SegmentedControl,
  Select,
  SimpleGrid,
  Skeleton,
  Stack,
  Text,
  TextInput,
  VisuallyHidden,
} from '@mantine/core'
import { useDebouncedValue } from '@mantine/hooks'
import { mdiAlertCircleOutline, mdiClose, mdiMagnify, mdiRefresh } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Navigate } from 'react-router'
import { ChallengeCard } from '@Components/ChallengeCard'
import { Empty } from '@Components/Empty'
import { GameChallengeModal } from '@Components/GameChallengeModal'
import { PageHeader } from '@Components/PageHeader'
import { WithNavBar } from '@Components/WithNavbar'
import { ChallengeCategoryList, SubmissionTypeIconMap, useChallengeCategoryLabelMap } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { OnceSWRConfig } from '@Hooks/useConfig'
import { useGame, useGameStatus } from '@Hooks/useGame'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUser } from '@Hooks/useUser'
import api, { ChallengeCatalogItem, ChallengeCatalogMode, ChallengeCategory, ChallengeInfo, SubmissionType } from '@Api'
import classes from '@Styles/ChallengeCatalog.module.css'

const ITEMS_PER_PAGE = 24
type SolveFilter = 'all' | 'solved' | 'unsolved'

const CHALLENGE_MODES: { value: ChallengeCatalogMode; label: string }[] = [
  { value: 'jeopardy', label: 'Jeopardy' },
  { value: 'koth', label: 'KOTH' },
  { value: 'attackDefense', label: 'A&D' },
]

const challengeHash = (id: number, title: string) => `#${id}-${encodeURIComponent(title.replace(/ /g, '-'))}`

const catalogChallengeInfo = (challenge: ChallengeCatalogItem): ChallengeInfo => ({
  id: challenge.id,
  title: challenge.title,
  category: challenge.category,
  type: challenge.type,
  score: challenge.score,
  solved: challenge.solveCount,
  bloods: [],
  disableBloodBonus: true,
})

interface CatalogChallengeModalProps {
  challenge: ChallengeCatalogItem
  onClose: () => void
  onAccepted: () => Promise<unknown>
}

const CatalogChallengeModal: FC<CatalogChallengeModalProps> = ({ challenge, onClose, onAccepted }) => {
  const { game } = useGame(challenge.gameId)
  const { finished } = useGameStatus({
    start: game?.start ?? challenge.gameStart,
    end: game?.end ?? challenge.gameEnd,
  })
  const categoryMap = useChallengeCategoryLabelMap()
  const eventHref = `/games/${challenge.gameId}/challenges${challengeHash(challenge.id, challenge.title)}`

  return (
    <GameChallengeModal
      gameId={challenge.gameId}
      gameTitle={game?.title ?? challenge.gameTitle}
      opened
      onClose={onClose}
      gameEnded={finished}
      practiceMode={game?.practiceMode}
      eventVpnRequired={game?.vpnAccessRequired}
      eventHref={eventHref}
      status={challenge.solved ? SubmissionType.Normal : SubmissionType.Unaccepted}
      cateData={categoryMap.get(challenge.category)!}
      title={challenge.title}
      score={challenge.score}
      challengeId={challenge.id}
      onAccepted={onAccepted}
    />
  )
}

const ChallengeCatalog: FC = () => {
  const { t } = useTranslation()
  const { user, error: userError } = useUser()
  const categoryMap = useChallengeCategoryLabelMap()
  const isMobile = useIsMobile()
  const searchInput = useRef<HTMLInputElement>(null)
  const [page, setPage] = useState(1)
  const [search, setSearch] = useState('')
  const [debouncedSearch] = useDebouncedValue(search.trim(), 300)
  const [category, setCategory] = useState<ChallengeCategory | null>(null)
  const [challengeMode, setChallengeMode] = useState<ChallengeCatalogMode | null>(null)
  const [solveFilter, setSolveFilter] = useState<SolveFilter>('all')
  const [selectedChallenge, setSelectedChallenge] = useState<ChallengeCatalogItem | null>(null)
  const { iconMap, colorMap } = SubmissionTypeIconMap(0.8)

  const {
    data: catalog,
    error: catalogError,
    isLoading,
    isValidating,
    mutate,
  } = api.game.useGameChallengeCatalog(
    {
      count: ITEMS_PER_PAGE,
      skip: (page - 1) * ITEMS_PER_PAGE,
      search: debouncedSearch || undefined,
      category: category ?? undefined,
      mode: challengeMode ?? undefined,
      solved: solveFilter === 'all' ? undefined : solveFilter === 'solved',
    },
    OnceSWRConfig,
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
    setChallengeMode(null)
    setSolveFilter('all')
    setPage(1)
    searchInput.current?.focus()
  }
  const filtersActive = Boolean(search || category || challengeMode || solveFilter !== 'all')
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
          <Group gap="xs">
            {catalog && (
              <Badge size="lg" variant="light">
                {t('challenge.catalog.total', '{{count}} challenges', { count: catalog.total })}
              </Badge>
            )}
            <Button
              size="compact-sm"
              variant="default"
              loading={isValidating}
              leftSection={<Icon path={mdiRefresh} size={0.7} aria-hidden="true" />}
              onClick={() => void mutate()}
            >
              {t('common.button.refresh', 'Refresh')}
            </Button>
          </Group>
        }
      />

      <Stack gap="xl" className={classes.catalog}>
        {catalogError && (
          <Alert
            color="red"
            role="alert"
            icon={<Icon path={mdiAlertCircleOutline} size={0.9} aria-hidden="true" />}
            title={t('challenge.catalog.load_failed', 'Could not load your challenges')}
          >
            <Group justify="space-between" align="center" wrap="wrap">
              <Text size="sm">
                {t('challenge.catalog.load_failed_hint', 'Check your connection, then retry this request.')}
              </Text>
              <Button size="compact-sm" variant="light" onClick={() => void mutate()}>
                {t('common.button.retry', 'Retry')}
              </Button>
            </Group>
          </Alert>
        )}
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
            value={challengeMode}
            data={CHALLENGE_MODES.map(({ value, label }) => ({
              value,
              label: t(`challenge.catalog.mode_${value}`, label),
            }))}
            onChange={(value) => {
              setChallengeMode(value as ChallengeCatalogMode | null)
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

        <div id="challenge-catalog-results" aria-busy={!catalog || isLoading || isValidating ? true : undefined}>
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
            <div className={classes.challengeGrid}>
              {catalog.data.map((challenge) => {
                return (
                  <ChallengeCard
                    key={`${challenge.gameId}:${challenge.id}`}
                    challenge={catalogChallengeInfo(challenge)}
                    contextLabel={challenge.gameTitle}
                    iconMap={iconMap}
                    colorMap={colorMap}
                    solved={challenge.solved}
                    onClick={() => setSelectedChallenge(challenge)}
                  />
                )
              })}
            </div>
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
      {selectedChallenge && (
        <CatalogChallengeModal
          challenge={selectedChallenge}
          onClose={() => setSelectedChallenge(null)}
          onAccepted={() => mutate()}
        />
      )}
    </WithNavBar>
  )
}

export default ChallengeCatalog
