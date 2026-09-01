import { ModalProps } from '@mantine/core'
import { useInputState } from '@mantine/hooks'
import { notifications, showNotification, updateNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, MutableRefObject, useCallback, useEffect, useMemo, useReducer, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useSWRConfig } from 'swr'
import type { AdStateOwner } from '@Components/AdChallengePanel'
import { ChallengeModal, SolverInfo } from '@Components/ChallengeModal'
import { useFeatureGuide } from '@Components/guide/PlayerGuide'
import {
  assertJsonResponse,
  captureChallengeReadFailure,
  challengeReadFailure,
  challengeRequestHeaders,
  createChallengeRecoveryOwner,
  createChallengeRequestId,
  NonJsonResponseError,
} from '@Utils/ChallengePolling'
import { encryptApiData } from '@Utils/Crypto'
import { downloadEventVpnConfig } from '@Utils/EventVpnDownload'
import { isEventVpnAccessError } from '@Utils/EventVpnProof'
import { FlagSubmitAttemptOwner } from '@Utils/FlagSubmitAttempt'
import { flagVerdictReducer } from '@Utils/FlagVerdict'
import { createFlagVerdictPoller, sameFlagVerdictIdentity, type FlagVerdictIdentity } from '@Utils/FlagVerdictPolling'
import { resolveChallengeDeliveryGuide } from '@Utils/GuideState'
import {
  clearDestroyedInstanceContext,
  confirmCreatedInstance,
  destroyReconciledInstance,
  extendReconciledInstance,
  mergeExtendedInstanceContext,
} from '@Utils/InstanceLifecycle'
import { ACCOUNT_STATS_PATH, CHALLENGE_CATALOG_PATH, invalidatePlayerReads } from '@Utils/PlayerReadCache'
import { httpErrorStatus } from '@Utils/ProfileRetry'
import { RetryableOperationKey } from '@Utils/RetryableOperationKey'
import { showErrorMsg } from '@Utils/Shared'
import { ChallengeCategoryItemProps } from '@Utils/Shared'
import { useChallengePolling } from '@Hooks/useChallengePolling'
import { useConfig } from '@Hooks/useConfig'
import { useUser } from '@Hooks/useUser'
import api, {
  AnswerResult,
  ChallengeDetailModel,
  ChallengeSolverPageModel,
  ChallengeType,
  ContainerPortMappingType,
  SubmissionType,
  ReviewRating,
} from '@Api'

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
  /** Proven by the current catalog/team response, not by a retained selection. */
  challengeOwned?: boolean
  adStateOwner?: AdStateOwner
}

interface PendingFlagVerdict extends FlagVerdictIdentity {
  attemptId: string
}

type ContainerOperationKind = 'create' | 'delete' | 'extend'
type ContainerOperationOwner = { scope: string; id: string; key: RetryableOperationKey }

export const operationStorageKey = (kind: ContainerOperationKind, scope: string) =>
  `rsctf:container-operation:${kind}:${encodeURIComponent(scope)}`

export const containerOperationScope = (
  userId: string | undefined,
  participationId: number | null | undefined,
  gameId: number,
  challengeId: number
) => `${userId ?? 'anonymous'}:${participationId ?? 'unresolved'}:${gameId}:${challengeId}`

const isTerminalContainerOperationError = (error: unknown) => {
  const status = httpErrorStatus(error)
  return status !== null && status >= 400 && status < 500 && status !== 409 && status !== 429
}

const retainContainerOperation = (
  kind: ContainerOperationKind,
  ownerRef: MutableRefObject<ContainerOperationOwner | null>,
  scope: string
) => {
  const current = ownerRef.current
  if (current?.scope === scope) {
    current.key.claim()
    return current
  }
  current?.key.release()
  const key = new RetryableOperationKey(undefined, operationStorageKey(kind, scope))
  const owner = { scope, id: key.claim(), key }
  ownerRef.current = owner
  return owner
}

const clearContainerOperation = (
  ownerRef: MutableRefObject<ContainerOperationOwner | null>,
  owner: ContainerOperationOwner
) => {
  if (ownerRef.current === owner) ownerRef.current = null
  owner.key.complete(owner.id)
}

export const GameChallengeModal: FC<GameChallengeModalProps> = (props) => {
  const { mutate: mutateCache } = useSWRConfig()
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
    challengeOwned = true,
    adStateOwner,
    ...modalProps
  } = props

  const readEnabled = shouldReadChallenge(modalProps.opened, challengeOwned, gameId, challengeId)
  const recoveryOwner = useMemo(createChallengeRecoveryOwner, [])
  const challengeRequest = useCallback(
    async (signal: AbortSignal) => {
      const requestId = createChallengeRequestId('challenge')
      try {
        const response = await api.game.gameGetChallenge(gameId, challengeId, {
          signal,
          headers: challengeRequestHeaders(requestId),
        })
        return assertJsonResponse(response)
      } catch (error) {
        throw captureChallengeReadFailure(error, 'challenge', requestId)
      }
    },
    [challengeId, gameId]
  )
  const {
    data: challenge,
    error: challengeError,
    mutate,
  } = useChallengePolling<ChallengeDetailModel>({
    key: gameId > 0 && challengeId > 0 ? `/api/game/${gameId}/challenges/${challengeId}` : null,
    active: readEnabled,
    // Challenge material is stable while this modal is open. Container,
    // submission, and review mutations explicitly reconcile through `mutate`;
    // periodically replacing a valid cached detail with a transient refresh
    // error only hides usable challenge content from the player.
    refreshInterval: 0,
    revalidateOnFocus: false,
    revalidateOnReconnect: false,
    recoveryOwner,
    recoveryKey: 'challenge-detail',
    request: challengeRequest,
  })

  const solverRequest = useCallback(
    async (signal: AbortSignal) => {
      const requestId = createChallengeRequestId('solvers')
      try {
        const response = await api.game.gameGetChallengeSolverPage(
          gameId,
          challengeId,
          { count: 20, skip: 0 },
          { signal, headers: challengeRequestHeaders(requestId) }
        )
        return assertJsonResponse(response)
      } catch (error) {
        throw captureChallengeReadFailure(error, 'solvers', requestId)
      }
    },
    [challengeId, gameId]
  )
  const {
    data: solverPage,
    error: solverError,
    mutate: mutateSolvers,
  } = useChallengePolling<ChallengeSolverPageModel>({
    key:
      gameId > 0 && challengeId > 0
        ? `/api/game/${gameId}/challenges/${challengeId}/solvers/page?count=20&skip=0`
        : null,
    active: readEnabled,
    refreshInterval: 30_000,
    recoveryOwner,
    recoveryKey: 'challenge-solvers',
    request: solverRequest,
  })

  const solvers = useMemo(
    (): SolverInfo[] =>
      (solverPage?.data ?? []).map((s) => ({
        teamName: s.teamName,
        teamAvatar: s.teamAvatar,
        userName: s.userName,
        type: s.type,
        time: s.time,
      })),
    [solverPage?.data]
  )

  const { config } = useConfig()
  const { user } = useUser()
  const { t } = useTranslation()

  const pollErrorMessage = (error: unknown, resource: 'challenge' | 'solvers') => {
    if (!error) return undefined
    const failure = challengeReadFailure(error)
    const reference = failure
      ? failure.serverTraceId === failure.requestId
        ? ` ${t('challenge.error.request_trace_reference', 'Request/server trace')}: ${failure.requestId}.`
        : ` ${t('challenge.error.request_reference', 'Request reference')}: ${failure.requestId}${
            failure.serverTraceId
              ? ` · ${t('challenge.error.server_trace', 'server trace')}: ${failure.serverTraceId}`
              : ''
          }.`
      : ''
    const message = (value: string) => `${value}${reference}`
    if (isEventVpnAccessError(error)) {
      if (error.kind === 'disconnected') {
        return message(t('challenge.error.vpn_disconnected', 'Connect to the event VPN, then retry the challenge.'))
      }
      if (error.kind === 'rate-limited') {
        return message(
          t(
            'challenge.error.vpn_rate_limited',
            'Event VPN verification is rate limited. Retry after the indicated delay.'
          )
        )
      }
      return message(
        t('challenge.error.vpn_unavailable', 'Event VPN verification is temporarily unavailable. Retry shortly.')
      )
    }
    if (error instanceof NonJsonResponseError) {
      return message(
        t('challenge.error.invalid_response', 'The server returned an invalid response. Automatic retries stopped.')
      )
    }
    const status = httpErrorStatus(error)
    if (status === 401) {
      return message(t('challenge.error.unauthorized', 'Your session expired. Sign in again to continue.'))
    }
    if (status === 403) {
      return message(
        t(
          'challenge.error.forbidden',
          'Your challenge access was revoked or is no longer valid. Rejoin the event or ask an organizer to check your participation.'
        )
      )
    }
    if (status === 404) {
      return message(
        resource === 'challenge'
          ? t('challenge.error.not_found', 'This challenge is no longer available.')
          : t('challenge.error.solvers_not_found', 'Solver history is unavailable for this challenge.')
      )
    }
    if (status === 429) {
      const seconds = failure?.retryAfterMilliseconds
        ? Math.max(1, Math.ceil(failure.retryAfterMilliseconds / 1_000))
        : null
      return message(
        seconds
          ? t('challenge.error.rate_limited_seconds', 'Too many requests. Retry in about {{seconds}} seconds.', {
              seconds,
            })
          : t('challenge.error.rate_limited', 'Too many requests. Retry after the server retry window.')
      )
    }
    if (status !== null && status >= 500) {
      return message(
        resource === 'challenge'
          ? t(
              'challenge.error.server_temporary',
              'The challenge service is temporarily unavailable (HTTP {{status}}). Automatic retries are bounded.',
              { status }
            )
          : t(
              'challenge.error.solver_temporary',
              'Solver history is temporarily unavailable (HTTP {{status}}); the challenge remains usable.',
              { status }
            )
      )
    }
    if (status !== null) {
      return message(
        resource === 'challenge'
          ? t('challenge.error.request_rejected', 'The challenge request failed (HTTP {{status}}).', { status })
          : t('challenge.error.solver_rejected', 'Solver history could not be loaded (HTTP {{status}}).', { status })
      )
    }
    return message(
      resource === 'challenge'
        ? t('challenge.error.network', 'The challenge request failed on the network. Automatic retries are bounded.')
        : t('challenge.error.solver_network', 'Solver history could not be refreshed; the challenge remains usable.')
    )
  }

  const retryFailedReads = useCallback(async () => {
    const reads: Promise<unknown>[] = []
    if (challenge === undefined || challengeError) reads.push(mutate())
    if (solverError) reads.push(mutateSolvers())
    await Promise.allSettled(reads)
  }, [challenge, challengeError, mutate, mutateSolvers, solverError])

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

  useFeatureGuide(deliveryFeature, Boolean(readEnabled && deliveryFeature), {
    eventVpnRequired,
    hasAttachment: Boolean(challenge?.context?.url),
    instanceActive: Boolean(challenge?.context?.instanceEntry),
  })

  const [disabled, setDisabled] = useState(false)
  const [eventVpnDownloading, setEventVpnDownloading] = useState(false)
  const [pendingSubmission, setPendingSubmission] = useState<PendingFlagVerdict | null>(null)
  const [flag, setFlag] = useInputState('')
  const [receiptProof, setReceiptProof] = useInputState('')
  const [solvedChallengeId, setSolvedChallengeId] = useState<number | null>(null)
  const [flagVerdict, dispatchFlagVerdict] = useReducer(flagVerdictReducer, null)
  const submitAttemptOwnerRef = useRef<FlagSubmitAttemptOwner | null>(null)
  const containerCreateOperationRef = useRef<ContainerOperationOwner | null>(null)
  const containerDeleteOperationRef = useRef<ContainerOperationOwner | null>(null)
  const containerExtendOperationRef = useRef<ContainerOperationOwner | null>(null)
  if (submitAttemptOwnerRef.current === null) {
    submitAttemptOwnerRef.current = new FlagSubmitAttemptOwner()
  }
  const submitAttemptOwner = submitAttemptOwnerRef.current
  const currentScope = useRef({ gameId, challengeId, opened: readEnabled, mounted: true })
  currentScope.current = { gameId, challengeId, opened: readEnabled, mounted: true }

  useEffect(() => {
    currentScope.current.mounted = true
    return () => {
      currentScope.current.mounted = false
    }
  }, [])

  useEffect(() => {
    dispatchFlagVerdict({ type: 'reset' })
    setDisabled(false)
    setPendingSubmission(null)
    setFlag('')
    setReceiptProof('')
    setSolvedChallengeId(null)
  }, [challengeId, gameId])

  useEffect(() => {
    if (readEnabled) return
    dispatchFlagVerdict({ type: 'reset' })
    setDisabled(false)
    setPendingSubmission(null)
    setFlag('')
    setReceiptProof('')
    setSolvedChallengeId(null)
  }, [readEnabled])

  useEffect(() => {
    setPendingSubmission((current) => {
      if (current && readEnabled && current.gameId === gameId && current.challengeId === challengeId) {
        return current
      }
      return null
    })
    setDisabled(false)
  }, [gameId, challengeId, readEnabled])

  const isLimitReached = (challenge?.limit && (challenge.attempts ?? 0) >= challenge.limit) || false
  const operationScope = containerOperationScope(user?.userId, challenge?.context?.participationId, gameId, challengeId)
  const eventVpnDisconnected = Boolean(
    eventVpnRequired && isEventVpnAccessError(challengeError) && challengeError.kind === 'disconnected'
  )

  const onDownloadEventVpn = async () => {
    if (eventVpnDownloading) return
    setEventVpnDownloading(true)
    try {
      await downloadEventVpnConfig(gameId)
    } catch (error) {
      showErrorMsg(error, t)
    } finally {
      setEventVpnDownloading(false)
    }
  }

  useEffect(
    () => () => {
      containerCreateOperationRef.current?.key.release()
      containerDeleteOperationRef.current?.key.release()
      containerExtendOperationRef.current?.key.release()
    },
    [operationScope, readEnabled]
  )

  const onCreate = async () => {
    if (!readEnabled || disabled) return
    setDisabled(true)
    let operation: ContainerOperationOwner | undefined

    try {
      operation = retainContainerOperation('create', containerCreateOperationRef, operationScope)
      const res = await api.game.gameCreateContainer(gameId, challengeId, {
        headers: { 'X-RSCTF-Operation-Id': operation.id },
      })
      if (!(await confirmCreatedInstance(res.data, mutate))) return
      clearContainerOperation(containerCreateOperationRef, operation)
      showNotification({
        color: 'teal',
        title: t('challenge.notification.instance.created.title'),
        message: t('challenge.notification.instance.created.message'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (e) {
      if (operation && isTerminalContainerOperationError(e)) {
        clearContainerOperation(containerCreateOperationRef, operation)
      }
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const requestDestroy = async () => {
    let operation = containerDeleteOperationRef.current?.scope.startsWith(`${operationScope}:`)
      ? containerDeleteOperationRef.current
      : undefined
    try {
      await destroyReconciledInstance<ChallengeDetailModel>({
        refresh: mutate,
        hasInstance: (latest) => Boolean(latest?.context?.instanceId || latest?.context?.instanceEntry),
        destroy: async (latest) => {
          const expectedContainerId = latest.context?.instanceId
          if (!expectedContainerId)
            throw new Error('The refreshed challenge response is missing its instance identity.')
          const scope = `${operationScope}:${expectedContainerId}`
          operation = retainContainerOperation('delete', containerDeleteOperationRef, scope)
          await api.game.gameDeleteContainer(
            gameId,
            challengeId,
            { expectedContainerId },
            { headers: { 'X-RSCTF-Operation-Id': operation.id } }
          )
        },
        publishAbsent: async (deleted) => {
          await mutate((current) => clearDestroyedInstanceContext(current, deleted), { revalidate: false })
        },
      })
      if (operation) clearContainerOperation(containerDeleteOperationRef, operation)
      showNotification({
        color: 'teal',
        title: t('challenge.notification.instance.destroyed.title'),
        message: t('challenge.notification.instance.destroyed.message'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (e) {
      if (operation && isTerminalContainerOperationError(e)) {
        clearContainerOperation(containerDeleteOperationRef, operation)
      }
      showErrorMsg(e, t)
    }
  }

  const onDestroy = async () => {
    if (!readEnabled || disabled) return
    setDisabled(true)
    try {
      await requestDestroy()
    } finally {
      setDisabled(false)
    }
  }

  const onExtend = async () => {
    if (!readEnabled || disabled) return
    setDisabled(true)
    let operation: ContainerOperationOwner | undefined

    try {
      await extendReconciledInstance({
        refresh: mutate,
        extend: async (expectedContainerId) => {
          operation = retainContainerOperation(
            'extend',
            containerExtendOperationRef,
            `${operationScope}:${expectedContainerId}`
          )
          return (
            await api.game.gameExtendContainerLifetime(
              gameId,
              challengeId,
              { expectedContainerId },
              { headers: { 'X-RSCTF-Operation-Id': operation.id } }
            )
          ).data
        },
        publish: async (extension) => {
          await mutate((latest) => mergeExtendedInstanceContext(latest, extension), { revalidate: false })
        },
      })
      if (operation) clearContainerOperation(containerExtendOperationRef, operation)
    } catch (e) {
      if (operation && isTerminalContainerOperationError(e)) {
        clearContainerOperation(containerExtendOperationRef, operation)
      }
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const onSubmit = async () => {
    const normalizedFlag = flag.trim()
    if (!readEnabled || !challengeId || !normalizedFlag) {
      showNotification({
        color: 'red',
        message: t('challenge.notification.flag.empty'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }

    const proof = receiptProof.trim() || undefined
    const dispatch = submitAttemptOwner.begin(
      { gameId, challengeId, flag: normalizedFlag, proof },
      async (attemptId) => ({
        attemptId,
        flag: await encryptApiData(t, normalizedFlag, config.apiPublicKey),
        proof,
      }),
      async (payload) => {
        const response = await api.game.gameSubmit(gameId, challengeId, payload)
        return response.data
      }
    )
    if (!dispatch.owner) return

    setDisabled(true)
    const submittingScope = { gameId, challengeId }

    try {
      const result = await dispatch.result
      const latestScope = currentScope.current
      if (
        !latestScope.mounted ||
        !latestScope.opened ||
        latestScope.gameId !== submittingScope.gameId ||
        latestScope.challengeId !== submittingScope.challengeId
      ) {
        return
      }

      setPendingSubmission({ ...submittingScope, submissionId: result.submissionId, attemptId: result.attemptId })
      notifications.clean()
      showNotification({
        id: 'flag-submitted',
        color: 'orange',
        title: t('challenge.notification.flag.submitted.title'),
        message: t('challenge.notification.flag.submitted.message'),
        loading: true,
        autoClose: false,
      })

      if (result.firstAcknowledgement) {
        const nxt = (challenge?.attempts ?? 0) + 1
        const attempts = challenge?.limit && challenge.limit > 0 ? Math.min(nxt, challenge.limit) : nxt

        // Spread the existing challenge FIRST, then override attempts — otherwise the
        // stale attempts value clobbers the increment and the "N remaining" counter
        // never decrements after a submit on limited-attempt challenges.
        mutate({
          ...challenge,
          attempts,
        })
      }
      return
    } catch (e) {
      const latestScope = currentScope.current
      if (
        !latestScope.mounted ||
        !latestScope.opened ||
        latestScope.gameId !== submittingScope.gameId ||
        latestScope.challengeId !== submittingScope.challengeId
      ) {
        return
      }
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
      throw e
    }
  }

  useEffect(() => {
    if (!pendingSubmission || !readEnabled) return
    if (pendingSubmission.gameId !== gameId || pendingSubmission.challengeId !== challengeId) return

    const poller = createFlagVerdictPoller({
      identity: pendingSubmission,
      request: async (identity, signal) => {
        const res = await api.game.gameStatus(identity.gameId, identity.challengeId, identity.submissionId, { signal })
        return res.data
      },
      onTerminal: (identity, result) => {
        if (!sameFlagVerdictIdentity(pendingSubmission, identity)) return
        const scope = currentScope.current
        if (
          !scope.mounted ||
          !scope.opened ||
          scope.gameId !== identity.gameId ||
          scope.challengeId !== identity.challengeId
        ) {
          return
        }
        submitAttemptOwner.complete(identity.gameId, identity.challengeId, pendingSubmission.attemptId)
        setDisabled(false)
        setFlag('')
        setReceiptProof('')
        setPendingSubmission(null)
        void checkDataFlag(identity, result)
      },
      onFailure: (identity, error) => {
        if (!sameFlagVerdictIdentity(pendingSubmission, identity)) return
        const scope = currentScope.current
        if (
          !scope.mounted ||
          !scope.opened ||
          scope.gameId !== identity.gameId ||
          scope.challengeId !== identity.challengeId
        ) {
          return
        }
        // The backend has already committed this submission. Keep the exact
        // flag/proof available for the player while surfacing recovery failure.
        setDisabled(false)
        setPendingSubmission(null)
        notifications.hide('flag-submitted')
        showErrorMsg(error, t)
      },
    })
    poller.start()
    return () => {
      const scope = currentScope.current
      const leftSubmission =
        !scope.mounted ||
        !scope.opened ||
        scope.gameId !== pendingSubmission.gameId ||
        scope.challengeId !== pendingSubmission.challengeId
      const wasPending = poller.pending()
      poller.cancel()
      if (wasPending && leftSubmission) notifications.hide('flag-submitted')
    }
  }, [pendingSubmission, gameId, challengeId, readEnabled])

  useEffect(() => {
    if (challengeId !== solvedChallengeId) return

    if (status !== SubmissionType.Unaccepted && status !== undefined) {
      // status has been updated, reset solved challenge id
      setSolvedChallengeId(null)
    }
  }, [status, challengeId, challenge])

  const checkDataFlag = async (identity: FlagVerdictIdentity, data: string) => {
    dispatchFlagVerdict({ type: 'show', result: data, sequence: identity.submissionId })

    if (data === AnswerResult.Accepted) {
      setSolvedChallengeId(identity.challengeId)
      void invalidatePlayerReads(mutateCache, [ACCOUNT_STATS_PATH, CHALLENGE_CATALOG_PATH])
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
          id: identity.submissionId,
        }),
        icon: <Icon path={mdiClose} size={1} />,
        autoClose: false,
        withCloseButton: true,
      })
    }
  }

  const challengePollError = pollErrorMessage(challengeError, 'challenge')

  return (
    <ChallengeModal
      {...modalProps}
      gameTitle={gameTitle}
      eventHref={eventHref}
      loading={readEnabled && challenge === undefined && challengeError === undefined}
      onRetryLoad={readEnabled ? () => void retryFailedReads() : undefined}
      challenge={{
        ...(challenge ?? {}),
        title: challenge?.title ?? title,
        score: challenge?.score ?? score,
      }}
      cateData={cateData}
      solved={(status !== SubmissionType.Unaccepted && status !== undefined) || solvedChallengeId === challengeId}
      justSolved={solvedChallengeId === challengeId}
      solvers={solvers}
      solverTotal={solverPage?.total}
      loadError={eventVpnDisconnected || challenge === undefined ? challengePollError : undefined}
      eventVpnDisconnected={eventVpnDisconnected}
      eventVpnDownloading={eventVpnDownloading}
      onDownloadEventVpn={eventVpnDisconnected ? onDownloadEventVpn : undefined}
      refreshError={challenge !== undefined && !eventVpnDisconnected ? challengePollError : undefined}
      solverError={challenge !== undefined ? pollErrorMessage(solverError, 'solvers') : undefined}
      flag={flag}
      setFlag={setFlag}
      receiptProof={receiptProof}
      setReceiptProof={setReceiptProof}
      onCreate={onCreate}
      onDestroy={onDestroy}
      onSubmitFlag={onSubmit}
      onReviewSubmit={onReviewSubmit}
      disabled={disabled || isLimitReached || !readEnabled || !challenge}
      // `disabled` covers both the POST and the owned verdict-recovery loop.
      submitting={disabled}
      onExtend={onExtend}
      gameEnded={gameEnded}
      practiceMode={practiceMode}
      gameId={gameId}
      flagVerdict={flagVerdict}
      onDismissFlagVerdict={() => {
        if (flagVerdict) dispatchFlagVerdict({ type: 'dismiss', sequence: flagVerdict.sequence })
      }}
      adStateOwner={adStateOwner}
    />
  )
}

export const shouldReadChallenge = (
  opened: boolean | undefined,
  challengeOwned: boolean,
  gameId: number,
  challengeId: number
) => Boolean(opened && challengeOwned && gameId > 0 && challengeId > 0)
