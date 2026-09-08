import { Button, Group, NumberInput, Paper, Stack, Text } from '@mantine/core'
import { useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import api, { type WriteupGradeChallenge, type WriteupGradeResult } from '@Api'
import classes from '@Styles/WriteupGrading.module.css'

export function WriteupGradeRow({
  gameId,
  participationId,
  challenge,
  onSaved,
}: {
  gameId: number
  participationId: number
  challenge: WriteupGradeChallenge
  onSaved: (result: WriteupGradeResult) => Promise<unknown>
}) {
  const { t } = useTranslation()
  const [value, setValue] = useState<string | number>(challenge.percentage ?? 100)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState('')
  const [operation, setOperation] = useState<{ percentage: number | null; id: string }>()
  useEffect(() => {
    setValue(challenge.percentage ?? 100)
    setOperation(undefined)
  }, [challenge.percentage, challenge.revision])
  const valid = typeof value === 'number' && Number.isInteger(value) && value >= 0 && value <= 100
  const save = async (percentage: number | null) => {
    if (saving) return
    const next = operation?.percentage === percentage ? operation : { percentage, id: crypto.randomUUID() }
    setOperation(next)
    setSaving(true)
    setError('')
    try {
      const result = await api.admin.saveWriteupGrade(gameId, participationId, challenge.challengeId, {
        percentage,
        expectedRevision: challenge.revision,
        operationId: next.id,
      })
      await onSaved(result.data)
    } catch {
      setError(
        t(
          'admin.grading.save_error',
          'Grade not confirmed. Retry, or refresh scores & grades if another admin edited this challenge.'
        )
      )
    } finally {
      setSaving(false)
    }
  }
  return (
    <Paper component="section" withBorder p="sm" radius="md" aria-label={challenge.title}>
      <Stack gap="xs">
        <Text fw={600} style={{ overflowWrap: 'anywhere' }}>
          {challenge.title}
        </Text>
        <Text size="xs" c="dimmed">
          {challenge.mode === 'AttackDefense' ? 'A&D' : challenge.mode === 'KingOfTheHill' ? 'KoTH' : 'Jeopardy'} ·{' '}
          {t('admin.grading.earned_points', '{{points}} earned points', {
            points: challenge.earnedPoints.toLocaleString(undefined, { maximumFractionDigits: 4 }),
          })}
        </Text>
        <Text size="sm">
          {challenge.percentage === null
            ? t('admin.grading.ungraded', 'Ungraded · retains 100%')
            : t('admin.grading.recorded', 'Saved grade: {{percentage}}%', { percentage: challenge.percentage })}
        </Text>
        <Text size="sm">
          {t('admin.grading.retained', '{{points}} points retained', {
            points: ((challenge.earnedPoints * (challenge.percentage ?? 100)) / 100).toLocaleString(undefined, {
              maximumFractionDigits: 4,
            }),
          })}
        </Text>
        <NumberInput
          label={t('admin.grading.percentage', 'Writeup grade (%)')}
          aria-label={t('admin.grading.percentage_for', 'Writeup grade (%) for {{challenge}}', {
            challenge: challenge.title,
          })}
          min={0}
          max={100}
          allowDecimal={false}
          clampBehavior="none"
          value={value}
          onChange={setValue}
          disabled={saving}
          error={!valid ? t('admin.grading.valid_percentage', 'Enter a whole number from 0 to 100.') : undefined}
        />
        <Group gap="xs">
          <Button
            size="xs"
            loading={saving}
            disabled={!valid || value === challenge.percentage}
            onClick={() => {
              if (valid) void save(Number(value))
            }}
          >
            {t('admin.grading.save', 'Save grade')}
          </Button>
          <Button
            size="xs"
            variant="default"
            disabled={saving || challenge.percentage === null}
            onClick={() => void save(null)}
          >
            {t('admin.grading.clear', 'Mark ungraded')}
          </Button>
        </Group>
        {error && (
        <Text size="sm" className={classes.errorText} role="alert">
            {error}
          </Text>
        )}
      </Stack>
    </Paper>
  )
}
