/**
 * Theater mode for the live arena: the globe takes the whole viewport while the
 * inspector and controls float over it. Native fullscreen is requested as an
 * enhancement; the fixed overlay works without it (iOS Safari, embedded views).
 * Focus returns to the toggle on exit, Escape leaves the overlay, and idle
 * pointer/keyboard time lets the chrome recede until the viewer moves again.
 */
export interface ArenaTheaterDeps {
  host: HTMLElement
  stage: HTMLElement
  button: HTMLButtonElement
  document: Document
  onChange: (on: boolean) => void
  idleMs?: number
  fullscreen?: {
    request: (element: HTMLElement) => unknown
    exit: () => unknown
    current: () => Element | null
  }
}

export interface ArenaTheater {
  enter(): void
  exit(): void
  toggle(): void
  isOn(): boolean
  destroy(): void
}

export const THEATER_IDLE_MS = 3_000

export function createArenaTheater(deps: ArenaTheaterDeps): ArenaTheater {
  const { host, stage, button, document: doc, onChange, fullscreen } = deps
  const idleMs = deps.idleMs ?? THEATER_IDLE_MS
  const view = doc.defaultView ?? window
  let on = false
  let native = false
  let overlay = false
  let idleTimer = 0
  // A fixed overlay inside the workspace is clipped by the container-query stack
  // around it; the popover top layer escapes every containing block.
  const popover = typeof (stage as HTMLElement & { showPopover?: unknown }).showPopover === 'function'
  const openOverlay = () => {
    if (overlay) return
    overlay = true
    doc.documentElement.style.overflow = 'hidden'
    if (!popover) return
    stage.setAttribute('popover', 'manual')
    try {
      stage.showPopover()
    } catch {}
  }
  const closeOverlay = () => {
    if (!overlay) return
    overlay = false
    doc.documentElement.style.overflow = ''
    if (!popover) return
    try {
      stage.hidePopover()
    } catch {}
    stage.removeAttribute('popover')
  }
  const events = new AbortController()
  const listen = (target: EventTarget, type: string, listener: EventListener) =>
    target.addEventListener(type, listener, { signal: events.signal, passive: type === 'pointermove' })

  const wake = () => {
    if (!on) return
    stage.dataset.idle = 'false'
    view.clearTimeout(idleTimer)
    idleTimer = view.setTimeout(() => {
      if (on) stage.dataset.idle = 'true'
    }, idleMs)
  }
  const apply = (next: boolean) => {
    on = next
    stage.classList.toggle('theater', on)
    host.dataset.theater = on ? 'on' : 'off'
    button.textContent = on ? '✕' : '⛶'
    button.setAttribute('aria-label', on ? 'Exit theater mode' : 'Enter theater mode')
    button.setAttribute('aria-pressed', String(on))
    button.title = on ? 'Exit theater mode' : 'Theater mode'
    if (on) wake()
    else {
      view.clearTimeout(idleTimer)
      delete stage.dataset.idle
    }
    onChange(on)
  }
  const leave = () => {
    if (!on) return
    if (native && fullscreen?.current() === stage) {
      try {
        fullscreen.exit()
      } catch {}
    }
    native = false
    closeOverlay()
    apply(false)
    button.focus()
  }

  listen(doc, 'keydown', ((event: KeyboardEvent) => {
    if (on && !native && event.key === 'Escape') leave()
  }) as EventListener)
  listen(doc, 'fullscreenchange', () => {
    // The browser left native fullscreen (Escape or a system gesture).
    if (native && fullscreen?.current() !== stage) {
      native = false
      if (on) {
        closeOverlay()
        apply(false)
        button.focus()
      }
    }
  })
  // Any pointer or key anywhere wakes the chrome: a keyboard user's first Tab
  // must find the controls visible again before focus moves.
  for (const type of ['pointermove', 'pointerdown', 'keydown']) listen(doc, type, wake)
  listen(stage, 'focusin', wake)

  return {
    enter() {
      if (on) return
      apply(true)
      if (!fullscreen) {
        openOverlay()
        return
      }
      try {
        const result = fullscreen.request(stage)
        native = true
        if (result && typeof (result as Promise<unknown>).catch === 'function')
          void (result as Promise<unknown>).catch(() => {
            // Not a user gesture, or fullscreen is unavailable: fall back to the overlay.
            native = false
            if (on) openOverlay()
          })
      } catch {
        native = false
        openOverlay()
      }
    },
    exit: leave,
    toggle() {
      if (on) leave()
      else this.enter()
    },
    isOn: () => on,
    destroy() {
      events.abort()
      view.clearTimeout(idleTimer)
      closeOverlay()
      on = false
    },
  }
}
