import {
  Accordion,
  Alert,
  Avatar,
  Badge,
  Box,
  Button,
  Center,
  Group,
  Input,
  Loader,
  Paper,
  ScrollArea,
  Stack,
  Switch,
  Table,
  Text,
  Title,
  useMantineTheme,
  VisuallyHidden,
} from '@mantine/core'
import { useLocalStorage } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiAlertCircle, mdiCheck, mdiKeyAlert, mdiRefresh, mdiTarget } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { ScrollingText } from '@Components/ScrollingText'
import { RequireRole } from '@Components/WithRole'
import { ParticipationStatusControl } from '@Components/admin/ParticipationStatusControl'
import { SwitchLabel } from '@Components/admin/SwitchLabel'
import { useLanguage } from '@Utils/I18n'
import { showErrorMsg, tryGetErrorMsg } from '@Utils/Shared'
import { useParticipationStatusMap } from '@Utils/Shared'
import { useDisplayInputStyles } from '@Utils/ThemeOverride'
import { useUserRole } from '@Hooks/useUser'
import api, { CheatInfoModel, ParticipationEditModel, ParticipationStatus, Role } from '@Api'
import classes from '@Styles/Accordion.module.css'
import misc from '@Styles/Misc.module.css'

enum CheatType {
  Submit = 'Submit',
  Owned = 'Owned',
}

const CheatTypeMap = new Map([
  [
    CheatType.Submit,
    {
      color: 'orange',
      iconPath: mdiTarget,
    },
  ],
  [
    CheatType.Owned,
    {
      color: 'red',
      iconPath: mdiKeyAlert,
    },
  ],
])

interface CheatSubmissionInfo {
  key: string
  time?: dayjs.Dayjs
  answer?: string
  user?: string
  challenge?: string
  relatedTeam?: string
  cheatType: CheatType
}

interface CheatTeamInfo {
  name?: string
  avatar?: string | null
  teamId?: number
  status?: ParticipationStatus
  lastSubmitTime?: dayjs.Dayjs
  participateId?: number
  division?: string | null
  divisionId?: number | null
  submissionInfo: Set<CheatSubmissionInfo>
}

interface KeyedCheatInfo {
  info: CheatInfoModel
  key: string
}

/**
 * The API does not expose a submission ID, so build a deterministic identity from
 * every wire field that identifies an incident. The occurrence suffix keeps exact
 * duplicate records collision-free without tying unrelated rows to their sort index.
 */
const ToKeyedCheatInfo = (cheatInfo: CheatInfoModel[]): KeyedCheatInfo[] => {
  const occurrences = new Map<string, number>()

  return cheatInfo.map((info) => {
    const signature = JSON.stringify([
      info.submission?.time ?? null,
      info.submission?.answer ?? null,
      info.submission?.status ?? null,
      info.submission?.user ?? null,
      info.submission?.team ?? null,
      info.submission?.challenge ?? null,
      info.ownedTeam?.id ?? null,
      info.submitTeam?.id ?? null,
    ])
    const occurrence = occurrences.get(signature) ?? 0
    occurrences.set(signature, occurrence + 1)

    return { info, key: JSON.stringify([signature, occurrence]) }
  })
}

const ToCheatTeamInfo = (cheatInfo: CheatInfoModel[]) => {
  const cheatTeamInfo = new Map<number, CheatTeamInfo>()
  for (const { info, key } of ToKeyedCheatInfo(cheatInfo)) {
    const { ownedTeam, submitTeam, submission } = info
    if (!ownedTeam || !submitTeam || !submission) continue

    const time = submission.time === undefined ? undefined : dayjs(submission.time)

    for (const part of [ownedTeam, submitTeam]) {
      if (!cheatTeamInfo.has(part.id ?? -1)) {
        cheatTeamInfo.set(part.id ?? -1, {
          name: part.team?.name,
          avatar: part.team?.avatar,
          teamId: part.team?.id,
          status: part.status,
          participateId: part.id,
          divisionId: part.divisionId,
          division: part.division,
          lastSubmitTime: time,
          submissionInfo: new Set<CheatSubmissionInfo>(),
        })
      }
    }

    const ownedTeamInfo = cheatTeamInfo.get(ownedTeam.id ?? -1)
    const submitTeamInfo = cheatTeamInfo.get(submitTeam.id ?? -1)

    if (!ownedTeamInfo || !submitTeamInfo) continue

    if (ownedTeamInfo.lastSubmitTime?.isBefore(time)) {
      ownedTeamInfo.lastSubmitTime = time
    }

    const cheatSubmissionInfo: CheatSubmissionInfo = {
      key: JSON.stringify([key, CheatType.Owned]),
      time: time,
      answer: submission.answer,
      user: submission.user,
      challenge: submission.challenge,
      cheatType: CheatType.Owned,
      relatedTeam: submitTeam.team?.name,
    }

    ownedTeamInfo.submissionInfo.add(cheatSubmissionInfo)

    if (submitTeamInfo.lastSubmitTime?.isBefore(time)) {
      submitTeamInfo.lastSubmitTime = time
    }

    const cheatSubmissionSourceInfo: CheatSubmissionInfo = {
      ...cheatSubmissionInfo,
      key: JSON.stringify([key, CheatType.Submit]),
      cheatType: CheatType.Submit,
      relatedTeam: ownedTeam.team?.name,
    }

    submitTeamInfo.submissionInfo.add(cheatSubmissionSourceInfo)
  }
  return cheatTeamInfo
}

interface CheatSubmissionInfoProps {
  submissionInfo: CheatSubmissionInfo
}

const CheatSubmissionInfo: FC<CheatSubmissionInfoProps> = (props) => {
  const { submissionInfo } = props
  const theme = useMantineTheme()
  const type = CheatTypeMap.get(submissionInfo.cheatType)!
  const { classes } = useDisplayInputStyles({ ff: 'monospace' })
  const { locale } = useLanguage()
  const { t } = useTranslation()

  return (
    <Group justify="space-between" w="100%" gap={0}>
      <Group justify="space-between" w="60%" pr="2rem">
        <Group justify="left">
          <Box
            component="span"
            role="img"
            aria-label={
              submissionInfo.cheatType === CheatType.Owned
                ? t('game.label.cheat_info.owned_flag', 'Flag owner')
                : t('game.label.cheat_info.submitted_flag', 'Submitted flag')
            }
          >
            <Icon path={type.iconPath} size={1} color={theme.colors[type.color][6]} aria-hidden />
          </Box>
          <Badge size="sm" color="indigo">
            {submissionInfo.time ? submissionInfo.time.locale(locale).format('SL HH:mm:ss') : '—'}
          </Badge>
          <ScrollingText text={submissionInfo.relatedTeam ?? ''} fw="bold" maw={150} />
        </Group>
        <ScrollingText text={submissionInfo.user ?? ''} size="sm" fw="bold" maw={120} />
      </Group>
      <Stack gap={0} w="40%">
        <Text fw="bold" size="xs" lineClamp={1}>
          {submissionInfo.challenge}
        </Text>
        <Input
          variant="unstyled"
          value={submissionInfo.answer}
          readOnly
          size="xs"
          classNames={classes}
          aria-label={t('game.label.cheat_info.answer_for_challenge', 'Submitted answer for {{challenge}}', {
            challenge: submissionInfo.challenge ?? t('common.label.challenge', 'challenge'),
          })}
        />
      </Stack>
    </Group>
  )
}

interface CheatInfoItemProps {
  userRole: Role
  disabled: boolean
  cheatTeamInfo: CheatTeamInfo
  setParticipation: (id: number, model: ParticipationEditModel) => Promise<void>
}

const CheatInfoItem: FC<CheatInfoItemProps> = (props) => {
  const { cheatTeamInfo, disabled, userRole, setParticipation } = props
  const theme = useMantineTheme()
  const part = useParticipationStatusMap().get(cheatTeamInfo.status!)!

  const { t } = useTranslation()
  const { locale } = useLanguage()

  return (
    <Accordion.Item value={cheatTeamInfo.participateId!.toString()}>
      <Box display="flex" className={misc.alignCenter} style={{ minWidth: 0 }}>
        <Accordion.Control style={{ flex: 1, minWidth: 0 }}>
          <Group justify="space-between" wrap="wrap">
            <Group justify="left">
              <Avatar
                imageProps={{ loading: 'lazy' }}
                alt={`${cheatTeamInfo.name ?? 'Team'} avatar`}
                src={cheatTeamInfo.avatar}
              >
                {!cheatTeamInfo.name ? 'T' : cheatTeamInfo.name.slice(0, 1)}
              </Avatar>
              <Stack gap={0}>
                <Group gap={4}>
                  <Title order={4} lineClamp={1} fw="bold">
                    {!cheatTeamInfo.name ? t('admin.placeholder.games.participation.team') : cheatTeamInfo.name}
                  </Title>
                  {cheatTeamInfo?.division && (
                    <Badge size="sm" variant="outline">
                      {cheatTeamInfo.division}
                    </Badge>
                  )}
                </Group>
                <Text size="sm" lineClamp={1}>
                  {cheatTeamInfo.lastSubmitTime ? cheatTeamInfo.lastSubmitTime.locale(locale).format('SL LTS') : '—'}
                </Text>
              </Stack>
            </Group>
            <Group gap={0} justify="space-between" wrap="nowrap">
              <Box w="6rem" ta="center">
                <Badge color={part.color}>{part.title}</Badge>
              </Box>
            </Group>
          </Group>
        </Accordion.Control>
        {RequireRole(Role.Admin, userRole) && (
          <ParticipationStatusControl
            disabled={disabled}
            participation={{
              id: cheatTeamInfo.participateId!,
              divisionId: cheatTeamInfo.divisionId!,
              status: cheatTeamInfo.status!,
            }}
            setParticipation={setParticipation}
            mx="xs"
            miw={theme.spacing.xl}
          />
        )}
      </Box>
      <Accordion.Panel>
        <Stack gap="sm">
          {[...cheatTeamInfo.submissionInfo]
            .sort((a, b) => (b.time?.valueOf() ?? 0) - (a.time?.valueOf() ?? 0))
            .map((submissionInfo) => (
              <CheatSubmissionInfo key={submissionInfo.key} submissionInfo={submissionInfo} />
            ))}
        </Stack>
      </Accordion.Panel>
    </Accordion.Item>
  )
}

interface CheatSubmissionEmptyStateProps {
  height: string
}

const CheatSubmissionEmptyState: FC<CheatSubmissionEmptyStateProps> = ({ height }) => {
  const { t } = useTranslation()

  return (
    <Center h={height}>
      <Stack gap={0} align="center" role="status" aria-live="polite">
        <Title order={3} ta="center">
          {t('game.content.cheat.submissions_empty_title', 'No suspicious flag submissions yet')}
        </Title>
        <Text c="dimmed" ta="center">
          {t(
            'game.content.cheat.submissions_empty_description',
            'New flag-sharing incidents will appear here automatically.'
          )}
        </Text>
      </Stack>
    </Center>
  )
}

interface CheatInfoTeamViewProps {
  disabled: boolean
  cheatTeamInfo: Map<number, CheatTeamInfo>
  setParticipation: (id: number, model: ParticipationEditModel) => Promise<void>
}

const CheatInfoTeamView: FC<CheatInfoTeamViewProps> = (props) => {
  const { role } = useUserRole()
  const { cheatTeamInfo, disabled, setParticipation } = props

  const { t } = useTranslation()

  return (
    <ScrollArea
      offsetScrollbars
      h="calc(100vh - 180px)"
      viewportProps={{
        role: 'region',
        tabIndex: 0,
        'aria-label': t('game.label.cheat_info.team_view_region', 'Suspicious submissions grouped by team'),
      }}
    >
      <Stack gap="xs" w="100%">
        {cheatTeamInfo.size === 0 ? (
          <CheatSubmissionEmptyState height="calc(100vh - 200px)" />
        ) : (
          <Accordion multiple variant="contained" chevronPosition="left" classNames={classes} className={classes.root}>
            {[...cheatTeamInfo.values()]
              .sort((a, b) => (b.lastSubmitTime?.unix() ?? 0) - (a.lastSubmitTime?.unix() ?? 0))
              .map((cheatInfo) => (
                <CheatInfoItem
                  key={cheatInfo.participateId}
                  userRole={role ?? Role.User}
                  cheatTeamInfo={cheatInfo}
                  disabled={disabled}
                  setParticipation={setParticipation}
                />
              ))}
          </Accordion>
        )}
      </Stack>
    </ScrollArea>
  )
}

interface CheatInfoTableViewProps {
  cheatInfo: CheatInfoModel[]
}

const CheatInfoTableView: FC<CheatInfoTableViewProps> = (props) => {
  const { classes: inputClasses } = useDisplayInputStyles({ ff: 'monospace' })
  const { t } = useTranslation()
  const { locale } = useLanguage()

  const rows = ToKeyedCheatInfo(props.cheatInfo)
    .sort((a, b) => (b.info.submission?.time ?? 0) - (a.info.submission?.time ?? 0))
    .map(({ info: item, key }) => (
      <Table.Tr key={key}>
        <Table.Td ff="monospace">
          <Badge size="sm" color="indigo">
            {dayjs(item.submission?.time).locale(locale).format('SL HH:mm:ss')}
          </Badge>
        </Table.Td>
        <Table.Td>
          <ScrollingText text={item.ownedTeam?.team?.name ?? 'Team'} size="sm" fw="bold" maw={150} />
        </Table.Td>
        <Table.Td>
          <Badge
            size="sm"
            color="orange"
            aria-label={t(
              'game.label.cheat_info.submission_direction',
              'Flag submitted from owner team to submitting team'
            )}
          >
            <span aria-hidden>{`>>>`}</span>
          </Badge>
        </Table.Td>
        <Table.Td>
          <ScrollingText text={item.submitTeam?.team?.name ?? 'Team'} size="sm" fw="bold" maw={150} />
        </Table.Td>
        <Table.Td>
          <ScrollingText text={item.submission?.user ?? 'User'} ff="monospace" size="sm" fw="bold" maw={120} />
        </Table.Td>
        <Table.Td>{item.submission?.challenge ?? 'Challenge'}</Table.Td>
        <Table.Td p="0" w="24vw">
          <Input
            variant="unstyled"
            value={item.submission?.answer}
            readOnly
            size="sm"
            classNames={inputClasses}
            aria-label={t('game.label.cheat_info.answer_for_challenge', 'Submitted answer for {{challenge}}', {
              challenge: item.submission?.challenge ?? t('common.label.challenge', 'challenge'),
            })}
          />
        </Table.Td>
      </Table.Tr>
    ))

  return (
    <Paper shadow="md" p="md">
      <ScrollArea
        offsetScrollbars
        h="calc(100vh - 200px)"
        viewportProps={{
          role: 'region',
          tabIndex: 0,
          'aria-label': t('game.label.cheat_info.submission_log_caption', 'Suspicious flag submission log'),
        }}
      >
        <Table className={classes.table}>
          <Table.Caption>
            <VisuallyHidden>
              {t('game.label.cheat_info.submission_log_caption', 'Suspicious flag submission log')}
            </VisuallyHidden>
          </Table.Caption>
          <Table.Thead>
            <Table.Tr>
              <Table.Th scope="col" w="8rem">
                {t('common.label.time')}
              </Table.Th>
              <Table.Th scope="col" miw="5rem">
                {t('game.label.cheat_info.owned_team')}
              </Table.Th>
              <Table.Th scope="col">
                <VisuallyHidden>{t('game.label.cheat_info.direction', 'Submission direction')}</VisuallyHidden>
              </Table.Th>
              <Table.Th scope="col" miw="5rem">
                {t('game.label.cheat_info.submit_team')}
              </Table.Th>
              <Table.Th scope="col" miw="5rem">
                {t('game.label.cheat_info.submit_user')}
              </Table.Th>
              <Table.Th scope="col" miw="3rem">
                {t('common.label.challenge')}
              </Table.Th>
              <Table.Th scope="col" className={classes.mono}>
                {t('common.label.flag')}
              </Table.Th>
            </Table.Tr>
          </Table.Thead>
          <Table.Tbody>
            {rows.length === 0 ? (
              <Table.Tr>
                <Table.Td colSpan={7}>
                  <CheatSubmissionEmptyState height="calc(100vh - 300px)" />
                </Table.Td>
              </Table.Tr>
            ) : (
              rows
            )}
          </Table.Tbody>
        </Table>
      </ScrollArea>
    </Paper>
  )
}

interface CheatSubmissionLogProps {
  gameId: number
}

export const CheatSubmissionLog: FC<CheatSubmissionLogProps> = ({ gameId }) => {
  const {
    data: cheatInfo,
    error,
    isLoading,
    isValidating,
    mutate,
  } = api.game.useGameCheatInfo(gameId, {
    refreshInterval: 0,
    revalidateOnFocus: false,
    revalidateOnReconnect: false,
    refreshWhenHidden: false,
    refreshWhenOffline: false,
    shouldRetryOnError: false,
    keepPreviousData: false,
  })

  const [disabled, setDisabled] = useState(false)
  const [teamView, setTeamView] = useLocalStorage({
    key: 'cheat-info-team-view',
    defaultValue: true,
    getInitialValueInEffect: false,
  })

  const { t } = useTranslation()
  const cheatTeamInfo = useMemo(() => ToCheatTeamInfo(cheatInfo ?? []), [cheatInfo])

  const refresh = async () => {
    try {
      await mutate()
    } catch {
      // SWR exposes the request error on the next render.
    }
  }

  const setParticipation = async (id: number, model: ParticipationEditModel) => {
    setDisabled(true)
    try {
      await api.admin.adminParticipation(id, model)
      await mutate(
        (current) =>
          (current ?? []).map((info) => ({
            ...info,
            ownedTeam:
              info.ownedTeam?.id === id
                ? { ...info.ownedTeam, status: model.status ?? info.ownedTeam.status }
                : info.ownedTeam,
            submitTeam:
              info.submitTeam?.id === id
                ? { ...info.submitTeam, status: model.status ?? info.submitTeam.status }
                : info.submitTeam,
          })),
        { revalidate: false }
      )
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.participation.updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (err: any) {
      showErrorMsg(err, t)
    } finally {
      setDisabled(false)
    }
  }

  if (error && !cheatInfo) {
    return (
      <Alert
        color="red"
        variant="light"
        role="alert"
        icon={<Icon path={mdiAlertCircle} size={1} aria-hidden />}
        title={t('game.content.cheat.submissions_load_failed_title', 'Failed to load suspicious submissions')}
      >
        <Stack gap="sm" align="flex-start">
          <Text size="sm">{tryGetErrorMsg(error, t)}</Text>
          <Button size="xs" variant="outline" color="red" loading={isValidating} onClick={() => void refresh()}>
            {t('common.button.retry', 'Retry')}
          </Button>
        </Stack>
      </Alert>
    )
  }

  if (isLoading || !cheatInfo) {
    return (
      <Center h="30vh">
        <Stack align="center" gap="sm" role="status" aria-live="polite">
          <Loader aria-hidden="true" />
          <Text c="dimmed" size="sm">
            {t('game.label.cheat_info.loading', 'Loading suspicious submissions…')}
          </Text>
        </Stack>
      </Center>
    )
  }

  return (
    <Stack gap="md">
      {error && (
        <Alert
          color="red"
          variant="light"
          role="alert"
          icon={<Icon path={mdiAlertCircle} size={1} aria-hidden />}
          title={t('game.content.cheat.submissions_refresh_failed_title', 'Could not refresh suspicious submissions')}
        >
          <Stack gap="sm" align="flex-start">
            <Text size="sm">{tryGetErrorMsg(error, t)}</Text>
            <Button size="xs" variant="outline" color="red" loading={isValidating} onClick={() => void refresh()}>
              {t('common.button.retry', 'Retry')}
            </Button>
          </Stack>
        </Alert>
      )}
      <Group justify="space-between" w="100%">
        <Switch
          label={SwitchLabel(
            t('game.content.team_view.label', 'Team View'),
            t('game.content.team_view.description', 'Group by team')
          )}
          checked={teamView}
          onChange={(e) => setTeamView(e.currentTarget.checked)}
        />
        <Button
          size="xs"
          variant="subtle"
          leftSection={<Icon path={mdiRefresh} size={0.8} aria-hidden />}
          loading={isValidating}
          aria-busy={isValidating}
          aria-label={t('game.label.cheat_info.refresh', 'Refresh suspicious submissions')}
          onClick={() => void refresh()}
        >
          {t('game.button.cheat_info.refresh', 'Refresh')}
        </Button>
      </Group>
      {teamView ? (
        <CheatInfoTeamView disabled={disabled} cheatTeamInfo={cheatTeamInfo} setParticipation={setParticipation} />
      ) : (
        <CheatInfoTableView cheatInfo={cheatInfo} />
      )}
    </Stack>
  )
}
