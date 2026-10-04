import { drawGlobeSurface } from '@Components/competition/globeSurface'
import { catalogChallengeHash, catalogChallengeIdFromHash, eventChallengeHash } from '@Utils/ChallengeLinks'
import { arenaHome, createArenaCamera } from './arenaCamera'
import { faceLocation, islandCoast, projectGlobe, sphereLocation, type GlobePoint } from './arenaGlobeModel'
import type { JeopCategory, JeopChallenge } from './arenaJeopardy'

interface Team {
  id: string
  name: string
  color: string
  x: number
  y: number
  globeVisible?: boolean
}
interface Hill {
  id: string
  name: string
  x: number
  y: number
  owner?: Team
}
interface Territory extends JeopChallenge {
  category: string
  color: string
  location: GlobePoint
  coast: GlobePoint[]
  shape: SVGPathElement
  cliff: SVGPathElement
  pin: HTMLButtonElement
  choice: HTMLButtonElement
}
interface GlobeDeps {
  root: ShadowRoot
  teams: () => Team[]
  hills: () => Hill[]
  frozen: () => boolean
  motion: () => boolean
  selectTeam: (id: string) => void
  showIsland?: () => void
}

/** Presentation only: consumes the same public snapshots/feed as the spectator arena. */
export function createArenaGlobe(deps: GlobeDeps) {
  const { root } = deps
  const get = <T extends HTMLElement>(id: string) => root.getElementById(id) as T
  const stage = get('arena')
  const canvas = get<HTMLCanvasElement>('globeSurface')
  const surface = root.getElementById('territories') as unknown as SVGSVGElement
  const routes = root.getElementById('conquestRoutes') as unknown as SVGSVGElement
  const pins = get('globePins')
  const directory = get('jeop')
  const detail = get('territoryDetail')
  const search = get<HTMLInputElement>('territorySearch')
  const rotate = get<HTMLButtonElement>('rotateBtn')
  let territories: Territory[] = []
  let selected: number | null = null
  let teamId: string | null = null
  let detailSignature = ''
  let filter = 'all'
  const camera = createArenaCamera()
  let stageWidth = stage.clientWidth || 560
  const routePaths = new Map<number, SVGPathElement>()
  let auto = true,
    dirty = true,
    destroyed = false
  let drag: { id: number; x: number; y: number; yaw: number; pitch: number; width: number } | null = null
  const events = new AbortController()
  const listen = (target: EventTarget, type: string, listener: EventListener) =>
    target.addEventListener(type, listener, { signal: events.signal })
  const path = () => document.createElementNS('http://www.w3.org/2000/svg', 'path')
  const teamLocation = (index: number) => sphereLocation(index, deps.teams().length, 1.65)
  const syncRotate = () => {
    rotate.textContent = auto && deps.motion() ? 'Pause rotation' : 'Rotate globe'
    rotate.setAttribute('aria-pressed', String(auto && deps.motion()))
    rotate.disabled = !deps.motion()
  }
  const pause = () => {
    auto = false
    syncRotate()
  }
  const focus = (location: GlobePoint) => {
    pause()
    camera.focus(faceLocation(location), deps.motion())
    dirty = true
    paint()
  }
  const setRotation = (dx: number, dy: number) => {
    pause()
    camera.nudge(dx, dy, deps.motion())
    dirty = true
    paint()
  }
  function selectTerritory(id: number) {
    deps.showIsland?.()
    selected = id
    const hash = catalogChallengeHash(window.location.hash, id)
    window.history.replaceState(window.history.state, '', `${window.location.pathname}${window.location.search}${hash}`)
    const territory = territories.find((t) => t.id === id)
    if (territory) focus(territory.location)
    refreshDetails()
  }
  function refreshDetails() {
    for (const territory of territories) {
      const active = territory.id === selected
      territory.choice.setAttribute('aria-pressed', String(active))
      territory.pin.setAttribute('aria-pressed', String(active))
      territory.shape.classList.toggle('is-selected', active)
      const summary = `${territory.name} · ${territory.category} · ${territory.base} pts · ${territory.solveCount ? `${territory.solveCount} solves` : 'Unconquered'}`
      if (territory.choice.getAttribute('aria-label') !== summary) {
        territory.choice.setAttribute('aria-label', summary)
        const title = document.createElement('span'),
          meta = document.createElement('span')
        title.textContent = territory.name
        title.className = 'territory-name'
        meta.textContent = `${territory.category} · ${territory.base} pts · ${territory.solveCount ? `${territory.solveCount} solves` : 'Unconquered'}`
        meta.className = 'territory-meta'
        territory.choice.replaceChildren(title, meta)
      }
      territory.pin.setAttribute('aria-label', summary)
      territory.pin.textContent = `${territory.solveCount ? '⚑' : '◇'} ${territory.name}`
      territory.pin.title = summary
      territory.choice.hidden =
        (filter === 'open' && territory.solveCount > 0) ||
        (filter === 'solved' && territory.solveCount === 0) ||
        !`${territory.name} ${territory.category}`.toLowerCase().includes(search.value.trim().toLowerCase())
    }
    get('challengeCount').textContent = String(territories.length)
    const conquered = territories.filter((t) => t.solveCount > 0).length
    const summary = `${conquered} / ${territories.length} islands conquered`
    if (get('territorySummary').textContent !== summary) get('territorySummary').textContent = summary
    const progress = get<HTMLProgressElement>('territoryProgress')
    if (progress) {
      progress.max = Math.max(1, territories.length)
      progress.value = conquered
    }
    const results = get('territoryResults')
    if (results) {
      const count = territories.filter((t) => !t.choice.hidden).length
      const message = count ? `${count} islands shown` : 'No islands match. Try another name or filter.'
      if (results.textContent !== message) results.textContent = message
    }
    const t = territories.find((t) => t.id === selected)
    dirty = true
    const signature = JSON.stringify(
      t ? [t.id, t.name, t.category, t.base, t.solveCount, t.solvers] : [null, territories.length > 0]
    )
    if (signature === detailSignature) return
    detailSignature = signature
    const focusedLink = detail.contains(root.activeElement) && root.activeElement?.tagName === 'A'
    detail.replaceChildren()
    const heading = document.createElement('h3')
    heading.textContent = t ? t.name : 'Explore an island'
    const copy = document.createElement('p')
    copy.textContent = t
      ? `${t.category} · ${t.base} points · ${t.solveCount} accepted solves`
      : 'Select a challenge on the globe or in the list to see its expedition history.'
    detail.append(heading, copy)
    if (t) {
      const badge = document.createElement('span')
      badge.className = 'territory-badge'
      badge.textContent = t.solveCount ? '⚑ Conquered' : '◇ Unconquered'
      detail.prepend(badge)
      const status = document.createElement('p')
      status.textContent = t.solveCount
        ? `First solve: ${t.solvers[0]?.name || 'Not available in the public snapshot'}`
        : 'Unconquered — no accepted solves yet.'
      detail.append(status)
      if (t.solvers.length) {
        const list = document.createElement('ul')
        list.tabIndex = 0
        list.setAttribute('aria-label', 'Teams with an accepted solve')
        for (const solver of t.solvers) {
          const row = document.createElement('li')
          row.textContent = solver.name
          list.append(row)
        }
        detail.append(list)
      }
      const link = document.createElement('a')
      link.href = `${window.location.pathname.replace(/\/attack\/?$/, '/challenges')}${eventChallengeHash(t.id, t.name)}`
      link.textContent = 'Open challenge'
      link.className = 'btn territory-open'
      detail.append(link)
      if (focusedLink) link.focus({ preventScroll: true })
    }
    if (!territories.length)
      copy.textContent = 'No Jeopardy islands in the current public snapshot. Teams and KotH hills remain on the globe.'
    dirty = true
  }
  // Clip surface polygons against the visible hemisphere before projection.
  function polygon(points: GlobePoint[], elevation: number) {
    const { yaw, pitch } = camera.current
    const projected = points.map((p) => projectGlobe(p, yaw, pitch, elevation))
    const visible: { x: number; y: number }[] = []
    for (let i = 0; i < projected.length; i++) {
      const a = projected[i],
        b = projected[(i + 1) % projected.length]
      if (a.z >= 0.015) visible.push(a)
      if (a.z >= 0.015 !== b.z >= 0.015) {
        const ratio = (0.015 - a.z) / (b.z - a.z)
        visible.push({ x: a.x + (b.x - a.x) * ratio, y: a.y + (b.y - a.y) * ratio })
      }
    }
    return visible.length < 3
      ? ''
      : visible.map((p, i) => `${i ? 'L' : 'M'}${p.x.toFixed(2)},${p.y.toFixed(2)}`).join('') + 'Z'
  }
  function paint() {
    if (!dirty || destroyed) return
    dirty = false
    const { yaw, pitch } = camera.current
    stage.dataset.globeYaw = String(yaw)
    stage.dataset.globePitch = String(pitch)
    stage.dataset.cameraMoving = String(camera.moving)
    drawGlobeSurface(
      canvas,
      yaw,
      pitch,
      root.host.getAttribute('data-arena-scheme') === 'dark',
      stageWidth * Math.max(1.25, window.devicePixelRatio || 1)
    )
    // Screen-space lighting stays fixed while the world rotates beneath it.
    const context = canvas.getContext('2d')
    if (context) {
      const shadow = context.createRadialGradient(285, 260, 80, 400, 400, 348)
      shadow.addColorStop(0, '#ffffff0a')
      shadow.addColorStop(0.55, '#03122200')
      shadow.addColorStop(1, '#02071599')
      context.fillStyle = shadow
      context.beginPath()
      context.arc(400, 400, 346, 0, Math.PI * 2)
      context.fill()
    }
    for (const t of territories) {
      const p = projectGlobe(t.location, yaw, pitch, 1.028)
      t.cliff.setAttribute('d', polygon(t.coast, 1.005))
      t.shape.setAttribute('d', polygon(t.coast, 1.028))
      t.shape.style.setProperty('--land-color', t.solvers[0]?.color || t.color)
      t.shape.setAttribute('fill-opacity', String(0.4 + Math.max(0, p.z) * 0.45))
      t.pin.hidden = !p.visible || p.z < 0.24
      t.pin.style.left = `clamp(var(--island-pin-half), ${p.x / 10}%, calc(100% - var(--island-pin-half)))`
      t.pin.style.top = `${p.y / 10}%`
      t.pin.style.zIndex = String(Math.round(p.z * 100))
      t.pin.dataset.conquered = String(t.solveCount > 0)
    }
    for (const [index, team] of deps.teams().entries()) {
      const p = projectGlobe(teamLocation(index), yaw, pitch, 1.008)
      team.x = p.x
      team.y = p.y
      team.globeVisible = p.visible
      const marker = root.getElementById(`base-${team.id}`)
      if (marker) {
        marker.style.display = p.visible ? '' : 'none'
        marker.setAttribute('transform', `translate(${p.x} ${p.y}) scale(${0.65 + Math.max(0, p.z) * 0.35})`)
        marker.setAttribute('data-depth', p.z.toFixed(3))
      }
    }
    for (const [index, hill] of deps.hills().entries()) {
      const p = projectGlobe(sphereLocation(index, deps.hills().length, -0.8), yaw, pitch, 1.04)
      hill.x = p.x
      hill.y = p.y
      const marker = root.getElementById(`hill-${hill.id}`)
      if (marker) {
        marker.style.display = p.visible ? '' : 'none'
        marker.setAttribute('transform', `translate(${p.x} ${p.y}) scale(${0.65 + Math.max(0, p.z) * 0.35})`)
      }
    }
    const usedRoutes = new Set<number>()
    const teamIndex = deps.teams().findIndex((t) => t.id === teamId)
    if (teamIndex >= 0) {
      const team = deps.teams()[teamIndex],
        start = teamLocation(teamIndex)
      for (const territory of territories.filter((t) => t.solvers.some((s) => s.name === team.name))) {
        usedRoutes.add(territory.id)
        let curve = routePaths.get(territory.id)
        if (!curve) {
          curve = path()
          routePaths.set(territory.id, curve)
          routes.append(curve)
        }
        let d = '',
          connected = false
        for (let step = 0; step <= 36; step++) {
          const t = step / 36
          const v = {
            x: start.x * (1 - t) + territory.location.x * t,
            y: start.y * (1 - t) + territory.location.y * t,
            z: start.z * (1 - t) + territory.location.z * t,
          }
          const length = Math.hypot(v.x, v.y, v.z) || 1
          const p = projectGlobe(
            { x: v.x / length, y: v.y / length, z: v.z / length },
            yaw,
            pitch,
            1.04 + Math.sin(t * Math.PI) * 0.08
          )
          if (!p.visible) {
            connected = false
            continue
          }
          d += `${connected ? 'L' : 'M'}${p.x.toFixed(1)},${p.y.toFixed(1)}`
          connected = true
        }
        curve.setAttribute('d', d)
        curve.setAttribute('stroke', team.color)
      }
    }
    for (const [id, curve] of routePaths) {
      if (!usedRoutes.has(id)) {
        curve.remove()
        routePaths.delete(id)
      }
    }
  }
  function setData(categories: JeopCategory[]) {
    const incoming = categories
      .flatMap((category) => category.challenges.map((c) => ({ ...c, category: category.name, color: category.color })))
      .sort((a, b) => a.id - b.id)
    const same = incoming.length === territories.length && incoming.every((c, i) => c.id === territories[i].id)
    if (same) incoming.forEach((c, i) => Object.assign(territories[i], c))
    else {
      surface.replaceChildren()
      pins.replaceChildren()
      directory.replaceChildren()
      territories = incoming.map((c, i) => {
        const location = sphereLocation(i, incoming.length)
        const shape = path(),
          cliff = path()
        shape.classList.add('island')
        cliff.classList.add('island-cliff')
        shape.dataset.territory = String(c.id)
        surface.append(cliff, shape)
        const pin = document.createElement('button'),
          choice = document.createElement('button')
        pin.type = choice.type = 'button'
        pin.className = 'island-pin'
        choice.className = 'territory-choice chhit'
        pin.dataset.challengeId = choice.dataset.challengeId = String(c.id)
        pin.onclick = choice.onclick = () => selectTerritory(c.id)
        pins.append(pin)
        directory.append(choice)
        return { ...c, location, coast: islandCoast(location, i, incoming.length), shape, cliff, pin, choice }
      })
    }
    if (!territories.some((t) => t.id === selected)) selected = null
    const bookmarked = catalogChallengeIdFromHash(window.location.hash)
    if (bookmarked !== null && bookmarked !== selected && territories.some((t) => t.id === bookmarked)) {
      selected = bookmarked
      focus(territories.find((t) => t.id === bookmarked)!.location)
    }
    refreshDetails()
    paint()
  }
  function markSolved(t: Territory, team: { name: string; color: string }) {
    if (deps.frozen()) return
    if (!t.solvers.some((s) => s.name === team.name)) {
      t.solvers = [...t.solvers, team]
      t.solveCount++
    }
    refreshDetails()
    paint()
  }
  listen(stage, 'pointerdown', ((event: PointerEvent) => {
    if (!event.isPrimary || event.button !== 0 || drag || (event.target as Element).closest('button')) return
    const marker = (event.target as Element).closest('.team-marker')
    if (marker) {
      deps.selectTeam(marker.id.replace('base-', ''))
      return
    }
    pause()
    camera.stop()
    stage.focus({ preventScroll: true })
    stage.setPointerCapture(event.pointerId)
    drag = {
      id: event.pointerId,
      x: event.clientX,
      y: event.clientY,
      ...camera.current,
      width: Math.max(240, stage.clientWidth),
    }
  }) as EventListener)
  listen(stage, 'pointermove', ((event: PointerEvent) => {
    if (!drag || drag.id !== event.pointerId) return
    const dx = event.clientX - drag.x,
      dy = event.clientY - drag.y
    camera.set({ yaw: drag.yaw + (dx * Math.PI) / drag.width, pitch: drag.pitch - (dy * Math.PI) / drag.width })
    dirty = true
    // Coalesce high-frequency pointer events into the existing animation frame.
    if (!deps.motion()) paint()
  }) as EventListener)
  for (const event of ['pointerup', 'pointercancel', 'lostpointercapture'])
    listen(stage, event, ((event: PointerEvent) => {
      if (drag?.id !== event.pointerId) return
      drag = null
      if (stage.hasPointerCapture(event.pointerId)) stage.releasePointerCapture(event.pointerId)
    }) as EventListener)
  listen(stage, 'keydown', ((event: KeyboardEvent) => {
    if (event.target !== stage || event.altKey || event.ctrlKey || event.metaKey) return
    const directions: Record<string, [number, number]> = {
      ArrowLeft: [-0.2, 0],
      ArrowRight: [0.2, 0],
      ArrowUp: [0, 0.2],
      ArrowDown: [0, -0.2],
    }
    if (event.key in directions) {
      event.preventDefault()
      setRotation(...directions[event.key])
    }
    if (event.key === 'Home') {
      event.preventDefault()
      reset()
    }
  }) as EventListener)
  const reset = () => {
    pause()
    camera.focus(arenaHome, deps.motion())
    dirty = true
    paint()
  }
  get('globeLeft').onclick = () => setRotation(-0.25, 0)
  get('globeRight').onclick = () => setRotation(0.25, 0)
  get('globeUp').onclick = () => setRotation(0, 0.25)
  get('globeDown').onclick = () => setRotation(0, -0.25)
  get('globeReset').onclick = reset
  rotate.onclick = () => {
    auto = !auto
    camera.stop()
    syncRotate()
  }
  listen(root, 'focusin', ((event: FocusEvent) => {
    if (stage.contains(event.target as Node)) pause()
  }) as EventListener)
  search.oninput = refreshDetails
  for (const button of root.querySelectorAll<HTMLButtonElement>('[data-territory-filter]')) {
    listen(button, 'click', (() => {
      filter = button.dataset.territoryFilter || 'all'
      for (const item of root.querySelectorAll<HTMLButtonElement>('[data-territory-filter]'))
        item.setAttribute('aria-pressed', String(item === button))
      refreshDetails()
    }) as EventListener)
  }
  listen(window, 'hashchange', (() => {
    const id = catalogChallengeIdFromHash(window.location.hash)
    if (id !== null && territories.some((t) => t.id === id)) selectTerritory(id)
    else {
      selected = null
      camera.stop()
      refreshDetails()
      paint()
    }
  }) as EventListener)
  const theme = new MutationObserver(() => {
    dirty = true
    paint()
  })
  theme.observe(root.host, { attributes: true, attributeFilter: ['data-arena-scheme'] })
  const resize = new ResizeObserver(([entry]) => {
    if (!entry || !entry.contentRect.width) return
    stageWidth = entry.contentRect.width
    dirty = true
    paint()
  })
  resize.observe(stage)
  return {
    setData,
    hasData: () => territories.length > 0,
    layout() {
      dirty = true
      paint()
    },
    refreshMotion() {
      if (!deps.motion()) camera.settle()
      syncRotate()
      dirty = true
      paint()
    },
    tick(_ts: number, dt: number) {
      if (document.hidden || destroyed) return
      if (camera.step(dt, auto && deps.motion() && !deps.frozen() && !drag)) dirty = true
      paint()
    },
    focusTeam(id: string | null) {
      teamId = id
      const index = deps.teams().findIndex((t) => t.id === id)
      if (index >= 0) focus(teamLocation(index))
      else camera.stop()
      dirty = true
      paint()
    },
    solveByTitle(_x: number, _y: number, title: string, team: { name: string; color: string }) {
      const matches = territories.filter((t) => t.name.toLowerCase() === title.toLowerCase())
      // The feed carries a title: ambiguous names wait for the authoritative poll.
      if (matches.length !== 1) return false
      markSolved(matches[0], team)
      return true
    },
    solveRandom(_x: number, _y: number, team: { name: string; color: string }) {
      const open = territories.filter((t) => !t.solvers.some((s) => s.name === team.name))
      const t = open[Math.floor(Math.random() * open.length)]
      if (!t) return null
      markSolved(t, team)
      return { name: t.name, base: t.base }
    },
    destroy() {
      destroyed = true
      camera.stop()
      drag = null
      events.abort()
      theme.disconnect()
      resize.disconnect()
      for (const id of ['globeLeft', 'globeRight', 'globeUp', 'globeDown', 'globeReset', 'rotateBtn'])
        get(id).onclick = null
      search.oninput = null
      for (const territory of territories) territory.pin.onclick = territory.choice.onclick = null
    },
  }
}
