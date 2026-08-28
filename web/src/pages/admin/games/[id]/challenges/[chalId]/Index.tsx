import {
  Alert,
  Badge,
  Button,
  Code,
  ComboboxItem,
  Divider,
  Grid,
  Group,
  Input,
  Loader,
  NumberInput,
  Paper,
  Select,
  SimpleGrid,
  Slider,
  Stack,
  Switch,
  Text,
  Textarea,
  TextInput,
  Title,
} from '@mantine/core'
import { DateTimePicker } from '@mantine/dates'
import { useModals } from '@mantine/modals'
import { showNotification } from '@mantine/notifications'
import {
  mdiAlertCircleOutline,
  mdiCheck,
  mdiContentSaveOutline,
  mdiDatabaseEditOutline,
  mdiDeleteOutline,
  mdiEyeOutline,
  mdiHammerWrench,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate, useParams } from 'react-router'
import { HintList } from '@Components/HintList'
import { InstanceEntry } from '@Components/InstanceEntry'
import { ChallengePreviewModal } from '@Components/admin/ChallengePreviewModal'
import { ContainerExecModal } from '@Components/admin/ContainerExecModal'
import { SwitchLabel } from '@Components/admin/SwitchLabel'
import { WithChallengeEdit } from '@Components/admin/WithChallengeEdit'
import { ScoreFunc } from '@Components/charts/ScoreFunc'
import { getInputNumber, NetworkModeItem, NetworkModeList, showErrorMsg, useNetworkModeMap } from '@Utils/Shared'
import {
  ChallengeCategoryItem,
  useChallengeCategoryLabelMap,
  ChallengeTypeItem,
  useChallengeTypeLabelMap,
  ChallengeCategoryList,
} from '@Utils/Shared'
import { createDefaultJeopardyWorkloadSpec, formatWorkloadSpec, parseJeopardyWorkloadSpec } from '@Utils/WorkloadSpec'
import { controlJobResultCount, createOperationId, startControlJob, waitForControlJob } from '@Utils/ControlJobs'
import { useEditChallenge, useEditChallenges } from '@Hooks/useEdit'
import { CompletionPollSWRConfig, useCompletionPolling } from '@Hooks/useCompletionPolling'
import { useAdminGame, useGameStatus } from '@Hooks/useGame'
import api, {
  ChallengeBuildStatus,
  ChallengeCategory,
  ChallengeEditDetailModel,
  ChallengeType,
  ChallengeVariantMode,
  ChallengeUpdateModel,
  NetworkMode,
  ScoreCurve,
  SolveReceiptMode,
} from '@Api'
import misc from '@Styles/Misc.module.css'

const WORKLOAD_SPEC_SECTION_HELP_ID = 'challenge-workload-spec-section-help'
const WORKLOAD_SPEC_EDITOR_HELP_ID = 'challenge-workload-spec-editor-help'
const WORKLOAD_SPEC_ERROR_ID = 'challenge-workload-spec-error'
const WORKLOAD_SPEC_ROLLOUT_HELP_ID = 'challenge-workload-spec-rollout-help'
const DEFAULT_JEOPARDY_MIN_SCORE_RATE = 0.01

/**
 * Inline live build-log surface for the challenge edit page.
 * Updates every ~2s while the build is in flight (driven by the
 * useEffect in the parent that re-fetches the challenge). Once
 * terminal, the badge color settles and the log sticks around as
 * the post-mortem record.
 */
const BuildLogSection: FC<{ buildStatus: ChallengeBuildStatus; lastBuildLog: string | null }> = ({
  buildStatus,
  lastBuildLog,
}) => {
  const { t } = useTranslation()
  const inFlight = buildStatus === 'Queued' || buildStatus === 'Building'
  const color =
    buildStatus === 'Success'
      ? 'teal'
      : buildStatus === 'Failed'
        ? 'red'
        : buildStatus === 'MissingDockerfile'
          ? 'orange'
          : buildStatus === 'Queued'
            ? 'blue'
            : 'yellow'
  return (
    <Paper p="sm" withBorder>
      <Stack gap={4}>
        <Group gap="xs" wrap="nowrap">
          {inFlight && <Loader size="xs" />}
          <Title order={3}>{t('admin.content.audit.build_log')}</Title>
          <Badge size="xs" color={color} variant={buildStatus === 'Failed' ? 'filled' : 'light'}>
            {buildStatus}
          </Badge>
        </Group>
        {lastBuildLog ? (
          <Code
            block
            tabIndex={0}
            aria-label={t('admin.content.audit.build_log')}
            style={{ whiteSpace: 'pre-wrap', maxHeight: '40vh', overflowY: 'auto', fontSize: 11 }}
          >
            {lastBuildLog}
          </Code>
        ) : (
          <Text size="xs" c="dimmed">
            {t('admin.content.audit.no_build_log')}
          </Text>
        )}
      </Stack>
    </Paper>
  )
}

const GameChallengeEdit: FC = () => {
  const navigate = useNavigate()
  const { id, chalId } = useParams()
  const [numId, numCId] = [parseInt(id ?? '-1'), parseInt(chalId ?? '-1')]

  const { game } = useAdminGame(numId)
  const { started: eventSecurityFrozen } = useGameStatus(game)
  const { challenge, mutate } = useEditChallenge(numId, numCId)
  const { challenges, mutate: mutateChals } = useEditChallenges(numId)

  const [challengeInfo, setChallengeInfo] = useState<ChallengeUpdateModel>({ ...challenge })
  const [deadline, setDeadline] = useState<dayjs.Dayjs | null>(
    challenge?.deadlineUtc ? dayjs(challenge?.deadlineUtc) : null
  )

  const [disabled, setDisabled] = useState(false)
  const rolloutPromiseRef = useRef<Promise<void> | null>(null)
  const rolloutAbortRef = useRef<AbortController | null>(null)

  const [minRate, setMinRate] = useState((challenge?.minScoreRate ?? DEFAULT_JEOPARDY_MIN_SCORE_RATE) * 100)
  const [category, setCategory] = useState<string | null>(challenge?.category ?? ChallengeCategory.Misc)
  const [networkMode, setNetworkMode] = useState<string | null>(challenge?.networkMode ?? NetworkMode.Open)
  const [type, setType] = useState<string | null>(challenge?.type ?? ChallengeType.StaticAttachment)
  // A&D and KotH share the same editor treatment (managed containers, no per-flag
  // scoring, the A&D config card for the checker image + egress).
  const isAdEngine = type === ChallengeType.AttackDefense || type === ChallengeType.KingOfTheHill
  const isJeopardyContainer = type === ChallengeType.StaticContainer || type === ChallengeType.DynamicContainer
  const isContainerType = isAdEngine || isJeopardyContainer
  const isKoth = type === ChallengeType.KingOfTheHill
  const adScoringStarted = type === ChallengeType.AttackDefense && game?.adScoringStartRound != null
  const [workloadEditorEnabled, setWorkloadEditorEnabled] = useState(challenge?.workloadSpec != null)
  const [workloadJson, setWorkloadJson] = useState(
    challenge?.workloadSpec ? formatWorkloadSpec(challenge.workloadSpec) : ''
  )
  const [workloadError, setWorkloadError] = useState<string | null>(null)
  const [rollingWorkload, setRollingWorkload] = useState(false)
  const workloadToggleRef = useRef<HTMLInputElement>(null)
  const workloadInputRef = useRef<HTMLTextAreaElement>(null)
  const [currentAcceptCount, setCurrentAcceptCount] = useState(0)
  const [previewOpened, setPreviewOpened] = useState(false)
  const [execOpened, setExecOpened] = useState(false)

  const modals = useModals()
  const challengeTypeLabelMap = useChallengeTypeLabelMap()
  const challengeCategoryLabelMap = useChallengeCategoryLabelMap()
  const networkModeLabelMap = useNetworkModeMap()

  const { t } = useTranslation()

  // Unsaved-changes guard. A stable serialization of the editable state is captured as
  // the "saved" baseline whenever the challenge (re)loads — including right after a save,
  // since onUpdate mutate()s `challenge`, re-running this effect and clearing dirty.
  const savedSnapshotRef = useRef<string>('')
  const savedWorkloadRef = useRef({ enabled: false, text: '' })
  const [dirty, setDirty] = useState(false)
  const makeSnapshot = (
    info: ChallengeUpdateModel,
    dl: dayjs.Dayjs | null,
    mr: number,
    cat: string | null,
    ty: string | null,
    nm: string | null,
    workloadEnabled: boolean,
    workloadText: string
  ) =>
    JSON.stringify({
      info,
      dl: dl ? dl.valueOf() : null,
      mr,
      cat,
      ty,
      nm,
      workloadEnabled,
      workloadText,
    })

  useEffect(() => {
    if (challenge) {
      const info = { ...challenge }
      const configuredWorkload = challenge.workloadSpec ?? null
      const workloadEnabled = configuredWorkload !== null
      const workloadText = configuredWorkload ? formatWorkloadSpec(configuredWorkload) : ''
      // The edit response uses null for an absent workload, while the update API
      // reserves a missing property for "preserve" and null for "clear".
      delete info.workloadSpec
      const dl = challenge.deadlineUtc ? dayjs(challenge.deadlineUtc) : null
      const mr = (challenge?.minScoreRate ?? DEFAULT_JEOPARDY_MIN_SCORE_RATE) * 100
      setChallengeInfo(info)
      setCategory(challenge.category)
      setType(challenge.type)
      setMinRate(mr)
      setCurrentAcceptCount(challenge.acceptedCount)
      setDeadline(dl)
      setNetworkMode(challenge.networkMode ?? NetworkMode.Open)
      setWorkloadEditorEnabled(workloadEnabled)
      setWorkloadJson(workloadText)
      setWorkloadError(null)
      savedWorkloadRef.current = { enabled: workloadEnabled, text: workloadText }
      savedSnapshotRef.current = makeSnapshot(
        info,
        dl,
        mr,
        challenge.category,
        challenge.type,
        challenge.networkMode ?? NetworkMode.Open,
        workloadEnabled,
        workloadText
      )
      setDirty(false)
    }
  }, [challenge])

  // Recompute dirty against the saved baseline whenever any tracked edit state changes.
  useEffect(() => {
    setDirty(
      makeSnapshot(
        challengeInfo,
        deadline,
        minRate,
        category,
        type,
        networkMode,
        workloadEditorEnabled,
        workloadJson
      ) !== savedSnapshotRef.current
    )
  }, [challengeInfo, deadline, minRate, category, type, networkMode, workloadEditorEnabled, workloadJson])

  // Warn on tab-close / reload / navigating to an external URL while there are unsaved
  // edits. NOTE: this can't intercept in-app SPA navigation — the app mounts a component
  // <BrowserRouter> (not a data router), so react-router's useBlocker isn't available;
  // beforeunload is the supported guard here and covers the common accidental-loss cases.
  useEffect(() => {
    if (!dirty) return
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault()
      e.returnValue = ''
    }
    window.addEventListener('beforeunload', handler)
    return () => window.removeEventListener('beforeunload', handler)
  }, [dirty])

  const validateWorkloadEditor = (focusOnError = false) => {
    const result = parseJeopardyWorkloadSpec(workloadJson)
    if (result.ok) {
      setWorkloadError(null)
      return result.value
    }

    setWorkloadError(result.error)
    if (focusOnError) {
      window.requestAnimationFrame(() => workloadInputRef.current?.focus())
    }
    return null
  }

  const parsedCurrentWorkload = workloadEditorEnabled ? parseJeopardyWorkloadSpec(workloadJson) : null
  const statefulRolloutServices = parsedCurrentWorkload?.ok
    ? parsedCurrentWorkload.value.services.filter((service) => !service.stateless).map((service) => service.name)
    : []
  const rolloutBlockedByStatefulService = statefulRolloutServices.length > 0

  const onUpdate = async (
    candidate: ChallengeUpdateModel,
    noFeedback?: boolean
  ): Promise<ChallengeEditDetailModel | null> => {
    const update = { ...candidate }
    if (isJeopardyContainer && workloadEditorEnabled) {
      const workloadSpec = validateWorkloadEditor(true)
      if (!workloadSpec) return null
      const workloadIsUnchanged = savedWorkloadRef.current.enabled && savedWorkloadRef.current.text === workloadJson
      if (workloadIsUnchanged) {
        delete update.workloadSpec
      } else {
        update.workloadSpec = workloadSpec
      }
    }

    setDisabled(true)

    try {
      const res = await api.edit.editUpdateGameChallenge(numId, numCId, {
        ...update,
        deadlineUtc: deadline ? deadline.valueOf() : 0,
        isEnabled: undefined,
      })
      if (!noFeedback) {
        showNotification({
          color: 'teal',
          message: t('admin.notification.games.challenges.updated'),
          icon: <Icon path={mdiCheck} size={1} />,
        })
      }
      mutate(res.data)
      mutateChals()
      return res.data
    } catch (e) {
      showErrorMsg(e, t)
      if (noFeedback) setDisabled(false)
      return null
    } finally {
      if (!noFeedback) {
        setDisabled(false)
      }
    }
  }

  const [building, setBuilding] = useState(false)
  const buildFlight = useRef<Promise<void> | null>(null)
  const buildAbort = useRef(new AbortController())
  const inFlightBuild = challenge?.buildStatus === 'Queued' || challenge?.buildStatus === 'Building'
  const isBuildable =
    (challenge?.type === 'StaticContainer' ||
      challenge?.type === 'DynamicContainer' ||
      challenge?.type === 'AttackDefense' ||
      challenge?.type === 'KingOfTheHill') &&
    challenge?.buildStatus !== 'NotApplicable'

  const buildStatusQuery = api.edit.useEditGetChallengeBuildStatus(
    numId,
    numCId,
    CompletionPollSWRConfig,
    inFlightBuild
  )
  useCompletionPolling({
    key: inFlightBuild ? `/api/edit/games/${numId}/challenges/${numCId}/buildstatus` : '',
    phase: `challenge:${numId}:${numCId}`,
    enabled: inFlightBuild,
    data: buildStatusQuery.data,
    error: buildStatusQuery.error,
    isValidating: buildStatusQuery.isValidating,
    mutate: buildStatusQuery.mutate,
    successDelay: () => 2_000,
  })

  useEffect(() => {
    const status = buildStatusQuery.data
    if (!status) return
    void mutate(
      (current) =>
        current
          ? { ...current, buildStatus: status.buildStatus, lastBuildLog: status.lastBuildLog ?? null }
          : current,
      { revalidate: false }
    )
    void mutateChals(
      (current) =>
        current?.map((item) =>
          item.id === numCId
            ? { ...item, buildStatus: status.buildStatus, lastBuildLog: status.lastBuildLog ?? null }
            : item
        ),
      { revalidate: false }
    )
  }, [buildStatusQuery.data, mutate, mutateChals, numCId])

  useEffect(() => () => buildAbort.current.abort(), [])

  const onBuildNow = () => {
    if (buildFlight.current) return buildFlight.current
    const operationId = createOperationId()
    const task = (async () => {
      setBuilding(true)
      try {
        const job = await startControlJob(
          operationId,
          () =>
            api.edit.editRebuildChallengeImage(numId, numCId, operationId, {
              signal: buildAbort.current.signal,
            }),
          buildAbort.current.signal
        )
        showNotification({
          color: 'teal',
          message: t('admin.notification.builds.enqueued'),
          icon: <Icon path={mdiCheck} size={1} />,
        })
        await waitForControlJob(job, buildAbort.current.signal)
        await Promise.all([mutate(), mutateChals(), buildStatusQuery.mutate()])
      } catch (e) {
        if (!(e instanceof DOMException && e.name === 'AbortError')) showErrorMsg(e, t)
      } finally {
        setBuilding(false)
        buildFlight.current = null
      }
    })()
    buildFlight.current = task
    return task
  }

  const onConfirmDelete = async () => {
    setDisabled(true)

    try {
      await api.edit.editRemoveGameChallenge(numId, numCId)
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.challenges.deleted'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutateChals(
        challenges?.filter((chal) => chal.id !== numCId),
        { revalidate: false }
      )
      navigate(`/admin/games/${id}/challenges`)
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const onCreateTestContainer = async () => {
    // disabled by Toggle function

    try {
      const res = await api.edit.editCreateTestContainer(numId, numCId)
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.instances.created'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      if (challenge) {
        mutate({ ...challenge, testContainer: res.data })
      }
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const onDestroyTestContainer = async () => {
    // disabled by Toggle function

    try {
      await api.edit.editDestroyTestContainer(numId, numCId)
      showNotification({
        color: 'teal',
        message: t('admin.notification.games.instances.deleted'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      if (challenge) {
        mutate({ ...challenge, testContainer: undefined })
      }
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const onToggleTestContainer = async () => {
    if (!challenge) return

    const updated = await onUpdate(
      {
        ...challengeInfo,
        category: category as ChallengeCategory,
        minScoreRate: minRate / 100,
      },
      true
    )
    if (!updated) return

    if (challenge?.testContainer) {
      await onDestroyTestContainer()
    } else {
      await onCreateTestContainer()
    }
  }

  useEffect(() => () => rolloutAbortRef.current?.abort(), [])

  const onRolloutWorkloads = async () => {
    if (rolloutBlockedByStatefulService) return
    if (rolloutPromiseRef.current) return rolloutPromiseRef.current
    const operationId = createOperationId()
    const controller = new AbortController()
    rolloutAbortRef.current?.abort()
    rolloutAbortRef.current = controller
    setRollingWorkload(true)
    const request = (async () => {
      const saved = await onUpdate(
        {
          ...challengeInfo,
          category: category as ChallengeCategory,
          minScoreRate: minRate / 100,
        },
        true
      )
      if (!saved) return
      try {
        let job
        try {
          job = (
            await api.edit.editRolloutChallengeWorkloads(numId, numCId, operationId, {
              headers: { 'X-RSCTF-Expected-Workload': saved.workloadIdentity ?? '' },
              signal: controller.signal,
            })
          ).data
        } catch (error) {
          if (controller.signal.aborted) throw error
          job = (await api.eventSecurity.getControlJobByOperation(operationId, { signal: controller.signal })).data
        }
        const completed = await waitForControlJob(job, controller.signal)
        const result = {
          matched: controlJobResultCount(completed, 'matched'),
          updated: controlJobResultCount(completed, 'updated'),
          alreadyCurrent: controlJobResultCount(completed, 'alreadyCurrent'),
          stale: controlJobResultCount(completed, 'stale'),
          incompatible: controlJobResultCount(completed, 'incompatible'),
          insufficientCapacity: controlJobResultCount(completed, 'insufficientCapacity'),
          failed: controlJobResultCount(completed, 'failed'),
        }
        const incomplete = result.stale + result.incompatible + result.insufficientCapacity + result.failed
        showNotification({
          color: incomplete === 0 ? 'teal' : 'orange',
          message: t('admin.content.games.challenges.workload_spec.rollout_result', { ...result }),
          icon: <Icon path={mdiCheck} size={1} />,
        })
        mutate()
      } catch (e) {
        if (!(e instanceof DOMException && e.name === 'AbortError')) showErrorMsg(e, t)
      }
    })()
    rolloutPromiseRef.current = request
    try {
      await request
    } finally {
      if (rolloutPromiseRef.current === request) rolloutPromiseRef.current = null
      if (rolloutAbortRef.current === controller) rolloutAbortRef.current = null
      setRollingWorkload(false)
      setDisabled(false)
    }
  }

  const enableWorkloadEditor = () => {
    setWorkloadEditorEnabled(true)
    setWorkloadError(null)
    setChallengeInfo((current) => {
      const next = { ...current }
      delete next.workloadSpec
      return next
    })
    if (!workloadJson.trim()) {
      setWorkloadJson(formatWorkloadSpec(createDefaultJeopardyWorkloadSpec()))
    }
  }

  const disableWorkloadEditor = () => {
    setWorkloadEditorEnabled(false)
    setWorkloadError(null)
    setChallengeInfo((current) => ({ ...current, workloadSpec: null }))
  }

  const clearWorkloadEditor = () => {
    disableWorkloadEditor()
    setWorkloadJson('')
    window.requestAnimationFrame(() => workloadToggleRef.current?.focus())
  }

  const tryDefault: <T>(values: T[], defaultValue?: NonNullable<T>) => NonNullable<T> | undefined = (vs, d) => {
    return vs.find((v) => !!v) ?? d
  }

  return (
    <WithChallengeEdit
      isLoading={!challenge}
      contentPos="space-between"
      backUrl={`/admin/games/${id}/challenges`}
      head={
        <>
          <Title order={2} lineClamp={1} className={misc.wordBreakAll}>
            # {challengeInfo?.title}
          </Title>
          <Group wrap="wrap" justify="right" w={{ base: '100%', lg: 'auto' }}>
            <Button
              w={{ base: '100%', sm: 'auto' }}
              disabled={disabled}
              color="red"
              leftSection={<Icon path={mdiDeleteOutline} size={1} />}
              variant="outline"
              onClick={() =>
                modals.openConfirmModal({
                  title: t('admin.button.challenges.delete'),
                  children: (
                    <Text size="sm">
                      {t('admin.content.games.challenges.delete', {
                        name: challengeInfo?.title,
                      })}
                    </Text>
                  ),
                  onConfirm: () => onConfirmDelete(),
                  confirmProps: { color: 'red' },
                })
              }
            >
              {t('admin.button.challenges.delete')}
            </Button>
            {isBuildable && (
              <Button
                w={{ base: '100%', sm: 'auto' }}
                disabled={disabled || building || inFlightBuild}
                color="orange"
                variant="outline"
                leftSection={<Icon path={mdiHammerWrench} size={1} />}
                onClick={onBuildNow}
                loading={building || inFlightBuild}
              >
                {inFlightBuild ? t('admin.button.challenges.build_in_flight') : t('admin.button.challenges.build_now')}
              </Button>
            )}
            <Button
              w={{ base: '100%', sm: 'auto' }}
              disabled={disabled}
              leftSection={<Icon path={mdiEyeOutline} size={1} />}
              onClick={() => setPreviewOpened(true)}
            >
              {t('admin.button.challenges.preview')}
            </Button>
            <Button
              w={{ base: '100%', sm: 'auto' }}
              disabled={disabled}
              component={Link}
              leftSection={<Icon path={mdiDatabaseEditOutline} size={1} />}
              to={`/admin/games/${numId}/challenges/${numCId}/flags`}
            >
              {t('admin.button.challenges.edit_more')}
            </Button>
            <Button
              w={{ base: '100%', sm: 'auto' }}
              disabled={disabled}
              leftSection={<Icon path={mdiContentSaveOutline} size={1} />}
              onClick={() =>
                onUpdate({
                  ...challengeInfo,
                  category: category as ChallengeCategory,
                  minScoreRate: minRate / 100,
                  networkMode: isAdEngine ? NetworkMode.Open : (challengeInfo.networkMode as NetworkMode),
                })
              }
            >
              {t('admin.button.save')}
            </Button>
          </Group>
        </>
      }
    >
      <Stack>
        <Grid columns={3}>
          <Grid.Col span={{ base: 3, md: 1 }}>
            <TextInput
              label={t('admin.content.games.challenges.title')}
              disabled={disabled}
              value={challengeInfo.title ?? ''}
              required
              onChange={(e) => setChallengeInfo({ ...challengeInfo, title: e.target.value })}
            />
          </Grid.Col>
          <Grid.Col span={{ base: 3, md: 1 }}>
            <Select
              label={
                <Group gap="sm" wrap="nowrap">
                  <Text size="sm">{t('admin.content.games.challenges.type.label')}</Text>
                  <Text size="xs" c="dimmed">
                    {t('admin.content.games.challenges.type.description')}
                  </Text>
                </Group>
              }
              placeholder="Type"
              value={type}
              disabled={disabled}
              readOnly
              renderOption={ChallengeTypeItem}
              data={Object.entries(ChallengeType).map((type) => {
                const data = challengeTypeLabelMap.get(type[1])
                return { value: type[1], label: data?.name, ...data } as ComboboxItem
              })}
            />
          </Grid.Col>
          <Grid.Col span={{ base: 3, md: 1 }}>
            <Select
              required
              label={t('admin.content.games.challenges.category')}
              placeholder="Category"
              value={category}
              disabled={disabled}
              onChange={(e) => {
                setCategory(e)
                setChallengeInfo({ ...challengeInfo, category: e as ChallengeCategory })
              }}
              renderOption={ChallengeCategoryItem}
              data={ChallengeCategoryList.map((category) => {
                const data = challengeCategoryLabelMap.get(category)
                return { value: category, label: data?.name, ...data } as ComboboxItem
              })}
            />
          </Grid.Col>
          <Grid.Col span={{ base: 3, md: 2 }}>
            <Textarea
              w="100%"
              label={
                <Group gap="sm">
                  <Text size="sm">{t('admin.content.games.challenges.description')}</Text>
                  <Text size="xs" c="dimmed">
                    {t('admin.content.markdown_support')}
                  </Text>
                </Group>
              }
              value={challengeInfo?.content ?? ''}
              autosize
              disabled={disabled}
              minRows={5}
              maxRows={5}
              onChange={(e) => setChallengeInfo({ ...challengeInfo, content: e.target.value })}
            />
          </Grid.Col>
          <Grid.Col span={{ base: 3, md: 1 }}>
            <Stack gap="0.425625rem">
              {!isAdEngine && (
                <NumberInput
                  label={t('admin.content.games.challenges.submission_limit.label')}
                  description={t('admin.content.games.challenges.submission_limit.description')}
                  placeholder={t('admin.content.games.challenges.submission_limit.placeholder')}
                  min={0}
                  max={10000}
                  disabled={disabled}
                  stepHoldDelay={500}
                  stepHoldInterval={(t) => Math.max(1000 / t ** 2, 25)}
                  value={challengeInfo?.submissionLimit || undefined}
                  onChange={(e) => {
                    const number = getInputNumber(e)
                    if (isNaN(number)) return
                    setChallengeInfo({ ...challengeInfo, submissionLimit: number })
                  }}
                />
              )}
              <DateTimePicker
                label={t('admin.content.games.challenges.deadline.label')}
                placeholder={t('admin.content.games.challenges.deadline.placeholder')}
                size="sm"
                value={deadline?.toDate()}
                valueFormat="L LT"
                disabled={disabled}
                clearable
                onChange={(e) => {
                  setDeadline(e ? dayjs(e) : null)
                }}
              />
            </Stack>
          </Grid.Col>
          {/* Hints column stretches to fill the row when the score /
              difficulty / ScoreFunc columns are hidden (A&D mode), so
              the grid doesn't leave two empty 1/3-width slots staring
              at the user. */}
          <Grid.Col span={{ base: 3, md: isAdEngine ? 3 : 1 }}>
            <Stack gap="sm">
              <HintList
                label={
                  <Group gap="sm">
                    <Text size="sm">{t('admin.content.games.challenges.hints')}</Text>
                    <Text size="xs" c="dimmed">
                      {t('admin.content.markdown_inline_support')}
                    </Text>
                  </Group>
                }
                hints={challengeInfo?.hints ?? []}
                disabled={disabled}
                height={180}
                onChangeHint={(hints) => setChallengeInfo({ ...challengeInfo, hints })}
              />
            </Stack>
          </Grid.Col>
          {!isAdEngine && (
            <>
              <Grid.Col span={{ base: 3, md: 1 }}>
                <Stack h="100%">
                  <Group wrap="wrap" grow>
                    <NumberInput
                      label={t('admin.content.games.challenges.score')}
                      min={0}
                      required
                      disabled={disabled}
                      stepHoldDelay={500}
                      stepHoldInterval={(t) => Math.max(1000 / t ** 2, 25)}
                      value={challengeInfo?.originalScore ?? 500}
                      onChange={(e) => {
                        const number = getInputNumber(e)
                        if (isNaN(number)) return
                        setChallengeInfo({ ...challengeInfo, originalScore: number })
                      }}
                    />
                    <NumberInput
                      label={t('admin.content.games.challenges.difficulty')}
                      decimalScale={2}
                      fixedDecimalScale
                      step={0.2}
                      min={0.1}
                      required
                      disabled={disabled}
                      value={challengeInfo?.difficulty ?? 100}
                      stepHoldDelay={500}
                      stepHoldInterval={(t) => Math.max(1000 / t ** 2, 25)}
                      onChange={(e) => {
                        const number = getInputNumber(e, true)
                        if (isNaN(number)) return
                        setChallengeInfo({ ...challengeInfo, difficulty: number })
                      }}
                    />
                  </Group>
                  <Select
                    label={t('admin.content.games.challenges.score_curve.label')}
                    description={t('admin.content.games.challenges.score_curve.description')}
                    disabled={disabled}
                    allowDeselect={false}
                    value={challengeInfo?.scoreCurve ?? ScoreCurve.Standard}
                    data={[
                      { value: ScoreCurve.Standard, label: t('admin.content.games.challenges.score_curve.standard') },
                      { value: ScoreCurve.Linear, label: t('admin.content.games.challenges.score_curve.linear') },
                      {
                        value: ScoreCurve.Logarithmic,
                        label: t('admin.content.games.challenges.score_curve.logarithmic'),
                      },
                    ]}
                    onChange={(e) =>
                      setChallengeInfo({ ...challengeInfo, scoreCurve: (e as ScoreCurve) ?? ScoreCurve.Standard })
                    }
                  />
                  <Input.Wrapper label={t('admin.content.games.challenges.min_score_radio.label')} h="3.8rem" required>
                    <Slider
                      thumbLabel={t('admin.content.games.challenges.min_score_radio.label')}
                      label={(value) =>
                        t('admin.content.games.challenges.min_score_radio.description', {
                          min_score: ((value / 100) * (challengeInfo?.originalScore ?? 500)).toFixed(0),
                        })
                      }
                      disabled={disabled}
                      value={minRate}
                      marks={[
                        { value: 20, label: '20%' },
                        { value: 50, label: '50%' },
                        { value: 80, label: '80%' },
                      ]}
                      onChange={setMinRate}
                      classNames={{ label: misc.challEditLabel }}
                    />
                  </Input.Wrapper>
                  <Switch
                    disabled={disabled}
                    checked={!challengeInfo?.disableBloodBonus}
                    label={SwitchLabel(
                      t('admin.content.games.challenges.blood_bonus.label'),
                      t('admin.content.games.challenges.blood_bonus.description')
                    )}
                    onChange={(e) => setChallengeInfo({ ...challengeInfo, disableBloodBonus: !e.target.checked })}
                  />
                </Stack>
              </Grid.Col>
              <Grid.Col span={{ base: 3, md: 1 }}>
                <ScoreFunc
                  currentAcceptCount={currentAcceptCount}
                  originalScore={challengeInfo.originalScore ?? 500}
                  minScoreRate={minRate / 100}
                  difficulty={challengeInfo.difficulty ?? 30}
                  curve={challengeInfo?.scoreCurve ?? ScoreCurve.Standard}
                />
              </Grid.Col>
            </>
          )}
        </Grid>
        {type === ChallengeType.DynamicAttachment && (
          <TextInput
            label={t('admin.content.games.challenges.attachment_name.label')}
            description={t('admin.content.games.challenges.attachment_name.description')}
            disabled={disabled}
            value={challengeInfo.fileName ?? 'attachment'}
            onChange={(e) => setChallengeInfo({ ...challengeInfo, fileName: e.target.value })}
          />
        )}

        {!isAdEngine && (
          <Paper p="md" withBorder>
            <Stack gap="md">
              <Stack gap={2}>
                <Title order={3}>{t('admin.event_security.challenge_title', 'Challenge provenance')}</Title>
                <Text size="sm" c="dimmed">
                  {t(
                    'admin.event_security.challenge_description',
                    'Optional deterministic per-team variants and one-use trusted-verifier receipts. These settings freeze when the event starts.'
                  )}
                </Text>
              </Stack>
              <SimpleGrid cols={{ base: 1, md: 2 }} spacing="md">
                <Select
                  label={t('admin.event_security.variant_mode', 'Variant mode')}
                  allowDeselect={false}
                  disabled={disabled || eventSecurityFrozen}
                  value={challengeInfo.variantMode ?? ChallengeVariantMode.Disabled}
                  data={[
                    { value: ChallengeVariantMode.Disabled, label: t('common.content.disabled', 'Disabled') },
                    {
                      value: ChallengeVariantMode.PerParticipation,
                      label: t('admin.event_security.per_participation', 'Per participation'),
                    },
                  ]}
                  onChange={(value) =>
                    setChallengeInfo({
                      ...challengeInfo,
                      variantMode: (value as ChallengeVariantMode) ?? ChallengeVariantMode.Disabled,
                    })
                  }
                />
                <Select
                  label={t('admin.event_security.receipt_mode', 'Trusted solve receipt')}
                  allowDeselect={false}
                  disabled={disabled || eventSecurityFrozen}
                  value={challengeInfo.solveReceiptMode ?? SolveReceiptMode.Disabled}
                  data={[
                    { value: SolveReceiptMode.Disabled, label: t('common.content.disabled', 'Disabled') },
                    { value: SolveReceiptMode.Optional, label: t('admin.event_security.optional', 'Optional') },
                    { value: SolveReceiptMode.Required, label: t('admin.event_security.required', 'Required') },
                  ]}
                  onChange={(value) =>
                    setChallengeInfo({
                      ...challengeInfo,
                      solveReceiptMode: (value as SolveReceiptMode) ?? SolveReceiptMode.Disabled,
                    })
                  }
                />
                {(challengeInfo.variantMode ?? ChallengeVariantMode.Disabled) !== ChallengeVariantMode.Disabled &&
                  (challenge?.variantGeneratorManaged ? (
                    <Stack gap="xs">
                      <Text size="sm" fw={500}>
                        {t('admin.event_security.managed_generator', 'Repository generator')}
                      </Text>
                      <Text size="sm" c="dimmed">
                        {t(
                          'admin.event_security.managed_generator_description',
                          'Built automatically from generator/Dockerfile. The immutable identity is managed by rsctf and is not written to challenge.yaml.'
                        )}
                      </Text>
                      {challenge.variantGeneratorResolvedDigest && (
                        <Code block>{challenge.variantGeneratorResolvedDigest}</Code>
                      )}
                      <BuildLogSection
                        buildStatus={challenge.variantGeneratorBuildStatus}
                        lastBuildLog={challenge.variantGeneratorLastBuildLog ?? null}
                      />
                    </Stack>
                  ) : (
                    <>
                      <TextInput
                        required
                        disabled={disabled || eventSecurityFrozen}
                        label={t('admin.event_security.generator_image', 'Generator image@digest')}
                        description={t(
                          'admin.event_security.generator_image_description',
                          'Immutable repository reference ending in the same sha256 digest below.'
                        )}
                        value={challengeInfo.variantGeneratorImage ?? ''}
                        onChange={(event) =>
                          setChallengeInfo({ ...challengeInfo, variantGeneratorImage: event.currentTarget.value })
                        }
                      />
                      <TextInput
                        required
                        disabled={disabled || eventSecurityFrozen}
                        label={t('admin.event_security.generator_digest', 'Generator sha256 digest')}
                        placeholder="sha256:…"
                        value={challengeInfo.variantGeneratorDigest ?? ''}
                        onChange={(event) =>
                          setChallengeInfo({ ...challengeInfo, variantGeneratorDigest: event.currentTarget.value })
                        }
                      />
                    </>
                  ))}
                {(challengeInfo.solveReceiptMode ?? SolveReceiptMode.Disabled) !== SolveReceiptMode.Disabled && (
                  <TextInput
                    required
                    disabled={disabled || eventSecurityFrozen}
                    label={t('admin.event_security.verifier_identity', 'Verifier identity')}
                    description={t(
                      'admin.event_security.verifier_identity_description',
                      'Exact trusted issuer name bound into every short-lived, one-use receipt.'
                    )}
                    value={challengeInfo.receiptVerifierIdentity ?? ''}
                    onChange={(event) =>
                      setChallengeInfo({ ...challengeInfo, receiptVerifierIdentity: event.currentTarget.value })
                    }
                  />
                )}
              </SimpleGrid>
              {eventSecurityFrozen && (
                <Text size="xs" c="dimmed">
                  {t(
                    'admin.event_security.frozen',
                    'Variant and receipt policy is frozen because the event has started.'
                  )}
                </Text>
              )}
            </Stack>
          </Paper>
        )}
        {(type === ChallengeType.StaticContainer || type === ChallengeType.DynamicContainer || isAdEngine) && (
          <Grid columns={12}>
            <Grid.Col span={{ base: 12, lg: 8 }}>
              <Group justify="space-between" align="flex-end" wrap="wrap">
                <TextInput
                  label={t('admin.content.games.challenges.container_image')}
                  disabled={disabled}
                  value={challengeInfo.containerImage ?? ''}
                  required
                  onChange={(e) => setChallengeInfo({ ...challengeInfo, containerImage: e.target.value })}
                  classNames={{ root: misc.flexGrow }}
                  style={{ flex: '1 1 18rem' }}
                />
                <NumberInput
                  label={t('admin.content.games.challenges.service_port.label')}
                  min={1}
                  max={65535}
                  w={{ base: '100%', xs: '8rem' }}
                  required
                  disabled={disabled}
                  stepHoldDelay={500}
                  stepHoldInterval={(t) => Math.max(1000 / t ** 2, 25)}
                  value={challengeInfo.exposePort ?? 80}
                  onChange={(e) => {
                    const number = getInputNumber(e)
                    if (isNaN(number)) return
                    setChallengeInfo({ ...challengeInfo, exposePort: number })
                  }}
                />
                <Button
                  w={{ base: '100%', xs: 'auto' }}
                  miw={{ base: 0, xs: '8rem' }}
                  color={challenge?.testContainer ? 'orange' : 'green'}
                  c="dark.9"
                  disabled={disabled}
                  onClick={onToggleTestContainer}
                >
                  {challenge?.testContainer
                    ? t('admin.button.challenges.test_container.destroy')
                    : t('admin.button.challenges.test_container.create')}
                </Button>
                <Button
                  w={{ base: '100%', xs: 'auto' }}
                  miw={{ base: 0, xs: '6rem' }}
                  variant="default"
                  disabled={disabled || !challenge?.testContainer || challenge.testContainer.status !== 'Running'}
                  onClick={() => setExecOpened(true)}
                >
                  {t('admin.button.challenges.test_container.shell')}
                </Button>
              </Group>
            </Grid.Col>
            <Grid.Col span={{ base: 12, lg: 4 }}>
              <InstanceEntry
                test
                label={`${challenge?.title} @ ${game?.title} (test)`}
                disabled={disabled}
                context={{
                  closeTime: challenge?.testContainer?.expectStopAt,
                  instanceId: challenge?.testContainer?.id,
                  instanceEntry: challenge?.testContainer?.entry,
                }}
              />
            </Grid.Col>
            <Grid.Col span={{ base: 12, xs: 6, lg: 2 }}>
              <Select
                required
                label={t('admin.content.games.challenges.network_mode.label')}
                description={t('admin.content.games.challenges.network_mode.description')}
                value={isAdEngine ? NetworkMode.Open : (networkMode ?? NetworkMode.Open)}
                disabled={disabled || isAdEngine}
                onChange={(e) => {
                  setNetworkMode(e)
                  setChallengeInfo({ ...challengeInfo, networkMode: e as NetworkMode })
                }}
                renderOption={NetworkModeItem}
                data={(isAdEngine ? [NetworkMode.Open] : NetworkModeList).map((mode) => {
                  const data = networkModeLabelMap.get(mode)
                  return { value: mode, ...data } as ComboboxItem
                })}
              />
            </Grid.Col>
            <Grid.Col span={{ base: 12, xs: 6, lg: 2 }}>
              <NumberInput
                label={t('admin.content.games.challenges.cpu_limit.label')}
                description={t('admin.content.games.challenges.cpu_limit.description')}
                min={1}
                max={1024}
                required
                disabled={disabled}
                stepHoldDelay={500}
                stepHoldInterval={(t) => Math.max(1000 / t ** 2, 25)}
                value={challengeInfo.cpuCount ?? 1}
                onChange={(e) => {
                  const number = getInputNumber(e)
                  if (isNaN(number)) return
                  setChallengeInfo({ ...challengeInfo, cpuCount: number })
                }}
              />
            </Grid.Col>
            <Grid.Col span={{ base: 12, xs: 6, lg: 2 }}>
              <NumberInput
                label={t('admin.content.games.challenges.memory_limit.label')}
                description={t('admin.content.games.challenges.memory_limit.description')}
                min={32}
                max={1048576}
                required
                disabled={disabled}
                stepHoldDelay={500}
                stepHoldInterval={(t) => Math.max(1000 / t ** 2, 25)}
                value={challengeInfo.memoryLimit ?? 32}
                onChange={(e) => {
                  const number = getInputNumber(e)
                  if (isNaN(number)) return
                  setChallengeInfo({ ...challengeInfo, memoryLimit: number })
                }}
              />
            </Grid.Col>
            <Grid.Col span={{ base: 12, xs: 6, lg: 2 }}>
              <NumberInput
                label={t('admin.content.games.challenges.storage_limit.label')}
                description={t('admin.content.games.challenges.storage_limit.description')}
                min={0}
                max={1048576}
                required
                disabled={disabled}
                stepHoldDelay={500}
                stepHoldInterval={(t) => Math.max(1000 / t ** 2, 25)}
                value={challengeInfo.storageLimit ?? 32}
                onChange={(e) => {
                  const number = getInputNumber(e)
                  if (isNaN(number)) return
                  setChallengeInfo({ ...challengeInfo, storageLimit: number })
                }}
              />
            </Grid.Col>
            {isContainerType && challenge?.storageQuotaEnforced === false && (
              <Grid.Col span={12}>
                <Alert
                  color="orange"
                  variant="light"
                  title={t('admin.content.games.challenges.storage_limit.unbounded_title')}
                  icon={<Icon path={mdiAlertCircleOutline} size={1} aria-hidden />}
                >
                  {t('admin.content.games.challenges.storage_limit.unbounded_warning')}
                </Alert>
              </Grid.Col>
            )}
            <Grid.Col span={{ base: 12, lg: 4 }} display="flex" className={misc.alignCenter}>
              <Switch
                disabled={disabled}
                checked={challengeInfo.enableTrafficCapture ?? false}
                label={SwitchLabel(
                  t('admin.content.games.challenges.traffic_capture.label'),
                  t('admin.content.games.challenges.traffic_capture.description')
                )}
                onChange={(e) => setChallengeInfo({ ...challengeInfo, enableTrafficCapture: e.target.checked })}
              />
            </Grid.Col>
            {type === ChallengeType.StaticContainer && (
              <Grid.Col span={{ base: 12, lg: 4 }} display="flex" className={misc.alignCenter}>
                <Switch
                  disabled={disabled}
                  checked={challengeInfo.enableSharedContainer ?? false}
                  label={SwitchLabel(
                    t('admin.content.games.challenges.shared_container.label', 'Shared instance'),
                    t(
                      'admin.content.games.challenges.shared_container.description',
                      'All teams connect to one shared container instead of one per team. Saves resources; the static flag is the same for everyone.'
                    )
                  )}
                  onChange={(e) => setChallengeInfo({ ...challengeInfo, enableSharedContainer: e.target.checked })}
                />
              </Grid.Col>
            )}
          </Grid>
        )}

        {isJeopardyContainer && (
          <Paper p="md" withBorder>
            <Stack gap="md">
              <Group justify="space-between" align="flex-start" wrap="wrap">
                <Stack gap={2} style={{ flex: '1 1 24rem' }}>
                  <Title order={3}>{t('admin.content.games.challenges.workload_spec.title')}</Title>
                  <Text id={WORKLOAD_SPEC_SECTION_HELP_ID} size="sm" c="dimmed">
                    {t('admin.content.games.challenges.workload_spec.section_help')}
                  </Text>
                </Stack>
                <Switch
                  ref={workloadToggleRef}
                  disabled={disabled}
                  checked={workloadEditorEnabled}
                  label={t('admin.content.games.challenges.workload_spec.enable_label')}
                  aria-describedby={WORKLOAD_SPEC_SECTION_HELP_ID}
                  onChange={(event) => {
                    if (event.currentTarget.checked) {
                      enableWorkloadEditor()
                    } else {
                      disableWorkloadEditor()
                    }
                  }}
                />
              </Group>
              {workloadEditorEnabled && (
                <Stack gap="sm">
                  <Textarea
                    ref={workloadInputRef}
                    id="challenge-workload-spec"
                    label={t('admin.content.games.challenges.workload_spec.editor_label')}
                    description={t('admin.content.games.challenges.workload_spec.editor_description')}
                    descriptionProps={{ id: WORKLOAD_SPEC_EDITOR_HELP_ID }}
                    error={
                      workloadError
                        ? t('admin.content.games.challenges.workload_spec.invalid', { message: workloadError })
                        : undefined
                    }
                    errorProps={{ id: WORKLOAD_SPEC_ERROR_ID, role: 'alert', 'aria-atomic': true }}
                    aria-describedby={`${WORKLOAD_SPEC_EDITOR_HELP_ID}${
                      workloadError ? ` ${WORKLOAD_SPEC_ERROR_ID}` : ''
                    }`}
                    aria-invalid={workloadError !== null}
                    disabled={disabled}
                    required
                    autosize
                    minRows={16}
                    maxRows={28}
                    spellCheck={false}
                    styles={{ input: { fontFamily: 'monospace' } }}
                    value={workloadJson}
                    onChange={(event) => {
                      setWorkloadJson(event.currentTarget.value)
                      setWorkloadError(null)
                    }}
                    onBlur={() => validateWorkloadEditor()}
                  />
                  {rolloutBlockedByStatefulService && (
                    <Text id={WORKLOAD_SPEC_ROLLOUT_HELP_ID} size="sm" role="status" aria-live="polite">
                      {t('admin.content.games.challenges.workload_spec.rollout_requires_stateless', {
                        services: statefulRolloutServices.join(', '),
                      })}
                    </Text>
                  )}
                  <Group justify="flex-end" wrap="wrap">
                    <Button
                      type="button"
                      disabled={disabled || rollingWorkload || rolloutBlockedByStatefulService}
                      loading={rollingWorkload}
                      aria-describedby={rolloutBlockedByStatefulService ? WORKLOAD_SPEC_ROLLOUT_HELP_ID : undefined}
                      onClick={onRolloutWorkloads}
                    >
                      {t('admin.content.games.challenges.workload_spec.save_and_rollout')}
                    </Button>
                    <Button
                      type="button"
                      variant="default"
                      disabled={disabled}
                      onClick={() => {
                        setWorkloadJson(formatWorkloadSpec(createDefaultJeopardyWorkloadSpec()))
                        setWorkloadError(null)
                        window.requestAnimationFrame(() => workloadInputRef.current?.focus())
                      }}
                    >
                      {t('admin.content.games.challenges.workload_spec.load_example')}
                    </Button>
                    <Button
                      type="button"
                      color="red"
                      variant="outline"
                      disabled={disabled}
                      onClick={clearWorkloadEditor}
                    >
                      {t('admin.content.games.challenges.workload_spec.clear')}
                    </Button>
                  </Group>
                </Stack>
              )}
            </Stack>
          </Paper>
        )}

        {/* Attack & Defense / King of the Hill — per-challenge config */}
        {isAdEngine && (
          <Stack gap="sm">
            <Divider
              label={
                isKoth
                  ? t('admin.content.games.challenges.koth.title', 'King of the Hill')
                  : t('admin.content.games.challenges.ad.title', 'Attack & Defense')
              }
              labelPosition="left"
            />
            <Text size="sm" c="dimmed">
              {isKoth
                ? t(
                    'admin.content.games.challenges.koth.description',
                    'Per-challenge config for the shared hill (checker + egress).'
                  )
                : t(
                    'admin.content.games.challenges.ad.description',
                    'Per-challenge config for Attack & Defense (checker image, egress, and self-reset).'
                  )}
            </Text>
            <Grid columns={12}>
              <Grid.Col span={{ base: 12, sm: 6 }}>
                <TextInput
                  label={t('admin.content.games.challenges.ad.checker_image.label', 'Checker image')}
                  description={t(
                    'admin.content.games.challenges.ad.checker_image.description',
                    'Image that probes the team service each tick to plant and retrieve flags.'
                  )}
                  placeholder="ghcr.io/myorg/vuln-flask-checker:1.0"
                  disabled={disabled || adScoringStarted}
                  value={challengeInfo.adCheckerImage ?? ''}
                  onChange={(e) => setChallengeInfo({ ...challengeInfo, adCheckerImage: e.target.value })}
                />
              </Grid.Col>
              {!isKoth && (
                <Grid.Col span={{ base: 12, sm: 6 }}>
                  <NumberInput
                    label={t('admin.content.games.challenges.ad.scoring_weight.label', 'Epoch service weight')}
                    description={t(
                      'admin.content.games.challenges.ad.scoring_weight.description',
                      '0.80–1.20. Use modestly for services that are genuinely harder or less AI-sloppable. Weights are normalized, so every epoch remains capped at 100. Locked once scoring starts.'
                    )}
                    disabled={disabled || adScoringStarted}
                    min={0.8}
                    max={1.2}
                    step={0.05}
                    decimalScale={2}
                    value={challengeInfo.adScoringWeight ?? 1}
                    onChange={(e) => {
                      const weight = getInputNumber(e, true)
                      if (!isNaN(weight)) setChallengeInfo({ ...challengeInfo, adScoringWeight: weight })
                    }}
                  />
                </Grid.Col>
              )}
              {/* tick / flag-lifetime / reset-cooldown / snapshot-download AND the
                  checker timing knobs (getflag jitter window + min grace period)
                  are event-wide now — edit them in the game's settings (Info tab),
                  not per challenge. The putflag jitter window was removed (flags
                  plant at round start; it was never honored). */}
              <Grid.Col span={{ base: 12, sm: 6 }} display="flex" className={misc.alignCenter}>
                <Switch
                  disabled={disabled}
                  checked={challengeInfo.adAllowEgress ?? true}
                  label={SwitchLabel(
                    t('admin.content.games.challenges.ad.allow_egress.label', 'Allow egress'),
                    t(
                      'admin.content.games.challenges.ad.allow_egress.description',
                      'Permit the challenge container to make outbound network connections.'
                    )
                  )}
                  onChange={(e) => setChallengeInfo({ ...challengeInfo, adAllowEgress: e.target.checked })}
                />
              </Grid.Col>
              {/* Self-reset is per-team-container only — meaningless for a KotH shared
                  hill (no per-team box to rebuild), so don't show the no-op switch. */}
              {!isKoth && (
                <Grid.Col span={{ base: 12, sm: 6 }} display="flex" className={misc.alignCenter}>
                  <Switch
                    disabled={disabled}
                    checked={challengeInfo.adAllowSelfReset ?? true}
                    label={SwitchLabel(
                      t('admin.content.games.challenges.ad.allow_self_reset.label', 'Allow self-reset'),
                      t(
                        'admin.content.games.challenges.ad.allow_self_reset.description',
                        'Let teams rebuild their own challenge container to clear a botched patch.'
                      )
                    )}
                    onChange={(e) => setChallengeInfo({ ...challengeInfo, adAllowSelfReset: e.target.checked })}
                  />
                </Grid.Col>
              )}
              {/* Offense-gates-defense: only let a team SSH into its box once it has
                  captured a flag for this challenge. Per-team container only → hide for KotH. */}
              {!isKoth && (
                <Grid.Col span={{ base: 12, sm: 6 }} display="flex" className={misc.alignCenter}>
                  <Switch
                    disabled={disabled}
                    checked={challengeInfo.adSshRequiresFlag ?? false}
                    label={SwitchLabel(
                      t('admin.content.games.challenges.ad.ssh_requires_flag.label', 'SSH requires a captured flag'),
                      t(
                        'admin.content.games.challenges.ad.ssh_requires_flag.description',
                        'Teams can only SSH into their service container after submitting at least one accepted captured flag for this challenge.'
                      )
                    )}
                    onChange={(e) => setChallengeInfo({ ...challengeInfo, adSshRequiresFlag: e.target.checked })}
                  />
                </Grid.Col>
              )}
              {/* Bring-your-own-container: the team runs the service on their own
                  machine and connects it to the game via an RSCTF relay. Per-team
                  service model → hide for KotH (single shared hill). */}
              {!isKoth && (
                <Grid.Col span={{ base: 12, sm: 6 }} display="flex" className={misc.alignCenter}>
                  <Switch
                    disabled={disabled}
                    checked={challengeInfo.adSelfHosted ?? false}
                    label={SwitchLabel(
                      t(
                        'admin.content.games.challenges.ad.self_hosted.label',
                        'Self-hosted / Bring Your Own Container (BYOC)'
                      ),
                      t(
                        'admin.content.games.challenges.ad.self_hosted.description',
                        'Teams run the service container on their own machine and connect it to the game network through an RSCTF relay, instead of RSCTF hosting it. The checker, attack proxy, and flag rotation still apply.'
                      )
                    )}
                    onChange={(e) => setChallengeInfo({ ...challengeInfo, adSelfHosted: e.target.checked })}
                  />
                </Grid.Col>
              )}
            </Grid>
          </Stack>
        )}

        {isBuildable && challenge?.buildStatus && challenge.buildStatus !== 'None' && (
          <BuildLogSection buildStatus={challenge.buildStatus} lastBuildLog={challenge.lastBuildLog ?? null} />
        )}
      </Stack>
      <ChallengePreviewModal
        size="min(56rem, calc(100vw - 2rem))"
        challenge={{
          title: tryDefault([challengeInfo?.title, challenge?.title], ''),
          content: tryDefault([challengeInfo?.content, challenge?.content]),
          hints: tryDefault([challengeInfo?.hints, challenge?.hints], []),
          score: tryDefault([challengeInfo?.originalScore, challenge?.originalScore], 0),
          limit: tryDefault([challengeInfo?.submissionLimit, challenge?.submissionLimit], 0),
          category: category as ChallengeCategory,
          deadline: deadline ? deadline.valueOf() : undefined,
          type: (type as ChallengeType) ?? ChallengeType.StaticAttachment,
        }}
        context={{
          closeTime: challenge?.testContainer?.expectStopAt ?? null,
          instanceId: challenge?.testContainer?.id ?? null,
          instanceEntry: challenge?.testContainer?.entry ?? null,
          url:
            challenge?.attachment?.url ??
            challenge?.flags.find((flag) => !!flag.attachment?.url)?.attachment?.url ??
            null,
          fileSize:
            challenge?.attachment?.fileSize ??
            challenge?.flags.find((flag) => !!flag.attachment?.url)?.attachment?.fileSize ??
            null,
        }}
        disabled={disabled}
        gameTitle={game?.title}
        onCreateInstance={onToggleTestContainer}
        onDestroyInstance={onToggleTestContainer}
        opened={previewOpened}
        onClose={() => setPreviewOpened(false)}
        cateData={challengeCategoryLabelMap.get(
          (challengeInfo?.category as ChallengeCategory) ?? ChallengeCategory.Misc
        )!}
      />
      <ContainerExecModal
        size="min(72rem, calc(100vw - 2rem))"
        containerGuid={challenge?.testContainer?.id ?? null}
        containerTitle={`${challenge?.title} (test)`}
        scopedGameId={numId}
        opened={execOpened}
        onClose={() => setExecOpened(false)}
      />
    </WithChallengeEdit>
  )
}

export default GameChallengeEdit
