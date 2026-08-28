import { Alert, Badge, Button, Card, Group, Loader, Select, Stack, Text, Textarea } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { tryGetErrorMsg } from '@Utils/Shared'
import api, { AntiCheatFindingRow, EvidenceFamily, FindingReviewStatus, FusedEvidenceBreakdown } from '@Api'

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
const MAX_REVIEW_NOTE_LENGTH = 4000

interface ReviewIdentity {
  gameId: number
  participationId: number
  findingId: number
}

interface ReviewDraft {
  identity: ReviewIdentity
  status: FindingReviewStatus
  note: string
}

interface SaveOwner {
  identity: ReviewIdentity
  operation: symbol
}

const sameReviewIdentity = (left: ReviewIdentity, right: ReviewIdentity): boolean =>
  left.gameId === right.gameId && left.participationId === right.participationId && left.findingId === right.findingId

export const fusedEvidenceMatchesScope = (
  value: FusedEvidenceBreakdown,
  gameId: number,
  participationId: number
): boolean =>
  value.participationId === participationId &&
  value.findings.every((finding) => finding.gameId === gameId && finding.participationId === participationId)

const findingStatus = (finding: AntiCheatFindingRow): FindingReviewStatus =>
  reviewOptions[finding.latestReviewStatus ?? -1]?.value ?? 'needsMoreEvidence'

export function FusedEvidencePanel({ gameId, participationId }: { gameId: number; participationId: number }) {
  const { t } = useTranslation()
  const [result, setResult] = useState<FusedEvidenceBreakdown | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [draft, setDraft] = useState<ReviewDraft | null>(null)
  const [failedDraft, setFailedDraft] = useState<ReviewIdentity | null>(null)
  const [saving, setSaving] = useState(false)
  const loadGeneration = useRef(0)
  const loadAbort = useRef<AbortController | null>(null)
  const saveOperation = useRef<SaveOwner | null>(null)
  const reviewNoteRef = useRef<HTMLTextAreaElement>(null)

  const reload = useCallback(async () => {
    const generation = ++loadGeneration.current
    loadAbort.current?.abort()
    const controller = new AbortController()
    loadAbort.current = controller
    setLoading(true)
    setError(null)
    try {
      const response = await api.eventSecurity.fusedBreakdown(gameId, participationId, { signal: controller.signal })
      if (controller.signal.aborted || loadGeneration.current !== generation) return
      if (!fusedEvidenceMatchesScope(response.data, gameId, participationId)) {
        throw new Error('The evidence response did not match the selected participation.')
      }
      setResult(response.data)
    } catch (requestError) {
      if (controller.signal.aborted || loadGeneration.current !== generation) return
      setError(tryGetErrorMsg(requestError, t))
    } finally {
      if (loadGeneration.current === generation) {
        if (loadAbort.current === controller) loadAbort.current = null
        setLoading(false)
      }
    }
  }, [gameId, participationId, t])

  useEffect(() => {
    loadGeneration.current += 1
    loadAbort.current?.abort()
    loadAbort.current = null
    saveOperation.current = null
    setResult(null)
    setError(null)
    setDraft(null)
    setFailedDraft(null)
    setSaving(false)
    void reload()
    return () => {
      loadGeneration.current += 1
      loadAbort.current?.abort()
      loadAbort.current = null
      saveOperation.current = null
    }
  }, [reload])

  useEffect(() => {
    if (!draft || !failedDraft || !sameReviewIdentity(draft.identity, failedDraft)) return
    const frame = window.requestAnimationFrame(() => reviewNoteRef.current?.focus({ preventScroll: true }))
    return () => window.cancelAnimationFrame(frame)
  }, [draft, failedDraft])

  const relationships = useMemo(() => {
    const counts = new Map<number, number>()
    for (const relation of result?.relationships || []) {
      counts.set(relation.findingId, (counts.get(relation.findingId) || 0) + 1)
    }
    return counts
  }, [result?.relationships])

  const saveReview = async (finding: AntiCheatFindingRow) => {
    const identity = { gameId, participationId, findingId: finding.id }
    if (!draft || !sameReviewIdentity(draft.identity, identity) || saveOperation.current) return
    const owner = { identity, operation: Symbol('finding-review') }
    const submittedDraft = draft
    saveOperation.current = owner
    setFailedDraft(null)
    setSaving(true)
    try {
      await api.eventSecurity.reviewFinding(gameId, finding.id, {
        status: submittedDraft.status,
        note: submittedDraft.note.trim() || null,
      })
      if (saveOperation.current !== owner) return
      showNotification({ color: 'teal', message: 'Evidence review recorded' })
      setDraft((current) => (current && sameReviewIdentity(current.identity, identity) ? null : current))
      await reload()
    } catch (requestError) {
      if (saveOperation.current === owner) {
        setError(tryGetErrorMsg(requestError, t))
        setFailedDraft(identity)
      }
    } finally {
      if (saveOperation.current === owner) {
        saveOperation.current = null
        setSaving(false)
      }
    }
  }

  const toggleReview = (finding: AntiCheatFindingRow) => {
    if (saving) return
    const identity = { gameId, participationId, findingId: finding.id }
    if (draft && sameReviewIdentity(draft.identity, identity)) {
      setDraft(null)
      setFailedDraft(null)
      return
    }
    setDraft({ identity, status: findingStatus(finding), note: '' })
    setFailedDraft(null)
    setError(null)
  }

  if (loading && !result) {
    return (
      <Group justify="center" py="sm">
        <Loader size="sm" />
        <Text size="sm" c="dimmed">
          Loading fused evidence…
        </Text>
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
      {error && (
        <Alert color="red" role="alert">
          {error}
        </Alert>
      )}
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
                <Text size="xs" fw={700}>
                  {familyLabels[family.family]}
                </Text>
                <Text size="xs" c="dimmed">
                  context {family.contextCount} · behavioral {family.behavioral} · strong {family.strong} · hard{' '}
                  {family.hard}
                </Text>
              </Card>
            ))}
          </Group>
          {result.findings.map((finding) => (
            <Card key={finding.id} withBorder padding="sm" data-finding-id={finding.id}>
              <Stack gap="xs">
                <Group justify="space-between" align="flex-start">
                  <div>
                    <Group gap="xs">
                      <Text size="sm" fw={700}>
                        {finding.detectorCode}
                      </Text>
                      <Badge size="xs" variant="light">
                        {tierLabels[finding.evidenceTier] || 'Unknown tier'}
                      </Badge>
                      {finding.shadow && (
                        <Badge size="xs" color="gray">
                          shadow / no score
                        </Badge>
                      )}
                      {finding.latestReviewStatus != null && (
                        <Badge size="xs" color="blue" variant="light">
                          {reviewLabels[finding.latestReviewStatus] || 'Reviewed'}
                        </Badge>
                      )}
                    </Group>
                    <Text size="xs" c="dimmed">
                      {new Date(finding.occurredAtUtc).toLocaleString()} · {relationships.get(finding.id) || 0} evidence
                      links
                    </Text>
                  </div>
                  <Button
                    size="compact-xs"
                    variant="light"
                    aria-label={`Review ${finding.detectorCode}`}
                    disabled={saving}
                    onClick={() => toggleReview(finding)}
                  >
                    Review
                  </Button>
                </Group>
                <Text size="xs" ff="monospace" style={{ overflowWrap: 'anywhere' }}>
                  {JSON.stringify(finding.details)}
                </Text>
                {draft?.identity.findingId === finding.id &&
                  draft.identity.gameId === gameId &&
                  draft.identity.participationId === participationId && (
                    <Stack gap="xs">
                      <Select
                        label="Disposition"
                        data={reviewOptions}
                        value={draft.status}
                        onChange={(value) =>
                          value &&
                          setDraft((current) =>
                            current && sameReviewIdentity(current.identity, draft.identity)
                              ? { ...current, status: value as FindingReviewStatus }
                              : current
                          )
                        }
                      />
                      <Textarea
                        ref={reviewNoteRef}
                        label="Reviewer note"
                        value={draft.note}
                        maxLength={MAX_REVIEW_NOTE_LENGTH}
                        data-finding-review-note={finding.id}
                        onChange={(event) => {
                          const note = event.currentTarget.value
                          setFailedDraft(null)
                          setDraft((current) =>
                            current && sameReviewIdentity(current.identity, draft.identity)
                              ? { ...current, note }
                              : current
                          )
                        }}
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
            <Text size="sm" c="dimmed">
              No bounded event-network findings were recorded for this participation.
            </Text>
          )}
        </>
      )}
    </Stack>
  )
}
