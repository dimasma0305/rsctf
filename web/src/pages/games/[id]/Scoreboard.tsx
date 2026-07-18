import { Alert, Center, SegmentedControl, Stack } from '@mantine/core'
import { useLocalStorage } from '@mantine/hooks'
import { mdiCrown, mdiFlagOutline, mdiSnowflake, mdiSwordCross } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate, useParams } from 'react-router'
import { AdScoreboardTable } from '@Components/AdScoreboardTable'
import { KothScoreboardTable } from '@Components/KothScoreboardTable'
import { ScoreboardTable } from '@Components/ScoreboardTable'
import { TeamRank } from '@Components/TeamRank'
import { WithGameTab } from '@Components/WithGameTab'
import { WithNavBar } from '@Components/WithNavbar'
import { ScoreTimeLine } from '@Components/charts/ScoreTimeLine'
import { MobileScoreboardTable } from '@Components/mobile/ScoreboardTable'
import { useIsMobile } from '@Utils/ThemeOverride'
import {
  getGameStatus,
  useAdScoreboard,
  useGame,
  useGameScoreboard,
  useGameTeamInfo,
  useKothScoreboard,
} from '@Hooks/useGame'
import classes from '@Styles/GameScoreboard.module.css'

type ScoreboardTab = 'jeopardy' | 'ad' | 'koth'
const ALL_TABS: ScoreboardTab[] = ['jeopardy', 'ad', 'koth']
// Per-game last-tab memory key. Keyed on gameId so switching between games
// doesn't carry the wrong tab over.
const tabStorageKey = (gameId: number) => `scoreboard-tab-${gameId}`

const Scoreboard: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  // These two general-game reads are needed once for tab discovery. The visible
  // A&D/KotH board owns live polling, so do not keep unrelated endpoints hot.
  const { teamInfo, error } = useGameTeamInfo(numId, false)
  const { t } = useTranslation()
  const navigate = useNavigate()
  const location = useLocation()
  // Keep the public catalog loaded even on direct #ad/#koth links. Anonymous
  // visitors cannot rely on the user-gated /Details response for tab discovery.
  const { scoreboard } = useGameScoreboard(numId, false)
  const { game } = useGame(numId)
  const { finished } = getGameStatus(game)

  const [divisionId, setDivisionId] = useState<number | null>(null)
  const isMobile = useIsMobile(1080)

  // Derive presence of each engine to pick the board(s) to show. The three boards
  // are independent: jeopardy uses ScoreboardTable, A&D uses the official epoch
  // AdScoreboardTable, and KotH uses its dedicated /Ad/Koth/Scoreboard board.
  //
  // Detect from the PUBLIC scoreboard's challenge list so anonymous (logged-out)
  // visitors get the correct board — the richer teamInfo (/Details) is
  // [RequireUser]-gated and 401s for the public, which would otherwise collapse an
  // A&D/KotH game to an empty jeopardy table. Prefer teamInfo when present (logged
  // in) for parity, else fall back to the public scoreboard.
  const { hasJeopardyChallenges, hasAdChallenges, hasKothChallenges } = useMemo(() => {
    const fromTeam = Object.values(teamInfo?.challenges ?? {}).flat()
    const fromBoard = Object.values(scoreboard?.challenges ?? {}).flat()
    const all = fromTeam.length > 0 ? fromTeam : fromBoard
    return {
      hasJeopardyChallenges: all.some((c) => c.type !== 'AttackDefense' && c.type !== 'KingOfTheHill'),
      hasAdChallenges: all.some((c) => c.type === 'AttackDefense'),
      hasKothChallenges: all.some((c) => c.type === 'KingOfTheHill'),
    }
  }, [teamInfo, scoreboard])
  const tabsResolved = teamInfo != null || scoreboard != null

  const presentTabs = (hasJeopardyChallenges ? 1 : 0) + (hasAdChallenges ? 1 : 0) + (hasKothChallenges ? 1 : 0)
  const showTabs = presentTabs >= 2
  // Default tab in priority: jeopardy if present, else AD, else KotH.
  const defaultTab: ScoreboardTab = hasJeopardyChallenges ? 'jeopardy' : hasAdChallenges ? 'ad' : 'koth'

  // Hash → tab parser (used on mount AND when the hash changes externally,
  // e.g. user pastes a new URL or clicks a #-link). Aliases accepted so a
  // friendly share-link form like #king-of-the-hill works too.
  const parseHash = (h: string): ScoreboardTab | null => {
    const raw = h.replace(/^#/, '').toLowerCase()
    if (raw === 'koth' || raw === 'king-of-the-hill' || raw === 'kingofthehill') return 'koth'
    if (raw === 'ad' || raw === 'attack-defense' || raw === 'attackdefense') return 'ad'
    if (raw === 'jeopardy' || raw === 'ctf') return 'jeopardy'
    return null
  }

  // Persisted active tab — per-game key so each game remembers independently.
  // A valid URL hash takes precedence so pasted/shared links are deterministic.
  const [storedTab, setStoredTab] = useLocalStorage<ScoreboardTab | null>({
    key: tabStorageKey(numId),
    // Keep an unseen game unset until challenge discovery resolves. Capturing
    // the pre-fetch fallback here would make a fresh mixed game remember KotH
    // before we know that its higher-priority A&D/Jeopardy boards exist.
    defaultValue: null,
    getInitialValueInEffect: false,
  })

  // Coerce to a tab that's actually present (e.g. localStorage said 'koth'
  // but the operator disabled all KotH challenges since last visit).
  const requestedTab = parseHash(location.hash)
  const preferredTab = requestedTab ?? storedTab ?? defaultTab
  const effectiveTab: ScoreboardTab =
    (preferredTab === 'jeopardy' && !hasJeopardyChallenges) ||
    (preferredTab === 'ad' && !hasAdChallenges) ||
    (preferredTab === 'koth' && !hasKothChallenges)
      ? defaultTab
      : preferredTab

  // Single click handler: update BOTH storage and URL in one go. No useEffect
  // round-trip; clicking 'A&D' immediately renders A&D and writes #ad.
  const setActiveTab = (v: string | null) => {
    if (!v || !ALL_TABS.includes(v as ScoreboardTab)) return
    const next = v as ScoreboardTab
    setStoredTab(next)
    if (parseHash(location.hash) !== next) {
      // replace: true so the back button doesn't accumulate a step per click.
      navigate(`${location.pathname}${location.search}#${next}`, { replace: true })
    }
  }

  // Persist external hash choices and canonicalize missing/unavailable hashes.
  // Deriving the visible tab from the hash above avoids a mount-time race where
  // the old localStorage value could overwrite a direct #ad/#koth link.
  useEffect(() => {
    // Challenge discovery is asynchronous. Before it resolves every presence
    // flag is false, so canonicalizing then would incorrectly replace #ad with
    // the fallback #koth on a direct page load.
    if (!tabsResolved) return
    if (storedTab !== effectiveTab) setStoredTab(effectiveTab)
    if (requestedTab !== effectiveTab) {
      navigate(`${location.pathname}${location.search}#${effectiveTab}`, { replace: true })
    }
  }, [effectiveTab, location.pathname, location.search, navigate, requestedTab, setStoredTab, storedTab, tabsResolved])

  // Each live board freezes independently (separate endpoints) — read the
  // freeze state from whichever board we're currently showing.
  // These duplicate the table-level reads intentionally: SWR dedupes each key,
  // while the page needs freeze metadata before rendering its shared banner.
  const { adScoreboard } = useAdScoreboard(numId, hasAdChallenges && effectiveTab === 'ad')
  const { kothScoreboard } = useKothScoreboard(numId, hasKothChallenges && effectiveTab === 'koth')
  const onAdTab = effectiveTab === 'ad' && hasAdChallenges
  const onKothTab = effectiveTab === 'koth' && hasKothChallenges
  const frozenView = onAdTab
    ? adScoreboard?.isFrozenView
    : onKothTab
      ? kothScoreboard?.isFrozenView
      : scoreboard?.isFrozenView
  const frozenAt = onAdTab ? adScoreboard?.freeze : onKothTab ? kothScoreboard?.freeze : scoreboard?.freeze

  // Once an event has ended, the returned board is already the final view. Do
  // not promise a future reveal or format a missing freeze timestamp (KotH can
  // legitimately return isFrozenView=true with freeze=null after closeout).
  const freezeBanner =
    frozenView && !finished && frozenAt ? (
      <Alert color="blue" icon={<Icon path={mdiSnowflake} size={1} />}>
        {t('game.content.frozen_banner', {
          time: frozenAt ? dayjs(frozenAt).format('LLL') : '',
        })}
      </Alert>
    ) : null

  const tabNavbar = showTabs ? (
    <div className={classes.switcherViewport}>
      <SegmentedControl
        className={classes.switcher}
        size="sm"
        value={effectiveTab}
        onChange={(v) => v && setActiveTab(v)}
        aria-label={t('game.content.scoreboard.board_selector', 'Scoreboard type')}
        data={[
          ...(hasJeopardyChallenges
            ? [
                {
                  value: 'jeopardy',
                  label: (
                    <Center style={{ gap: 4 }} aria-label={t('game.content.scoreboard.tab.jeopardy', 'Jeopardy')}>
                      <Icon path={mdiFlagOutline} size={0.8} color="var(--mantine-color-blue-6)" aria-hidden="true" />
                      <span>{t('game.content.scoreboard.tab.jeopardy', 'Jeopardy')}</span>
                    </Center>
                  ),
                },
              ]
            : []),
          ...(hasAdChallenges
            ? [
                {
                  value: 'ad',
                  label: (
                    <Center style={{ gap: 4 }} aria-label={t('game.content.scoreboard.tab.ad', 'Attack & Defense')}>
                      <Icon path={mdiSwordCross} size={0.8} color="var(--mantine-color-red-6)" aria-hidden="true" />
                      <span className={classes.fullBoardLabel}>
                        {t('game.content.scoreboard.tab.ad', 'Attack & Defense')}
                      </span>
                      <span className={classes.shortBoardLabel} aria-hidden="true">
                        {t('game.content.scoreboard.tab.ad_short', 'A&D')}
                      </span>
                    </Center>
                  ),
                },
              ]
            : []),
          ...(hasKothChallenges
            ? [
                {
                  value: 'koth',
                  label: (
                    <Center style={{ gap: 4 }} aria-label={t('game.content.scoreboard.tab.koth', 'King of the Hill')}>
                      <Icon path={mdiCrown} size={0.8} color="var(--mantine-color-violet-6)" aria-hidden="true" />
                      <span className={classes.fullBoardLabel}>
                        {t('game.content.scoreboard.tab.koth', 'King of the Hill')}
                      </span>
                      <span className={classes.shortBoardLabel} aria-hidden="true">
                        {t('game.content.scoreboard.tab.koth_short', 'KotH')}
                      </span>
                    </Center>
                  ),
                },
              ]
            : []),
        ]}
      />
    </div>
  ) : null

  const showJeopardy = effectiveTab === 'jeopardy' && hasJeopardyChallenges
  const showAd = effectiveTab === 'ad' && hasAdChallenges
  const showKoth = effectiveTab === 'koth' && hasKothChallenges

  return (
    <WithNavBar width="90%">
      <WithGameTab>
        {isMobile ? (
          <Stack pt="md">
            {freezeBanner}
            {teamInfo && !error && showJeopardy && <TeamRank />}
            {tabNavbar}
            {showAd ? (
              <AdScoreboardTable numId={numId} />
            ) : showKoth ? (
              <KothScoreboardTable numId={numId} />
            ) : (
              <MobileScoreboardTable divisionId={divisionId} setDivisionId={setDivisionId} />
            )}
          </Stack>
        ) : (
          <Stack pb="2rem">
            {freezeBanner}
            {tabNavbar}
            {showAd ? (
              <AdScoreboardTable numId={numId} />
            ) : showKoth ? (
              <KothScoreboardTable numId={numId} />
            ) : (
              <>
                {showJeopardy && <ScoreTimeLine divisionId={divisionId} />}
                <ScoreboardTable divisionId={divisionId} setDivisionId={setDivisionId} />
              </>
            )}
          </Stack>
        )}
      </WithGameTab>
    </WithNavBar>
  )
}

export default Scoreboard
