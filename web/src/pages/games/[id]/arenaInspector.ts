import { parseUrlFragment, updateUrlFragment } from '@Utils/UrlFragment'

type Pane = 'islands' | 'teams' | 'activity'

/** Native tabs inside the isolated scene; no data, timers or React lifecycle ownership. */
export function createArenaInspector(root: ShadowRoot) {
  const events = new AbortController()
  const tabs = [...root.querySelectorAll<HTMLButtonElement>('[data-arena-pane]')]
  const show = (pane: Pane, focus = false, persist = true) => {
    for (const tab of tabs) {
      const selected = tab.dataset.arenaPane === pane
      tab.setAttribute('aria-selected', String(selected))
      tab.tabIndex = selected ? 0 : -1
      const panel = root.getElementById(tab.getAttribute('aria-controls')!)
      if (panel) panel.hidden = !selected
      if (selected && focus) tab.focus()
    }
    if (persist) {
      const hash = updateUrlFragment(window.location.hash, { arena: pane === 'islands' ? null : pane })
      window.history.replaceState(
        window.history.state,
        '',
        `${window.location.pathname}${window.location.search}${hash}`
      )
    }
  }
  tabs.forEach((tab, index) => {
    tab.addEventListener('click', () => show(tab.dataset.arenaPane as Pane), { signal: events.signal })
    tab.addEventListener(
      'keydown',
      (event) => {
        const next =
          event.key === 'ArrowRight'
            ? (index + 1) % tabs.length
            : event.key === 'ArrowLeft'
              ? (index + tabs.length - 1) % tabs.length
              : event.key === 'Home'
                ? 0
                : event.key === 'End'
                  ? tabs.length - 1
                  : null
        if (next === null) return
        event.preventDefault()
        show(tabs[next].dataset.arenaPane as Pane, true)
      },
      { signal: events.signal }
    )
  })
  for (const [id, pane, target] of [
    ['browseIslands', 'islands', 'territorySearch'],
    ['browseTeams', 'teams', 'ranklist'],
  ] as const) {
    root.getElementById(id)?.addEventListener(
      'click',
      () => {
        show(pane)
        root.getElementById(target)?.focus()
      },
      { signal: events.signal }
    )
  }
  const restore = () => {
    const values = parseUrlFragment(window.location.hash).params.getAll('arena')
    const pane = values.length === 1 && (values[0] === 'teams' || values[0] === 'activity') ? values[0] : 'islands'
    show(pane, false, false)
  }
  window.addEventListener('hashchange', restore, { signal: events.signal })
  restore()
  return { show, destroy: () => events.abort() }
}
