import {
  ActionIcon,
  Affix,
  Box,
  Button,
  Center,
  Divider,
  Grid,
  Group,
  Image,
  Input,
  NumberInput,
  Paper,
  SimpleGrid,
  Stack,
  Switch,
  Text,
  Textarea,
  TextInput,
  Title,
} from '@mantine/core'
import { DateTimePicker } from '@mantine/dates'
import { Dropzone } from '@mantine/dropzone'
import { useClipboard, useInputState } from '@mantine/hooks'
import { useModals } from '@mantine/modals'
import { notifications, showNotification, updateNotification } from '@mantine/notifications'
import {
  mdiCheck,
  mdiClipboard,
  mdiClose,
  mdiContentSaveOutline,
  mdiDeleteOutline,
  mdiDiceMultiple,
  mdiDotsHorizontal,
  mdiDownload,
  mdiFileDocumentCheckOutline,
  mdiImageMultipleOutline,
  mdiSwordCross,
  mdiTextBoxOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import localizedFormat from 'dayjs/plugin/localizedFormat'
import { FC, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate, useParams } from 'react-router'
import { IconTabs } from '@Components/IconTabs'
import { SwitchLabel } from '@Components/admin/SwitchLabel'
import { WithGameEditTab } from '@Components/admin/WithGameEditTab'
import { downloadBlob } from '@Utils/ApiHelper'
import { getInputNumber, randomInviteCode, showErrorMsg, tryGetErrorMsg } from '@Utils/Shared'
import { IMAGE_MIME_TYPES } from '@Utils/Shared'
import { useAdminGame } from '@Hooks/useGame'
import api, { GameInfoModel } from '@Api'
import classes from '@Styles/AdminGameInfo.module.css'
import misc from '@Styles/Misc.module.css'

dayjs.extend(localizedFormat)

const GameInfoEdit: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const { game: gameSource, mutate } = useAdminGame(numId)
  const [game, setGame] = useState<GameInfoModel>()
  const navigate = useNavigate()

  const [disabled, setDisabled] = useState(false)
  const [start, setStart] = useInputState(dayjs())
  const [end, setEnd] = useInputState(dayjs())
  const [freeze, setFreeze] = useState<dayjs.Dayjs | null>(null)
  const [wpddl, setWpddl] = useInputState(3)

  const modals = useModals()
  const clipboard = useClipboard()

  const { t } = useTranslation()
  const adScoringStarted = game?.adScoringStartRound != null
  const kothScoringStarted = game?.kothScoringStartRound != null
  const engineScoringStarted = adScoringStarted || kothScoringStarted
  const kothEpochTicks = game?.kothEpochTicks ?? 12
  const kothCycleTicks = game?.kothCycleTicks ?? 3
  const kothCooldownTicks = game?.kothChampionCooldownTicks ?? 1
  const kothConfirmationTicks = game?.kothClaimConfirmationTicks ?? 2
  const kothCrownShapeError =
    kothEpochTicks < 2 || kothEpochTicks > 64
      ? t('admin.error.games.koth_epoch_range', 'KotH epoch length must be between 2 and 64 ticks.')
      : kothCycleTicks < 1 || kothCycleTicks > Math.floor(kothEpochTicks / 2) || kothEpochTicks % kothCycleTicks !== 0
        ? t(
            'admin.error.games.koth_cycle_shape',
            'Crown-cycle length must divide the epoch into at least two complete cycles.'
          )
        : kothCooldownTicks < 0 || kothCooldownTicks >= kothCycleTicks
          ? t('admin.error.games.koth_cooldown_range', 'Champion cooldown must be shorter than the crown cycle.')
          : kothConfirmationTicks < 1 || kothConfirmationTicks > kothCycleTicks
            ? t(
                'admin.error.games.koth_confirmation_range',
                'Claim confirmation must be between 1 tick and the crown-cycle length.'
              )
            : null

  const endError =
    end < start ? t('admin.error.games.end_before_start', 'End time must be after the start time.') : undefined
  const freezeError =
    freeze && (freeze.isBefore(start) || freeze.isAfter(end))
      ? t('admin.error.games.freeze_out_of_range', 'Freeze time must be between the start and end times.')
      : undefined
  const timeRangeInvalid = !!endError || !!freezeError

  useEffect(() => {
    if (numId < 0) {
      showNotification({
        color: 'red',
        message: t('common.error.param_error'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      navigate('/admin/games')
      return
    }

    if (gameSource) {
      setGame(gameSource)
      setStart(dayjs(gameSource.start))
      setEnd(dayjs(gameSource.end))
      setFreeze(gameSource.freeze ? dayjs(gameSource.freeze) : null)

      const wpddl = dayjs(gameSource.writeupDeadline).diff(gameSource.end, 'h')
      setWpddl(wpddl < 0 ? 0 : wpddl)
    }
  }, [id, gameSource])

  const onUpdatePoster = async (file: File | undefined) => {
    if (!game || !file) return

    setDisabled(true)
    notifications.clean()
    showNotification({
      id: 'upload-poster',
      color: 'orange',
      message: t('admin.notification.games.info.poster.uploading'),
      loading: true,
      autoClose: false,
    })

    try {
      const res = await api.edit.editUpdateGamePoster(game.id!, { file })
      updateNotification({
        id: 'upload-poster',
        color: 'teal',
        message: t('admin.notification.games.info.poster.uploaded'),
        icon: <Icon path={mdiCheck} size={1} />,
        autoClose: true,
        loading: false,
      })
      mutate({ ...game, poster: res.data })
    } catch (err) {
      updateNotification({
        id: 'upload-poster',
        color: 'red',
        title: t('admin.notification.games.info.poster.upload_failed'),
        message: tryGetErrorMsg(err, t),
        icon: <Icon path={mdiClose} size={1} />,
        autoClose: true,
        loading: false,
      })
    } finally {
      setDisabled(false)
    }
  }

  const onUpdateInfo = async () => {
    if (!game?.title) {
      showNotification({
        color: 'orange',
        message: t('admin.notification.games.title_required', 'A game title is required.'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }
    if (timeRangeInvalid) {
      showNotification({
        color: 'orange',
        message: endError ?? freezeError,
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }
    if (kothCrownShapeError) {
      showNotification({
        color: 'orange',
        message: kothCrownShapeError,
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }
    setDisabled(true)

    try {
      await api.edit.editUpdateGame(game.id!, {
        ...game,
        inviteCode: (game.inviteCode?.length ?? 0) > 6 ? game.inviteCode : null,
        start: start.valueOf(),
        end: end.valueOf(),
        freeze: freeze ? freeze.valueOf() : null,
        writeupDeadline: end.add(wpddl, 'h').valueOf(),
      })
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.info.info_updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutate()
      api.game.mutateGameGames()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const onConfirmDelete = async () => {
    if (!game) return

    try {
      await api.edit.editDeleteGame(game.id!)
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.info.deleted'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      navigate('/admin/games')
    } catch (e) {
      showErrorMsg(e, t)
    }
  }

  const onCopyPublicKey = () => {
    clipboard.copy(game?.publicKey || '')
    showNotification({
      color: 'teal',
      message: t('admin.notification.games.info.public_key_copied'),
      icon: <Icon path={mdiCheck} size={1} />,
    })
  }

  const onExportGame = async () => {
    if (!game?.id) return

    await downloadBlob(api.edit.editExportGame(game.id, { format: 'blob' }), setDisabled, t)
  }

  // Section tabs (mirrors /admin/settings): one category is shown at a time via the
  // IconTabs bar, and the Save action lives in a sticky bar at the bottom.
  const sections = [
    { key: 'general', icon: mdiTextBoxOutline, label: t('admin.content.games.info.section.general', 'General') },
    {
      key: 'writeups',
      icon: mdiFileDocumentCheckOutline,
      label: t('admin.content.games.info.section.writeups', 'Summary & writeups'),
    },
    {
      key: 'ad',
      icon: mdiSwordCross,
      label: t('admin.content.games.info.section.ad', 'Attack & Defense · King of the Hill'),
    },
    {
      key: 'content',
      icon: mdiImageMultipleOutline,
      label: t('admin.content.games.info.section.content', 'Description & media'),
    },
  ]
  const [activeSection, setActiveSection] = useState('general')

  return (
    <WithGameEditTab
      headProps={{ justify: 'space-between' }}
      contentPos="right"
      isLoading={!game}
      head={
        <>
          <Button
            disabled={disabled}
            color="red"
            leftSection={<Icon path={mdiDeleteOutline} size={1} />}
            variant="outline"
            onClick={() =>
              modals.openConfirmModal({
                title: t('admin.button.games.delete'),
                children: <Text size="sm">{t('admin.content.games.info.delete', { name: game?.title })}</Text>,
                onConfirm: () => onConfirmDelete(),
                confirmProps: { color: 'red' },
              })
            }
          >
            {t('admin.button.games.delete')}
          </Button>
          <Button
            leftSection={<Icon path={mdiDownload} size={1} />}
            disabled={disabled}
            onClick={onExportGame}
            variant="outline"
          >
            {t('admin.button.games.export')}
          </Button>
          <Button leftSection={<Icon path={mdiClipboard} size={1} />} disabled={disabled} onClick={onCopyPublicKey}>
            {t('admin.button.games.copy_public_key')}
          </Button>
        </>
      }
    >
      <Stack gap="md" w="100%" pb={100} className={classes.formContent}>
        <IconTabs
          idPrefix="game-info"
          active={sections.findIndex((s) => s.key === activeSection)}
          onTabChange={(_, key) => setActiveSection(key)}
          tabs={sections.map((s) => ({
            tabKey: s.key,
            icon: <Icon path={s.icon} size={1} />,
            label: (
              <Text size="sm" fw={500}>
                {s.label}
              </Text>
            ),
          }))}
        />
        <Stack
          id="game-info-panel"
          role="tabpanel"
          aria-labelledby={`game-info-tab-${activeSection}`}
          tabIndex={0}
          gap="md"
        >
          {activeSection === 'general' && (
            <Stack gap="sm">
              <Title order={3}>{t('admin.content.games.info.section.general', 'General')}</Title>
              <Divider />
              <SimpleGrid cols={{ base: 1, sm: 2, lg: 4 }}>
                <TextInput
                  label={t('admin.content.games.info.title.label')}
                  description={t('admin.content.games.info.title.description')}
                  disabled={disabled}
                  value={game?.title}
                  required
                  onChange={(e) => game && setGame({ ...game, title: e.target.value })}
                />
                <NumberInput
                  label={t('admin.content.games.info.member_limit.label')}
                  description={t('admin.content.games.info.member_limit.description')}
                  disabled={disabled}
                  min={0}
                  required
                  value={game?.teamMemberCountLimit}
                  onChange={(e) => {
                    const number = getInputNumber(e)
                    if (!game || isNaN(number)) return
                    setGame({ ...game, teamMemberCountLimit: number })
                  }}
                />
                <NumberInput
                  label={t('admin.content.games.info.container_limit.label')}
                  description={t('admin.content.games.info.container_limit.description')}
                  disabled={disabled}
                  min={0}
                  required
                  value={game?.containerCountLimit}
                  onChange={(e) => {
                    const number = getInputNumber(e)
                    if (!game || isNaN(number)) return
                    setGame({ ...game, containerCountLimit: number })
                  }}
                />
                <TextInput
                  label={t('admin.content.games.info.invite_code.label')}
                  description={t('admin.content.games.info.invite_code.description')}
                  placeholder={t('admin.content.games.info.invite_code.placeholder')}
                  value={game?.inviteCode || ''}
                  disabled={disabled}
                  onChange={(e) => game && setGame({ ...game, inviteCode: e.target.value })}
                  rightSectionWidth={48}
                  rightSection={
                    <ActionIcon
                      size={44}
                      disabled={disabled}
                      aria-label={t('admin.content.games.info.invite_code.generate', 'Generate invite code')}
                      onClick={() => game && setGame({ ...game, inviteCode: randomInviteCode() })}
                    >
                      <Icon path={mdiDiceMultiple} size={0.9} />
                    </ActionIcon>
                  }
                />
                <TextInput
                  label={t('admin.content.games.info.discord_webhook.label')}
                  description={t('admin.content.games.info.discord_webhook.description')}
                  placeholder={t('admin.content.games.info.discord_webhook.placeholder')}
                  value={game?.discordWebhook || ''}
                  disabled={disabled}
                  onChange={(e) => game && setGame({ ...game, discordWebhook: e.target.value })}
                />
                <DateTimePicker
                  label={t('admin.content.games.info.start_time')}
                  size="sm"
                  value={start.toDate()}
                  valueFormat="L LT"
                  disabled={disabled}
                  clearable={false}
                  onChange={(e) => {
                    const newDate = dayjs(e)
                    setStart(newDate)
                    if (newDate && end < newDate) {
                      setEnd(newDate.add(2, 'h'))
                    }
                  }}
                  required
                />
                <DateTimePicker
                  label={t('admin.content.games.info.end_time')}
                  size="sm"
                  disabled={disabled}
                  minDate={start.toDate()}
                  value={end.toDate()}
                  valueFormat="L LT"
                  clearable={false}
                  onChange={(e) => {
                    setEnd(dayjs(e))
                  }}
                  error={endError}
                  required
                />
                <DateTimePicker
                  label={t('admin.content.games.info.freeze_time')}
                  size="sm"
                  disabled={disabled}
                  minDate={start.toDate()}
                  maxDate={end.toDate()}
                  value={freeze?.toDate() ?? null}
                  valueFormat="L LT"
                  clearable
                  onChange={(e) => setFreeze(e ? dayjs(e) : null)}
                  error={freezeError}
                />
                <Switch
                  disabled={disabled}
                  checked={game?.acceptWithoutReview ?? false}
                  classNames={{ root: misc.switchVerticalMiddle }}
                  label={SwitchLabel(
                    t('admin.content.games.info.accept_without_review.label'),
                    t('admin.content.games.info.accept_without_review.description')
                  )}
                  onChange={(e) => game && setGame({ ...game, acceptWithoutReview: e.target.checked })}
                />
                <Switch
                  disabled={disabled}
                  checked={game?.practiceMode ?? true}
                  classNames={{ root: misc.switchVerticalMiddle }}
                  label={SwitchLabel(
                    t('admin.content.games.info.practice_mode.label'),
                    t('admin.content.games.info.practice_mode.description')
                  )}
                  onChange={(e) => game && setGame({ ...game, practiceMode: e.target.checked })}
                />
                <Switch
                  disabled={disabled}
                  checked={game?.allowUserSubmissions ?? false}
                  classNames={{ root: misc.switchVerticalMiddle }}
                  label={SwitchLabel(
                    t('admin.content.games.info.allow_user_submissions.label'),
                    t('admin.content.games.info.allow_user_submissions.description')
                  )}
                  onChange={(e) => game && setGame({ ...game, allowUserSubmissions: e.target.checked })}
                />
              </SimpleGrid>
            </Stack>
          )}
          {activeSection === 'writeups' && (
            <Stack gap="sm">
              <Title order={3}>{t('admin.content.games.info.section.writeups', 'Summary & writeups')}</Title>
              <Divider />
              <Group grow justify="space-between" align="flex-start">
                <Textarea
                  label={t('admin.content.games.info.summary.label')}
                  description={t('admin.content.games.info.summary.description')}
                  value={game?.summary}
                  w="100%"
                  autosize
                  disabled={disabled}
                  minRows={8}
                  maxRows={8}
                  onChange={(e) => game && setGame({ ...game, summary: e.target.value })}
                />
                <Stack gap="0.488125rem">
                  <Group grow justify="space-between">
                    <Switch
                      disabled={disabled}
                      checked={game?.writeupRequired ?? false}
                      classNames={{ root: misc.switchVerticalMiddle }}
                      label={SwitchLabel(
                        t('admin.content.games.info.writeup_required.label'),
                        t('admin.content.games.info.writeup_required.description')
                      )}
                      onChange={(e) => game && setGame({ ...game, writeupRequired: e.target.checked })}
                    />
                    <NumberInput
                      label={t('admin.content.games.info.writeup_deadline.label')}
                      description={t('admin.content.games.info.writeup_deadline.description')}
                      disabled={disabled}
                      min={0}
                      required
                      value={wpddl}
                      onChange={(e) => setWpddl(getInputNumber(e))}
                    />
                  </Group>
                  <Textarea
                    label={t('admin.content.games.info.writeup_instruction')}
                    description={t('admin.content.markdown_support')}
                    value={game?.writeupNote}
                    w="100%"
                    autosize
                    disabled={disabled}
                    minRows={4}
                    maxRows={4}
                    onChange={(e) => game && setGame({ ...game, writeupNote: e.target.value })}
                  />
                </Stack>
              </Group>
            </Stack>
          )}
          {activeSection === 'ad' && (
            <Stack gap="sm">
              <Title order={3}>{t('admin.content.games.info.section.ad', 'Attack & Defense · King of the Hill')}</Title>
              <Text size="xs" c="dimmed">
                {t(
                  'admin.content.games.info.section.ad_hint',
                  'Only applies to games that contain A&D or KotH challenges.'
                )}
              </Text>
              <Divider />
              <Paper withBorder p="md" radius="md">
                <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="md">
                  <Stack gap={4}>
                    <Text size="sm" fw={500}>
                      {t('admin.content.games.info.ad_epoch_scoring.label', 'Official epoch scoring')}
                    </Text>
                    <Text size="xs" c="dimmed">
                      {t(
                        'admin.content.games.info.ad_epoch_scoring.description',
                        'Every completed epoch receives the same 100-point ceiling. Scoring starts automatically once at least two accepted teams have every enabled A&D service and every enabled A&D challenge has a prepared exact custom checker.'
                      )}
                    </Text>
                  </Stack>
                  <NumberInput
                    label={t('admin.content.games.info.ad_epoch_ticks.label', 'Epoch length (ticks)')}
                    description={t(
                      'admin.content.games.info.ad_epoch_ticks.description',
                      'Choose 1–64 ticks. Evidence is aggregated first, then every completed epoch receives the same 100-point ceiling.'
                    )}
                    disabled={disabled || adScoringStarted}
                    min={1}
                    max={64}
                    value={game?.adEpochTicks ?? 8}
                    onChange={(e) => {
                      const n = getInputNumber(e)
                      if (!isNaN(n)) game && setGame({ ...game, adEpochTicks: n })
                    }}
                  />
                </SimpleGrid>
                {adScoringStarted && (
                  <Text size="sm" c="dimmed" mt="sm">
                    {t('admin.content.games.info.ad_epoch_scoring.started', {
                      defaultValue:
                        'Official epoch scoring started at round {{round}}. Its epoch length and scoring timing are now locked.',
                      round: game?.adScoringStartRound,
                    })}
                  </Text>
                )}
              </Paper>
              <Paper withBorder p="md" radius="md">
                <Stack gap="sm">
                  <Group justify="space-between" align="flex-start" wrap="wrap">
                    <Stack gap={2}>
                      <Text size="sm" fw={600}>
                        {t('admin.content.games.info.koth_cycle_scoring.label', 'KotH crown-cycle scoring')}
                      </Text>
                      <Text size="xs" c="dimmed">
                        {t(
                          'admin.content.games.info.koth_cycle_scoring.description',
                          'Each hill is reset to the same pristine image several times per epoch. These settings are snapshotted when official KotH scoring starts.'
                        )}
                      </Text>
                    </Stack>
                  </Group>
                  <SimpleGrid cols={{ base: 1, sm: 2, lg: 4 }} spacing="md">
                    <NumberInput
                      label={t('admin.content.games.info.koth_epoch_ticks.label', 'KotH epoch length (ticks)')}
                      description={t(
                        'admin.content.games.info.koth_epoch_ticks.description',
                        'Complete epochs share one fixed 100-point budget. Must be divisible by crown-cycle length.'
                      )}
                      disabled={disabled || kothScoringStarted}
                      min={2}
                      max={64}
                      value={kothEpochTicks}
                      onChange={(value) => {
                        const ticks = getInputNumber(value)
                        if (!isNaN(ticks)) game && setGame({ ...game, kothEpochTicks: ticks })
                      }}
                    />
                    <NumberInput
                      label={t('admin.content.games.info.koth_cycle_ticks.label', 'Crown-cycle length (ticks)')}
                      description={t(
                        'admin.content.games.info.koth_cycle_ticks.description',
                        'The shared container is finalized, destroyed, recreated, and checked at each boundary.'
                      )}
                      disabled={disabled || kothScoringStarted}
                      min={1}
                      max={Math.max(1, Math.floor(kothEpochTicks / 2))}
                      value={kothCycleTicks}
                      onChange={(value) => {
                        const ticks = getInputNumber(value)
                        if (!isNaN(ticks)) game && setGame({ ...game, kothCycleTicks: ticks })
                      }}
                    />
                    <NumberInput
                      label={t('admin.content.games.info.koth_cooldown_ticks.label', 'Champion cooldown (ticks)')}
                      description={t(
                        'admin.content.games.info.koth_cooldown_ticks.description',
                        'Opening opportunity for challengers after readiness. The forced tick is excluded from the champion’s denominator.'
                      )}
                      disabled={disabled || kothScoringStarted}
                      min={0}
                      max={Math.max(0, kothCycleTicks - 1)}
                      value={kothCooldownTicks}
                      onChange={(value) => {
                        const ticks = getInputNumber(value)
                        if (!isNaN(ticks)) game && setGame({ ...game, kothChampionCooldownTicks: ticks })
                      }}
                    />
                    <NumberInput
                      label={t('admin.content.games.info.koth_confirmation_ticks.label', 'Claim confirmation (ticks)')}
                      description={t(
                        'admin.content.games.info.koth_confirmation_ticks.description',
                        'Consecutive scorable Ok verdicts required before acquisition credit is awarded.'
                      )}
                      disabled={disabled || kothScoringStarted}
                      min={1}
                      max={Math.max(1, kothCycleTicks)}
                      value={kothConfirmationTicks}
                      onChange={(value) => {
                        const ticks = getInputNumber(value)
                        if (!isNaN(ticks)) game && setGame({ ...game, kothClaimConfirmationTicks: ticks })
                      }}
                    />
                  </SimpleGrid>
                  {kothCrownShapeError && (
                    <Text size="xs" c="red" role="alert">
                      {kothCrownShapeError}
                    </Text>
                  )}
                  {kothScoringStarted && (
                    <Text size="sm" c="dimmed">
                      {t('admin.content.games.info.koth_cycle_scoring.started', {
                        round: game?.kothScoringStartRound,
                        defaultValue:
                          'Official KotH scoring started at round {{round}}. Formula, roster, epoch, cycle, cooldown, and confirmation settings are immutable.',
                      })}
                    </Text>
                  )}
                </Stack>
              </Paper>
              <SimpleGrid cols={{ base: 1, sm: 2 }} spacing="md">
                <NumberInput
                  label={t('admin.content.games.info.ad_warmup_seconds.label', 'A&D warmup (seconds)')}
                  description={t(
                    'admin.content.games.info.ad_warmup_seconds.description',
                    'Seconds between game start and round 1. Teams use this gap to SSH in + write initial patches (default 1800 = 30 min). Only applies to games with A&D challenges.'
                  )}
                  disabled={disabled}
                  min={0}
                  max={86400}
                  value={game?.adWarmupSeconds ?? 1800}
                  onChange={(e) => {
                    const n = getInputNumber(e)
                    if (!isNaN(n)) game && setGame({ ...game, adWarmupSeconds: n })
                  }}
                />
                <NumberInput
                  label={t(
                    'admin.content.games.info.ad_snapshot_retention_days.label',
                    'A&D snapshot retention (days)'
                  )}
                  description={t(
                    'admin.content.games.info.ad_snapshot_retention_days.description',
                    'How long the per-team container snapshots stay available for download after game end. Leave empty to keep forever (the default).'
                  )}
                  placeholder={t('admin.content.games.info.ad_snapshot_retention_days.placeholder', '∞ (keep forever)')}
                  disabled={disabled}
                  min={1}
                  max={3650}
                  // Empty input = null = keep snapshots forever (default).
                  value={game?.adSnapshotRetentionDays ?? ''}
                  onChange={(e) => {
                    if (!game) return
                    if (e === '' || e === null || e === undefined) {
                      setGame({ ...game, adSnapshotRetentionDays: null })
                      return
                    }
                    const n = getInputNumber(e)
                    if (!isNaN(n)) setGame({ ...game, adSnapshotRetentionDays: n })
                  }}
                />
              </SimpleGrid>
              <SimpleGrid cols={{ base: 1, sm: 2, lg: 4 }} spacing="md" verticalSpacing="md">
                <NumberInput
                  label={t('admin.content.games.info.ad_tick_seconds.label', 'A&D tick (seconds)')}
                  description={t(
                    'admin.content.games.info.ad_tick_seconds.description',
                    'Length of one scoring tick. Every A&D service in the game shares this — flags rotate and the checker runs once per tick (default 60).'
                  )}
                  disabled={disabled || engineScoringStarted}
                  min={30}
                  max={600}
                  value={game?.adTickSeconds ?? 60}
                  onChange={(e) => {
                    const n = getInputNumber(e)
                    if (!isNaN(n)) game && setGame({ ...game, adTickSeconds: n })
                  }}
                />
                <NumberInput
                  label={t('admin.content.games.info.ad_flag_lifetime_ticks.label', 'A&D flag lifetime (ticks)')}
                  description={t(
                    'admin.content.games.info.ad_flag_lifetime_ticks.description',
                    'How many ticks a planted flag stays submittable — the attack window (default 5).'
                  )}
                  disabled={disabled || adScoringStarted}
                  min={1}
                  max={50}
                  value={game?.adFlagLifetimeTicks ?? 5}
                  onChange={(e) => {
                    const n = getInputNumber(e)
                    if (!isNaN(n)) game && setGame({ ...game, adFlagLifetimeTicks: n })
                  }}
                />
                <NumberInput
                  label={t('admin.content.games.info.ad_reset_cooldown_minutes.label', 'A&D reset cooldown (minutes)')}
                  description={t(
                    'admin.content.games.info.ad_reset_cooldown_minutes.description',
                    "Minimum minutes between a team's container self-resets (default 5). Whether a service can be reset at all is per-challenge."
                  )}
                  disabled={disabled}
                  min={0}
                  max={60}
                  value={game?.adResetCooldownMinutes ?? 5}
                  onChange={(e) => {
                    const n = getInputNumber(e)
                    if (!isNaN(n)) game && setGame({ ...game, adResetCooldownMinutes: n })
                  }}
                />
                <NumberInput
                  label={t('admin.content.games.info.ad_getflag_window_fraction.label', 'A&D getflag jitter window')}
                  description={t(
                    'admin.content.games.info.ad_getflag_window_fraction.description',
                    "Fraction of the tick used for the random SLA-check offset after the grace period (default 0.5). Runtime reserves a complete probe and persistence budget; every service gets an independent offset."
                  )}
                  disabled={disabled || engineScoringStarted}
                  min={0.05}
                  max={0.9}
                  step={0.05}
                  decimalScale={2}
                  value={game?.adGetflagWindowFraction ?? 0.5}
                  onChange={(e) => {
                    const n = getInputNumber(e)
                    if (!isNaN(n)) game && setGame({ ...game, adGetflagWindowFraction: n })
                  }}
                />
                <NumberInput
                  label={t('admin.content.games.info.ad_min_grace_period_seconds.label', 'A&D min grace period (s)')}
                  description={t(
                    'admin.content.games.info.ad_min_grace_period_seconds.description',
                    "Seconds after each service's immutable flag-delivery receipt before getflag may fire. The value must leave bounded publication, checker, and persistence runway (default 3)."
                  )}
                  disabled={disabled || engineScoringStarted}
                  min={1}
                  max={Math.min(60, Math.max(1, (game?.adTickSeconds ?? 60) - 12))}
                  value={game?.adMinGracePeriodSeconds ?? 3}
                  onChange={(e) => {
                    const n = getInputNumber(e)
                    if (!isNaN(n)) game && setGame({ ...game, adMinGracePeriodSeconds: n })
                  }}
                />
                <Switch
                  mt="md"
                  label={t('admin.content.games.info.ad_allow_snapshot_download.label', 'A&D snapshot download')}
                  description={t(
                    'admin.content.games.info.ad_allow_snapshot_download.description',
                    'Snapshot each team container at game end and offer the tarball for download.'
                  )}
                  disabled={disabled}
                  checked={game?.adAllowSnapshotDownload ?? true}
                  onChange={(e) => game && setGame({ ...game, adAllowSnapshotDownload: e.currentTarget.checked })}
                />
              </SimpleGrid>
            </Stack>
          )}
          {activeSection === 'content' && (
            <Stack gap="sm">
              <Title order={3}>{t('admin.content.games.info.section.content', 'Description & media')}</Title>
              <Divider />
              <Grid grow>
                <Grid.Col span={8}>
                  <Textarea
                    label={
                      <Group gap="sm">
                        <Text size="sm">{t('admin.content.games.info.content')}</Text>
                        <Text size="xs" c="dimmed">
                          {t('admin.content.markdown_support')}
                        </Text>
                      </Group>
                    }
                    value={game?.content}
                    w="100%"
                    autosize
                    disabled={disabled}
                    minRows={10}
                    maxRows={10}
                    onChange={(e) => game && setGame({ ...game, content: e.target.value })}
                  />
                </Grid.Col>
                <Grid.Col span={4}>
                  <Input.Wrapper label={t('admin.content.games.info.poster')}>
                    <Dropzone
                      onDrop={(files) => onUpdatePoster(files[0])}
                      onReject={() => {
                        showNotification({
                          color: 'red',
                          title: t('common.error.file_invalid.title'),
                          message: t('common.error.file_invalid.message'),
                          icon: <Icon path={mdiClose} size={1} />,
                        })
                      }}
                      maxSize={3 * 1024 * 1024}
                      accept={IMAGE_MIME_TYPES}
                      disabled={disabled}
                      data-poster={game?.poster || undefined}
                      classNames={{ root: misc.gamePoster }}
                    >
                      <Center className={misc.noPointerEvents}>
                        {game?.poster ? (
                          <Image height="231px" fit="contain" src={game.poster} alt="poster" />
                        ) : (
                          <Center h="231px">
                            <Stack gap={0}>
                              <Text size="xl" inline>
                                {t('common.content.drop_zone.content', {
                                  type: t('common.content.drop_zone.type.poster'),
                                })}
                              </Text>
                              <Text size="sm" c="dimmed" inline mt={7}>
                                {t('common.content.drop_zone.limit')}
                              </Text>
                            </Stack>
                          </Center>
                        )}
                      </Center>
                    </Dropzone>
                  </Input.Wrapper>
                </Grid.Col>
              </Grid>
            </Stack>
          )}
        </Stack>
      </Stack>
      {/* Sticky save bar (mirrors /admin/settings), so Save is always reachable
          without scrolling regardless of which section tab is open. */}
      <Affix position={{ bottom: 16, left: '50%' }} className={classes.saveAffix}>
        <Paper shadow="md" radius="md" p="sm" withBorder className={classes.saveBar}>
          <Group gap="md" align="center">
            <Group gap={6}>
              <Box w={8} h={8} style={{ borderRadius: '50%', background: 'var(--mantine-color-teal-5)' }} />
              <Text size="sm" c="dimmed">
                {t('admin.content.games.info.save_hint', 'Save changes')}
              </Text>
            </Group>
            <Button
              size="md"
              variant="filled"
              leftSection={<Icon path={disabled ? mdiDotsHorizontal : mdiContentSaveOutline} size={0.9} />}
              disabled={disabled || timeRangeInvalid}
              onClick={onUpdateInfo}
            >
              {t('admin.button.save')}
            </Button>
          </Group>
        </Paper>
      </Affix>
    </WithGameEditTab>
  )
}

export default GameInfoEdit
