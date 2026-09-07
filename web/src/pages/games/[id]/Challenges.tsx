import { Flex, Text } from '@mantine/core'
import { useDisclosure } from '@mantine/hooks'
import { mdiArchiveOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { AdGuideModal } from '@Components/AdGuideModal'
import { useAdToken } from '@Components/AdToolkitSections'
import { ChallengePanel } from '@Components/ChallengePanel'
import { GameNoticePanel } from '@Components/GameNoticePanel'
import { KothGuideModal } from '@Components/KothGuideModal'
import { TeamRank } from '@Components/TeamRank'
import { WithGameTab } from '@Components/WithGameTab'
import { GAME_PAGE_CONTENT_WIDTH, WithNavBar } from '@Components/WithNavbar'
import { WithRole } from '@Components/WithRole'
import { CompetitionStatus } from '@Components/competition/CompetitionStatus'
import { isReadOnlyGameArchive } from '@Utils/gameArchive'
import { useAdState, useGameStatus, useGameTeamInfo } from '@Hooks/useGame'
import { ChallengeType, Role } from '@Api'
import classes from '@Styles/GameWorkspace.module.css'

const Challenges: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const { t } = useTranslation()

  const teamState = useGameTeamInfo(numId)
  const { teamInfo, game } = teamState
  const { now: serverNow } = useGameStatus(game)
  const archived = isReadOnlyGameArchive(game, serverNow.valueOf())
  // Three separate flags so the toolkit buttons can be shown / hidden
  // independently — a pure-AD game has no KotH button to confuse anyone,
  // and vice versa. hasAdEngine still gates the shared engine plumbing
  // (round counter, adState polling, VPN config download) since both
  // engines share that.
  const { hasAdChallenges, hasKothChallenges, hasAdEngine } = useMemo(() => {
    if (!teamInfo?.challenges) return { hasAdChallenges: false, hasKothChallenges: false, hasAdEngine: false }
    let a = false,
      k = false
    for (const list of Object.values(teamInfo.challenges)) {
      for (const c of list ?? []) {
        if (c.type === ChallengeType.AttackDefense) a = true
        else if (c.type === ChallengeType.KingOfTheHill) k = true
        if (a && k) break
      }
      if (a && k) break
    }
    return { hasAdChallenges: a, hasKothChallenges: k, hasAdEngine: a || k }
  }, [teamInfo])

  const { adState, error: adStateError, mutate: mutateAdState } = useAdState(numId, hasAdEngine && !archived)
  const adStateOwner = useMemo(
    () => ({ data: adState, error: adStateError, mutate: mutateAdState }),
    [adState, adStateError, mutateAdState]
  )
  const [adGuideOpened, adGuideHandlers] = useDisclosure(false)
  const [kothGuideOpened, kothGuideHandlers] = useDisclosure(false)
  const tokenOwner = useAdToken(numId, hasAdEngine && !archived)

  return (
    <WithNavBar width={GAME_PAGE_CONTENT_WIDTH} competition>
      <WithRole requiredRole={Role.User}>
        <WithGameTab summary={<TeamRank teamState={teamState} compact />}>
          {archived && (
            <aside
              className={classes.archiveNotice}
              aria-label={t('game.content.archive.title', 'Event archive')}
              data-event-archive
            >
              <Icon path={mdiArchiveOutline} size={0.85} aria-hidden="true" />
              <Text size="sm" c="dimmed">
                <Text span inherit fw={600}>
                  {t('game.content.archive.title', 'Event archive')}.{' '}
                </Text>
                {t(
                  'game.content.archive.description',
                  'Challenges and final results remain available for review. Submissions and challenge workloads are closed.'
                )}
              </Text>
            </aside>
          )}
          <Flex direction="column" gap="sm" w="100%">
            {!archived && hasAdEngine && (
              <CompetitionStatus
                state={adState}
                nowMs={serverNow.valueOf()}
                hasAd={hasAdChallenges}
                hasKoth={hasKothChallenges}
                onAdToolkit={adGuideHandlers.open}
                onKothToolkit={kothGuideHandlers.open}
              />
            )}
            <ChallengePanel
              teamState={teamState}
              adStateOwner={hasAdEngine && !archived ? adStateOwner : undefined}
              activity={<GameNoticePanel compact />}
            />
          </Flex>

          {hasAdChallenges && (
            <AdGuideModal
              gameId={numId}
              tokenOwner={tokenOwner}
              opened={adGuideOpened}
              onClose={adGuideHandlers.close}
            />
          )}
          {hasKothChallenges && (
            <KothGuideModal
              gameId={numId}
              tokenOwner={tokenOwner}
              opened={kothGuideOpened}
              onClose={kothGuideHandlers.close}
            />
          )}
        </WithGameTab>
      </WithRole>
    </WithNavBar>
  )
}

export default Challenges
