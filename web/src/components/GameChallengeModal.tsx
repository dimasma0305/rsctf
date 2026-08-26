import { ModalProps } from '@mantine/core'
import { useInputState } from '@mantine/hooks'
import { notifications, showNotification, updateNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useMemo, useReducer, useState } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import { ChallengeModal, SolverInfo } from '@Components/ChallengeModal'
import { useFeatureGuide } from '@Components/guide/PlayerGuide'
import { encryptApiData } from '@Utils/Crypto'
import { flagVerdictReducer } from '@Utils/FlagVerdict'
import { resolveChallengeDeliveryGuide } from '@Utils/GuideState'
import {
  clearDestroyedInstanceContext,
  destroyReconciledInstance,
  mergeExtendedInstanceContext,
  mergeInstanceContext,
} from '@Utils/InstanceLifecycle'
import { showErrorMsg } from '@Utils/Shared'
import { ChallengeCategoryItemProps } from '@Utils/Shared'
import { useConfig } from '@Hooks/useConfig'
import api, {
  AnswerResult,
  ChallengeDetailModel,
  ChallengeType,
  ContainerPortMappingType,
  SubmissionType,
  ReviewRating,
} from '@Api'

interface ChallengeSolverModel {
  rank: number
  teamName: string
  teamAvatar: string | null
  userName: string | null
  type: SubmissionType
  time: string
  score: number
}

const fetcher = (url: string) => fetch(url, { credentials: 'include' }).then((r) => (r.ok ? r.json() : []))

interface GameChallengeModalProps extends ModalProps {
  gameId: number
  gameTitle: string
  gameEnded: boolean
  practiceMode?: boolean
  eventVpnRequired?: boolean
  eventHref?: string
  cateData: ChallengeCategoryItemProps
  title: string
  score: number
  challengeId: number
  status?: SubmissionType
}

export const GameChallengeModal: FC<GameChallengeModalProps> = (props) => {
  const {
    gameId,
    gameTitle,
    gameEnded,
    practiceMode,
    eventVpnRequired,
    eventHref,
    challengeId,
    cateData,
    status,
    title,
    score,
    ...modalProps
  } = props

  const { data: challenge, mutate } = api.game.useGameGetChallenge(gameId, challengeId, {
    refreshInterval: 120 * 1000,
  })

  const { data: solverData } = useSWR<ChallengeSolverModel[]>(
    gameId > 0 && challengeId > 0 ? `/api/game/${gameId}/challenges/${challengeId}/solvers` : null,
    fetcher,
    { refreshInterval: 30000, revalidateOnFocus: false }
  )

  const solvers = useMemo(
    (): SolverInfo[] =>
      (solverData ?? []).map((s) => ({
        rank: s.rank,
        teamName: s.teamName,
        teamAvatar: s.teamAvatar,
        userName: s.userName,
        type: s.type,
        time: new Date(s.time).getTime(),
        score: s.score,
      })),
    [solverData]
  )

  const { config } = useConfig()
  const { t } = useTranslation()

  const wrongFlagHints = t('challenge.content.wrong_flag_hints', {
    returnObjects: true,
  }) as string[]

  const isDynamic =
    challenge?.type === ChallengeType.StaticContainer || challenge?.type === ChallengeType.DynamicContainer
  const deliveryFeature = useMemo(
    () =>
      resolveChallengeDeliveryGuide({
        staticChallenge:
          challenge?.type === ChallengeType.StaticAttachment || challenge?.type === ChallengeType.DynamicAttachment,
        containerChallenge: isDynamic,
        eventVpnRequired: Boolean(eventVpnRequired),
        platformProxy: config.portMapping === ContainerPortMappingType.PlatformProxy,
      }),
    [challenge?.type, config.portMapping, eventVpnRequired, isDynamic]
  )

  useFeatureGuide(deliveryFeature, Boolean(modalProps.opened && deliveryFeature), {
    eventVpnRequired,
    hasAttachment: Boolean(challenge?.context?.url),
    instanceActive: Boolean(challenge?.context?.instanceEntry),
  })

  const [disabled, setDisabled] = useState(false)
  const [submitId, setSubmitId] = useState(0)
  const [flag, setFlag] = useInputState('')
  const [receiptProof, setReceiptProof] = useInputState('')
  const [solvedChallengeId, setSolvedChallengeId] = useState<number | null>(null)
  const [flagVerdict, dispatchFlagVerdict] = useReducer(flagVerdictReducer, null)

  useEffect(() => {
    dispatchFlagVerdict({ type: 'reset' })
  }, [challengeId])

  useEffect(() => {
    if (!modalProps.opened) dispatchFlagVerdict({ type: 'reset' })
  }, [modalProps.opened])

  const isLimitReached = (challenge?.limit && (challenge.attempts ?? 0) >= challenge.limit) || false

  const onCreate = async () => {
    if (!challengeId || disabled) return
    setDisabled(true)

    try {
      const res = await api.game.gameCreateContainer(gameId, challengeId)
      await mutate(
        (latest) =>
          mergeInstanceContext(latest, {
            closeTime: res.data.expectStopAt,
            instanceEntry: res.data.entry,
          }),
        { revalidate: false }
      )
      showNotification({
        color: 'teal',
        title: t('challenge.notification.instance.created.title'),
        message: t('challenge.notification.instance.created.message'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const requestDestroy = async () => {
    try {
      await destroyReconciledInstance<ChallengeDetailModel>({
        refresh: mutate,
        hasInstance: (latest) => Boolean(latest?.context?.instanceEntry),
        destroy: async () => {
          await api.game.gameDeleteContainer(gameId, challengeId)
        },
        publishAbsent: async (deleted) => {
          await mutate((current) => clearDestroyedInstanceContext(current, deleted), { revalidate: false })
        },
      })
      showNotification({
        color: 'teal',
        title: t('challenge.notification.instance.destroyed.title'),
        message: t('challenge.notification.instance.destroyed.message'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (e) {
      showErrorMsg(e, t)
    }
  }

  const onDestroy = async () => {
    if (!challengeId || disabled) return
    setDisabled(true)
    try {
      await requestDestroy()
    } finally {
      setDisabled(false)
    }
  }

  const onExtend = async () => {
    if (!challengeId || disabled) return
    setDisabled(true)

    try {
      const res = await api.game.gameExtendContainerLifetime(gameId, challengeId)
      await mutate((latest) => mergeExtendedInstanceContext(latest, res.data), { revalidate: false })
    } finally {
      setDisabled(false)
    }
  }

  const onSubmit = async () => {
    if (!challengeId || !flag) {
      showNotification({
        color: 'red',
        message: t('challenge.notification.flag.empty'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }

    setDisabled(true)

    try {
      const res = await api.game.gameSubmit(gameId, challengeId, {
        flag: await encryptApiData(t, flag, config.apiPublicKey),
        proof: receiptProof.trim() || undefined,
      })
      setSubmitId(res.data)
      notifications.clean()
      showNotification({
        id: 'flag-submitted',
        color: 'orange',
        title: t('challenge.notification.flag.submitted.title'),
        message: t('challenge.notification.flag.submitted.message'),
        loading: true,
        autoClose: false,
      })

      const nxt = (challenge?.attempts ?? 0) + 1
      const attempts = challenge?.limit && challenge.limit > 0 ? Math.min(nxt, challenge.limit) : nxt

      // Spread the existing challenge FIRST, then override attempts — otherwise the
      // stale attempts value clobbers the increment and the "N remaining" counter
      // never decrements after a submit on limited-attempt challenges.
      mutate({
        ...challenge,
        attempts,
      })
      return
    } catch (e) {
      showErrorMsg(e, t)
      setDisabled(false)
      return
    }
  }

  const onReviewSubmit = async (rating: ReviewRating, comment: string) => {
    try {
      await api.game.gameReviewChallenge(gameId, challengeId, { rating, comment })
      showNotification({
        color: 'teal',
        message: t('challenge.review.submitted', 'Review submitted'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (e) {
      showErrorMsg(e, t)
    }
  }

  useEffect(() => {
    if (!submitId) return

    const polling = setInterval(async () => {
      try {
        const res = await api.game.gameStatus(gameId, challengeId, submitId)
        if (res.data !== AnswerResult.FlagSubmitted) {
          setDisabled(false)
          setFlag('')
          setReceiptProof('')
          checkDataFlag(submitId, res.data)
          clearInterval(polling)
          setSubmitId(0) // reset so the next attempt starts clean
        }
      } catch (err) {
        setDisabled(false)
        setFlag('')
        setReceiptProof('')
        showErrorMsg(err, t)
        clearInterval(polling)
        setSubmitId(0)
      }
    }, 500)

    return () => clearInterval(polling)
  }, [submitId])

  useEffect(() => {
    if (challengeId !== solvedChallengeId) return

    if (status !== SubmissionType.Unaccepted && status !== undefined) {
      // status has been updated, reset solved challenge id
      setSolvedChallengeId(null)
    }
  }, [status, challengeId, challenge])

  const checkDataFlag = async (id: number, data: string) => {
    dispatchFlagVerdict({ type: 'show', result: data, sequence: id })

    if (data === AnswerResult.Accepted) {
      setSolvedChallengeId(challengeId)
      updateNotification({
        id: 'flag-submitted',
        color: 'teal',
        title: t('challenge.notification.flag.accepted.title'),
        message: gameEnded
          ? t('challenge.notification.flag.accepted.ended')
          : t('challenge.notification.flag.accepted.message'),
        icon: <Icon path={mdiCheck} size={1} />,
        autoClose: 8000,
        loading: false,
      })
      if (isDynamic && challenge.context?.instanceEntry) await requestDestroy()
      // props.onClose()  <-- Disable auto-close to allow user to review
    } else if (data === AnswerResult.WrongAnswer) {
      updateNotification({
        id: 'flag-submitted',
        color: 'red',
        title: t('challenge.notification.flag.wrong'),
        message: wrongFlagHints[Math.floor(Math.random() * wrongFlagHints.length)],
        icon: <Icon path={mdiClose} size={1} />,
        autoClose: 8000,
        loading: false,
      })
    } else if (data === AnswerResult.CheatDetected) {
      updateNotification({
        id: 'flag-submitted',
        color: 'red',
        title: t('challenge.notification.flag.cheat.title', 'Cheating detected'),
        message: t(
          'challenge.notification.flag.cheat.message',
          'This submission has been flagged as cheating. Please contact an administrator if you believe this is a mistake.'
        ),
        icon: <Icon path={mdiClose} size={1} />,
        autoClose: false,
        withCloseButton: true,
      })
    } else if (data === AnswerResult.NotFound) {
      updateNotification({
        id: 'flag-submitted',
        color: 'red',
        title: t('challenge.notification.flag.not_found.title', 'Submission not found'),
        message: t(
          'challenge.notification.flag.not_found.message',
          'The submission could not be found. Please try submitting again.'
        ),
        icon: <Icon path={mdiClose} size={1} />,
        autoClose: 8000,
        withCloseButton: true,
      })
    } else {
      updateNotification({
        id: 'flag-submitted',
        color: 'yellow',
        title: t('challenge.notification.flag.unknown.title'),
        message: t('challenge.notification.flag.unknown.message', {
          id,
        }),
        icon: <Icon path={mdiClose} size={1} />,
        autoClose: false,
        withCloseButton: true,
      })
    }
  }

  return (
    <ChallengeModal
      {...modalProps}
      gameTitle={gameTitle}
      eventHref={eventHref}
      challenge={{
        ...(challenge ?? {}),
        title: challenge?.title ?? title,
        score: challenge?.score ?? score,
      }}
      cateData={cateData}
      solved={(status !== SubmissionType.Unaccepted && status !== undefined) || solvedChallengeId === challengeId}
      justSolved={solvedChallengeId === challengeId}
      solvers={solvers}
      flag={flag}
      setFlag={setFlag}
      receiptProof={receiptProof}
      setReceiptProof={setReceiptProof}
      onCreate={onCreate}
      onDestroy={onDestroy}
      onSubmitFlag={onSubmit}
      onReviewSubmit={onReviewSubmit}
      disabled={disabled || isLimitReached}
      // `disabled` alone covers both the POST and the 500ms /gameStatus
      // poll loop (the poll calls setDisabled(false) only when it resolves).
      // Previously also ORed submitId > 0, but submitId is never reset so
      // the button would stay in loading state forever after a wrong flag.
      submitting={disabled}
      onExtend={onExtend}
      gameEnded={gameEnded}
      practiceMode={practiceMode}
      gameId={gameId}
      flagVerdict={flagVerdict}
      onDismissFlagVerdict={() => {
        if (flagVerdict) dispatchFlagVerdict({ type: 'dismiss', sequence: flagVerdict.sequence })
      }}
    />
  )
}
