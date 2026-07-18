import { Badge, Button, Flex, Group, Paper, Stack, Text } from '@mantine/core'
import { useDisclosure } from '@mantine/hooks'
import { mdiCrown, mdiOpenInNew, mdiSword, mdiSwordCross, mdiToolboxOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { AdGuideModal } from '@Components/AdGuideModal'
import { ChallengePanel } from '@Components/ChallengePanel'
import { GameNoticePanel } from '@Components/GameNoticePanel'
import { KothGuideModal } from '@Components/KothGuideModal'
import { TeamRank } from '@Components/TeamRank'
import { WithGameTab } from '@Components/WithGameTab'
import { WithNavBar } from '@Components/WithNavbar'
import { WithRole } from '@Components/WithRole'
import { useIsMobile } from '@Utils/ThemeOverride'
import { epochProgress } from '@Utils/epochProgress'
import { useAdState, useGameTeamInfo } from '@Hooks/useGame'
import { useTicker } from '@Hooks/useTicker'
import { ChallengeType, Role } from '@Api'

const Challenges: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const { t } = useTranslation()
  const isCompact = useIsMobile(1200)

  const { teamInfo } = useGameTeamInfo(numId)
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

  const { adState } = useAdState(numId, hasAdEngine)
  const [adGuideOpened, adGuideHandlers] = useDisclosure(false)
  const [kothGuideOpened, kothGuideHandlers] = useDisclosure(false)

  // useTicker fires every 1s so the countdown actually counts down between
  // SWR refreshes (which only happen every 10s). Without this the value is
  // frozen until the next adState refetch.
  const now = useTicker()
  const roundEndsIn = adState?.roundEndsAt ? Math.max(0, dayjs(adState.roundEndsAt).diff(now, 'second')) : null
  const currentEpochProgress = adState
    ? epochProgress(adState.currentRound, adState.startRound, adState.epochTicks)
    : null
  return (
    <WithNavBar width="90%">
      <WithRole requiredRole={Role.User}>
        <WithGameTab>
          <Flex direction={isCompact ? 'column' : 'row'} gap="sm" justify="space-between" align="flex-start" w="100%">
            <ChallengePanel />
            <Stack gap="sm" w={isCompact ? '100%' : '22rem'} miw={isCompact ? 0 : '22rem'}>
              <Button
                component="a"
                href={`/games/${numId}/attack`}
                target="_blank"
                rel="noreferrer"
                variant="light"
                fullWidth
                leftSection={<Icon path={mdiSword} size={1} />}
                rightSection={<Icon path={mdiOpenInNew} size={0.8} />}
              >
                {t('game.button.attack')}
              </Button>
              {hasAdEngine && (
                <Paper p="sm" withBorder>
                  <Group justify="space-between" wrap="nowrap" align="center">
                    <Stack gap={0}>
                      <Text size="xs" c="dimmed" tt="uppercase" fw={600}>
                        {t('game.content.ad.round', 'Round')}
                      </Text>
                      <Text fw="bold" size="lg">
                        {adState?.currentRound ?? '—'}
                      </Text>
                    </Stack>
                    <Stack gap={0} align="flex-end">
                      <Text size="xs" c="dimmed" tt="uppercase" fw={600}>
                        {t('game.content.ad.round_ends', 'Round ends')}
                      </Text>
                      <Text fw="bold" size="lg">
                        {roundEndsIn === null
                          ? t('game.content.ad.no_round_yet', 'No round yet — warmup')
                          : `${roundEndsIn}s`}
                      </Text>
                    </Stack>
                  </Group>
                  {hasAdChallenges && currentEpochProgress && (
                    <Group gap={6} mt="xs" wrap="wrap">
                      <Badge color="violet" variant="light" size="sm">
                        {t('game.content.ad.epoch_number', {
                          epoch: currentEpochProgress.epoch,
                          defaultValue: 'Epoch {{epoch}}',
                        })}
                      </Badge>
                      <Badge color="blue" variant="light" size="sm">
                        {t('game.content.ad.epoch_tick', {
                          tick: currentEpochProgress.tick,
                          total: currentEpochProgress.totalTicks,
                          defaultValue: 'Tick {{tick}}/{{total}}',
                        })}
                      </Badge>
                    </Group>
                  )}
                  {adState?.currentRound === 0 && (
                    <Badge color="blue" variant="light" size="sm" mt="xs" w="100%">
                      {t('game.content.ad.warmup_pill', 'Warmup — scoring not yet active')}
                    </Badge>
                  )}
                  {hasAdChallenges && adState && adState.currentRound > 0 && !adState.flagsReady && (
                    <Badge color="yellow" variant="light" size="sm" mt="xs" w="100%">
                      {t('game.content.ad.flags_syncing.label', 'Flags syncing — wait before attacking')}
                    </Badge>
                  )}
                  {hasAdChallenges && adState && adState.flagDeliveryFailures > 0 && (
                    <Badge color="orange" variant="light" size="sm" mt="xs" w="100%">
                      {t('game.content.ad.flag_delivery_failed.label', {
                        count: adState.flagDeliveryFailures,
                        defaultValue: '{{count}} flag deliveries need attention',
                      })}
                    </Badge>
                  )}
                </Paper>
              )}
              {hasAdChallenges && (
                <Button
                  variant="default"
                  fullWidth
                  leftSection={<Icon path={mdiToolboxOutline} size={1} />}
                  rightSection={<Icon path={mdiSwordCross} size={0.8} color="var(--mantine-color-red-6)" />}
                  onClick={adGuideHandlers.open}
                >
                  {t('game.button.ad.open_toolkit', 'A&D Toolkit')}
                </Button>
              )}
              {hasKothChallenges && (
                <Button
                  variant="default"
                  fullWidth
                  leftSection={<Icon path={mdiToolboxOutline} size={1} />}
                  rightSection={<Icon path={mdiCrown} size={0.8} color="var(--mantine-color-violet-6)" />}
                  onClick={kothGuideHandlers.open}
                >
                  {t('game.button.koth.open_toolkit', 'KotH Toolkit')}
                </Button>
              )}
              <TeamRank />
              <GameNoticePanel />
            </Stack>
          </Flex>

          {hasAdChallenges && <AdGuideModal gameId={numId} opened={adGuideOpened} onClose={adGuideHandlers.close} />}
          {hasKothChallenges && (
            <KothGuideModal gameId={numId} opened={kothGuideOpened} onClose={kothGuideHandlers.close} />
          )}
        </WithGameTab>
      </WithRole>
    </WithNavBar>
  )
}

export default Challenges
