import {
  Button,
  Card,
  Center,
  Divider,
  Group,
  ScrollArea,
  SegmentedControl,
  SimpleGrid,
  Skeleton,
  Stack,
  Switch,
  Tabs,
  Text,
  Title,
  Tooltip,
  VisuallyHidden,
} from '@mantine/core'
import { useLocalStorage } from '@mantine/hooks'
import { mdiCrown, mdiFileUploadOutline, mdiFlagOutline, mdiPuzzle, mdiSwordCross } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useEffect, useState, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useParams } from 'react-router'
import useSWR from 'swr'
import { ChallengeCard } from '@Components/ChallengeCard'
import { Empty } from '@Components/Empty'
import { GameChallengeModal } from '@Components/GameChallengeModal'
import { WriteupSubmitModal } from '@Components/WriteupSubmitModal'
import { useChallengeCategoryLabelMap, SubmissionTypeIconMap } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import { useGame, useGameTeamInfo } from '@Hooks/useGame'
import { ChallengeInfo, ChallengeCategory, ChallengeType, SubmissionType } from '@Api'
import classes from '@Styles/ChallengePanel.module.css'

interface RatingSummary {
  challengeId: number
  likes: number
  dislikes: number
}

const ratingSWRFetcher = (url: string) => fetch(url, { credentials: 'include' }).then((r) => (r.ok ? r.json() : []))

export const ChallengePanel: FC = () => {
  const { hash } = useLocation()
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')

  const { teamInfo } = useGameTeamInfo(numId)
  const challenges = teamInfo?.challenges

  const { game } = useGame(numId)
  const isCompact = useIsMobile()

  const { data: ratingsData } = useSWR<RatingSummary[]>(
    numId > 0 ? `/api/game/${numId}/Reviews/Summary` : null,
    ratingSWRFetcher,
    { refreshInterval: 60000, revalidateOnFocus: false }
  )

  const ratingMap = useMemo(() => {
    const map = new Map<number, { likes: number; dislikes: number }>()
    for (const r of ratingsData ?? []) {
      map.set(r.challengeId, { likes: r.likes, dislikes: r.dislikes })
    }
    return map
  }, [ratingsData])

  const categories = Object.keys(challenges ?? {}).sort()
  const [activeTab, setActiveTab] = useState<ChallengeCategory | 'All'>('All')

  // Sync state if activeTab becomes invalid (e.g. after data load updates categories)
  useEffect(() => {
    if (activeTab !== 'All' && !categories.includes(activeTab)) {
      setActiveTab('All')
    }
  }, [categories, activeTab])

  const [hideSolved, setHideSolved] = useLocalStorage({
    key: 'hide-solved',
    defaultValue: false,
    getInitialValueInEffect: false,
  })

  // 4-way filter: All / CTF (jeopardy) / A&D / KotH — distinct buckets so a
  // mixed game can drill into one engine at a time. Visible only when the
  // game actually has more than one kind to switch between.
  const [challengeKind, setChallengeKind] = useLocalStorage<'all' | 'jeopardy' | 'ad' | 'koth'>({
    key: 'challenge-kind-filter',
    defaultValue: 'all',
    getInitialValueInEffect: false,
  })

  const kindOf = (c: ChallengeInfo): 'jeopardy' | 'ad' | 'koth' =>
    c.type === ChallengeType.AttackDefense ? 'ad' : c.type === ChallengeType.KingOfTheHill ? 'koth' : 'jeopardy'

  const matchesKind = (c: ChallengeInfo) => challengeKind === 'all' || kindOf(c) === challengeKind

  const allChallenges = useMemo(() => {
    const all = Object.values(challenges ?? {}).flat()
    // Stable sort by ID first
    return all.sort((a, b) => a.id - b.id)
  }, [challenges])

  // Switcher visibility — show only when there's more than one bucket to choose between.
  const { hasJeopardy, hasAd, hasKoth, kindsPresent } = useMemo(() => {
    let j = false,
      a = false,
      k = false
    for (const c of allChallenges) {
      const kind = kindOf(c)
      if (kind === 'jeopardy') j = true
      else if (kind === 'ad') a = true
      else k = true
      if (j && a && k) break
    }
    return { hasJeopardy: j, hasAd: a, hasKoth: k, kindsPresent: [j, a, k].filter(Boolean).length }
  }, [allChallenges])

  // Coerce out-of-band state if the game doesn't actually have the selected kind
  // (e.g. operator disabled all KotH challenges while the user had it selected).
  useEffect(() => {
    if (challengeKind === 'jeopardy' && !hasJeopardy) setChallengeKind('all')
    if (challengeKind === 'ad' && !hasAd) setChallengeKind('all')
    if (challengeKind === 'koth' && !hasKoth) setChallengeKind('all')
  }, [challengeKind, hasJeopardy, hasAd, hasKoth, setChallengeKind])

  const currentChallenges = useMemo(() => {
    if (!challenges) return []

    // Seeded RNG (Linear Congruential Generator)
    const seed = teamInfo?.rank?.id ?? 0
    const seededRandom = (s: number) => {
      let t = s + 0x6d2b79f5
      t = Math.imul(t ^ (t >>> 15), t | 1)
      t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296
    }

    // Create a deterministic shuffle for this team
    const shuffle = (array: ChallengeInfo[]) => {
      const shuffled = [...array] // Copy to match original array length
      for (let i = shuffled.length - 1; i > 0; i--) {
        // Generate a random index based on seed + current index + challenge ID to vary variance
        // using a combination of teamID and index ensures order is fixed for this team
        const r = seededRandom(seed + i * 997 + shuffled[i].id * 13)
        const j = Math.floor(r * (i + 1))
        const temp = shuffled[i]
        shuffled[i] = shuffled[j]
        shuffled[j] = temp
      }
      return shuffled
    }

    const processList = (list: ChallengeInfo[]) => {
      const filtered = list.filter(
        (chal) =>
          matchesKind(chal) &&
          (!hideSolved || (teamInfo && teamInfo.rank?.solvedChallenges?.find((c) => c.id === chal.id)) === undefined)
      )
      // Ensure base order is stable (by ID) before shuffling
      filtered.sort((a, b) => a.id - b.id)
      return shuffle(filtered)
    }

    if (activeTab !== 'All') {
      return processList(challenges[activeTab] ?? [])
    }

    // Iterate over sorted categories and process each list separately
    const result: ChallengeInfo[] = []
    categories.forEach((cat) => {
      if (challenges[cat]) {
        result.push(...processList(challenges[cat]))
      }
    })
    return result
  }, [challenges, activeTab, allChallenges, hideSolved, teamInfo, categories, challengeKind])

  // When the user is viewing "All" on a mixed game, split the rendered list
  // into kind-segregated sections (Jeopardy / A&D / KotH) with a visual
  // header + divider between them — otherwise a hill tile sandwiched between
  // jeopardy tiles is easy to miss. Single-kind games skip the headers
  // (no value in a "KotH" header when everything is KotH anyway).
  const groupedSections = useMemo(() => {
    const filtered = currentChallenges
    const groupingEnabled = challengeKind === 'all' && kindsPresent >= 2
    if (!groupingEnabled) {
      return [{ kind: null as 'jeopardy' | 'ad' | 'koth' | null, items: filtered }]
    }
    const j: ChallengeInfo[] = []
    const a: ChallengeInfo[] = []
    const k: ChallengeInfo[] = []
    for (const c of filtered) {
      const kind = kindOf(c)
      if (kind === 'jeopardy') j.push(c)
      else if (kind === 'ad') a.push(c)
      else k.push(c)
    }
    return [
      { kind: 'jeopardy' as const, items: j },
      { kind: 'ad' as const, items: a },
      { kind: 'koth' as const, items: k },
    ].filter((s) => s.items.length > 0)
  }, [currentChallenges, challengeKind, kindsPresent])

  const [challenge, setChallenge] = useState<ChallengeInfo | null>(null)
  const [detailOpened, setDetailOpened] = useState(false)
  const { iconMap, colorMap } = SubmissionTypeIconMap(0.8)
  const [writeupSubmitOpened, setWriteupSubmitOpened] = useState(false)
  const challengeCategoryLabelMap = useChallengeCategoryLabelMap()
  const { t } = useTranslation()
  const challengeKindLabels = {
    all: t('game.button.kind.all', { defaultValue: 'All' }),
    jeopardy: t('game.button.kind.jeopardy', { defaultValue: 'CTF' }),
    ad: t('game.button.kind.ad', { defaultValue: 'A&D' }),
    koth: t('game.button.kind.koth', { defaultValue: 'KotH' }),
  }

  const renderKindLabel = (kind: keyof typeof challengeKindLabels, path: string, color?: string) => {
    const label = challengeKindLabels[kind]
    const content = (
      <Center className={classes.kindOption}>
        <Icon path={path} size={isCompact ? 0.72 : 0.7} color={color} aria-hidden="true" />
        {isCompact ? (
          <Text component="span" className={classes.kindOptionText}>
            {label}
          </Text>
        ) : (
          <VisuallyHidden>{label}</VisuallyHidden>
        )}
      </Center>
    )

    return isCompact ? (
      content
    ) : (
      <Tooltip label={label} withArrow openDelay={200}>
        {content}
      </Tooltip>
    )
  }

  useEffect(() => {
    const challId = hash.slice(1).split('-')[0]
    if (challId && allChallenges) {
      const id = parseInt(challId)
      if (isNaN(id) || id < 0) return
      if (challenge?.id === id) return

      const chal = allChallenges.find((c) => c.id === id)
      if (chal) {
        setChallenge(chal)
        setDetailOpened(true)
      }
    }
  }, [hash, challenge, allChallenges])

  // skeleton for loading
  if (!challenges) {
    return (
      <div className={classes.panel}>
        <Stack className={classes.filters}>
          {Array(9)
            .fill(null)
            .map((_v, i) => (
              <Group key={i} wrap="nowrap" p={10}>
                <Skeleton height="1.5rem" width="1.5rem" />
                <Skeleton height="1rem" />
              </Group>
            ))}
        </Stack>
        <SimpleGrid
          p="xs"
          pt={0}
          spacing="sm"
          pos="relative"
          w="100%"
          cols={{ base: 1, xs: 2, lg: 3, w18: 4, w24: 6, w30: 8, w36: 10, w42: 12, w48: 14 }}
        >
          {Array(13)
            .fill(null)
            .map((_v, i) => (
              <Card key={i} shadow="sm">
                <Stack gap="sm" pos="relative" style={{ zIndex: 99 }}>
                  <Skeleton height="1.5rem" width="70%" mt={4} />
                  <Divider />
                  <Group wrap="nowrap" justify="space-between" align="start">
                    <Center>
                      <Skeleton height="1.5rem" width="5rem" />
                    </Center>
                    <Stack gap="xs">
                      <Skeleton height="1rem" width="6rem" mt={5} />
                      <Group justify="center" gap="md" h={20}>
                        <Skeleton height="1.2rem" width="1.2rem" />
                        <Skeleton height="1.2rem" width="1.2rem" />
                        <Skeleton height="1.2rem" width="1.2rem" />
                      </Group>
                    </Stack>
                  </Group>
                </Stack>
              </Card>
            ))}
        </SimpleGrid>
      </div>
    )
  }

  if (allChallenges.length === 0) {
    return (
      <Center h="calc(100vh - 100px)" w="100%">
        <Empty
          bordered
          description={t('game.content.no_challenge')}
          fontSize="xl"
          mdiPath={mdiFlagOutline}
          iconSize={2.6}
        />
      </Center>
    )
  }

  return (
    <>
      <div className={classes.panel}>
        <Stack className={classes.filters}>
          {game?.writeupRequired && (
            <>
              <Button
                px="xs"
                justify="space-between"
                leftSection={<Icon path={mdiFileUploadOutline} size={1} />}
                onClick={() => setWriteupSubmitOpened(true)}
              >
                {t('game.button.submit_writeup')}
              </Button>
              <Divider />
            </>
          )}
          {kindsPresent >= 2 && (
            <Stack gap={6} className={classes.kindFilterGroup}>
              <Text component="span" className={classes.mobileFilterLabel}>
                {t('game.label.challenge_type', { defaultValue: 'Challenge type' })}
              </Text>
              {/* The desktop sidebar stays icon-only to fit its compact rail. On
                  touch layouts, the same options expose their labels directly. */}
              <SegmentedControl
                size={isCompact ? 'sm' : 'xs'}
                w="100%"
                aria-label={t('game.label.challenge_kind', { defaultValue: 'Filter challenges by type' })}
                value={challengeKind}
                onChange={(v) => setChallengeKind(v as 'all' | 'jeopardy' | 'ad' | 'koth')}
                classNames={{
                  root: classes.kindControlRoot,
                  control: classes.kindControl,
                  label: classes.kindControlLabel,
                }}
                data={[
                  { value: 'all', label: renderKindLabel('all', mdiPuzzle) },
                  ...(hasJeopardy
                    ? [
                        {
                          value: 'jeopardy',
                          label: renderKindLabel('jeopardy', mdiFlagOutline, 'var(--mantine-color-blue-6)'),
                        },
                      ]
                    : []),
                  ...(hasAd
                    ? [
                        {
                          value: 'ad',
                          label: renderKindLabel('ad', mdiSwordCross, 'var(--mantine-color-red-6)'),
                        },
                      ]
                    : []),
                  ...(hasKoth
                    ? [
                        {
                          value: 'koth',
                          label: renderKindLabel('koth', mdiCrown, 'var(--mantine-color-violet-6)'),
                        },
                      ]
                    : []),
                ]}
              />
            </Stack>
          )}
          <Switch
            w="100%"
            checked={hideSolved}
            onChange={(e) => setHideSolved(e.target.checked)}
            classNames={{ body: classes.switch }}
            label={
              <Text fz="md" fw="bold" ta="right">
                {t('game.button.hide_solved')}
              </Text>
            }
          />
          <Text component="span" className={classes.mobileFilterLabel}>
            {t('game.label.challenge_category', { defaultValue: 'Category' })}
          </Text>
          <Tabs
            orientation={isCompact ? 'horizontal' : 'vertical'}
            variant="pills"
            value={activeTab}
            onChange={(value) => setActiveTab(value as ChallengeCategory)}
            classNames={{
              root: classes.tabRoot,
              list: classes.tabList,
              tabLabel: classes.tabLabel,
              tab: classes.tab,
            }}
          >
            <Tabs.List aria-label={t('game.label.challenge_category', { defaultValue: 'Filter by category' })}>
              <Tabs.Tab value={'All'} leftSection={<Icon path={mdiPuzzle} size={1} />}>
                <Group justify="space-between" wrap="nowrap" gap={2}>
                  <Text fz="sm" fw="bold">
                    {challengeKindLabels.all}
                  </Text>
                  <Text fz="sm" fw="bold">
                    {allChallenges.length}
                  </Text>
                </Group>
              </Tabs.Tab>
              {categories.map((tab) => {
                const data = challengeCategoryLabelMap.get(tab as ChallengeCategory)!
                return (
                  <Tabs.Tab key={tab} value={tab} leftSection={<Icon path={data?.icon} size={1} />} color={data?.color}>
                    <Group justify="space-between" wrap="nowrap" gap={2}>
                      <Text fz="sm" fw="bold">
                        {data?.name}
                      </Text>
                      <Text fz="sm" fw="bold">
                        {challenges && challenges[tab].length}
                      </Text>
                    </Group>
                  </Tabs.Tab>
                )
              })}
            </Tabs.List>
          </Tabs>
        </Stack>
        <ScrollArea
          h={isCompact ? undefined : 'calc(100vh - 6.67rem)'}
          pos="relative"
          offsetScrollbars
          scrollbarSize={4}
          classNames={{ root: classes.scrollArea }}
        >
          {/* if rank is 0, and have no division, means scoreboard not ready yet */}
          {!teamInfo.rank?.divisionId && !teamInfo?.rank?.rank ? (
            <Center h="calc(100vh - 10rem)">
              <Stack gap={0}>
                <Title order={2}>{t('game.content.scoreboard_not_ready.title')}</Title>
                <Text>{t('game.content.scoreboard_not_ready.comment')}</Text>
              </Stack>
            </Center>
          ) : currentChallenges && currentChallenges.length ? (
            <Stack gap="sm" p="xs" pt={0}>
              {groupedSections.map((section, idx) => {
                const sectionHeader = section.kind ? (
                  <Group gap="xs" align="center" wrap="nowrap" mt={idx === 0 ? 0 : 'sm'}>
                    <Icon
                      path={
                        section.kind === 'jeopardy' ? mdiFlagOutline : section.kind === 'ad' ? mdiSwordCross : mdiCrown
                      }
                      size={0.8}
                      color={
                        section.kind === 'jeopardy'
                          ? 'var(--mantine-color-blue-6)'
                          : section.kind === 'ad'
                            ? 'var(--mantine-color-red-6)'
                            : 'var(--mantine-color-violet-6)'
                      }
                    />
                    <Title
                      order={5}
                      c={section.kind === 'jeopardy' ? 'blue' : section.kind === 'ad' ? 'red' : 'violet'}
                    >
                      {section.kind === 'jeopardy'
                        ? t('game.content.section.jeopardy', 'Jeopardy challenges')
                        : section.kind === 'ad'
                          ? t('game.content.section.ad', 'Attack & Defense')
                          : t('game.content.section.koth', 'King of the Hill')}
                    </Title>
                    <Text size="xs" c="dimmed">
                      ({section.items.length})
                    </Text>
                    <Divider
                      flex={1}
                      ml="xs"
                      color={section.kind === 'jeopardy' ? 'blue' : section.kind === 'ad' ? 'red' : 'violet'}
                      opacity={0.4}
                    />
                  </Group>
                ) : null
                return (
                  <Stack key={section.kind ?? 'all'} gap="xs">
                    {sectionHeader}
                    <SimpleGrid
                      w="100%"
                      spacing="sm"
                      cols={{ base: 1, xs: 2, lg: 3, w18: 4, w24: 6, w30: 8, w36: 10, w42: 12, w48: 14 }}
                    >
                      {section.items.map((chal) => {
                        const status = teamInfo?.rank?.solvedChallenges?.find((c) => c.id === chal.id)?.type
                        const solved = status !== SubmissionType.Unaccepted && status !== undefined

                        return (
                          <ChallengeCard
                            key={chal.id}
                            challenge={chal}
                            iconMap={iconMap}
                            colorMap={colorMap}
                            onClick={() => {
                              setChallenge(chal)
                              setDetailOpened(true)
                              // update hash after modal opened, so don't trigger useEffect
                              window.location.hash = `#${chal.id}-${encodeURIComponent(chal.title?.replace(/ /g, '-') ?? '')}`
                            }}
                            solved={solved}
                            teamId={teamInfo?.rank?.id}
                            rating={solved || dayjs(game?.end) < dayjs() ? ratingMap.get(chal.id) : undefined}
                          />
                        )
                      })}
                    </SimpleGrid>
                  </Stack>
                )
              })}
            </Stack>
          ) : (
            <Center h="calc(100vh - 10rem)">
              <Stack gap={0}>
                <Title order={2}>{t('game.content.all_solved.title')}</Title>
                <Text>{t('game.content.all_solved.comment')}</Text>
              </Stack>
            </Center>
          )}
        </ScrollArea>
      </div>
      {game?.writeupRequired && (
        <WriteupSubmitModal
          opened={writeupSubmitOpened}
          onClose={() => setWriteupSubmitOpened(false)}
          withCloseButton
          size="min(32rem, calc(100vw - 1.5rem))"
          gameId={numId}
          writeupDeadline={teamInfo.writeupDeadline}
        />
      )}
      {challenge?.id && (
        <GameChallengeModal
          gameId={numId}
          gameTitle={game?.title ?? ''}
          opened={detailOpened}
          withCloseButton
          onClose={() => {
            window.location.hash = ''
            setDetailOpened(false)
          }}
          gameEnded={dayjs(game?.end) < dayjs()}
          practiceMode={game?.practiceMode}
          status={teamInfo?.rank?.solvedChallenges?.find((c) => c.id === challenge?.id)?.type}
          cateData={
            challengeCategoryLabelMap.get((challenge?.category as ChallengeCategory) ?? ChallengeCategory.Misc)!
          }
          title={challenge?.title ?? ''}
          score={challenge?.score ?? 0}
          challengeId={challenge.id}
        />
      )}
    </>
  )
}
