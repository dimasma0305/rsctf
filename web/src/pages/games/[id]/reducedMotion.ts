/**
 * Live-read of the OS "reduce motion" accessibility setting, shared by the attack-arena
 * Pixi renderers. Read per-use (not cached) so toggling the OS/browser setting takes
 * effect on the next frame without a page reload. The heavy full-screen first-blood /
 * crown cinematics are skipped entirely under reduced motion, and the ambient jeopardy
 * star twinkle falls back to a static alpha — the board still renders, it just doesn't move.
 */
const reduceMotionQuery: MediaQueryList | null =
  typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia('(prefers-reduced-motion: reduce)')
    : null

export const prefersReducedMotion = (): boolean => reduceMotionQuery?.matches ?? false
