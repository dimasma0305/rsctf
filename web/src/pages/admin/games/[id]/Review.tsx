import {
  Accordion,
  ActionIcon,
  Alert,
  Avatar,
  Badge,
  Box,
  Button,
  Center,
  Grid,
  Group,
  Input,
  Loader,
  Pagination,
  ScrollArea,
  Select,
  Stack,
  Text,
  TextInput,
  Title,
  useMantineTheme,
} from '@mantine/core'
import { useDebouncedValue, useInputState } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import {
  mdiAccountGroupOutline,
  mdiAccountOutline,
  mdiAlertCircleOutline,
  mdiBadgeAccountHorizontalOutline,
  mdiCheck,
  mdiClose,
  mdiEmailOutline,
  mdiIdentifier,
  mdiPencil,
  mdiPhoneOutline,
  mdiStar,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import cx from 'clsx'
import { FC, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate, useParams } from 'react-router'
import { ScrollingText } from '@Components/ScrollingText'
import { ParticipationDivisionEditModal } from '@Components/admin/ParticipationDivisionEditModal'
import { ParticipationStatusControl } from '@Components/admin/ParticipationStatusControl'
import { WithGameEditTab } from '@Components/admin/WithGameEditTab'
import { showErrorMsg, useParticipationStatusMap } from '@Utils/Shared'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api, {
  ParticipationEditModel,
  ParticipationReviewMemberModel,
  ParticipationReviewSummaryModel,
  ParticipationStatus,
} from '@Api'
import classes from '@Styles/Accordion.module.css'
import misc from '@Styles/Misc.module.css'
import reviewClasses from '@Styles/Review.module.css'

interface MemberItemProps {
  user: ParticipationReviewMemberModel
}

const iconProps = {
  size: 0.9,
  color: 'gray',
  'aria-hidden': true,
} as const

const MemberItem: FC<MemberItemProps> = ({ user }) => {
  const theme = useMantineTheme()
  const { t } = useTranslation()
  const displayName = user.userName || t('admin.placeholder.empty')

  return (
    <Group wrap="nowrap" gap="xl" justify="space-between" className={reviewClasses.memberRow}>
      <Group wrap="nowrap" className={reviewClasses.memberDetails}>
        <Avatar alt={t('account.content.avatar_alt', '{{user}} avatar', { user: displayName })} src={user.avatar}>
          {displayName.slice(0, 1)}
        </Avatar>
        <Grid className={reviewClasses.root}>
          <Grid.Col span={{ base: 12, xs: 6, md: 3 }} className={reviewClasses.col}>
            <Icon path={mdiIdentifier} {...iconProps} />
            <Text fw="bold" lineClamp={1} title={displayName}>
              {displayName}
            </Text>
          </Grid.Col>
          <Grid.Col span={{ base: 12, xs: 6, md: 3 }} className={reviewClasses.col}>
            <Icon path={mdiBadgeAccountHorizontalOutline} {...iconProps} />
            <Input
              aria-label={t('account.label.student_number', 'Student number')}
              variant="unstyled"
              value={user.stdNumber || t('admin.placeholder.empty')}
              readOnly
              classNames={{ input: reviewClasses.input }}
            />
          </Grid.Col>
          <Grid.Col span={{ base: 12, xs: 6 }} className={reviewClasses.col}>
            <Icon path={mdiEmailOutline} {...iconProps} />
            <Text className={reviewClasses.memberValue} title={user.email ?? undefined}>
              {user.email || t('admin.placeholder.empty')}
            </Text>
          </Grid.Col>
          <Grid.Col span={{ base: 12, xs: 6 }} className={reviewClasses.col}>
            <Icon path={mdiAccountOutline} {...iconProps} />
            <Input
              aria-label={t('account.label.real_name', 'Real name')}
              variant="unstyled"
              value={user.realName || t('admin.placeholder.empty')}
              readOnly
              classNames={{ input: reviewClasses.input }}
            />
          </Grid.Col>
          <Grid.Col span={{ base: 12, xs: 6 }} className={reviewClasses.col}>
            <Icon path={mdiPhoneOutline} {...iconProps} />
            <Text className={reviewClasses.memberValue} title={user.phone ?? undefined}>
              {user.phone || t('admin.placeholder.empty')}
            </Text>
          </Grid.Col>
        </Grid>
      </Group>
      <Group wrap="nowrap" justify="right" className={reviewClasses.memberStatus}>
        {user.isCaptain && (
          <Group gap={4} wrap="nowrap">
            <Icon path={mdiStar} color={theme.colors.yellow[4]} size={0.9} aria-hidden />
            <Text size="sm" fw={500} c="yellow">
              {t('team.content.role.captain')}
            </Text>
          </Group>
        )}
        <Text size="sm" fw="bold" c={user.isRegistered ? 'teal' : 'orange'}>
          {user.isRegistered
            ? t('admin.content.games.review.participation.joined')
            : t('admin.content.games.review.participation.not_joined')}
        </Text>
      </Group>
    </Group>
  )
}

interface ParticipationItemProps {
  gameId: number
  participation: ParticipationReviewSummaryModel
  expanded: boolean
  disabled: boolean
  onEditDiv: () => void
  setParticipation: (id: number, model: ParticipationEditModel) => Promise<void>
  hasDivisions: boolean
  divisionName?: string | null
}

const ParticipationItem: FC<ParticipationItemProps> = (props) => {
  const { gameId, participation, expanded, disabled, onEditDiv, setParticipation, hasDivisions, divisionName } = props
  const part = useParticipationStatusMap().get(participation.status)!
  const { t } = useTranslation()
  const {
    data: detail,
    error: detailError,
    isLoading: detailLoading,
    mutate: retryDetail,
  } = api.game.useGameParticipationDetail(gameId, participation.id, OnceSWRConfig, expanded)

  return (
    <Accordion.Item value={participation.id.toString()}>
      <Box className={reviewClasses.participationHeader}>
        <Accordion.Control className={reviewClasses.participationControl}>
          <Group justify="space-between" wrap="nowrap" className={reviewClasses.participationRow}>
            <Group wrap="nowrap" miw={0}>
              <Avatar
                alt={t('account.content.avatar_alt', '{{user}} avatar', { user: participation.teamName })}
                src={participation.teamAvatar}
              >
                {participation.teamName.slice(0, 1) || 'T'}
              </Avatar>
              <Box miw={0} style={{ flex: 1, minWidth: 0 }}>
                <ScrollingText text={participation.teamName} fw={500} maw={320} />
                <Text size="sm" c="dimmed">
                  {t('admin.content.games.review.participation.team_id', 'Team #{{id}}', {
                    id: participation.teamId,
                  })}
                </Text>
              </Box>
            </Group>
            <Group wrap="nowrap" justify="space-between" className={reviewClasses.participationMeta}>
              <Box w="10em" maw="100%">
                {hasDivisions && participation.status !== ParticipationStatus.Rejected && (
                  <Text fz="sm" fw="bold" truncate>
                    {divisionName ?? t('admin.content.games.review.participation.no_division')}
                  </Text>
                )}
                <Text size="sm" c="dimmed" fw="bold">
                  {t('admin.content.games.review.participation.stats', {
                    count: participation.registeredMemberCount,
                    total: participation.teamMemberCount,
                  })}
                </Text>
              </Box>
              <Center miw="5.5em">
                <Badge color={part.color}>{part.title}</Badge>
              </Center>
            </Group>
          </Group>
        </Accordion.Control>
        <Group gap={4} wrap="nowrap" className={reviewClasses.participationActions}>
          {hasDivisions && participation.status !== ParticipationStatus.Rejected && (
            <ActionIcon
              size="sm"
              onClick={(event) => {
                event.stopPropagation()
                onEditDiv()
              }}
              disabled={disabled}
              aria-label={t('admin.button.games.review.edit_division', 'Edit division')}
            >
              <Icon path={mdiPencil} size={0.6} aria-hidden />
            </ActionIcon>
          )}
          <ParticipationStatusControl
            disabled={disabled}
            participation={participation}
            setParticipation={setParticipation}
          />
        </Group>
      </Box>
      <Accordion.Panel>
        <Box
          role="region"
          aria-label={t('admin.content.games.review.participation.roster', '{{team}} roster', {
            team: participation.teamName,
          })}
        >
          {detailLoading && (
            <Center py="lg" role="status" aria-live="polite">
              <Loader size="sm" />
              <Text ms="sm">{t('common.content.loading', 'Loading roster…')}</Text>
            </Center>
          )}
          {detailError && (
            <Alert
              color="red"
              icon={<Icon path={mdiAlertCircleOutline} size={1} aria-hidden />}
              title={t('common.error.fetch_failed', 'Could not load roster')}
            >
              <Button mt="xs" size="compact-sm" variant="light" color="red" onClick={() => void retryDetail()}>
                {t('common.button.retry', 'Retry')}
              </Button>
            </Alert>
          )}
          {!detailError && detail && detail.members.length === 0 && (
            <Text c="dimmed" role="status">
              {t('admin.content.games.review.participation.no_members', 'This team has no roster members.')}
            </Text>
          )}
          {!detailError && detail && detail.members.length > 0 && (
            <Stack>
              {detail.members.map((member) => (
                <MemberItem key={member.userId} user={member} />
              ))}
            </Stack>
          )}
        </Box>
      </Accordion.Panel>
    </Accordion.Item>
  )
}

const PART_NUM_PER_PAGE = 10

const GameTeamReview: FC = () => {
  const navigate = useNavigate()
  const { id } = useParams()
  const numId = parseInt(id ?? '-1', 10)
  const { t } = useTranslation()

  const [disabled, setDisabled] = useState(false)
  const [selectedStatus, setSelectedStatus] = useState<ParticipationStatus | null>(null)
  const [selectedDivisionId, setSelectedDivisionId] = useState<string | null>(null)
  const [search, setSearch] = useInputState('')
  const [debouncedSearch] = useDebouncedValue(search.trim(), 300)
  const [activePage, setPage] = useState(1)
  const [openedParticipation, setOpenedParticipation] = useState<string | null>(null)
  const [divModalOpened, setDivModalOpened] = useState(false)
  const [curParticipation, setCurParticipation] = useState<ParticipationReviewSummaryModel | null>(null)
  const participationStatusMap = useParticipationStatusMap()

  const { data: divisions } = api.edit.useEditGetDivisions(numId, OnceSWRConfig, numId > 0)
  const participationQuery = useMemo(
    () => ({
      count: PART_NUM_PER_PAGE,
      skip: (activePage - 1) * PART_NUM_PER_PAGE,
      status: selectedStatus ?? undefined,
      divisionId: selectedDivisionId ? parseInt(selectedDivisionId, 10) : undefined,
      search: debouncedSearch || undefined,
    }),
    [activePage, debouncedSearch, selectedDivisionId, selectedStatus]
  )
  const {
    data: participationPage,
    error: participationError,
    mutate: mutateParticipationPage,
  } = api.game.useGameParticipationPage(numId, participationQuery, OnceSWRConfig, numId > 0)

  const participations = participationPage?.data
  const totalCount = participationPage?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(totalCount / PART_NUM_PER_PAGE))

  const divisionNameMap = useMemo(() => {
    const map = new Map<number, string>()
    divisions?.forEach((division) => {
      map.set(division.id, division.name && division.name.trim().length > 0 ? division.name : `#${division.id}`)
    })
    return map
  }, [divisions])

  const divisionSelectOptions = useMemo(
    () =>
      (divisions ?? [])
        .map((division) => ({
          value: division.id.toString(),
          label: divisionNameMap.get(division.id) ?? `#${division.id}`,
        }))
        .sort((a, b) => a.label.localeCompare(b.label)),
    [divisions, divisionNameMap]
  )

  const hasDivisions = (divisions?.length ?? 0) > 0

  const setParticipation = async (participationId: number, model: ParticipationEditModel) => {
    setDisabled(true)
    try {
      await api.admin.adminParticipation(participationId, model)
      await mutateParticipationPage()
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.participation.updated'),
        icon: <Icon path={mdiCheck} size={1} aria-hidden />,
      })
    } catch (err: any) {
      showErrorMsg(err, t)
    } finally {
      setDisabled(false)
    }
  }

  useEffect(() => {
    setPage(1)
    setOpenedParticipation(null)
  }, [selectedStatus, selectedDivisionId, debouncedSearch])

  useEffect(() => {
    setOpenedParticipation(null)
  }, [activePage])

  useEffect(() => {
    if (participationPage && activePage > totalPages) {
      setPage(totalPages)
    }
  }, [activePage, participationPage, totalPages])

  useEffect(() => {
    if (numId < 0) {
      showNotification({
        color: 'red',
        message: t('common.error.param_error'),
        icon: <Icon path={mdiClose} size={1} aria-hidden />,
      })
      navigate('/admin/games')
    }
  }, [navigate, numId, t])

  return (
    <WithGameEditTab
      headProps={{ justify: 'space-between' }}
      isLoading={(participationPage === undefined && !participationError) || (numId > 0 && divisions === undefined)}
      head={
        <Box component="search" w="100%" aria-label={t('admin.content.games.review.filters', 'Review filters')}>
          <Group justify="space-between" wrap="wrap" w="100%" className={reviewClasses.filterToolbar}>
            <TextInput
              className={reviewClasses.searchInput}
              aria-label={t('admin.placeholder.teams.search')}
              aria-controls="participation-review-results"
              placeholder={t('admin.placeholder.teams.search')}
              value={search}
              onChange={setSearch}
              maxLength={100}
              rightSection={<Icon path={mdiAccountGroupOutline} size={1} aria-hidden />}
            />
            <Group justify="right" wrap="wrap" className={reviewClasses.filterGroup}>
              {divisionSelectOptions.length > 0 && (
                <Select
                  aria-label={t('admin.label.games.review.division_filter', 'Filter by division')}
                  placeholder={t('admin.content.show_all')}
                  clearable
                  data={divisionSelectOptions}
                  value={selectedDivisionId}
                  onChange={setSelectedDivisionId}
                />
              )}
              <Select
                aria-label={t('admin.label.games.review.status_filter', 'Filter by participation status')}
                placeholder={t('admin.content.show_all')}
                clearable
                data={Array.from(participationStatusMap, (value) => ({ value: value[0], label: value[1].title }))}
                value={selectedStatus}
                onChange={(value) => setSelectedStatus(value as ParticipationStatus | null)}
              />
            </Group>
          </Group>
        </Box>
      }
    >
      {participationPage && (
        <Text size="sm" c="dimmed" role="status" aria-live="polite" className={reviewClasses.resultStatus}>
          {t('admin.content.games.review.result_count', '{{count}} matching teams', { count: totalCount })}
        </Text>
      )}
      <ScrollArea
        id="participation-review-results"
        type="auto"
        pos="relative"
        h="calc(100vh - 280px)"
        viewportProps={{
          tabIndex: 0,
          'aria-label': t('admin.content.games.review.title', 'Team participation review'),
        }}
      >
        {participationError ? (
          <Alert
            color="red"
            icon={<Icon path={mdiAlertCircleOutline} size={1} aria-hidden />}
            title={t('common.error.fetch_failed', 'Could not load participations')}
          >
            <Button
              mt="xs"
              size="compact-sm"
              variant="light"
              color="red"
              onClick={() => void mutateParticipationPage()}
            >
              {t('common.button.retry', 'Retry')}
            </Button>
          </Alert>
        ) : participations && participations.length === 0 ? (
          <Center h="calc(100vh - 240px)">
            <Stack gap={0} ta="center">
              <Title order={2}>{t('admin.content.games.review.empty.title')}</Title>
              <Text>{t('admin.content.games.review.empty.description')}</Text>
            </Stack>
          </Center>
        ) : (
          <Accordion
            value={openedParticipation}
            onChange={setOpenedParticipation}
            variant="contained"
            chevronPosition="left"
            classNames={classes}
            className={classes.root}
          >
            {participations?.map((participation) => (
              <ParticipationItem
                key={participation.id}
                gameId={numId}
                participation={participation}
                expanded={openedParticipation === participation.id.toString()}
                disabled={disabled}
                onEditDiv={() => {
                  if (!hasDivisions) return
                  setCurParticipation(participation)
                  setDivModalOpened(true)
                }}
                setParticipation={setParticipation}
                hasDivisions={hasDivisions}
                divisionName={participation.divisionId ? divisionNameMap.get(participation.divisionId) : null}
              />
            ))}
          </Accordion>
        )}
      </ScrollArea>
      {totalPages > 1 && (
        <Pagination
          value={activePage}
          onChange={setPage}
          total={totalPages}
          getControlProps={(control) => ({
            'aria-label':
              control === 'first'
                ? t('common.pagination.first', 'First page')
                : control === 'previous'
                  ? t('common.pagination.previous', 'Previous page')
                  : control === 'next'
                    ? t('common.pagination.next', 'Next page')
                    : t('common.pagination.last', 'Last page'),
          })}
          classNames={{
            root: cx(misc.flex, misc.flexRow, misc.justifyEnd),
          }}
        />
      )}
      {hasDivisions && curParticipation && (
        <ParticipationDivisionEditModal
          title={t('admin.content.games.review.edit_division')}
          opened={divModalOpened}
          divisions={divisions ?? []}
          participateId={curParticipation.id}
          currentDivisionId={curParticipation.divisionId ?? null}
          setParticipation={setParticipation}
          onClose={() => {
            setDivModalOpened(false)
            setCurParticipation(null)
          }}
        />
      )}
    </WithGameEditTab>
  )
}

export default GameTeamReview
