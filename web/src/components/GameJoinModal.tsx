import { Alert, Button, Select, Stack, TextInput } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AccessibleModal, AccessibleModalProps } from '@Components/AccessibleModal'
import { showErrorMsg, tryGetErrorMsg } from '@Utils/Shared'
import api, { DetailedGameInfoModel, GameJoinCheckInfoModel, GameJoinModel, TeamSelectorModel } from '@Api'

interface GameJoinModalProps extends AccessibleModalProps {
  accountId?: string
  game?: DetailedGameInfoModel
  gameId: number
  teams?: TeamSelectorModel[]
  refreshTeams: () => Promise<TeamSelectorModel[] | undefined>
  onSubmitJoin: (info: GameJoinModel, signal: AbortSignal) => Promise<void>
}

interface JoinContext {
  checkInfo: GameJoinCheckInfoModel
  teams: TeamSelectorModel[]
}

const teamSignature = (teams: TeamSelectorModel[] | undefined) =>
  (teams ?? [])
    .flatMap((team) => (typeof team.id === 'number' ? [`${team.id}:${team.name ?? ''}`] : []))
    .sort()
    .join('|')

export const GameJoinModal: FC<GameJoinModalProps> = ({
  accountId,
  game,
  gameId,
  teams,
  refreshTeams,
  onSubmitJoin,
  onClose,
  opened,
  ...modalProps
}) => {
  const [inviteCode, setInviteCode] = useState('')
  const [divisionId, setDivisionId] = useState('')
  const [team, setTeam] = useState<string | null>(null)
  const [joining, setJoining] = useState(false)
  const [joinContext, setJoinContext] = useState<JoinContext | null>(null)
  const [contextError, setContextError] = useState<string | null>(null)
  const [submissionError, setSubmissionError] = useState<string | null>(null)
  const [fieldError, setFieldError] = useState<'team' | 'division' | 'invite' | null>(null)
  const [errorFocus, setErrorFocus] = useState<'team' | 'division' | 'invite' | null>(null)

  const validationGeneration = useRef(0)
  const validationAbort = useRef<AbortController | null>(null)
  const verifiedTeamsVisible = useRef(false)
  const submissionGeneration = useRef(0)
  const submissionInFlight = useRef(false)
  const submissionAbort = useRef<AbortController | null>(null)
  const teamInputRef = useRef<HTMLInputElement>(null)
  const divisionInputRef = useRef<HTMLInputElement>(null)
  const inviteInputRef = useRef<HTMLInputElement>(null)
  const { t } = useTranslation()

  const resetForm = useCallback(() => {
    setInviteCode('')
    setDivisionId('')
    setTeam(null)
    setSubmissionError(null)
    setFieldError(null)
    setErrorFocus(null)
  }, [])

  const invalidateWork = useCallback(() => {
    validationGeneration.current += 1
    validationAbort.current?.abort()
    validationAbort.current = null
    submissionGeneration.current += 1
    submissionAbort.current?.abort()
    submissionAbort.current = null
    submissionInFlight.current = false
  }, [])

  useEffect(() => () => invalidateWork(), [invalidateWork])

  const refreshJoinContext = useCallback(async () => {
    const generation = ++validationGeneration.current
    validationAbort.current?.abort()
    const controller = new AbortController()
    validationAbort.current = controller
    verifiedTeamsVisible.current = false
    setJoinContext(null)
    setContextError(null)

    try {
      const [freshTeams, checkResponse] = await Promise.all([
        refreshTeams(),
        api.game.gameGetGameJoinCheckInfo(gameId, { signal: controller.signal }),
      ])
      if (generation !== validationGeneration.current || controller.signal.aborted) return
      if (!freshTeams) throw new Error(t('game.content.join.check_failed', 'Could not verify your current teams.'))
      setJoinContext({ teams: freshTeams, checkInfo: checkResponse.data })
    } catch (error) {
      if (generation !== validationGeneration.current || controller.signal.aborted) return
      setContextError(tryGetErrorMsg(error, t))
    } finally {
      if (generation === validationGeneration.current) validationAbort.current = null
    }
  }, [gameId, refreshTeams, t])

  useEffect(() => {
    invalidateWork()
    setJoining(false)
    setJoinContext(null)
    setContextError(null)
    setSubmissionError(null)
    setFieldError(null)
    setErrorFocus(null)
    if (opened && gameId > 0 && accountId) void refreshJoinContext()
  }, [accountId, gameId, invalidateWork, opened, refreshJoinContext])

  const currentTeamsSignature = teamSignature(teams)
  const validatedTeamsSignature = teamSignature(joinContext?.teams)
  useEffect(() => {
    if (!joinContext || !teams || currentTeamsSignature === validatedTeamsSignature) return
    if (!verifiedTeamsVisible.current) return
    setJoinContext((current) => (current ? { ...current, teams } : current))
  }, [currentTeamsSignature, joinContext, teams, validatedTeamsSignature])

  useEffect(() => {
    if (joinContext && currentTeamsSignature === validatedTeamsSignature) verifiedTeamsVisible.current = true
  }, [currentTeamsSignature, joinContext, validatedTeamsSignature])

  const currentGame = game?.id === gameId ? game : undefined
  const currentTeams = joinContext?.teams ?? []
  const checkInfo = joinContext?.checkInfo

  const teamsData = useMemo(
    () =>
      currentTeams.flatMap((currentTeam) =>
        typeof currentTeam.id === 'number'
          ? [{ label: currentTeam.name ?? `Team #${currentTeam.id}`, value: currentTeam.id.toString() }]
          : []
      ),
    [currentTeams]
  )

  const gameCheckInfo = useMemo(() => {
    const map = new Map<string, number>()
    checkInfo?.joinedTeams?.forEach((joinedTeam) => map.set(joinedTeam.id.toString(), joinedTeam.division))
    return map
  }, [checkInfo])

  const joinedDivisionId = team ? gameCheckInfo.get(team) : undefined
  const joinedDivision =
    typeof joinedDivisionId === 'number'
      ? (currentGame?.divisions?.find((division) => division.id === joinedDivisionId) ?? null)
      : null
  const canSelectDivision = typeof joinedDivisionId !== 'number'
  const gameHasDivisions = (currentGame?.divisions?.length ?? 0) > 0

  const divisionOptions = useMemo(
    () =>
      (currentGame?.divisions ?? [])
        .filter(
          (division) =>
            typeof division.id === 'number' &&
            (!checkInfo?.joinableDivisions || checkInfo.joinableDivisions.includes(division.id))
        )
        .map((division) => ({
          value: division.id!.toString(),
          label: division.name ?? `Division #${division.id}`,
        })),
    [checkInfo?.joinableDivisions, currentGame?.divisions]
  )

  useEffect(() => {
    if (!joinContext) return
    setTeam((current) =>
      current && teamsData.some((option) => option.value === current) ? current : (teamsData[0]?.value ?? null)
    )
  }, [joinContext, teamsData])

  useEffect(() => {
    if (!joinContext || !canSelectDivision || !gameHasDivisions) {
      setDivisionId('')
      return
    }

    setDivisionId((current) => {
      if (divisionOptions.some((option) => option.value === current)) return current
      const preferred = currentGame?.division?.toString()
      return divisionOptions.some((option) => option.value === preferred)
        ? preferred!
        : (divisionOptions[0]?.value ?? '')
    })
  }, [canSelectDivision, currentGame?.division, divisionOptions, gameHasDivisions, joinContext])

  const selectedDivision = useMemo(
    () => currentGame?.divisions?.find((division) => division.id?.toString() === divisionId) ?? null,
    [currentGame?.divisions, divisionId]
  )
  const effectiveDivision = canSelectDivision ? selectedDivision : joinedDivision
  const shouldRequireInviteCode = gameHasDivisions
    ? Boolean(effectiveDivision?.inviteCodeRequired)
    : Boolean(currentGame?.inviteCodeRequired)

  useEffect(() => {
    if (!shouldRequireInviteCode) setInviteCode('')
  }, [shouldRequireInviteCode])

  const teamIsCurrent = Boolean(team && teamsData.some((option) => option.value === team))
  const divisionIsCurrent = !gameHasDivisions
    ? true
    : canSelectDivision
      ? divisionOptions.some((option) => option.value === divisionId)
      : Boolean(joinedDivision)
  const contextReady = Boolean(joinContext && currentGame && accountId)
  const canSubmit =
    contextReady &&
    teamIsCurrent &&
    divisionIsCurrent &&
    (!shouldRequireInviteCode || inviteCode.trim().length > 0) &&
    !joining

  const focusRelevantField = (field: 'team' | 'division' | 'invite') => {
    const target = field === 'team' ? teamInputRef : field === 'division' ? divisionInputRef : inviteInputRef
    target.current?.focus()
  }

  useEffect(() => {
    if (!submissionError || !errorFocus) return
    const timer = setTimeout(() => focusRelevantField(errorFocus), 0)
    return () => clearTimeout(timer)
  }, [errorFocus, submissionError])

  const rejectField = (field: 'team' | 'division' | 'invite', message: string) => {
    setFieldError(field)
    setErrorFocus(field)
    setSubmissionError(message)
    showNotification({
      color: 'orange',
      message,
      icon: <Icon path={mdiClose} size={1} />,
    })
  }

  const onJoinGame = async () => {
    if (submissionInFlight.current) return
    setSubmissionError(null)
    setFieldError(null)
    setErrorFocus(null)

    if (!contextReady || !teamIsCurrent) {
      rejectField('team', t('game.notification.no_team'))
      return
    }
    if (!divisionIsCurrent) {
      rejectField('division', t('game.notification.no_division'))
      return
    }
    if (shouldRequireInviteCode && !inviteCode.trim()) {
      rejectField('invite', t('game.notification.no_invite_code'))
      return
    }

    const generation = ++submissionGeneration.current
    const controller = new AbortController()
    submissionAbort.current?.abort()
    submissionAbort.current = controller
    submissionInFlight.current = true
    setJoining(true)
    try {
      await onSubmitJoin(
        {
          teamId: Number.parseInt(team!, 10),
          inviteCode: shouldRequireInviteCode ? inviteCode : undefined,
          divisionId: gameHasDivisions
            ? canSelectDivision
              ? Number.parseInt(divisionId, 10)
              : joinedDivision?.id
            : undefined,
        },
        controller.signal
      )
      if (generation !== submissionGeneration.current) return
      resetForm()
      onClose()
    } catch (error) {
      if (generation !== submissionGeneration.current) return
      const message = tryGetErrorMsg(error, t)
      const focus = shouldRequireInviteCode ? 'invite' : canSelectDivision && gameHasDivisions ? 'division' : 'team'
      setErrorFocus(focus)
      setSubmissionError(message)
      showErrorMsg(error, t)
    } finally {
      if (generation === submissionGeneration.current) {
        submissionAbort.current = null
        submissionInFlight.current = false
        setJoining(false)
      }
    }
  }

  const closeAndReset = () => {
    invalidateWork()
    setJoining(false)
    setJoinContext(null)
    setContextError(null)
    resetForm()
    onClose()
  }

  const guideTarget = !teamIsCurrent
    ? 'team'
    : canSelectDivision && gameHasDivisions && !divisionIsCurrent
      ? 'division'
      : shouldRequireInviteCode && !inviteCode
        ? 'code'
        : 'submit'

  return (
    <AccessibleModal {...modalProps} opened={opened} onClose={closeAndReset}>
      <Stack
        component="form"
        onSubmit={(event) => {
          event.preventDefault()
          void onJoinGame()
        }}
      >
        {contextError && (
          <Alert color="red" role="alert" title={t('common.error.encountered')}>
            <Stack gap="xs">
              <span>{contextError}</span>
              <Button type="button" variant="light" size="xs" onClick={() => void refreshJoinContext()}>
                {t('common.button.retry', 'Retry')}
              </Button>
            </Stack>
          </Alert>
        )}
        {submissionError && !contextError && (
          <Alert color="red" role="alert" title={t('common.error.encountered')}>
            {submissionError}
          </Alert>
        )}
        <Select
          ref={teamInputRef}
          data-guide={guideTarget === 'team' ? 'event-join-team' : undefined}
          required
          label={t('game.content.join.team.label')}
          description={t('game.content.join.team.description')}
          data={teamsData}
          disabled={joining || !contextReady}
          error={fieldError === 'team' ? t('game.notification.no_team') : undefined}
          value={team}
          onChange={(value) => {
            setTeam(value)
            setFieldError(null)
            setErrorFocus(null)
            setSubmissionError(null)
          }}
        />
        {canSelectDivision && gameHasDivisions && (
          <Select
            ref={divisionInputRef}
            data-guide={guideTarget === 'division' ? 'event-join-division' : undefined}
            required
            label={t('game.content.join.division.label')}
            description={t('game.content.join.division.description')}
            data={divisionOptions}
            disabled={joining || !contextReady}
            error={fieldError === 'division' ? t('game.notification.no_division') : undefined}
            value={divisionId}
            onChange={(value) => {
              setDivisionId(value ?? '')
              setFieldError(null)
              setErrorFocus(null)
              setSubmissionError(null)
            }}
          />
        )}
        {!canSelectDivision && joinedDivision && (
          <Select
            required
            label={t('game.content.join.division.label')}
            description={t('game.content.join.division.description')}
            readOnly
            disabled
            data={[
              {
                label: joinedDivision.name ?? `Division #${joinedDivision.id}`,
                value: joinedDivision.id!.toString(),
              },
            ]}
            value={joinedDivision.id!.toString()}
          />
        )}
        {shouldRequireInviteCode && (
          <TextInput
            ref={inviteInputRef}
            data-guide={guideTarget === 'code' ? 'event-join-code' : undefined}
            required
            label={t('game.content.join.invite_code.label')}
            description={t('game.content.join.invite_code.description')}
            value={inviteCode}
            error={fieldError === 'invite' ? t('game.notification.no_invite_code') : undefined}
            onChange={(event) => {
              setInviteCode(event.target.value)
              setFieldError(null)
              setErrorFocus(null)
              setSubmissionError(null)
            }}
            disabled={joining || !contextReady}
          />
        )}
        <Button
          type="submit"
          data-guide={guideTarget === 'submit' ? 'event-join-submit' : undefined}
          disabled={!canSubmit}
          loading={joining}
        >
          {t('game.button.join')}
        </Button>
      </Stack>
    </AccessibleModal>
  )
}
