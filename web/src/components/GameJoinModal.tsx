import { Button, Select, Stack, TextInput } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { AccessibleModal, AccessibleModalProps } from '@Components/AccessibleModal'
import { OnceSWRConfig } from '@Hooks/useConfig'
import { useGame } from '@Hooks/useGame'
import { useTeams } from '@Hooks/useUser'
import api, { GameJoinModel } from '@Api'

interface GameJoinModalProps extends AccessibleModalProps {
  onSubmitJoin: (info: GameJoinModel, signal: AbortSignal) => Promise<boolean>
}

export const GameJoinModal: FC<GameJoinModalProps> = (props) => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const { onSubmitJoin, ...modalProps } = props

  const { teams } = useTeams()
  const { game } = useGame(numId)

  const { data: checkInfo } = api.game.useGameGetGameJoinCheckInfo(numId, OnceSWRConfig, props.opened && numId > 0)

  const [inviteCode, setInviteCode] = useState('')
  const [divisionId, setDivisionId] = useState('')
  const [team, setTeam] = useState<string | null>(null)
  const [disabled, setDisabled] = useState(false)
  const submitInFlight = useRef(false)
  const submitController = useRef<AbortController | null>(null)
  const generation = useRef(0)
  const activeGame = useRef(numId)

  const { t } = useTranslation()

  useEffect(() => {
    if (activeGame.current === numId) return
    submitController.current?.abort()
    submitController.current = null
    activeGame.current = numId
    generation.current += 1
    submitInFlight.current = false
    setInviteCode('')
    setDivisionId('')
    setTeam(null)
    setDisabled(false)
    if (props.opened) props.onClose()
  }, [numId, props.opened, props.onClose])

  useEffect(() => () => submitController.current?.abort(), [])

  useEffect(() => {
    const available = new Set((teams ?? []).flatMap((candidate) => candidate.id ? [candidate.id.toString()] : []))
    if (team && available.has(team)) return
    setTeam(available.values().next().value ?? null)
  }, [team, teams])

  useEffect(() => {
    if (divisionId) return

    if (typeof game?.division === 'number') {
      setDivisionId(game.division.toString())
    } else if (game?.divisions && game.divisions.length >= 1 && !!game.divisions[0].id) {
      setDivisionId(game.divisions[0].id.toString())
    }
  }, [divisionId, game])

  const divisionOptions = useMemo(
    () =>
      (game?.divisions ?? [])
        .filter((d) => d.id && (!checkInfo?.joinableDivisions || checkInfo.joinableDivisions.includes(d.id!)))
        .map((d) => ({
          value: d.id!.toString(),
          label: d.name ?? `Division #${d.id}`,
        })),
    [game?.divisions, checkInfo]
  )

  const gameCheckInfo = useMemo(() => {
    const map = new Map<string, { div: number | null; joinable?: boolean }>()
    checkInfo?.joinedTeams?.forEach((jt) => {
      map.set(jt.id.toString(), { div: jt.division, joinable: checkInfo.joinableDivisions?.includes(jt.division) })
    })
    return map
  }, [checkInfo])

  const selectedDivision = useMemo(
    () => (game?.divisions ?? []).find((d) => d.id?.toString() === divisionId) ?? null,
    [divisionId, game?.divisions]
  )

  const teamsData = useMemo(() => {
    return teams?.map((t) => ({ label: t.name!, value: t.id!.toString() })) ?? []
  }, [teams])

  const joinedTeam = team ? gameCheckInfo.get(team) : null
  const joinedDivision = joinedTeam?.div ? game?.divisions?.find((d) => d.id === joinedTeam?.div) : null
  const hasDivision = divisionOptions.length > 0 || joinedTeam?.div
  const canSelectDivision = !joinedTeam
  const selectedTeamAvailable = Boolean(team && teamsData.some((candidate) => candidate.value === team))
  const selectedDivisionAvailable =
    !canSelectDivision || !hasDivision || Boolean(divisionId && divisionOptions.some((option) => option.value === divisionId))

  const shouldRequireInviteCode = hasDivision
    ? Boolean(selectedDivision?.inviteCodeRequired)
    : Boolean(game?.inviteCodeRequired)
  const guideTarget = !team
    ? 'team'
    : canSelectDivision && hasDivision && !divisionId
      ? 'division'
      : shouldRequireInviteCode && !inviteCode
        ? 'code'
        : 'submit'

  useEffect(() => {
    if (!shouldRequireInviteCode) {
      setInviteCode('')
    }
  }, [shouldRequireInviteCode])

  const onJoinGame = async () => {
    if (submitInFlight.current) return
    const requestGeneration = generation.current
    submitInFlight.current = true
    setDisabled(true)

    if (!team || !selectedTeamAvailable) {
      showNotification({
        color: 'orange',
        message: t('game.notification.no_team'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      submitInFlight.current = false
      setDisabled(false)
      return
    }

    if (shouldRequireInviteCode && !inviteCode) {
      showNotification({
        color: 'orange',
        message: t('game.notification.no_invite_code'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      submitInFlight.current = false
      setDisabled(false)
      return
    }

    if (!selectedDivisionAvailable) {
      showNotification({
        color: 'orange',
        message: t('game.notification.no_division'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      submitInFlight.current = false
      setDisabled(false)
      return
    }

    const controller = new AbortController()
    submitController.current = controller
    try {
      const accepted = await onSubmitJoin(
        {
          teamId: parseInt(team, 10),
          inviteCode: shouldRequireInviteCode ? inviteCode : undefined,
          divisionId:
            canSelectDivision && hasDivision
              ? parseInt(divisionId, 10)
              : !canSelectDivision && joinedDivision
                ? joinedDivision.id
                : undefined,
        },
        controller.signal
      )
      if (!accepted || generation.current !== requestGeneration) return
      setInviteCode('')
      setDivisionId('')
      props.onClose()
    } finally {
      if (generation.current === requestGeneration) {
        if (submitController.current === controller) submitController.current = null
        submitInFlight.current = false
        setDisabled(false)
      }
    }
  }

  return (
    <AccessibleModal
      {...modalProps}
      onClose={() => {
        submitController.current?.abort()
        submitController.current = null
        generation.current += 1
        submitInFlight.current = false
        setDisabled(false)
        modalProps.onClose()
      }}
    >
      <Stack>
        <Select
          data-guide={guideTarget === 'team' ? 'event-join-team' : undefined}
          required
          label={t('game.content.join.team.label')}
          description={t('game.content.join.team.description')}
          data={teamsData}
          disabled={disabled}
          value={team}
          onChange={setTeam}
        />
        {canSelectDivision && hasDivision && (
          <Select
            data-guide={guideTarget === 'division' ? 'event-join-division' : undefined}
            required
            label={t('game.content.join.division.label')}
            description={t('game.content.join.division.description')}
            readOnly={!canSelectDivision}
            data={divisionOptions}
            disabled={disabled}
            value={divisionId}
            onChange={(e) => setDivisionId(e ?? '')}
          />
        )}
        {!canSelectDivision && joinedDivision && (
          <Select
            required
            label={t('game.content.join.division.label')}
            description={t('game.content.join.division.description')}
            readOnly={true}
            disabled={true}
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
            data-guide={guideTarget === 'code' ? 'event-join-code' : undefined}
            required
            label={t('game.content.join.invite_code.label')}
            description={t('game.content.join.invite_code.description')}
            value={inviteCode}
            onChange={(e) => setInviteCode(e.target.value)}
            disabled={disabled}
          />
        )}
        <Button
          data-guide={guideTarget === 'submit' ? 'event-join-submit' : undefined}
          disabled={disabled || !selectedTeamAvailable || !selectedDivisionAvailable}
          onClick={onJoinGame}
        >
          {t('game.button.join')}
        </Button>
      </Stack>
    </AccessibleModal>
  )
}
