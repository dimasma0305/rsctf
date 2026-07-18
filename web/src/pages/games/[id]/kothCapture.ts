/**
 * Pure, DOM-free model of A&D King-of-the-Hill ownership + the FIRST CROWN
 * cinematic, extracted from Attack.tsx so the capture / dedup / deferral logic
 * is unit-testable in isolation.
 *
 * Hill ownership is observed from TWO sources — the live WebSocket `koth` frame
 * (instant) and the 15s scoreboard poll (reliable backstop). Either may arrive
 * first and the other must NOT re-fire the effects; that dedup, plus the
 * once-per-match FIRST CROWN latch and its deferral past a running cinematic,
 * all live here. The arena performs the FX implied by each result — this file
 * renders nothing.
 */

export type SvcStatus = 'def' | 'vuln' | 'down' | 'error' | 'none'

/** Map a backend SLA check verdict to the arena's internal status key. */
export function statusFromCheck(cs: string | null | undefined): SvcStatus {
  return cs === 'Ok'
    ? 'def'
    : cs === 'Mumble'
      ? 'vuln'
      : cs === 'InternalError'
        ? 'error'
        : cs === 'Offline'
          ? 'down'
          : cs == null
            ? 'none'
            : 'down'
}

/** What the arena should render in response to an applied capture. */
export type CaptureKind =
  | 'noop' // ownership unchanged — the other source already applied it (dedup)
  | 'neutral' // hill went uncontrolled
  | 'crown' // first capture of the match — play the FIRST CROWN cinematic now
  | 'defer' // first capture, but a cinematic is mid-play — crown deferred to takePendingCrown()
  | 'capture' // a normal (non-first) capture — capture FX

export interface CaptureResult {
  /** ownership actually changed (false ⇒ a deduped re-report from the other source) */
  changed: boolean
  /** taken from a live rival (true) vs claimed from neutral (false) */
  contested: boolean
  kind: CaptureKind
}

export interface PendingCrown {
  owner: string
  hill: string
}

/**
 * Owns the per-hill last-seen owner ledger (for WS/poll dedup) and the global
 * FIRST CROWN latch + deferral. Side-effect free: callers perform the FX implied
 * by each {@link CaptureResult} / {@link takePendingCrown} result.
 */
export class KothDirector {
  private firstCrownFired = false
  private pending: PendingCrown | null = null
  private owner: Record<string, string | null> = {}

  /** Reset for a fresh match (preview rematch). */
  reset(): void {
    this.firstCrownFired = false
    this.pending = null
    this.owner = {}
  }

  /** Seed a hill's current owner at load WITHOUT firing anything. */
  seed(hill: string, owner: string | null): void {
    this.owner[hill] = owner ?? null
  }

  get crownFired(): boolean {
    return this.firstCrownFired
  }

  get pendingCrown(): PendingCrown | null {
    return this.pending
  }

  /**
   * Apply an observed holder for `hill` (from the WS frame or the 15s poll —
   * whichever arrives first). Deduped by last-seen owner so the second source
   * no-ops. Returns the FX the arena should play.
   */
  applyCapture(hill: string, newOwner: string | null, cinema: boolean): CaptureResult {
    const prev = this.owner[hill] ?? null
    const next = newOwner ?? null
    if (prev === next) return { changed: false, contested: false, kind: 'noop' }
    const contested = prev != null && next != null
    this.owner[hill] = next
    if (next == null) return { changed: true, contested, kind: 'neutral' }
    if (!this.firstCrownFired) {
      if (!cinema) {
        this.firstCrownFired = true
        return { changed: true, contested, kind: 'crown' }
      }
      this.pending = { owner: next, hill }
      return { changed: true, contested, kind: 'defer' }
    }
    return { changed: true, contested, kind: 'capture' }
  }

  /**
   * Per-frame: once any running cinematic clears, fire a single deferred crown.
   * Returns the crown to play, or null.
   */
  takePendingCrown(cinema: boolean): PendingCrown | null {
    if (this.pending && !cinema && !this.firstCrownFired) {
      this.firstCrownFired = true
      const p = this.pending
      this.pending = null
      return p
    }
    return null
  }
}
