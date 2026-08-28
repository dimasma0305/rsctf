import {
  ActionIcon,
  Alert,
  Badge,
  Button,
  Center,
  Checkbox,
  ComboboxItem,
  Group,
  Indicator,
  Modal,
  ScrollArea,
  Select,
  SegmentedControl,
  SimpleGrid,
  Stack,
  Text,
  TextInput,
  Title,
  Tooltip,
} from '@mantine/core'
import { useModals } from '@mantine/modals'
import { showNotification } from '@mantine/notifications'
import {
  mdiAlertCircleOutline,
  mdiCheck,
  mdiHammerWrench,
  mdiHexagonSlice6,
  mdiPauseCircleOutline,
  mdiPlayCircleOutline,
  mdiPlus,
  mdiRefresh,
  mdiTrashCanOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { Dispatch, FC, SetStateAction, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { BloodBonusModel } from '@Components/admin/BloodBonusModel'
import { ChallengeCreateModal } from '@Components/admin/ChallengeCreateModal'
import { ChallengeEditCard } from '@Components/admin/ChallengeEditCard'
import { WithGameEditTab } from '@Components/admin/WithGameEditTab'
import { controlJobResultCount, createOperationId, startControlJob, waitForControlJob } from '@Utils/ControlJobs'
import { showErrorMsg } from '@Utils/Shared'
import { ChallengeCategoryItem, ChallengeCategoryList, useChallengeCategoryLabelMap } from '@Utils/Shared'
import { CompletionPollSWRConfig, useCompletionPolling } from '@Hooks/useCompletionPolling'
import { useEditChallenges } from '@Hooks/useEdit'
import api, { BulkChallengeAction, ChallengeInfoModel, ChallengeCategory, ChallengeType } from '@Api'

const MAX_BULK_SELECTION = 100

// Engine = the scoring family, a separate filter axis from category (Web/Pwn/…).
// Mirrors the public scoreboard's 3-way split. 'jeopardy' = every non-AD-engine type.
type EngineFilter = 'all' | 'jeopardy' | 'ad' | 'koth'

const matchesEngine = (type: ChallengeType | undefined, engine: EngineFilter): boolean => {
  if (engine === 'all') return true
  if (engine === 'ad') return type === ChallengeType.AttackDefense
  if (engine === 'koth') return type === ChallengeType.KingOfTheHill
  // jeopardy: anything that isn't an A&D-engine type
  return type !== ChallengeType.AttackDefense && type !== ChallengeType.KingOfTheHill
}

const GameChallengeEdit: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')

  const [createOpened, setCreateOpened] = useState(false)
  const [bonusOpened, setBonusOpened] = useState(false)
  const [category, setCategory] = useState<ChallengeCategory | null>(null)
  const [engine, setEngine] = useState<EngineFilter>('all')
  const challengeCategoryLabelMap = useChallengeCategoryLabelMap()
  const [disabled, setDisabled] = useState(false)
  const bulkBuildFlight = useRef<Promise<void> | null>(null)
  const bulkBuildAbort = useRef(new AbortController())
  const bulkMutationOwner = useRef(false)
  const bulkOperation = useRef<{
    operationId: string
    expectedRevision: number
    action: BulkChallengeAction
    challengeIds: number[]
  } | null>(null)

  const { t } = useTranslation()

  useEffect(() => () => bulkBuildAbort.current.abort(), [])

  const { challenges, mutate } = useEditChallenges(numId)
  const buildPollingActive =
    challenges?.some(
      (challenge) =>
        matchesEngine(challenge.type, engine) &&
        (!category || challenge.category === category) &&
        (challenge.buildStatus === 'Queued' || challenge.buildStatus === 'Building')
    ) ?? false
  const buildStatusesQuery = api.edit.useEditGetChallengeBuildStatuses(
    numId,
    CompletionPollSWRConfig,
    buildPollingActive
  )

  useCompletionPolling({
    key: buildPollingActive ? `/api/edit/games/${numId}/challenges/buildstatuses` : '',
    phase: `game:${numId}`,
    enabled: buildPollingActive,
    data: buildStatusesQuery.data,
    error: buildStatusesQuery.error,
    isValidating: buildStatusesQuery.isValidating,
    mutate: buildStatusesQuery.mutate,
    successDelay: () => 2_000,
  })

  useEffect(() => {
    if (!buildStatusesQuery.data) return
    const statuses = new Map(buildStatusesQuery.data.map((status) => [status.challengeId, status]))
    void mutate(
      (current) =>
        current?.map((challenge) => {
          const status = challenge.id == null ? undefined : statuses.get(challenge.id)
          return status ? { ...challenge, buildStatus: status.buildStatus } : challenge
        }),
      { revalidate: false }
    )
  }, [buildStatusesQuery.data, mutate])

  // Two independent filter axes, both active at once: engine (scoring family)
  // then category (Web/Pwn/…).
  const filteredChallenges = useMemo(() => {
    let list = challenges ?? []
    if (engine !== 'all') list = list.filter((c) => matchesEngine(c.type, engine))
    if (category) list = list.filter((c) => c.category === category)
    return challenges ? list : challenges
  }, [challenges, engine, category])

  // At-a-glance build-state summary for the CURRENT filtered view — answers the
  // organizer's "which are built / building / failed" without reading each card.
  const buildSummary = useMemo(() => {
    const buildable = (filteredChallenges ?? []).filter(
      (c) => c.buildStatus && c.buildStatus !== 'None' && c.buildStatus !== 'NotApplicable'
    )
    return {
      total: filteredChallenges?.length ?? 0,
      built: buildable.filter((c) => c.buildStatus === 'Success').length,
      building: buildable.filter((c) => c.buildStatus === 'Building' || c.buildStatus === 'Queued').length,
      failed: buildable.filter((c) => c.buildStatus === 'Failed' || c.buildStatus === 'MissingDockerfile').length,
    }
  }, [filteredChallenges])

  const modals = useModals()

  // --- batch selection / delete ---
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set())

  const filteredIds = useMemo(
    () => (filteredChallenges ?? []).map((c) => c.id).filter((x): x is number => x != null),
    [filteredChallenges]
  )
  const selectableIds = filteredIds.slice(0, MAX_BULK_SELECTION)
  // Only count selections that are still in the current (filtered) view.
  const visibleSelected = filteredIds.filter((id) => selectedIds.has(id))
  const allSelected = selectableIds.length > 0 && selectableIds.every((id) => selectedIds.has(id))
  const someSelected = visibleSelected.length > 0 && !allSelected

  const toggleSelect = (cid: number, checked: boolean) =>
    setSelectedIds((prev) => {
      const next = new Set(prev)
      if (checked && (next.has(cid) || next.size < MAX_BULK_SELECTION)) next.add(cid)
      else next.delete(cid)
      return next
    })

  const toggleSelectAll = () =>
    setSelectedIds((prev) => {
      const next = new Set(prev)
      if (allSelected) selectableIds.forEach((id) => next.delete(id))
      else selectableIds.forEach((id) => next.add(id))
      return next
    })

  const clearSelection = () => setSelectedIds(new Set())

  // Type-to-confirm: the operator must type "delete" before the batch
  // delete fires — guards against an accidental click wiping challenges.
  const [deleteModalOpen, setDeleteModalOpen] = useState(false)
  const [confirmText, setConfirmText] = useState('')
  const canConfirmDelete = confirmText.trim().toLowerCase() === 'delete'

  const onBatchDelete = () => {
    if (visibleSelected.length === 0) return
    setConfirmText('')
    setDeleteModalOpen(true)
  }

  const performBatchDelete = async () => {
    const ids = filteredIds.filter((id) => selectedIds.has(id))
    if (ids.length === 0 || !canConfirmDelete || bulkMutationOwner.current) return
    setDeleteModalOpen(false)
    await performBulkMutation('Delete', ids)
  }

  const performBulkMutation = async (action: BulkChallengeAction, ids: number[]) => {
    if (ids.length === 0 || ids.length > MAX_BULK_SELECTION || bulkMutationOwner.current) return
    const expectedRevision = challenges?.find(
      (challenge) => challenge.configurationRevision != null
    )?.configurationRevision
    if (expectedRevision == null) return
    const canonicalIds = [...ids].sort((left, right) => left - right)
    const previous = bulkOperation.current
    const sameIntent =
      previous?.expectedRevision === expectedRevision &&
      previous.action === action &&
      previous.challengeIds.join(',') === canonicalIds.join(',')
    if (!sameIntent) {
      bulkOperation.current = {
        operationId: crypto.randomUUID(),
        expectedRevision,
        action,
        challengeIds: canonicalIds,
      }
    }
    bulkMutationOwner.current = true
    setDisabled(true)
    try {
      let response = await api.edit.editMutateGameChallengesBulk(numId, bulkOperation.current!)
      let pollCount = 0
      while (response.data.state === 'Pending' && pollCount < 150) {
        await new Promise((resolve) => window.setTimeout(resolve, 2000))
        response = await api.edit.editMutateGameChallengesBulk(numId, bulkOperation.current!)
        pollCount += 1
      }
      if (response.data.state === 'Pending') {
        throw new Error('Deletion is still running. Retry to recover the same operation.')
      }
      const failed = response.data.outcomes.filter((outcome) => outcome.status === 'Rejected').length
      const ok = response.data.outcomes.length - failed
      const message =
        action === 'Delete'
          ? failed > 0
            ? t('admin.notification.games.challenges.batch_deleted_partial', { ok, failed })
            : t('admin.notification.games.challenges.batch_deleted', { count: ok })
          : action === 'Enable'
            ? failed > 0
              ? t('admin.notification.games.challenges.batch_enabled_partial', { ok, failed })
              : t('admin.notification.games.challenges.batch_enabled', { count: ok })
            : failed > 0
              ? t('admin.notification.games.challenges.batch_disabled_partial', { ok, failed })
              : t('admin.notification.games.challenges.batch_disabled', { count: ok })
      showNotification({
        color: failed > 0 ? 'orange' : 'teal',
        message,
        icon: <Icon path={mdiCheck} size={1} />,
      })
      bulkOperation.current = null
      clearSelection()
      await mutate()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      bulkMutationOwner.current = false
      setDisabled(false)
    }
  }

  // Batch enable/disable of the selected challenges. Reversible, so a single
  // click-to-confirm (not the type-"delete" guard) is enough. The backend
  // returns one durable per-challenge outcome for the complete intent.
  const onBatchSetEnabled = (enable: boolean) => {
    if (visibleSelected.length === 0) return
    modals.openConfirmModal({
      title: enable ? t('admin.button.challenges.activate_selected') : t('admin.button.challenges.deactivate_selected'),
      children: (
        <Text size="sm">
          {enable
            ? t('admin.content.games.challenges.activate_selected_confirm', { count: visibleSelected.length })
            : t('admin.content.games.challenges.deactivate_selected_confirm', { count: visibleSelected.length })}
        </Text>
      ),
      onConfirm: () => performBatchSetEnabled(enable),
      confirmProps: { color: enable ? 'teal' : 'orange' },
    })
  }

  const performBatchSetEnabled = async (enable: boolean) => {
    const ids = filteredIds.filter((id) => selectedIds.has(id))
    await performBulkMutation(enable ? 'Enable' : 'Disable', ids)
  }

  const onToggle = (challenge: ChallengeInfoModel, setDisabled: Dispatch<SetStateAction<boolean>>) => {
    modals.openConfirmModal({
      title: challenge.isEnabled ? t('admin.button.challenges.disable') : t('admin.button.challenges.enable'),
      children: (
        <Text size="sm">
          {challenge.isEnabled
            ? t('admin.content.games.challenges.disable', { name: challenge.title })
            : t('admin.content.games.challenges.enable', { name: challenge.title })}
        </Text>
      ),
      onConfirm: () => onConfirmToggle(challenge, setDisabled),
      confirmProps: { color: 'orange' },
    })
  }

  const onConfirmToggle = async (challenge: ChallengeInfoModel, setDisabled: Dispatch<SetStateAction<boolean>>) => {
    if (challenge.id == null || challenge.revision == null) return
    const numId = parseInt(id ?? '-1')
    setDisabled(true)

    try {
      await api.edit.editUpdateGameChallenge(numId, challenge.id, {
        operationId: crypto.randomUUID(),
        expectedRevision: challenge.revision,
        isEnabled: !challenge.isEnabled,
      })
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.challenges.updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      await mutate()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const failedBuildCount =
    challenges?.filter((c) => c.buildStatus === 'Failed' || c.buildStatus === 'MissingDockerfile').length ?? 0

  const onBulkRebuild = () => {
    if (!numId || failedBuildCount === 0) return
    modals.openConfirmModal({
      title: t('admin.button.challenges.bulk_rebuild'),
      children: (
        <Text size="sm">{t('admin.content.games.challenges.bulk_rebuild_confirm', { count: failedBuildCount })}</Text>
      ),
      onConfirm: () => {
        if (bulkBuildFlight.current) return bulkBuildFlight.current
        const operationId = createOperationId()
        const task = (async () => {
          setDisabled(true)
          try {
            const job = await startControlJob(
              operationId,
              () =>
                api.admin.adminBulkRebuildFailed(numId, operationId, {
                  signal: bulkBuildAbort.current.signal,
                }),
              bulkBuildAbort.current.signal
            )
            const completed = await waitForControlJob(job, bulkBuildAbort.current.signal)
            showNotification({
              color: 'teal',
              message: t('admin.notification.builds.bulk_enqueued', {
                count: controlJobResultCount(completed, 'enqueued'),
              }),
              icon: <Icon path={mdiCheck} size={1} />,
            })
            await Promise.all([mutate(), buildStatusesQuery.mutate()])
          } catch (e) {
            if (!(e instanceof DOMException && e.name === 'AbortError')) showErrorMsg(e, t)
          } finally {
            setDisabled(false)
            bulkBuildFlight.current = null
          }
        })()
        bulkBuildFlight.current = task
        return task
      },
      confirmProps: { color: 'orange' },
    })
  }

  const onFlushScoreboard = async () => {
    if (!numId) return

    setDisabled(true)

    try {
      await api.edit.editFlushScoreboardCache(numId)
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.info.scoreboard_flushed'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutate()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  return (
    <WithGameEditTab
      headProps={{ justify: 'space-between' }}
      isLoading={!challenges}
      head={
        <>
          <Group gap="sm" wrap="wrap" w={{ base: '100%', lg: 'auto' }}>
            {/* Engine (scoring family) filter — separate axis from category below.
                Mirrors the public scoreboard's Jeopardy / A&D / KotH split. */}
            <SegmentedControl
              size="xs"
              w={{ base: '100%', sm: 'auto' }}
              aria-label={t('admin.label.games.challenges.engine_filter', 'Filter by scoring format')}
              value={engine}
              onChange={(v) => setEngine(v as EngineFilter)}
              data={[
                { value: 'all', label: t('admin.content.games.challenges.engine.all', 'All') },
                { value: 'jeopardy', label: t('admin.content.games.challenges.engine.jeopardy', 'Jeopardy') },
                { value: 'ad', label: t('admin.content.games.challenges.engine.ad', 'A&D') },
                { value: 'koth', label: t('admin.content.games.challenges.engine.koth', 'KotH') },
              ]}
            />
            <Select
              aria-label={t('admin.label.games.challenges.category_filter', 'Filter by category')}
              placeholder={t('admin.content.show_all')}
              clearable
              searchable
              w={{ base: '100%', sm: '16rem' }}
              value={category}
              nothingFoundMessage={t('admin.content.nothing_found')}
              onChange={(value) => setCategory(value as ChallengeCategory | null)}
              renderOption={ChallengeCategoryItem}
              data={ChallengeCategoryList.map((cate) => {
                const data = challengeCategoryLabelMap.get(cate)
                return { value: cate, label: data?.name, ...data } as ComboboxItem
              })}
            />
            {/* Build-state summary for the filtered set — shows built / building /
                failed counts at a glance (only when something in view is buildable). */}
            {buildSummary.built + buildSummary.building + buildSummary.failed > 0 && (
              <Group gap={6} wrap="wrap">
                <Tooltip label={t('admin.content.games.challenges.build_summary.built', 'Image built')}>
                  <Badge size="sm" color="teal" variant="light">
                    {t('admin.content.games.challenges.build_summary.built_n', {
                      count: buildSummary.built,
                      defaultValue: '{{count}} built',
                    })}
                  </Badge>
                </Tooltip>
                {buildSummary.building > 0 && (
                  <Tooltip label={t('admin.content.games.challenges.build_summary.building', 'Building or queued')}>
                    <Badge size="sm" color="yellow" variant="light">
                      {t('admin.content.games.challenges.build_summary.building_n', {
                        count: buildSummary.building,
                        defaultValue: '{{count}} building',
                      })}
                    </Badge>
                  </Tooltip>
                )}
                {buildSummary.failed > 0 && (
                  <Tooltip
                    label={t('admin.content.games.challenges.build_summary.failed', 'Build failed — needs attention')}
                  >
                    <Badge size="sm" color="red" variant="filled">
                      {t('admin.content.games.challenges.build_summary.failed_n', {
                        count: buildSummary.failed,
                        defaultValue: '{{count}} failed',
                      })}
                    </Badge>
                  </Tooltip>
                )}
              </Group>
            )}
            <Checkbox
              label={t('admin.button.challenges.select_all')}
              checked={allSelected}
              indeterminate={someSelected}
              disabled={filteredIds.length === 0}
              onChange={toggleSelectAll}
            />
            {/* Batch actions live with the selection controls (left) so the
                right-side buttons — New Challenge etc. — never shift when
                they appear. */}
            {visibleSelected.length > 0 && (
              <>
                <Tooltip
                  label={`${t('admin.button.challenges.activate_selected')} (${visibleSelected.length})`}
                  withArrow
                >
                  <ActionIcon
                    size="lg"
                    color="teal"
                    variant="light"
                    disabled={disabled}
                    onClick={() => onBatchSetEnabled(true)}
                    aria-label={t('admin.button.challenges.activate_selected')}
                  >
                    <Icon path={mdiPlayCircleOutline} size={0.9} />
                  </ActionIcon>
                </Tooltip>
                <Tooltip
                  label={`${t('admin.button.challenges.deactivate_selected')} (${visibleSelected.length})`}
                  withArrow
                >
                  <ActionIcon
                    size="lg"
                    color="orange"
                    variant="light"
                    disabled={disabled}
                    onClick={() => onBatchSetEnabled(false)}
                    aria-label={t('admin.button.challenges.deactivate_selected')}
                  >
                    <Icon path={mdiPauseCircleOutline} size={0.9} />
                  </ActionIcon>
                </Tooltip>
                <Tooltip
                  label={`${t('admin.button.challenges.delete_selected')} (${visibleSelected.length})`}
                  withArrow
                >
                  <Indicator label={visibleSelected.length} size={16} color="red">
                    <ActionIcon
                      size="lg"
                      color="red"
                      variant="light"
                      disabled={disabled}
                      onClick={onBatchDelete}
                      aria-label={t('admin.button.challenges.delete_selected')}
                    >
                      <Icon path={mdiTrashCanOutline} size={0.9} />
                    </ActionIcon>
                  </Indicator>
                </Tooltip>
                <Button size="sm" variant="subtle" color="gray" disabled={disabled} onClick={clearSelection}>
                  {t('admin.button.challenges.clear_selection')}
                </Button>
              </>
            )}
          </Group>
          <Group justify="right" wrap="wrap" w={{ base: '100%', lg: 'auto' }}>
            {failedBuildCount > 0 && (
              <Button
                w={{ base: '100%', sm: 'auto' }}
                leftSection={<Icon path={mdiHammerWrench} size={1} />}
                variant="default"
                color="orange"
                disabled={disabled}
                onClick={onBulkRebuild}
              >
                {t('admin.button.challenges.bulk_rebuild')} ({failedBuildCount})
              </Button>
            )}
            <Button
              w={{ base: '100%', sm: 'auto' }}
              leftSection={<Icon path={mdiRefresh} size={1} />}
              disabled={disabled}
              onClick={onFlushScoreboard}
            >
              {t('admin.button.challenges.flush_scoreboard')}
            </Button>
            <Button
              w={{ base: '100%', sm: 'auto' }}
              leftSection={<Icon path={mdiHexagonSlice6} size={1} />}
              onClick={() => setBonusOpened(true)}
            >
              {t('admin.button.challenges.bonus')}
            </Button>
            <Button
              w={{ base: '100%', sm: 'auto' }}
              leftSection={<Icon path={mdiPlus} size={1} />}
              onClick={() => setCreateOpened(true)}
            >
              {t('admin.button.challenges.new')}
            </Button>
          </Group>
        </>
      }
    >
      <ScrollArea h="clamp(20rem, calc(100dvh - 18rem), 70rem)" pos="relative" offsetScrollbars type="auto">
        {!filteredChallenges || filteredChallenges.length === 0 ? (
          <Center mih="20rem">
            <Stack gap={0}>
              <Title order={2}>{t('admin.content.games.challenges.empty.title')}</Title>
              <Text>{t('admin.content.games.challenges.empty.description')}</Text>
            </Stack>
          </Center>
        ) : (
          <SimpleGrid pr={6} cols={{ base: 1, sm: 2, w18: 3, w24: 4, w30: 5, w36: 6, w42: 7, w48: 8 }} spacing="sm">
            {filteredChallenges &&
              filteredChallenges.map((challenge) => (
                <ChallengeEditCard
                  key={challenge.id}
                  challenge={challenge}
                  onToggle={onToggle}
                  onMutate={() => mutate()}
                  selectable
                  selected={challenge.id != null && selectedIds.has(challenge.id)}
                  onSelectChange={(checked) => challenge.id != null && toggleSelect(challenge.id, checked)}
                />
              ))}
          </SimpleGrid>
        )}
      </ScrollArea>
      <ChallengeCreateModal
        title={t('admin.button.challenges.new')}
        size="min(42rem, calc(100vw - 2rem))"
        opened={createOpened}
        onClose={() => setCreateOpened(false)}
        onAddChallenge={(challenge) => mutate([challenge, ...(challenges ?? [])])}
      />
      <BloodBonusModel
        title={t('admin.button.challenges.bonus')}
        size="min(36rem, calc(100vw - 2rem))"
        opened={bonusOpened}
        onClose={() => setBonusOpened(false)}
      />
      <Modal
        opened={deleteModalOpen}
        onClose={() => setDeleteModalOpen(false)}
        title={t('admin.button.challenges.delete_selected')}
        size="min(34rem, calc(100vw - 2rem))"
        centered
      >
        <Stack gap="sm">
          <Alert color="red" variant="light" icon={<Icon path={mdiAlertCircleOutline} size={1} />}>
            {t('admin.content.games.challenges.delete_selected_confirm', { count: visibleSelected.length })}
          </Alert>
          <Text size="sm">{t('admin.content.games.challenges.delete_type_to_confirm')}</Text>
          <TextInput
            value={confirmText}
            aria-label={t('admin.content.games.challenges.delete_type_to_confirm')}
            onChange={(e) => setConfirmText(e.currentTarget.value)}
            placeholder="delete"
            data-autofocus
            autoComplete="off"
            onKeyDown={(e) => {
              if (e.key === 'Enter' && canConfirmDelete) performBatchDelete()
            }}
          />
          <Group justify="flex-end" gap="sm" wrap="wrap">
            <Button w={{ base: '100%', xs: 'auto' }} variant="default" onClick={() => setDeleteModalOpen(false)}>
              {t('common.modal.cancel', 'Cancel')}
            </Button>
            <Button
              w={{ base: '100%', xs: 'auto' }}
              color="red"
              leftSection={<Icon path={mdiTrashCanOutline} size={0.9} />}
              disabled={!canConfirmDelete || disabled}
              onClick={performBatchDelete}
            >
              {t('admin.button.challenges.delete_selected')} ({visibleSelected.length})
            </Button>
          </Group>
        </Stack>
      </Modal>
    </WithGameEditTab>
  )
}

export default GameChallengeEdit
