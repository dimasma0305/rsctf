import { Alert, Badge, Button, Card, Group, Loader, Select, Stack, Text, Textarea } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
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
const REVIEW_NOTE_LIMIT = 4_000

export const truncateReviewNote = (value: string) => Array.from(value).slice(0, REVIEW_NOTE_LIMIT).join('')

export function FusedEvidencePanel({ gameId, participationId }: { gameId: number; participationId: number }) {
  const { t } = useTranslation()
  const [result, setResult] = useState<FusedEvidenceBreakdown | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [activeFinding, setActiveFinding] = useState<number | null>(null)
  const [status, setStatus] = useState<FindingReviewStatus>('needsMoreEvidence')
  const [note, setNote] = useState('')
  const [saving, setSaving] = useState(false)
  const identity = `${gameId}:${participationId}`
  const identityRef = useRef(identity)
  const loadGeneration = useRef(0)
  const loadAbort = useRef<AbortController | null>(null)
  const savingRef = useRef(false)
  const activeFindingRef = useRef<number | null>(null)

  const reload = useCallback(async () => {
    const requestIdentity = `${gameId}:${participationId}`
    const generation = ++loadGeneration.current
    loadAbort.current?.abort()
    const controller = new AbortController()
    loadAbort.current = controller
    setLoading(true)
    setError(null)
    try {
      const response = await api.eventSecurity.fusedBreakdown(gameId, participationId, { signal: controller.signal })
      if (identityRef.current !== requestIdentity || loadGeneration.current !== generation) return
      setResult(response.data)
    } catch (requestError) {
      if (controller.signal.aborted || identityRef.current !== requestIdentity || loadGeneration.current !== generation)
        return
      setError(tryGetErrorMsg(requestError, t))
    } finally {
      if (identityRef.current === requestIdentity && loadGeneration.current === generation) setLoading(false)
    }
  }, [gameId, participationId, t])

  useEffect(() => {
    identityRef.current = identity
    loadGeneration.current += 1
    loadAbort.current?.abort()
    setResult(null)
    setError(null)
    setActiveFinding(null)
    activeFindingRef.current = null
    setStatus('needsMoreEvidence')
    setNote('')
    savingRef.current = false
    setSaving(false)
    void reload()
    return () => {
      loadGeneration.current += 1
      loadAbort.current?.abort()
    }
  }, [identity, reload])

  const relationships = useMemo(() => {
    const counts = new Map<number, number>()
    for (const relation of result?.relationships || []) {
      counts.set(relation.findingId, (counts.get(relation.findingId) || 0) + 1)
    }
    return counts
  }, [result?.relationships])

  const saveReview = async (finding: AntiCheatFindingRow) => {
    if (savingRef.current || activeFinding !== finding.id) return
    const reviewIdentity = `${identity}:${finding.id}`
    savingRef.current = true
    setSaving(true)
    try {
      await api.eventSecurity.reviewFinding(gameId, finding.id, { status, note: note.trim() || null })
      if (`${identityRef.current}:${activeFindingRef.current}` !== reviewIdentity) return
      showNotification({ color: 'teal', message: 'Evidence review recorded' })
      setActiveFinding(null)
      activeFindingRef.current = null
      setStatus('needsMoreEvidence')
      setNote('')
      await reload()
    } catch (requestError) {
      if (`${identityRef.current}:${activeFindingRef.current}` === reviewIdentity)
        setError(tryGetErrorMsg(requestError, t))
    } finally {
      savingRef.current = false
      if (identityRef.current === identity) setSaving(false)
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
                    onClick={() => {
                      const opening = activeFinding !== finding.id
                      const nextFinding = opening ? finding.id : null
                      activeFindingRef.current = nextFinding
                      setActiveFinding(nextFinding)
                      setStatus('needsMoreEvidence')
                      setNote('')
                      setError(null)
                    }}
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
                      description={`${Array.from(note).length} / ${REVIEW_NOTE_LIMIT}`}
                      onChange={(event) => setNote(truncateReviewNote(event.currentTarget.value))}
                    />
                    <Button size="xs" loading={saving} disabled={saving} onClick={() => void saveReview(finding)}>
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
