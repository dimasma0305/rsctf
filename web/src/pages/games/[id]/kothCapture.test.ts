import assert from 'node:assert/strict'
import { test } from 'node:test'
import { KothDirector, statusFromCheck } from './kothCapture'

test('statusFromCheck maps every backend verdict (incl. InternalError / never-checked)', () => {
  assert.equal(statusFromCheck('Ok'), 'def')
  assert.equal(statusFromCheck('Mumble'), 'vuln')
  assert.equal(statusFromCheck('Offline'), 'down')
  assert.equal(statusFromCheck('InternalError'), 'error')
  assert.equal(statusFromCheck(null), 'none') // never checked yet
  assert.equal(statusFromCheck(undefined), 'none')
  assert.equal(statusFromCheck('SomethingNew'), 'down') // unknown → safe default
})

test('first capture of the match plays the FIRST CROWN cinematic exactly once', () => {
  const d = new KothDirector()
  const r = d.applyCapture('h1', 'tA', false)
  assert.deepEqual(r, { changed: true, contested: false, kind: 'crown' })
  assert.equal(d.crownFired, true)
})

test('REGRESSION: a hill seen only via the poll backstop still fires the crown', () => {
  // The reported bug: koth-throne (WS frame landed) animated, koth-pwn (only the
  // 15s poll saw it) silently went neutral→held with no crown. The poll path must
  // be able to fire the crown on its own.
  const d = new KothDirector()
  const r = d.applyCapture('koth-pwn', 'tB', false) // ONLY the poll reports this hill
  assert.equal(r.kind, 'crown')
})

test('REGRESSION: WS then poll (or poll then WS) for the same capture fires the crown ONCE', () => {
  const ws = new KothDirector()
  assert.equal(ws.applyCapture('h1', 'tA', false).kind, 'crown') // WS arrives first
  assert.equal(ws.applyCapture('h1', 'tA', false).kind, 'noop') // poll re-reports → deduped
  assert.equal(ws.applyCapture('h1', 'tA', false).changed, false)

  const poll = new KothDirector()
  assert.equal(poll.applyCapture('h1', 'tA', false).kind, 'crown') // poll arrives first
  assert.equal(poll.applyCapture('h1', 'tA', false).kind, 'noop') // WS re-reports → deduped
})

test('captures after the first are normal seizes, not crowns', () => {
  const d = new KothDirector()
  d.applyCapture('h1', 'tA', false) // burns the crown latch
  const r = d.applyCapture('h2', 'tB', false)
  assert.deepEqual(r, { changed: true, contested: false, kind: 'capture' })
})

test('contested (taken from a live rival) vs claimed-from-neutral', () => {
  const d = new KothDirector()
  d.applyCapture('h0', 'tZ', false) // burn the crown so we test capture/contested cleanly
  assert.equal(d.applyCapture('h1', 'tA', false).contested, false) // neutral → tA
  assert.equal(d.applyCapture('h1', 'tB', false).contested, true) // tA → tB (seized)
})

test('a hill going neutral logs neutral, never a crown', () => {
  const d = new KothDirector()
  d.seed('h1', 'tA')
  const r = d.applyCapture('h1', null, false)
  assert.deepEqual(r, { changed: true, contested: false, kind: 'neutral' })
  assert.equal(d.crownFired, false) // a release must not consume the crown
})

test('seeded (already-held on arrival) hills do NOT fire a spurious crown', () => {
  const d = new KothDirector()
  d.seed('h1', 'tA') // viewer arrives; tA already holds h1
  const r = d.applyCapture('h1', 'tA', false) // first poll re-confirms tA
  assert.deepEqual(r, { changed: false, contested: false, kind: 'noop' })
  assert.equal(d.crownFired, false)
})

test('first crown during a running cinematic is DEFERRED, then fired once free', () => {
  const d = new KothDirector()
  const r = d.applyCapture('h1', 'tA', /* cinema */ true)
  assert.equal(r.kind, 'defer')
  assert.equal(d.crownFired, false)
  assert.deepEqual(d.pendingCrown, { owner: 'tA', hill: 'h1' })

  assert.equal(d.takePendingCrown(true), null) // still mid-cinematic → hold
  const fired = d.takePendingCrown(false) // cinematic cleared → fire
  assert.deepEqual(fired, { owner: 'tA', hill: 'h1' })
  assert.equal(d.crownFired, true)
  assert.equal(d.takePendingCrown(false), null) // never fires twice
})

test('a deferred crown is not double-counted if a later capture lands first', () => {
  const d = new KothDirector()
  d.applyCapture('h1', 'tA', true) // deferred (pending h1)
  // cinematic clears and the deferred crown fires before any new capture
  assert.deepEqual(d.takePendingCrown(false), { owner: 'tA', hill: 'h1' })
  // subsequent captures are normal seizes
  assert.equal(d.applyCapture('h2', 'tB', false).kind, 'capture')
})

test('reset() clears the latch and ledger for a fresh match', () => {
  const d = new KothDirector()
  d.applyCapture('h1', 'tA', false) // crown burned
  d.reset()
  assert.equal(d.crownFired, false)
  assert.equal(d.applyCapture('h1', 'tA', false).kind, 'crown') // crowns again next match
})
