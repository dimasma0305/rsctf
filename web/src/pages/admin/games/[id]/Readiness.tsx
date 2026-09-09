import { Alert, Button, Group, Skeleton, Stack, Text, Title } from '@mantine/core'
import { useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useParams } from 'react-router'
import { WithGameEditTab } from '@Components/admin/WithGameEditTab'
import { canShowReadinessCache, eventReadiness, readinessError, type ReadinessState } from '@Utils/EventReadiness'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api, { ChallengeType } from '@Api'
import classes from '@Styles/EventReadiness.module.css'

const priority: Record<ReadinessState, number> = { attention: 0, unverified: 1, checked: 2, info: 3 }

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
  const attention = checks.filter((check) => check.state === 'attention').length
  const hasCompetitiveServices =
    hasSnapshot &&
    challenges.some(
      (challenge) => challenge.type === ChallengeType.AttackDefense || challenge.type === ChallengeType.KingOfTheHill
    )
  const root = `/admin/games/${eventId}`

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
            <div className={classes.snapshot} data-readiness-snapshot>
              <Stack gap="xs">
                <Text fw={650} style={{ overflowWrap: 'anywhere' }}>
                  {game.title}
                </Text>
                <Group gap="xs">
                  <Text size="xs" c="dimmed">
                    {t(game.hidden ? 'admin.readiness.hidden' : 'admin.readiness.public')}
                  </Text>
                  <Text size="xs" c="dimmed">
                    {t(game.practiceMode ? 'admin.readiness.practice' : 'admin.readiness.competition')}
                  </Text>
                </Group>
                <Text size="sm" role="status" aria-live="polite">
                  {refreshing
                    ? t('admin.readiness.refreshing')
                    : errorKind
                      ? t('admin.readiness.stale')
                      : t('admin.readiness.summary', { count: attention })}
                </Text>
                <Text size="sm" c="dimmed">
                  {t('admin.readiness.snapshot_note')}
                </Text>
              </Stack>
            </div>
            <div className={classes.checks}>
              {checks.map((check) => (
                <section
                  key={check.key}
                  className={classes.check}
                  aria-labelledby={`readiness-${check.key}`}
                  data-readiness-check={check.key}
                >
                  <Stack gap={6} className={classes.checkCopy}>
                    <Group justify="space-between" gap="xs">
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
            <section className={classes.manual} aria-labelledby="readiness-runtime">
              <Stack gap="sm">
                <Title order={2} size="h5" id="readiness-runtime">
                  {t('admin.readiness.runtime_title')}
                </Title>
                <Text size="sm" c="dimmed">
                  {t('admin.readiness.runtime_note')}
                </Text>
                {[
                  { key: 'teams', section: 'review' },
                  { key: 'attachments', section: 'challenges' },
                  ...(hasCompetitiveServices ? [{ key: 'services', section: 'adops' }] : []),
                  ...(game.vpnAccessRequired ? [{ key: 'vpn', section: 'info' }] : []),
                ].map((item) => (
                  <div key={item.key} className={classes.manualRow}>
                    <Text size="sm">{t(`admin.readiness.manual.${item.key}`)}</Text>
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
                ))}
              </Stack>
            </section>
          </>
        )}
      </Stack>
    </WithGameEditTab>
  )
}
