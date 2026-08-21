import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'

const reportPage = readFileSync('src/pages/games/[id]/monitor/CheatCheck.tsx', 'utf8')
const analysis = readFileSync('src/components/monitor/CheatInfo.tsx', 'utf8')
const evidenceReview = readFileSync('src/components/monitor/SuspicionEvidenceReview.tsx', 'utf8')
const blockHistory = readFileSync('src/pages/admin/anti-cheat.tsx', 'utf8')
const oauthButtons = readFileSync('src/components/OAuthButtons.tsx', 'utf8')
const settings = readFileSync('src/pages/admin/Settings.tsx', 'utf8')
const apiTypes = readFileSync('src/Api.ts', 'utf8')

test('the report exposes freshness, failures, coverage, and an explicit refresh action', () => {
  assert.match(reportPage, /refreshInterval: CHEAT_REPORT_REFRESH_INTERVAL_MS/)
  assert.match(reportPage, /keepPreviousData: false/)
  assert.match(reportPage, /isCheatReportStale\(lastReconciledAt\)/)
  assert.match(reportPage, /Last evaluated: \{\{time\}\}/)
  assert.match(reportPage, /Refresh failed — showing the last report/)
  assert.match(reportPage, /Detector reconciliation failed/)
  assert.match(reportPage, /evaluation jobs pending/)
  assert.match(reportPage, /Final evidence sealed/)
  assert.match(reportPage, /Detector coverage/)
  assert.match(reportPage, /View detector inventory/)
  assert.match(reportPage, /report\?\.detectorCapabilities\?\.map/)
  assert.match(reportPage, /Detector implementation and scoring coverage|detector_inventory_caption/)
})

test('participation mutations are admin-gated and evidence shows stable IDs and applied scores', () => {
  assert.match(reportPage, /canManageParticipations=\{user\?\.role === Role\.Admin\}/)
  assert.match(analysis, /canManageParticipations && item\.participationId !== undefined/)
  assert.match(analysis, /eventId == null \? '—' : `#\$\{evt\.eventId\}`/)
  assert.match(analysis, /const contribution = evidenceContribution\(evt\)/)
  assert.match(analysis, /0 \(not counted\)/)
  assert.doesNotMatch(analysis, /One browser across teams is conclusive/)
})

test('each suspicion event has a lazy source-backed review with explicit proof limits', () => {
  assert.match(analysis, /SuspicionEvidenceReviewPanel gameId=\{gameId\} eventId=\{evt\.eventId\}/)
  assert.match(analysis, /Review evidence/)
  assert.match(analysis, /This score is triage, not a verdict/)
  assert.match(apiTypes, /export interface SuspicionEvidenceReview/)
  assert.match(apiTypes, /assessment: "directEvidence" \| "strongIndicator" \| "behavioralIndicator" \| "contextOnly"/)
  assert.match(apiTypes, /cheatreport\/events\/\$\{eventId\}/)
  assert.match(evidenceReview, /Direct source verified/)
  assert.match(evidenceReview, /Human review required/)
  assert.match(evidenceReview, /Limitations/)
  assert.match(evidenceReview, /Admin review checklist/)
  assert.match(evidenceReview, /Download evidence JSON/)
  assert.doesNotMatch(evidenceReview, /rawIp|rawFingerprint|flagValue/)
})

test('analysis filters and scrollable evidence tables expose keyboard semantics and filtered empty states', () => {
  const namedRegions = analysis.match(
    /viewportProps=\{\{[\s\S]*?role: 'region',[\s\S]*?tabIndex: 0,[\s\S]*?'aria-label':[\s\S]*?\}\}/g
  )

  assert.equal(namedRegions?.length, 7)
  assert.match(analysis, /<Combobox\.Option/)
  assert.doesNotMatch(analysis, /withRoles=\{false\}/)
  assert.match(analysis, /const AnalysisEmptyState/)
  assert.match(analysis, /No results match these filters/)
})

test('block adjudication refetches retained audit history and exposes exemption state accessibly', () => {
  const mutation = blockHistory.slice(blockHistory.indexOf('const onAllow'), blockHistory.indexOf('return ('))

  assert.match(mutation, /await api\.admin\.adminClearAntiCheatBlock\(b\.id\)/)
  assert.match(mutation, /await mutate\(\)/)
  assert.match(blockHistory, /Allow this exact account and identity match for 7 days/)
  assert.match(blockHistory, /exemptionState === 'active'/)
  assert.match(blockHistory, /tabIndex: 0/)
  assert.match(blockHistory, /'aria-label': t\('admin\.content\.anti_cheat\.scroll_region'/)
  assert.doesNotMatch(blockHistory, /mdiDeleteOutline|Block cleared/)
})

test('anti-cheat timestamps and submission relations use their numeric, required wire contracts', () => {
  assert.match(apiTypes, /export interface AntiCheatBlockModel[\s\S]*occurredAtUtc: number/)
  assert.match(apiTypes, /export interface CheatInfoModel[\s\S]*ownedTeam: ParticipationModel/)
  assert.match(apiTypes, /export interface CheatInfoModel[\s\S]*submitTeam: ParticipationModel/)
  assert.match(apiTypes, /export interface SequenceSuspectDetail[\s\S]*timeA\?: number/)
})

test('OAuth entry points fail closed when browser fingerprint enforcement is enabled', () => {
  assert.match(oauthButtons, /if \(config\.enableBrowserFingerprint\)/)
  assert.match(oauthButtons, /External sign-in unavailable/)
  assert.match(oauthButtons, /external-provider redirect cannot return/)
  assert.ok(
    oauthButtons.indexOf('if (config.enableBrowserFingerprint)') < oauthButtons.indexOf('const from = params.get'),
    'fingerprint policy must be checked before OAuth navigation is constructed'
  )
  assert.match(settings, /accountPolicy\?\.enableBrowserFingerprint[\s\S]*oauth\.fingerprint_disabled/)
  assert.match(settings, /oauthConfigured[\s\S]*accountUniqueness\.fingerprintCollectionEnabled/)
})
