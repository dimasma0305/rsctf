import { Alert, Badge, Button, Card, Group, Loader, Select, Stack, Text, Textarea } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'

import api, {
  AntiCheatFindingRow,
  EvidenceFamily,
  FindingReviewStatus,
  FusedEvidenceBreakdown,
} from '@Api'
import { tryGetErrorMsg } from '@Utils/Shared'

const familyLabels: Record<EvidenceFamily, string> = {
  identityCorrelation: 'Identity correlation',
  networkSession: 'Network session',
  timingCadence: 'Timing and cadence',
  trajectorySimilarity: 'Solve trajectory',
  crossTeamPossession: 'Cross-team possession',
  trustedProvenance: 'Trusted provenance',
}

const reviewOptions: Array<{ value: FindingReviewStatus; label: string }> = [
  { value: 'explained', label: 'Explained' },
  { value: 'suspicious', label: 'Suspicious' },
  { value: 'confirmed', label: 'Confirmed' },
  { value: 'dismissed', label: 'Dismissed' },
  { value: 'needsMoreEvidence', label: 'Needs more evidence' },
]

const reviewLabels = reviewOptions.map(({ label }) => label)
const tierLabels = ['Context / 0 points', 'Behavioral', 'Strong', 'Hard']

export function FusedEvidencePanel({ gameId, participationId }: { gameId: number; participationId: number }) {
  const { t } = useTranslation()
  const [result, setResult] = useState<FusedEvidenceBreakdown | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [activeFinding, setActiveFinding] = useState<number | null>(null)
  const [status, setStatus] = useState<FindingReviewStatus>('needsMoreEvidence')
  const [note, setNote] = useState('')
  const [saving, setSaving] = useState(false)

  const reload = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const response = await api.eventSecurity.fusedBreakdown(gameId, participationId)
      setResult(response.data)
    } catch (requestError) {
      setError(tryGetErrorMsg(requestError, t))
    } finally {
      setLoading(false)
    }
  }, [gameId, participationId, t])

  useEffect(() => {
    void reload()
  }, [reload])

  const relationships = useMemo(() => {
    const counts = new Map<number, number>()
    for (const relation of result?.relationships || []) {
      counts.set(relation.findingId, (counts.get(relation.findingId) || 0) + 1)
    }
    return counts
  }, [result?.relationships])

  const saveReview = async (finding: AntiCheatFindingRow) => {
    setSaving(true)
    try {
      await api.eventSecurity.reviewFinding(gameId, finding.id, { status, note: note.trim() || null })
      showNotification({ color: 'teal', message: 'Evidence review recorded' })
      setActiveFinding(null)
      setNote('')
      await reload()
    } catch (requestError) {
      setError(tryGetErrorMsg(requestError, t))
    } finally {
      setSaving(false)
    }
  }

  if (loading && !result) {
    return (
      <Group justify="center" py="sm">
        <Loader size="sm" />
        <Text size="sm" c="dimmed">Loading fused evidence…</Text>
      </Group>
    )
  }

  return (
    <Stack gap="sm">
      <Group justify="space-between" align="center">
        <div>
          <Text fw={700}>Fused evidence families</Text>
          <Text size="xs" c="dimmed">
            Independent signals are related for review; no combination is presented as 100% certainty.
          </Text>
        </div>
        <Button variant="subtle" size="compact-sm" loading={loading} onClick={() => void reload()}>
          Refresh
        </Button>
      </Group>
      {error && <Alert color="red">{error}</Alert>}
      {result && (
        <>
          <Group gap="xs">
            <Badge color={result.band === 'evidenced' ? 'red' : result.band === 'investigate' ? 'orange' : 'gray'}>
              {result.bandLabel}
            </Badge>
            <Badge variant="light">Fused score {result.total}</Badge>
            <Badge variant="light">{result.independentActionableFamilies} actionable families</Badge>
            <Badge color={result.reviewerConfirmed ? 'teal' : 'gray'} variant="light">
              {result.reviewerConfirmed ? 'Reviewer confirmed' : 'Not reviewer confirmed'}
            </Badge>
          </Group>
          <Group gap="xs" align="stretch">
            {result.families.map((family) => (
              <Card key={family.family} withBorder padding="xs" miw={180}>
                <Text size="xs" fw={700}>{familyLabels[family.family]}</Text>
                <Text size="xs" c="dimmed">
                  context {family.contextCount} · behavioral {family.behavioral} · strong {family.strong} · hard{' '}
                  {family.hard}
                </Text>
              </Card>
            ))}
          </Group>
          {result.findings.map((finding) => (
            <Card key={finding.id} withBorder padding="sm">
              <Stack gap="xs">
                <Group justify="space-between" align="flex-start">
                  <div>
                    <Group gap="xs">
                      <Text size="sm" fw={700}>{finding.detectorCode}</Text>
                      <Badge size="xs" variant="light">{tierLabels[finding.evidenceTier] || 'Unknown tier'}</Badge>
                      {finding.shadow && <Badge size="xs" color="gray">shadow / no score</Badge>}
                      {finding.latestReviewStatus != null && (
                        <Badge size="xs" color="blue" variant="light">
                          {reviewLabels[finding.latestReviewStatus] || 'Reviewed'}
                        </Badge>
                      )}
                    </Group>
                    <Text size="xs" c="dimmed">
                      {new Date(finding.occurredAtUtc).toLocaleString()} · {relationships.get(finding.id) || 0} evidence links
                    </Text>
                  </div>
                  <Button
                    size="compact-xs"
                    variant="light"
                    onClick={() => setActiveFinding(activeFinding === finding.id ? null : finding.id)}
                  >
                    Review
                  </Button>
                </Group>
                <Text size="xs" ff="monospace" style={{ overflowWrap: 'anywhere' }}>
                  {JSON.stringify(finding.details)}
                </Text>
                {activeFinding === finding.id && (
                  <Stack gap="xs">
                    <Select
                      label="Disposition"
                      data={reviewOptions}
                      value={status}
                      onChange={(value) => value && setStatus(value as FindingReviewStatus)}
                    />
                    <Textarea
                      label="Reviewer note"
                      value={note}
                      maxLength={4000}
                      onChange={(event) => setNote(event.currentTarget.value)}
                    />
                    <Button size="xs" loading={saving} onClick={() => void saveReview(finding)}>
                      Record review
                    </Button>
                  </Stack>
                )}
              </Stack>
            </Card>
          ))}
          {!result.findings.length && (
            <Text size="sm" c="dimmed">No bounded event-network findings were recorded for this participation.</Text>
          )}
        </>
      )}
    </Stack>
  )
}
