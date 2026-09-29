import { Alert, Badge, Button, Checkbox, Group, Skeleton, Stack, Text, Title } from '@mantine/core'
import { mdiAlertCircle, mdiCheckCircle, mdiHelpCircleOutline, mdiInformationOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useParams } from 'react-router'
import { ImagePreflightPanel } from '@Components/admin/ImagePreflightPanel'
import { WithGameEditTab } from '@Components/admin/WithGameEditTab'
import {
  canShowReadinessCache,
  eventReadiness,
  loadManualReadiness,
  manualReadinessItems,
  readinessCounts,
  readinessError,
  saveManualReadiness,
  type ManualReadinessKey,
  type ReadinessState,
} from '@Utils/EventReadiness'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api, { ChallengeType } from '@Api'
import classes from '@Styles/EventReadiness.module.css'

const priority: Record<ReadinessState, number> = { attention: 0, unverified: 1, checked: 2, info: 3 }
const STATES: ReadinessState[] = ['attention', 'unverified', 'checked', 'info']
const STATE_ICON: Record<ReadinessState, string> = {
  attention: mdiAlertCircle,
  unverified: mdiHelpCircleOutline,
  checked: mdiCheckCircle,
  info: mdiInformationOutline,
}

const StateIcon = ({ state }: { state: ReadinessState }) => (
  <span className={classes.stateIcon} data-state={state} aria-hidden="true">
    <Icon path={STATE_ICON[state]} size={0.85} />
  </span>
)

export default function EventReadiness() {
  const { id } = useParams()
  const eventId = Number(id)
  const validId = Number.isSafeInteger(eventId) && eventId > 0
  const { t } = useTranslation()
  const gameQuery = api.edit.useEditGetGame(eventId, OnceSWRConfig, validId)
  const challengesQuery = api.edit.useEditGetGameChallenges(eventId, OnceSWRConfig, validId)
  const [refreshing, setRefreshing] = useState(false)
  const refreshOwner = useRef(false)
  const errors = [gameQuery.error, challengesQuery.error].filter(Boolean)
  const accessError = errors.find((error) => !canShowReadinessCache([error]))
  const errorKind = !validId ? 'missing' : errors.length ? readinessError(accessError ?? errors[0]) : undefined
  const game = gameQuery.data
  const challenges = challengesQuery.data
  const hasSnapshot = validId && game && Array.isArray(challenges) && canShowReadinessCache(errors)
  const checks = (hasSnapshot ? eventReadiness(game, challenges) : []).toSorted(
    (left, right) => priority[left.state] - priority[right.state]
  )
  const counts = readinessCounts(checks)
  const attention = counts.attention
  const hasCompetitiveServices =
    hasSnapshot &&
    challenges.some(
      (challenge) => challenge.type === ChallengeType.AttackDefense || challenge.type === ChallengeType.KingOfTheHill
    )
  const root = `/admin/games/${eventId}`
  // Time of the snapshot on screen; a new read replaces both objects.
  const snapshotAt = useMemo(() => (game && challenges ? Date.now() : null), [game, challenges])
  const scheduleWindow =
    game && Number.isFinite(game.start) && Number.isFinite(game.end) && game.start < game.end
      ? t('admin.readiness.schedule_window', {
          start: dayjs(game.start).format('LLL'),
          end: dayjs(game.end).format('LLL'),
          zone: `UTC${dayjs(game.start).format('Z')}`,
        })
      : null

  const manualItems = hasSnapshot
    ? manualReadinessItems({
        competitiveServices: hasCompetitiveServices === true,
        vpnRequired: game.vpnAccessRequired === true,
      })
    : []
  const [manualTicks, setManualTicks] = useState(() => ({ eventId, keys: loadManualReadiness(eventId) }))
  const ticked = manualTicks.eventId === eventId ? manualTicks.keys : loadManualReadiness(eventId)
  const manualDone = manualItems.filter((item) => ticked.includes(item.key)).length
  const setTicked = (key: ManualReadinessKey, done: boolean) => {
    const keys = done ? [...ticked.filter((item) => item !== key), key] : ticked.filter((item) => item !== key)
    saveManualReadiness(eventId, keys)
    setManualTicks({ eventId, keys })
  }

  const refresh = async () => {
    if (refreshOwner.current) return
    refreshOwner.current = true
    setRefreshing(true)
    try {
      // Both reads settle independently; failures remain visible through SWR.
      await Promise.allSettled([gameQuery.mutate(), challengesQuery.mutate()])
    } finally {
      refreshOwner.current = false
      setRefreshing(false)
    }
  }

  return (
    <WithGameEditTab>
      <Stack gap="lg" data-event-readiness>
        <Group justify="space-between" align="center" gap="sm">
          <Text size="sm" c="dimmed" maw="42rem">
            {t('admin.readiness.intro')}
          </Text>
          <Button
            variant="default"
            mih={44}
            onClick={refresh}
            loading={refreshing}
            disabled={!validId}
            data-readiness-refresh
          >
            {t('admin.readiness.refresh')}
          </Button>
        </Group>
        {errorKind && (
          <Alert color="orange" role="alert" title={t('admin.readiness.load_failed')} data-readiness-error>
            <Stack gap="xs">
              <Text size="sm">{t(`admin.readiness.errors.${errorKind}`)}</Text>
              {hasSnapshot && (
                <Text size="sm" fw={600}>
                  {t('admin.readiness.stale')}
                </Text>
              )}
              {errorKind === 'session' && (
                <Button
                  component={Link}
                  to={`/account/login?from=${encodeURIComponent(`${root}/readiness`)}`}
                  variant="default"
                  mih={44}
                >
                  {t('admin.readiness.sign_in')}
                </Button>
              )}
            </Stack>
          </Alert>
        )}
        {!hasSnapshot && !errorKind && (
          <Stack aria-busy="true" data-readiness-loading>
            <Text role="status">{t('admin.readiness.loading')}</Text>
            {[0, 1, 2].map((key) => (
              <Skeleton key={key} height={90} />
            ))}
          </Stack>
        )}
        {hasSnapshot && (
          <>
            <section className={classes.snapshot} aria-labelledby="readiness-overview" data-readiness-snapshot>
              <Stack gap="sm">
                <Group justify="space-between" align="flex-start" gap="xs">
                  <Title order={2} size="h4" id="readiness-overview" style={{ overflowWrap: 'anywhere', minWidth: 0 }}>
                    {game.title}
                  </Title>
                  <Group gap={6}>
                    <Badge variant="default" tt="none">
                      {t(game.hidden ? 'admin.readiness.hidden' : 'admin.readiness.public')}
                    </Badge>
                    <Badge variant="default" tt="none">
                      {t(game.practiceMode ? 'admin.readiness.practice' : 'admin.readiness.competition')}
                    </Badge>
                  </Group>
                </Group>
                {scheduleWindow && (
                  <Text size="sm" c="dimmed" data-readiness-window>
                    {scheduleWindow}
                  </Text>
                )}
                <ul className={classes.counts} aria-label={t('admin.readiness.counts_label')} data-readiness-counts>
                  {STATES.map((state) => (
                    <li key={state} className={classes.count} data-state={state} data-empty={counts[state] === 0}>
                      <StateIcon state={state} />
                      <span className={classes.countValue}>{counts[state]}</span>
                      <span className={classes.countLabel}>{t(`admin.readiness.states.${state}`)}</span>
                    </li>
                  ))}
                </ul>
                <Text size="sm" fw={600} role="status" aria-live="polite">
                  {refreshing
                    ? t('admin.readiness.refreshing')
                    : errorKind
                      ? t('admin.readiness.stale')
                      : t('admin.readiness.summary', { count: attention })}
                </Text>
                <Text size="xs" c="dimmed">
                  {t('admin.readiness.snapshot_note')}
                  {snapshotAt !== null && (
                    <> {t('admin.readiness.snapshot_time', { time: dayjs(snapshotAt).format('LTS') })}</>
                  )}
                </Text>
              </Stack>
            </section>
            <div className={classes.checks}>
              {checks.map((check) => (
                <section
                  key={check.key}
                  className={classes.check}
                  aria-labelledby={`readiness-${check.key}`}
                  data-readiness-check={check.key}
                  data-state={check.state}
                >
                  <StateIcon state={check.state} />
                  <Stack gap={4} className={classes.checkCopy}>
                    <Group gap="xs">
                      <Title order={2} size="h5" id={`readiness-${check.key}`}>
                        {t(`admin.readiness.checks.${check.key}`)}
                      </Title>
                      <Text component="span" size="xs" className={classes.status} data-state={check.state}>
                        {t(`admin.readiness.states.${check.state}`)}
                      </Text>
                    </Group>
                    <Text size="sm">{t(`admin.readiness.reasons.${check.reason}`, { count: check.count })}</Text>
                  </Stack>
                  <Button
                    component={Link}
                    to={`${root}/${check.section}`}
                    variant="subtle"
                    size="compact-sm"
                    mih={44}
                    aria-label={t('admin.readiness.open_check', { check: t(`admin.readiness.checks.${check.key}`) })}
                  >
                    {t(`admin.readiness.actions.${check.section}`)}
                  </Button>
                </section>
              ))}
            </div>
            <section className={classes.manual} aria-labelledby="readiness-runtime" data-readiness-manual>
              <Stack gap="sm">
                <Group justify="space-between" align="baseline" gap="xs">
                  <Title order={2} size="h5" id="readiness-runtime">
                    {t('admin.readiness.runtime_title')}
                  </Title>
                  <Text size="sm" fw={600} role="status" aria-live="polite" data-readiness-manual-progress>
                    {t('admin.readiness.manual_progress', { done: manualDone, total: manualItems.length })}
                  </Text>
                </Group>
                <Text size="sm" c="dimmed">
                  {t('admin.readiness.runtime_note')} {t('admin.readiness.manual_local')}
                </Text>
                <div>
                  {manualItems.map((item) => {
                    const done = ticked.includes(item.key)
                    return (
                      <div
                        key={item.key}
                        className={classes.manualRow}
                        data-readiness-manual-item={item.key}
                        data-done={done}
                      >
                        <Checkbox
                          checked={done}
                          onChange={(event) => setTicked(item.key, event.currentTarget.checked)}
                          label={t(`admin.readiness.manual.${item.key}`)}
                          classNames={{ root: classes.manualCheck, label: classes.manualLabel }}
                        />
                        <div>
                          <Button
                            component={Link}
                            to={`${root}/${item.section}`}
                            variant="subtle"
                            size="compact-sm"
                            mih={44}
                          >
                            {t(`admin.readiness.actions.${item.key}`)}
                          </Button>
                        </div>
                      </div>
                    )
                  })}
                </div>
              </Stack>
            </section>
            <ImagePreflightPanel eventId={eventId} enabled={validId} />
          </>
        )}
      </Stack>
    </WithGameEditTab>
  )
}
