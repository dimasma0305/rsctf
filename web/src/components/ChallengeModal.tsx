import {
  ActionIcon,
  Alert,
  Avatar,
  Badge,
  Button,
  CopyButton,
  Divider,
  Group,
  Modal,
  ModalProps,
  ScrollArea,
  Stack,
  TextInput,
  Text,
  Title,
  useMantineTheme,
  ScrollAreaAutosize,
  Input,
  Textarea,
  Tooltip,
} from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import {
  mdiAlertCircleOutline,
  mdiArrowRight,
  mdiCheck,
  mdiContentCopy,
  mdiDownload,
  mdiFlag,
  mdiHexagonSlice2,
  mdiHexagonSlice4,
  mdiHexagonSlice6,
  mdiLightbulbOnOutline,
  mdiOpenInNew,
  mdiThumbUp,
  mdiThumbDown,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import duration from 'dayjs/plugin/duration'
import relativeTime from 'dayjs/plugin/relativeTime'
import { FC, MouseEvent as ReactMouseEvent, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AdChallengePanel } from '@Components/AdChallengePanel'
import { FlagVerdictOverlay } from '@Components/FlagVerdictOverlay'
import { InstanceEntry } from '@Components/InstanceEntry'
import { KothChallengePanel } from '@Components/KothChallengePanel'
import { ContentPlaceholder, InlineMarkdown, Markdown } from '@Components/MarkdownRenderer'
import { ScrollingText } from '@Components/ScrollingText'
import { abbreviatedSha256, attachmentDownloadInfo } from '@Utils/AttachmentDownload'
import { FlagVerdictKind, FlagVerdictState } from '@Utils/FlagVerdict'
import { useLanguage } from '@Utils/I18n'
import { ChallengeCategoryItemProps, HunamizeSize } from '@Utils/Shared'
import { useTicker } from '@Hooks/useTicker'
import { ChallengeDetailModel, ChallengeType, ReviewRating, SolveReceiptMode, SubmissionType } from '@Api'
import classes from '@Styles/ChallengeModal.module.css'
import misc from '@Styles/Misc.module.css'

dayjs.extend(relativeTime)

export interface SolverInfo {
  rank: number
  teamName: string
  teamAvatar: string | null
  userName: string | null
  type: SubmissionType
  time: number
  score: number
}

dayjs.extend(duration)

interface ChallengeDeadlineNoticeProps {
  deadline: dayjs.Dayjs
  onExpiredChange: (expired: boolean) => void
}

const ChallengeDeadlineNotice: FC<ChallengeDeadlineNoticeProps> = ({ deadline, onExpiredChange }) => {
  const { t } = useTranslation()
  // Shared 1s ticker so multiple deadline widgets share one interval.
  const now = useTicker()
  const { locale } = useLanguage()

  useEffect(() => {
    onExpiredChange(now.isAfter(deadline))
  }, [now, deadline, onExpiredChange])

  if (now.isAfter(deadline)) {
    return null
  }

  const formattedDeadline = useMemo(() => deadline.locale(locale).format('L LTS'), [deadline, locale])

  const diff = deadline.diff(now)
  const duration = dayjs.duration(diff)
  const countdownText = `${Math.floor(duration.asHours())}:${duration.format('mm:ss')}`

  return (
    <Group gap="xs" justify="space-between" wrap="nowrap">
      <Text fw="bold" size="sm">
        {t('challenge.content.deadline.remaining')}&nbsp;
        <Text span ff="monospace" fw="bold" size="sm" c="brand">
          {countdownText}
        </Text>
      </Text>
      <Text fw="bold" size="xs" c="dimmed">
        {t('challenge.content.deadline.label')}&nbsp;
        <Text span ff="monospace" c="dimmed" fw="bold" size="xs">
          {formattedDeadline}
        </Text>
      </Text>
    </Group>
  )
}

export interface ChallengeModalProps extends Omit<ModalProps, 'children' | 'stackId' | 'title'> {
  challenge?: ChallengeDetailModel
  cateData: ChallengeCategoryItemProps
  solved?: boolean
  disabled?: boolean
  /** True while a flag submission is in-flight (network or server-side
   *  check) — renders a spinner inside the submit button. */
  submitting?: boolean
  /** Route the instance through the administrator test-container proxy while
   * keeping the player-facing lifecycle controls available in the preview. */
  testInstance?: boolean
  gameTitle?: string
  /** Optional event route shown only when the modal was opened from a global
   * challenge catalog rather than from inside the event. */
  eventHref?: string
  gameEnded?: boolean
  practiceMode?: boolean
  flag: string
  setFlag: (value: string | React.ChangeEvent<any> | null | undefined) => void
  receiptProof: string
  setReceiptProof: (value: string | React.ChangeEvent<any> | null | undefined) => void
  onCreate: () => void
  onExtend?: () => void
  onDestroy: () => void
  onSubmitFlag: () => void
  onDownload?: () => void
  onReviewSubmit?: (rating: ReviewRating, comment: string) => Promise<void>
  /** True only when the flag was accepted in this browser session (not a pre-existing solve). */
  justSolved?: boolean
  solvers?: SolverInfo[]
  /** When set, the modal is rendering an A&D challenge — switches the footer
   *  from the flag-submit form to the AdChallengePanel (status + API docs). */
  gameId?: number
  flagVerdict?: FlagVerdictState | null
  onDismissFlagVerdict?: () => void
}

export const ChallengeModal: FC<ChallengeModalProps> = (props) => {
  const {
    challenge,
    cateData,
    solved,
    justSolved,
    disabled,
    submitting,
    testInstance,
    gameTitle,
    eventHref,
    gameEnded,
    practiceMode,
    flag,
    setFlag,
    receiptProof,
    setReceiptProof,
    onCreate,
    onExtend,
    onDestroy,
    onDownload,
    onSubmitFlag,
    onReviewSubmit,
    solvers,
    gameId,
    flagVerdict,
    onDismissFlagVerdict,
    withOverlay = true,
    overlayProps,
    withCloseButton = true,
    closeButtonProps,
    ...modalProps
  } = props
  // A&D and KotH both run on the live engine — neither has a static challenge
  // score worth showing. Without including KotH, the modal would print the
  // default OriginalScore (e.g. "100 pts") in the header — meaningless for a hill.
  const isKoth = challenge?.type === ChallengeType.KingOfTheHill
  const isAd = challenge?.type === ChallengeType.AttackDefense || isKoth
  // Once an A&D/KotH game has ENDED in practice mode, its challenges fall back to
  // the standard per-team practice container (its own connection address) instead
  // of the live defending-service / hill panel — the backend gates this the same
  // way (GameChallenge.AllowsPracticeContainer) and serves the container context.
  const isPracticeContainer = isAd && !!gameEnded && !!practiceMode
  const readOnlyArchive = !!gameEnded && !practiceMode
  const { t } = useTranslation()
  const theme = useMantineTheme()
  const { locale } = useLanguage()

  const placeholders = t('challenge.content.flag_placeholders', {
    returnObjects: true,
  }) as string[]

  const [placeholder, setPlaceholder] = useState('')
  useEffect(() => {
    setPlaceholder(placeholders[Math.floor(Math.random() * placeholders.length)])
  }, [challenge])

  const [rating, setRating] = useState<ReviewRating>(ReviewRating.None)
  const [comment, setComment] = useState('')
  const [isSubmittingReview, setIsSubmittingReview] = useState(false)
  const [reviewSubmitted, setReviewSubmitted] = useState(false)
  const flagInputRef = useRef<HTMLInputElement>(null)
  const reviewStartRef = useRef<HTMLButtonElement>(null)
  const closeButtonRef = useRef<HTMLButtonElement>(null)
  const focusAfterVerdictRef = useRef<FlagVerdictKind | null>(null)

  const dismissFlagVerdict = () => {
    if (!flagVerdict || !onDismissFlagVerdict) return
    focusAfterVerdictRef.current = flagVerdict.kind
    onDismissFlagVerdict()
  }

  useEffect(() => {
    if (flagVerdict || !focusAfterVerdictRef.current) return

    const kind = focusAfterVerdictRef.current
    focusAfterVerdictRef.current = null
    const frame = window.requestAnimationFrame(() => {
      const preferredTarget = kind === 'success' ? reviewStartRef.current : flagInputRef.current
      const target = preferredTarget && !preferredTarget.disabled ? preferredTarget : closeButtonRef.current
      target?.focus({ preventScroll: true })
    })

    return () => window.cancelAnimationFrame(frame)
  }, [flagVerdict])

  // Reset review state only when a fresh in-session solve occurs
  useEffect(() => {
    if (justSolved) setReviewSubmitted(false)
  }, [justSolved])

  // On switching to a different challenge, sync the review controls to it: prefill
  // the user's existing rating/comment and clear the "submitted this session" flag.
  // Keyed on the challenge id — otherwise submitting one review leaves
  // reviewSubmitted=true and blocks the review UI on every OTHER challenge.
  useEffect(() => {
    setRating((challenge as any)?.userRating ?? ReviewRating.None)
    setComment((challenge as any)?.userComment ?? '')
    setReviewSubmitted(false)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [(challenge as any)?.id])

  // Block close only for challenges solved in this session that haven't been reviewed yet
  const handleClose = () => {
    if (justSolved && !reviewSubmitted) {
      showNotification({
        color: 'orange',
        message: t('challenge.review.required_to_close', 'Please rate this challenge before closing'),
        icon: <Icon path={mdiAlertCircleOutline} size={1} />,
        autoClose: 3000,
      })
      return
    }
    setFlag('')
    modalProps.onClose()
  }

  const deadlineTime = useMemo(() => (challenge?.deadline ? dayjs(challenge.deadline) : null), [challenge?.deadline])
  const [isDeadlinePassed, setIsDeadlinePassed] = useState(() => (deadlineTime ? dayjs().isAfter(deadlineTime) : false))

  useEffect(() => {
    setIsDeadlinePassed(deadlineTime ? dayjs().isAfter(deadlineTime) : false)
  }, [deadlineTime])

  const isLimitReached = (challenge?.limit && (challenge.attempts ?? 0) >= challenge.limit) || false

  const isContainer =
    challenge?.type === ChallengeType.StaticContainer ||
    challenge?.type === ChallengeType.DynamicContainer ||
    isPracticeContainer

  const title = (
    <Stack gap="xs">
      <Group wrap="nowrap" w="100%" justify="space-between" gap="sm">
        <Group wrap="nowrap" gap="sm" w="calc(100% - 6.75rem)">
          {cateData && <Icon path={cateData.icon} size={1.2} color={theme.colors[cateData.color][5]} />}
          <Title order={2} size="h4" lineClamp={1} title={challenge?.title ?? ''}>
            {challenge?.title ?? ''}
          </Title>
        </Group>
        {readOnlyArchive ? (
          <Text miw="6rem" fw="bold" c="dimmed" ff="monospace" ta="right">
            {t('challenge.content.archived', 'ARCHIVED')}
          </Text>
        ) : isAd ? (
          <Text miw="6rem" fw="bold" c={isKoth ? 'violet' : 'red'} ff="monospace" ta="right">
            {t('challenge.content.ad_live', 'LIVE')}
          </Text>
        ) : (
          <Text miw="6rem" fw="bold" ff="monospace" ta="right">
            {challenge?.score ?? 0} pts
          </Text>
        )}
      </Group>
      <Divider size="md" color={isKoth ? 'violet' : isAd ? 'red' : cateData?.color} />
    </Stack>
  )

  const solverIconMap = new Map([
    [SubmissionType.FirstBlood, { path: mdiHexagonSlice6, color: theme.colors.yellow[5] }],
    [SubmissionType.SecondBlood, { path: mdiHexagonSlice4, color: theme.colors.gray[4] }],
    [SubmissionType.ThirdBlood, { path: mdiHexagonSlice2, color: theme.colors.orange[6] }],
    [SubmissionType.Normal, { path: mdiFlag, color: theme.colors[theme.primaryColor][5] }],
  ])

  const content = (
    <ScrollAreaAutosize
      mah="52vh"
      maw="100%"
      scrollbars="y"
      scrollbarSize={6}
      type="scroll"
      data-guide="challenge-material"
    >
      {challenge?.content === undefined ? (
        <ContentPlaceholder />
      ) : (
        <>
          {readOnlyArchive && (
            <Alert mb="md" color="blue" variant="light" icon={<Icon path={mdiAlertCircleOutline} size={0.9} />}>
              {t(
                'challenge.content.read_only_archive',
                'This event has ended. Challenge material is read-only; submissions and workloads are closed.'
              )}
            </Alert>
          )}
          <Markdown source={challenge.content ?? ''} />
          {challenge.hints && challenge.hints.length > 0 && (
            <Stack gap={2} pt="sm">
              {challenge.hints.map((hint) => (
                <Group key={hint} gap="xs" align="flex-start" wrap="nowrap">
                  <Icon path={mdiLightbulbOnOutline} size={0.8} color={theme.colors.yellow[5]} />
                  <InlineMarkdown key={hint} size="sm" maw="calc(100% - 2rem)" source={hint} />
                </Group>
              ))}
            </Stack>
          )}

          {solvers && solvers.length > 0 && (
            <Stack gap={4} pt="md">
              <Divider
                label={
                  <Text size="xs" c="dimmed" fw={500}>
                    Solved by {solvers.length} {solvers.length === 1 ? 'team' : 'teams'}
                  </Text>
                }
                labelPosition="left"
              />
              <ScrollArea h={Math.min(solvers.length * 30, 165)} scrollbarSize={4}>
                <Stack gap={1}>
                  {solvers.map((s, i) => {
                    const icon = solverIconMap.get(s.type) ?? solverIconMap.get(SubmissionType.Normal)!
                    return (
                      <Group key={i} gap={6} wrap="nowrap" px={2} style={{ minWidth: 0 }}>
                        {/* Solve position */}
                        <Text size="xs" c="dimmed" ff="monospace" w={22} ta="right" style={{ flexShrink: 0 }}>
                          {i + 1}.
                        </Text>

                        {/* Blood / solve-type icon */}
                        <Icon path={icon.path} size={0.7} color={icon.color} style={{ flexShrink: 0 }} />

                        {/* Team avatar */}
                        <Avatar src={s.teamAvatar} size={18} radius="xl" style={{ flexShrink: 0 }}>
                          {s.teamName.slice(0, 1)}
                        </Avatar>

                        {/* Team name */}
                        <ScrollingText text={s.teamName} size="xs" fw={600} style={{ flex: '2 1 0', minWidth: 0 }} />

                        {/* Username */}
                        {s.userName && (
                          <ScrollingText
                            text={s.userName}
                            size="xs"
                            c="dimmed"
                            style={{ flex: '1.5 1 0', minWidth: 0 }}
                          />
                        )}

                        {/* Relative time */}
                        <Text size="xs" c="dimmed" ff="monospace" ml="auto" style={{ flexShrink: 0 }}>
                          {dayjs(s.time).fromNow()}
                        </Text>
                      </Group>
                    )
                  })}
                </Stack>
              </ScrollArea>
            </Stack>
          )}
        </>
      )}
    </ScrollAreaAutosize>
  )

  const withDeadline = deadlineTime && !isDeadlinePassed
  const deadline = withDeadline && (
    <ChallengeDeadlineNotice deadline={deadlineTime} onExpiredChange={setIsDeadlinePassed} />
  )

  const withAttachment = !!challenge?.context?.url || onDownload

  const link = challenge?.context?.url
  const downloadInfo = attachmentDownloadInfo(link, challenge?.context?.sha256)
  const local = downloadInfo.isLocal
  const attachmentSize = challenge?.context?.fileSize

  const attachment = withAttachment && (
    <Stack className={classes.attachment} gap={6} data-guide="challenge-attachment">
      <Group gap="sm" justify="space-between" align="flex-start" wrap="wrap">
        <Stack gap={1} style={{ flex: '1 1 14rem', minWidth: 0 }}>
          <Text fw={700} size="sm">
            {t('challenge.button.download.attachment')}
          </Text>
          {downloadInfo.filename && (
            <Text size="xs" c="dimmed" title={downloadInfo.filename} truncate>
              {downloadInfo.filename}
            </Text>
          )}
        </Stack>
        <Button
          component="a"
          href={link ?? '#'}
          data-guide="challenge-attachment-download"
          variant="light"
          size="compact-sm"
          target={local || onDownload ? undefined : '_blank'}
          rel={local || onDownload ? undefined : 'noreferrer'}
          download={local ? (downloadInfo.filename ?? true) : undefined}
          leftSection={<Icon path={local ? mdiDownload : mdiOpenInNew} size={0.8} />}
          onClick={
            onDownload &&
            ((e: ReactMouseEvent<HTMLAnchorElement>) => {
              e.preventDefault()
              onDownload()
            })
          }
        >
          {local ? t('challenge.button.download.now', 'Download now') : t('common.content.external_link')}
        </Button>
      </Group>

      {local && (
        <Group gap={6} align="center" wrap="wrap">
          {typeof attachmentSize === 'number' && attachmentSize >= 0 && (
            <Badge variant="light" color="gray" size="sm">
              {HunamizeSize(attachmentSize)}
            </Badge>
          )}
          <Badge variant="light" color="green" size="sm">
            {t('challenge.content.attachment.resumable', 'Resumable')}
          </Badge>
          {downloadInfo.sha256 && (
            <Group gap={3} wrap="nowrap" style={{ minWidth: 0 }}>
              <Text size="xs" c="dimmed">
                SHA-256
              </Text>
              <Text size="xs" ff="monospace" c="dimmed">
                {abbreviatedSha256(downloadInfo.sha256)}
              </Text>
              <CopyButton value={downloadInfo.sha256} timeout={1500}>
                {({ copied, copy }) => (
                  <Tooltip label={copied ? t('common.content.copied', 'Copied') : t('common.button.copy')}>
                    <ActionIcon
                      variant="subtle"
                      size="sm"
                      color={copied ? 'green' : 'gray'}
                      onClick={copy}
                      aria-label={copied ? t('common.content.copied', 'Copied') : t('common.button.copy')}
                    >
                      <Icon path={copied ? mdiCheck : mdiContentCopy} size={0.7} />
                    </ActionIcon>
                  </Tooltip>
                )}
              </CopyButton>
            </Group>
          )}
        </Group>
      )}
    </Stack>
  )

  const withInstance = !readOnlyArchive && isContainer && challenge?.context

  const instance = withInstance && (
    <InstanceEntry
      test={testInstance}
      lifecycleControls={testInstance}
      label={`${challenge.title} @ ${gameTitle}`}
      context={challenge.context!}
      onCreate={onCreate}
      onExtend={onExtend}
      onDestroy={onDestroy}
      disabled={disabled}
    />
  )

  const attemptsInfo = useMemo(() => {
    if (typeof challenge?.attempts !== 'number' || solved) return null

    let content = null
    if (deadlineTime && isDeadlinePassed) {
      content = t('challenge.content.deadline.expired', {
        deadline: deadlineTime.locale(locale).format('L LTS'),
      })
    } else if (challenge?.limit) {
      const remaining = challenge.limit - challenge.attempts
      if (remaining > 0) {
        content = t('challenge.content.attempts.remaining', { remaining })
      } else {
        content = t('challenge.content.attempts.exhausted')
      }
    } else {
      content = t('challenge.content.attempts.count', { count: challenge.attempts })
    }

    return <Input.Label>{content}</Input.Label>
  }, [challenge?.attempts, challenge?.limit, solved, deadlineTime, locale, isDeadlinePassed, t])

  const inputValue = solved
    ? t('challenge.content.already_solved')
    : isLimitReached
      ? t('challenge.content.attempts.placeholder')
      : flag

  // Allow submission if deadline not passed OR (game ended AND practice mode enabled)
  const canSubmitDespiteDeadline = !isDeadlinePassed || (gameEnded && practiceMode)
  const inputDisabled = disabled || solved || isLimitReached || !canSubmitDespiteDeadline

  // Any SOLVED challenge can be rated/edited (matches the backend upsert), not only
  // one solved in this browser session, and the controls stay visible after
  // submitting so the rating/comment remains editable. The "please rate before
  // closing" nudge stays fresh-solve only.
  const hasExistingReview =
    (challenge as any)?.userRating != null && (challenge as any)?.userRating !== ReviewRating.None
  const reviewSection = solved && (
    <Stack gap="sm">
      <Divider label={t('challenge.review.label', 'Rate this challenge')} labelPosition="center" />
      {justSolved && (
        <Alert icon={<Icon path={mdiAlertCircleOutline} size={0.9} />} color="orange" p="xs">
          <Text size="xs">
            {t('challenge.review.required_notice', 'Please rate this challenge — you can close after submitting.')}
          </Text>
        </Alert>
      )}
      <Group grow>
        <Button
          ref={reviewStartRef}
          variant={rating === ReviewRating.Like ? 'filled' : 'default'}
          color="teal"
          radius="md"
          size="md"
          leftSection={<Icon path={mdiThumbUp} size="1.2rem" />}
          onClick={() => setRating(ReviewRating.Like)}
          styles={(theme) => ({
            root: {
              borderColor: rating === ReviewRating.Like ? undefined : theme.colors.teal[6],
              color: rating === ReviewRating.Like ? undefined : theme.colors.teal[6],
              borderWidth: rating === ReviewRating.Like ? undefined : '1px',
            },
          })}
        >
          {t('common.label.like', 'Recommended')}
        </Button>
        <Button
          variant={rating === ReviewRating.Dislike ? 'filled' : 'default'}
          color="red"
          radius="md"
          size="md"
          leftSection={<Icon path={mdiThumbDown} size="1.2rem" />}
          onClick={() => setRating(ReviewRating.Dislike)}
          styles={(theme) => ({
            root: {
              borderColor: rating === ReviewRating.Dislike ? undefined : theme.colors.red[6],
              color: rating === ReviewRating.Dislike ? undefined : theme.colors.red[6],
              borderWidth: rating === ReviewRating.Dislike ? undefined : '1px',
            },
          })}
        >
          {t('common.label.dislike', 'Not Recommended')}
        </Button>
      </Group>

      <Stack gap={4}>
        <Textarea
          label={t('challenge.review.comment', 'Comment')}
          placeholder={t('challenge.review.placeholder', 'Leave a comment...')}
          value={comment}
          autosize
          minRows={3}
          maxRows={6}
          maxLength={1000}
          onChange={(e) => setComment(e.currentTarget.value)}
        />
        <Group justify="space-between">
          <Text size="xs" c="dimmed">
            {/* Spacer or additional info if needed */}
          </Text>
          <Text size="xs" c={comment.length >= 1000 ? 'red' : 'dimmed'}>
            {comment.length} / 1000
          </Text>
        </Group>
      </Stack>

      <Group justify="flex-end">
        <Button
          loading={isSubmittingReview}
          disabled={rating === ReviewRating.None}
          onClick={async () => {
            if (onReviewSubmit) {
              setIsSubmittingReview(true)
              await onReviewSubmit(rating, comment)
              setIsSubmittingReview(false)
              setReviewSubmitted(true)
              // Fresh-solve nudge: submit then close. When editing an existing
              // review, keep the modal open so it stays editable (onReviewSubmit
              // already shows a "saved" toast).
              if (justSolved) {
                setFlag('')
                modalProps.onClose()
              }
            }
          }}
        >
          {justSolved
            ? t('challenge.review.submit_and_close', 'Submit & Close')
            : hasExistingReview
              ? t('challenge.review.update', 'Update review')
              : t('challenge.review.save', 'Save review')}
        </Button>
      </Group>
    </Stack>
  )

  const eventAction = eventHref && (
    <>
      <Divider />
      <Group justify="space-between" gap="sm" wrap="wrap">
        <Text size="sm" c="dimmed" fw={600} lineClamp={1} style={{ flex: '1 1 12rem' }}>
          {gameTitle}
        </Text>
        <Button
          component="a"
          href={eventHref}
          variant="light"
          size="compact-sm"
          rightSection={<Icon path={mdiArrowRight} size={0.75} />}
        >
          {t('challenge.button.go_to_event', 'Go to event')}
        </Button>
      </Group>
    </>
  )

  const footer =
    isAd && gameId && !isPracticeContainer && !readOnlyArchive ? (
      <Stack gap="xs" className={classes.footer}>
        <Divider />
        {/* A&D/KotH challenges can ship a downloadable attachment (e.g. the
          service source to attack + patch) just like jeopardy challenges. */}
        {withAttachment && attachment}
        {/* KotH has a shared hill, no per-team service — AdChallengePanel's
          adState.services.find would return undefined and render the
          misleading "no service for your team yet" alert. Route to the
          KotH-specific panel that knows about the hill + per-tick token. */}
        {isKoth ? (
          <KothChallengePanel gameId={gameId} challengeId={challenge?.id ?? 0} />
        ) : (
          <AdChallengePanel gameId={gameId} challengeId={challenge?.id ?? 0} />
        )}
        {eventAction}
      </Stack>
    ) : (
      <Stack gap="xs" className={classes.footer}>
        {(withAttachment || withInstance || withDeadline) && (
          <>
            <Divider mb={attemptsInfo ? '0.2rem' : undefined} />
            {attachment}
            {instance}
            {deadline}
          </>
        )}
        {/* A&D/KotH shown as a post-end practice container: keep the team's
          defended-service backup (snapshot) download available here. */}
        {isPracticeContainer && gameId && (
          <AdChallengePanel gameId={gameId} challengeId={challenge?.id ?? 0} snapshotOnly />
        )}
        {!readOnlyArchive && (
          <>
            <Divider label={attemptsInfo} my={attemptsInfo ? '-0.4rem' : undefined} />
            <form
              data-guide="flag-submit"
              onSubmit={(e) => {
                e.preventDefault()
                if (!solved && canSubmitDespiteDeadline) {
                  onSubmitFlag()
                }
              }}
            >
              <Stack gap="xs">
                {(challenge?.solveReceiptMode ?? SolveReceiptMode.Disabled) !== SolveReceiptMode.Disabled && (
                  <Textarea
                    label={t('challenge.label.solve_receipt', 'Trusted solve receipt')}
                    description={t('challenge.content.solve_receipt', {
                      defaultValue:
                        'One-use proof from {{issuer}}. It is bound to this answer and expires after 10 minutes.',
                      issuer:
                        challenge?.receiptVerifierIdentity ??
                        t('common.label.not_available', 'the configured verifier'),
                    })}
                    required={challenge?.solveReceiptMode === SolveReceiptMode.Required}
                    minRows={2}
                    maxRows={4}
                    autosize
                    value={receiptProof}
                    disabled={inputDisabled}
                    onChange={setReceiptProof}
                    styles={{ input: { fontFamily: 'monospace', fontSize: 'var(--mantine-font-size-xs)' } }}
                  />
                )}
                <Group justify="space-between" gap="sm" align="flex-end">
                  <TextInput
                    ref={flagInputRef}
                    label={t('challenge.label.flag', 'Flag')}
                    placeholder={placeholder}
                    value={inputValue}
                    disabled={inputDisabled}
                    onChange={setFlag}
                    classNames={{ root: misc.flexGrow, input: misc.ffmono }}
                  />
                  <Button miw="6rem" type="submit" disabled={inputDisabled} loading={submitting}>
                    {t('challenge.button.submit_flag')}
                  </Button>
                </Group>
              </Stack>
            </form>
          </>
        )}
        {reviewSection}
        {eventAction}
      </Stack>
    )

  return (
    <Modal.Root
      size="min(46rem, calc(100vw - 1.5rem))"
      {...modalProps}
      onClose={handleClose}
      closeOnEscape={flagVerdict ? false : modalProps.closeOnEscape}
      closeOnClickOutside={flagVerdict ? false : modalProps.closeOnClickOutside}
      zIndex={flagVerdict ? 6000 : modalProps.zIndex}
      centered
      classNames={classes}
    >
      {withOverlay && <Modal.Overlay {...overlayProps} />}
      <Modal.Content className={flagVerdict ? classes.verdictContent : undefined}>
        {flagVerdict ? (
          <FlagVerdictOverlay
            key={flagVerdict.sequence}
            verdict={flagVerdict}
            challengeTitle={challenge?.title ?? ''}
            score={flagVerdict.kind === 'success' && !gameEnded ? challenge?.score : undefined}
            onDismiss={dismissFlagVerdict}
          />
        ) : (
          <>
            <div className={classes.header}>
              <Modal.Title>{title}</Modal.Title>
              {withCloseButton && (
                <Modal.CloseButton
                  {...closeButtonProps}
                  ref={closeButtonRef}
                  aria-label={closeButtonProps?.['aria-label'] ?? t('common.button.close', 'Close')}
                />
              )}
            </div>
            <Modal.Body>{content}</Modal.Body>
            {footer}
          </>
        )}
      </Modal.Content>
    </Modal.Root>
  )
}
