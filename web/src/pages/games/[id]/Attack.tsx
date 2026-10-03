/**
 * Public per-game live Attack & Defense / King of the Hill battle arena.
 *
 * Route: /games/{id}/attack  (no authentication).
 *
 * Globe presentation follows the competition workspace. Original scene effects
 * and animation by lawbyte
 * (https://github.com/lawbyte). Ported into the platform here and wired to the
 * live plain-WebSocket attack feed (/hub/attack/ws?game={id}) plus the public
 * A&D / KotH scoreboards under the canonical lowercase `/api/game/{id}` routes.
 *
 * Shared navigation and page header surround an isolated SVG/canvas scene.
 * The Shadow DOM keeps legacy celebration effects scoped while inheriting the
 * application theme. Public data, sockets and timers remain owned by this engine;
 * arenaGlobe owns only camera, 3D projection and challenge-island presentation.
 */
import { Button, Group, Stack, useComputedColorScheme } from '@mantine/core'
import { FC, useEffect, useRef } from 'react'
import { Link, useParams, useSearchParams } from 'react-router'
import { PageHeader } from '@Components/PageHeader'
import { WithNavBar } from '@Components/WithNavbar'
import {
  ArenaHttpError,
  arenaLiveRoutes,
  arenaPollDelay,
  arenaReconnectDelay,
  mergeArenaRoster,
  parseArenaRetryAfter,
} from '@Utils/ArenaLive'
import { eventVpnFetch } from '@Utils/EventVpnProof'
import { epochProgress } from '@Utils/epochProgress'
import type { AdScoreboardModel } from '@Api'
import arenaEffects from './arenaEffects.css?inline'
import { createArenaGlobe } from './arenaGlobe'
import { acceptedTerritorySolvers } from './arenaGlobeModel'
import type { JeopCategory } from './arenaJeopardy'
import { arenaTeamInitials, arenaTeamLabel, arenaTeamPosition, initialArenaRanking } from './arenaPresentation'
import arenaTheme from './arenaTheme.css?inline'
import { createSoundEngine } from './audio'
import { createFbRenderer } from './fbRenderer'
import { createFxRenderer } from './fxRenderer'
import { createFzRenderer } from './fzRenderer'
import { KothDirector, statusFromCheck, type CaptureResult } from './kothCapture'
import { createWinRenderer } from './winRenderer'

/* -------------------------------------------------------------------------- */
/* Inherited workspace theme and isolated legacy celebration effects.         */
/* -------------------------------------------------------------------------- */
const ARENA_CSS = arenaEffects + arenaTheme

/* -------------------------------------------------------------------------- */
/* Scene markup. Dynamic regions (#svg / #log / #ranklist / #stats / FB       */
/* portraits) are populated by the engine.                                    */
/* -------------------------------------------------------------------------- */
const ARENA_BODY = `
  <div class="shell">
    <div class="topbar">
      <div class="brand">
        <a class="back-link" id="eventLink" href="/games"><span aria-hidden="true">←</span> Event</a>
        <div class="brand-copy">
          <p class="logo" id="brandLogo">Competition spectator view</p>
        </div>
      </div>
      <div class="topright">
        <span class="connection" id="connectionStatus" role="status" data-state="connecting">Connecting</span>
        <div class="freezepill" id="freezeTag">❄ Frozen</div>
        <div class="countpill" id="countPill" role="timer" aria-label="Time remaining">0:00</div>
        <div class="roundpill" id="roundPill">Tick 0</div>
      </div>
    </div>
    <dl class="overview" aria-label="Arena overview">
      <div class="metric"><dt>Teams</dt><dd id="teamCount">0</dd></div>
      <div class="metric"><dt>Challenge islands</dt><dd id="challengeCount">0</dd></div>
      <div class="metric"><dt>A&amp;D services</dt><dd id="serviceCount">0</dd></div>
      <div class="metric"><dt>KotH hills</dt><dd id="hillCount">0</dd></div>
    </dl>
    <div class="devbar" role="group" aria-label="Spectator controls">
      <span class="label">Spectator view</span>
      <button class="btn ghost" id="speedBtn" aria-pressed="false">Speed 1×</button>
      <button class="btn ghost" id="soundBtn" aria-pressed="false">Sound off</button>
      <button class="btn ghost on" id="motionBtn" aria-pressed="true">Motion on</button>
      <span id="cfgBtns" style="display:none">
        <label class="cfg">Teams<input id="cfgTeams" type="number" min="2" max="20" value="8"></label>
        <label class="cfg">A&amp;D<input id="cfgAd" type="number" min="0" max="10" value="4"></label>
        <label class="cfg">KotH<input id="cfgKoth" type="number" min="0" max="12" value="3"></label>
        <label class="cfg">Jeopardy<input id="cfgJeop" type="number" min="0" max="40" value="24"></label>
      </span>
      <span id="fbBtns" style="display:none">
        <button class="btn fb-ad" id="fbAdBtn">First blood · A&amp;D</button>
        <button class="btn fb-jeo" id="fbJeoBtn">First blood · Jeopardy</button>
        <button class="btn fb-koth" id="fbKothBtn">First crown · KotH</button>
        <button class="btn patch" id="patchBtn">Patch</button>
        <button class="btn frz" id="freezeBtn">Freeze</button>
        <button class="btn end" id="endBtn">End</button>
      </span>
      <span class="sp"></span>
      <a class="btn" id="scoreboardLink" href="/games">Scoreboard <span aria-hidden="true">↗</span></a>
    </div>
    <div class="midrow">
      <section class="panel arena-wrap" aria-labelledby="globeTitle">
        <div class="map-heading">
          <div><h2 id="globeTitle">Conquest globe</h2><p>One world. Every challenge is an island to explore.</p></div>
          <button id="fsBtn" class="fs-btn" title="Fullscreen globe" aria-label="Fullscreen globe">⛶</button>
        </div>
        <div class="arena" id="arena" tabindex="0" role="group" aria-label="3D conquest globe" aria-describedby="globeHelp">
          <canvas id="globeSurface" aria-hidden="true"></canvas>
          <svg id="territories" viewBox="0 0 1000 1000" aria-hidden="true"></svg>
          <svg id="conquestRoutes" viewBox="0 0 1000 1000" aria-hidden="true"></svg>
          <canvas id="fxbg" width="870" height="870" aria-hidden="true"></canvas>
          <svg id="svg" viewBox="0 0 1000 1000" preserveAspectRatio="xMidYMid meet" aria-hidden="true"></svg>
          <canvas id="fx" width="870" height="870" aria-hidden="true"></canvas>
          <div id="globePins"></div>
        </div>
        <div class="globe-navigation" role="group" aria-label="Globe controls">
          <div class="globe-directions" role="group" aria-label="Rotate view">
          <button class="btn" id="globeUp" aria-label="Rotate globe up">↑</button>
          <button class="btn" id="globeLeft" aria-label="Rotate globe left">←</button>
          <button class="btn" id="globeRight" aria-label="Rotate globe right">→</button>
          <button class="btn" id="globeDown" aria-label="Rotate globe down">↓</button>
          </div>
          <button class="btn" id="globeReset">Reset view</button>
          <button class="btn" id="rotateBtn" aria-pressed="true">Pause rotation</button>
        </div>
        <p class="globe-help" id="globeHelp">Drag/swipe or use arrow keys to rotate; Home resets. Scroll outside the globe to move the page.</p>
        <div class="selection" aria-label="Highlighted team">
          <span class="selection-name" id="selectionName">Select a team in the rankings to highlight it.</span>
          <span class="selection-score" id="selectionScore"></span>
        </div>
        <div class="territory-browser">
          <section id="territoryDetail" aria-label="Selected island" aria-live="polite"></section>
          <div class="territory-directory">
            <h3>Challenge islands</h3>
            <p id="territorySummary" role="status">Loading islands</p>
            <progress id="territoryProgress" value="0" max="1" aria-label="Islands with an accepted solve"></progress>
            <label for="territorySearch">Find a challenge</label>
            <input id="territorySearch" type="search" placeholder="Name or category">
            <div class="territory-filters" role="group" aria-label="Filter challenge islands">
              <button class="btn" data-territory-filter="all" aria-pressed="true">All</button>
              <button class="btn" data-territory-filter="open" aria-pressed="false">Unconquered</button>
              <button class="btn" data-territory-filter="solved" aria-pressed="false">Solved</button>
            </div>
            <p id="territoryResults" role="status"></p>
            <div id="jeop" role="region" tabindex="0" aria-label="Challenge islands"></div>
          </div>
        </div>
        <p class="globe-help">◇ Unconquered · ⚑ Solved · Numbered outposts are teams. Every team can solve an island. Solved islands take the first team's color, not exclusive ownership.</p>
      </section>
      <div class="rightcol">
        <section class="panel rank" aria-labelledby="rankingTitle">
          <div class="phead">
            <h2 class="t" id="rankingTitle">Rankings</h2>
            <span class="rank-tabs" id="rankTabs" role="group" aria-label="Scoring mode">
              <button data-rm="ad" class="on" aria-pressed="true">A&amp;D</button>
              <button data-rm="koth" aria-pressed="false">KotH</button>
              <button data-rm="jeopardy" aria-pressed="false">Jeopardy</button>
            </span>
          </div>
          <div id="ranklist" role="region" tabindex="0" aria-label="Live team ranking"></div>
        </section>
        <section class="panel log-panel" aria-labelledby="logTitle">
          <div class="phead"><h2 class="t" id="logTitle">Recent activity</h2></div>
          <div id="log" role="log" tabindex="0" aria-live="polite" aria-label="Battle event log"></div>
        </section>
      </div>
    </div>
  </div>

  <div class="fb-overlay" id="fbOverlay">
    <div class="fb-tele">
      <div class="fb-tele-vig"></div>
      <div class="fb-tele-ban"><b class="fb-tele-txt">INCOMING STRIKE</b></div>
    </div>
    <div class="fb-rays"></div>
    <div class="fb-bar t"></div>
    <div class="fb-bar b"></div>
    <div class="fb-core">
      <div class="fb-vs" id="fbVs">
        <div class="fb-fighter atk"><div class="por" id="fbAtkPor"></div><div class="nm" id="fbAtkNm"></div><span class="fb-rtag" id="fbAtkTag"></span></div>
        <div class="fb-vs-x" id="fbVsx">VS</div>
        <div class="fb-fighter vic"><div class="por" id="fbVicPor"></div><div class="nm" id="fbVicNm"></div><span class="fb-rtag" id="fbVicTag"></span></div>
      </div>
      <div class="fb-title">FIRST BLOOD</div>
      <div class="fb-sub" id="fbSub"></div>
      <div class="fb-chal" id="fbChal"></div>
    </div>
  </div>
  <!-- ===== SCOREBOARD FREEZE CINEMATIC ===== -->
  <div class="fz-overlay" id="fzOverlay">
    <div class="fz-dark"></div>
    <canvas class="fz-cv" id="fzCanvas"></canvas>
    <div class="fz-vig"></div>
    <div class="fz-core">
      <div class="fz-panel">
        <span class="fz-brk tl"></span><span class="fz-brk tr"></span><span class="fz-brk bl"></span><span class="fz-brk br"></span>
        <div class="fz-badge"><svg viewBox="0 0 100 100" fill="none" stroke="currentColor"><polygon points="50,4 93.8,35.8 77,87.2 23,87.2 6.2,35.8" stroke-width="3" fill="currentColor" fill-opacity=".09"/><path d="M40 47 V41 a10 10 0 0 1 20 0 V47" stroke-width="4" stroke-linecap="round"/><rect x="33" y="47" width="34" height="23" rx="4" fill="currentColor" fill-opacity=".2" stroke-width="3"/><circle cx="50" cy="56.5" r="3.6" fill="currentColor"/><rect x="48.4" y="58.6" width="3.2" height="9" rx="1.6" fill="currentColor"/></svg></div>
        <div class="fz-title">BOARD LOCKED</div>
        <div class="fz-bar"><span class="fz-fill"></span></div>
        <div class="fz-secured">&#10003; SECURED &middot; RESULTS AT MATCH END</div>
        <div class="fz-count" id="fzCount"></div>
      </div>
    </div>
  </div>

  <!-- ===== MATCH WINNER SCREEN ===== -->
  <div class="win-overlay" id="winOverlay">
    <canvas class="win-cv" id="winCanvas"></canvas>
    <div class="win-core">
      <div class="win-eyebrow">MATCH COMPLETE &middot; FINAL STANDINGS</div>
      <div class="win-title" id="winTitle">CHAMPIONS</div>
      <div class="podium big" id="podium"></div>
      <button class="btn rematch" id="rematchBtn" style="display:none">&#8635; REMATCH</button>
    </div>
  </div>

  <div class="grain"></div>
`

/* -------------------------------------------------------------------------- */
/* The imperative engine. Operates entirely within `root` (the shadow root)   */
/* and returns a teardown function. Heavily uses `any` because this is a       */
/* self-contained DOM/canvas scene, not app data flow.                        */
/* -------------------------------------------------------------------------- */
function runArena(root: ShadowRoot, gameId: string, preview: boolean): () => void {
  let killed = false
  const motionQuery = window.matchMedia('(prefers-reduced-motion: reduce)')
  let motionRequested = true
  let motionEnabled = !motionQuery.matches
  root.host.setAttribute('data-motion', motionEnabled ? 'on' : 'off')
  const timers: number[] = []
  const deferred = new Set<number>()
  const ownedTimeout = (callback: () => void, delay: number) => {
    const id = window.setTimeout(() => {
      deferred.delete(id)
      if (!killed) callback()
    }, delay)
    deferred.add(id)
    return id
  }
  let raf = 0
  let liveClockStarted = false,
    livePollStarted = false
  let ws: WebSocket | null = null
  let wsRetry = 0,
    reconnectTimer = 0 // single reconnect handle (never >1 pending) — don't accumulate in timers[]
  let wsOpenedAt = 0
  let livePollTimer = 0
  let livePollFailures = 0
  let livePollController: AbortController | null = null
  const liveRequestControllers = new Set<AbortController>()
  let liveModelSignature = ''
  const liveRoutes = arenaLiveRoutes(gameId)

  const $ = (id: string): any => root.getElementById(id)
  const NS = 'http://www.w3.org/2000/svg'
  const el = (tag: string, attrs: Record<string, any>): any => {
    const e = document.createElementNS(NS, tag)
    for (const k in attrs) e.setAttribute(k, String(attrs[k]))
    return e
  }
  const rng = (a: number, b: number) => a + Math.random() * (b - a)
  const pick = (arr: any[]) => arr[Math.floor(Math.random() * arr.length)]
  const esc = (s: any) =>
    String(s == null ? '' : s).replace(
      /[&<>"]/g,
      (c: string) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c] as string
    )

  const CX = 500,
    CY = 500,
    HILLR = 165
  const PALETTE = [
    '#ff4d5e',
    '#27e3ff',
    '#ffc637',
    '#ff39a8',
    '#b9ff42',
    '#ff7a3a',
    '#9d6bff',
    '#4d8bff',
    '#3dffb0',
    '#ff9d63',
    '#7fd7ff',
    '#e667ff',
    '#ffd23a',
    '#5ad1a8',
  ]
  const LOOKS = [
    { hair: '#ff5a6a', skin: '#ffd9c2', eye: '#ff4d5e', style: 'spiky', gear: 'horns', expr: 'angry', prop: 'gaunt' },
    { hair: '#7fe9ff', skin: '#ffe2cf', eye: '#27e3ff', style: 'bob', gear: 'headset', expr: 'cool', prop: 'none' },
    { hair: '#ffd86b', skin: '#ffd9c2', eye: '#ffb020', style: 'pony', gear: 'clip', expr: 'soft', prop: 'orb' },
    { hair: '#ff7ec2', skin: '#ffe2cf', eye: '#ff39a8', style: 'twin', gear: 'catears', expr: 'wink', prop: 'none' },
    {
      hair: '#c8ff6e',
      skin: '#ecc6a6',
      eye: '#9bff42',
      style: 'spiky',
      gear: 'headband',
      expr: 'keen',
      prop: 'katana',
    },
    { hair: '#ff9d63', skin: '#ffd9c2', eye: '#ff7a3a', style: 'long', gear: 'visor', expr: 'grin', prop: 'none' },
    { hair: '#b99dff', skin: '#e9d6ff', eye: '#9d6bff', style: 'long', gear: 'hood', expr: 'calm', prop: 'kunai' },
    { hair: '#86b3ff', skin: '#ffe2cf', eye: '#4d8bff', style: 'bob', gear: 'headset', expr: 'cool', prop: 'shield' },
  ]

  let TEAMS: any[] = [],
    SERVICES: any[] = [],
    HILLS: any[] = []
  let round = 0,
    totalFlags = 0,
    cinema = false,
    slamCovering = false
  // hill ownership + FIRST CROWN latch/deferral live in this pure model (see kothCapture.ts)
  const kothDir = new KothDirector()
  let tNow = Date.now(),
    tickLeft = 0,
    liveRoundEndsAt: number | null = null
  let adEpochTicks = preview ? 4 : 0
  let adStartRound: number | null = preview ? 1 : null
  let kothRound = 0,
    kothRoundEndsAt: number | null = null
  let kothEpochTicks = 0
  let kothStartRound: number | null = null
  let speed = 1
  // preview-only knobs: how many teams / A&D services / KotH hills / jeopardy challenges
  let cfgTeams = 8,
    cfgAd = 4,
    cfgKoth = 3,
    cfgJeop = 24
  let arenaRect: any = null // cached arena.getBoundingClientRect(); refreshed in sizeCanvas
  const snd = createSoundEngine() // procedural Web Audio engine (see audio.ts)
  snd.setEnabled(false) // Sound is opt-in, not unlocked by unrelated navigation.
  let stopIncomingSound: (() => void) | null = null
  let stopFirstBloodSound: (() => void) | null = null
  let rankDirty = false,
    logDirty = false // per-frame DOM-flush flags
  // preroll = the attention-seeking telegraph (board stays visible, warning builds)
  // that plays BEFORE the slam cinematic; soundDelay/slam/total are relative to the slam.
  // 5s telegraph + 5s slam = ~10s total. The slam length tracks the CSS anim duration
  // (5s); the FIRST BLOOD title lands at ~15% (=750ms), so onImpact/shake fire at slam=750.
  const FB = { total: 5000, slam: 750, soundDelay: 0, preroll: 5000 }

  // match clock + scoreboard freeze + winner.
  // live: gameEndMs = real EndTimeUtc; freeze driven by the board's isFrozenView.
  // preview: gameEndMs = boot + MATCH_SECONDS; freeze in the final FREEZE_SECONDS.
  const MATCH_SECONDS = 360,
    FREEZE_SECONDS = 90
  let frozen = false,
    matchOver = false,
    endingMatch = false,
    gameEndMs: number | null = null
  let nextEndCheckMs = 0
  // while frozen the board shows the snapshot taken at freeze; real values keep updating underneath.
  // RANKING panel mode — switchable between the three score boards.
  let rankMode: 'ad' | 'koth' | 'jeopardy' = 'ad'
  let rankModeChosen = false
  let selectedTeamId: string | null = null
  // during the freeze, show the snapshot captured at freeze time instead of the live value
  const shownOr = (t: any, snap: string, live: string) => (frozen && t[snap] != null ? t[snap] : t[live])
  const adScore = (t: any) => shownOr(t, 'shown', 'score')
  const dispScore = (t: any) =>
    rankMode === 'koth' ? t.kothScore || 0 : rankMode === 'jeopardy' ? t.jpScore || 0 : adScore(t)
  const dispProjected = (t: any) => shownOr(t, 'shownProjected', 'projectedScore')
  const dispOffense = (t: any) => shownOr(t, 'shownOffense', 'offenseRate')
  const dispDefense = (t: any) => shownOr(t, 'shownDefense', 'defenseRate')
  const dispSla = (t: any) => shownOr(t, 'shownSla', 'slaRate')
  const dispCaptures = (t: any) => shownOr(t, 'shownCaptures', 'captureEvidence')
  const fmtAdScore = (n: number) => Math.max(0, Number(n) || 0).toFixed(1)
  const boundedRate = (n: number) => Math.max(0, Math.min(1, Number(n) || 0))
  const fmtMS = (s: number) => {
    const m = Math.floor(s / 60),
      x = Math.floor(s % 60)
    return m + ':' + String(x).padStart(2, '0')
  }
  const validRank = (value: any) => (Number.isInteger(value) && value > 0 ? value : Number.MAX_SAFE_INTEGER)
  const stableTeamOrder = (a: any, b: any) => {
    const aId = Number.isInteger(a.pid) ? a.pid : Number.MAX_SAFE_INTEGER
    const bId = Number.isInteger(b.pid) ? b.pid : Number.MAX_SAFE_INTEGER
    return aId - bId || String(a.id).localeCompare(String(b.id))
  }

  const teamByName = (n: any) => TEAMS.find((t) => t.name === n)
  function makeLook(t: any, i: number) {
    const base = LOOKS[i % LOOKS.length]
    const h = t.hue != null ? t.hue : (i * 47) % 360
    return { ...base, eye: t.color, hair: `hsl(${h} 80% 70%)`, skin: base.skin }
  }

  /* -------- avatar = PROFILE portrait: a head+shoulders BUST of the SAME full-body character
     (reuses chibiHead/chibiCollar so the FB/champion/ranklist profile always matches the wheel). -------- */
  function avatar(L: any, color: string) {
    const u = String(L.eye).replace('#', '') + Math.floor(rng(0, 99999))
    return `<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg">
      <defs><radialGradient id="bg_${u}" cx="50%" cy="35%" r="75%">
        <stop offset="0%" stop-color="${color}" stop-opacity=".32"/><stop offset="100%" stop-color="#0a0818"/>
      </radialGradient></defs>
      <rect width="64" height="64" fill="url(#bg_${u})"/>
      <g transform="translate(32 50) scale(1.28)">${chibiCollar(color)}${chibiHead(L, color)}</g>
    </svg>`
  }

  const svg: any = $('svg')
  const fx: any = $('fx')
  const ctx: any = fx.getContext('2d')
  const fxbg: any = $('fxbg')
  const ctxbg: any = fxbg.getContext('2d')
  const arena: any = $('arena')
  // Pixi v8 WebGL FX renderer (its own overlay canvas); the 2D #fx is the fallback
  // used until fxRenderer.ready, or if WebGL init fails. See fxRenderer.ts.
  const fxRenderer = createFxRenderer(fx)
  // Pixi v8 first-blood slam graphics (dark + radial splash + flash) on a full-viewport
  // GPU canvas (z-94, under the DOM FB text z-95) — replaces the screen-sized DOM splash
  // layers that caused the first-blood lag. See fbRenderer.ts.
  const fbRenderer = createFbRenderer(root)
  // 2D-canvas WINDOW FROST for the scoreboard-freeze cinematic (corner frost ferns, baked).
  const fzRenderer = createFzRenderer($('fzCanvas') as HTMLCanvasElement)
  // 2D-canvas VICTORY effects (god-rays + confetti + sparkles) for MATCH COMPLETE / podium.
  const winRenderer = createWinRenderer($('winCanvas') as HTMLCanvasElement)
  // One camera projects teams, hills and challenge islands onto the same world.
  const jeop = createArenaGlobe({
    root,
    teams: () => TEAMS,
    hills: () => HILLS,
    frozen: () => frozen,
    motion: () => motionEnabled,
    selectTeam: (id) => {
      selectedTeamId = id
      updateSelection()
      jeop.focusTeam(id)
    },
  })
  const logEl: any = $('log')
  const rankEl: any = $('ranklist')

  function buildArena() {
    svg.innerHTML = ''
    const defs = el('defs', {})
    defs.innerHTML = `
      <radialGradient id="coreG" cx="50%" cy="50%" r="50%">
        <stop offset="0%" stop-color="#ffffff"/><stop offset="22%" stop-color="#7fe9ff"/>
        <stop offset="60%" stop-color="#9d6bff"/><stop offset="100%" stop-color="#1a1040"/>
      </radialGradient>
      <filter id="soft"><feGaussianBlur stdDeviation="3"/></filter>
      <filter id="glow"><feGaussianBlur stdDeviation="6" result="b"/><feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge></filter>`
    svg.appendChild(defs)

    // The globe controller positions these markers after each camera change.
    svg.setAttribute('data-dense', String(TEAMS.length > 16))
    HILLS.forEach((h) => svg.appendChild(buildHill(h)))
    TEAMS.forEach((t) => svg.appendChild(buildBase(t)))
    TEAMS.forEach((t) => renderSvc(t))
    updateSelection()
    jeop.layout()
  }

  function buildHill(h: any) {
    const g = el('g', { id: 'hill-' + h.id, transform: `translate(${h.x} ${h.y})` })
    g.style.color = h.owner ? h.owner.color : '#7b78a6'
    const owned = !!h.owner
    g.innerHTML = `
      <ellipse cx="0" cy="22" rx="30" ry="9" fill="currentColor" opacity="${owned ? 0.18 : 0.08}" filter="url(#soft)"/>
      <ellipse cx="0" cy="22" rx="20" ry="5.5" fill="#0b0a1c" stroke="currentColor" stroke-width="1.2" stroke-opacity="0.7"/>
      <circle cx="0" cy="0" r="27" fill="none" stroke="currentColor" stroke-width="1.5" stroke-opacity="0.5" stroke-dasharray="3 6"/>
      <g fill="currentColor">
        <rect x="-13" y="-14" width="4" height="34" rx="1"/>
        <rect x="9" y="-14" width="4" height="34" rx="1"/>
        <rect x="-18" y="-2.5" width="36" height="3.4" rx="1"/>
        <rect x="-2" y="-13" width="4" height="8"/>
        <rect x="-4" y="-11" width="8" height="5" rx="1"/>
        <path d="M-23 -15 Q0 -8.5 23 -15 L23 -10.6 Q0 -4.1 -23 -10.6 Z"/>
        <path d="M-19 -10 Q0 -4.5 19 -10 L19 -7 Q0 -1.5 -19 -7 Z" opacity="0.85"/>
      </g>
      <rect id="hstat-${h.id}" x="-9" y="29" width="18" height="5" rx="2.5" fill="${SVC_COLOR[h.status] || SVC_COLOR.none}" stroke="#06050f" stroke-width="1"/>
      <text x="0" y="44" text-anchor="middle" fill="#cfd2ee" font-family="'Press Start 2P'" font-size="8" paint-order="stroke" stroke="#06050f" stroke-width="3.5">${esc(h.name)}</text>
      <text id="hown-${h.id}" x="0" y="55" text-anchor="middle" fill="currentColor" font-family="'VT323'" font-size="15" paint-order="stroke" stroke="#06050f" stroke-width="3">${owned ? esc(h.owner.name) : 'NEUTRAL'}</text>`
    return g
  }
  function renderHill(h: any) {
    if (frozen) return
    const g = $('hill-' + h.id)
    if (!g) return
    g.style.color = h.owner ? h.owner.color : '#7b78a6'
    const own = $('hown-' + h.id)
    if (own) own.textContent = h.owner ? h.owner.name : 'NEUTRAL'
    const st = $('hstat-' + h.id)
    if (st) st.setAttribute('fill', SVC_COLOR[h.status] || SVC_COLOR.none)
  }

  /* -------- shared chibi head: the SINGLE source of truth for the team head, used by both the
     full-body wheel character (buildBase) and the profile portrait (avatar), so they always match. -------- */
  function chibiHead(L: any, c: string) {
    const { hair, skin, eye, style, gear, expr } = L
    const EW = `<ellipse cx="-6" cy="-12" rx="4" ry="5.2" fill="#fff"/><ellipse cx="6" cy="-12" rx="4" ry="5.2" fill="#fff"/><circle cx="-5.4" cy="-11" r="2.7" fill="${eye}"/><circle cx="6.6" cy="-11" r="2.7" fill="${eye}"/><circle cx="-6.6" cy="-12.6" r="1" fill="#fff"/><circle cx="5.4" cy="-12.6" r="1" fill="#fff"/>`
    const FACE: any = {
      angry: `${EW}<path d="M-10 -17 L-3 -14" stroke="#7a2230" stroke-width="2" stroke-linecap="round"/><path d="M10 -17 L3 -14" stroke="#7a2230" stroke-width="2" stroke-linecap="round"/><path d="M-3 -3 Q0 -6 3 -3 Q0 -1 -3 -3 Z" fill="#5a0f1a"/><rect x="-1" y="-4" width="2" height="2" fill="#fff"/>`,
      cool: `<ellipse cx="-6" cy="-11" rx="4" ry="3.4" fill="#fff"/><ellipse cx="6" cy="-11" rx="4" ry="3.4" fill="#fff"/><circle cx="-5.6" cy="-10.6" r="2.4" fill="${eye}"/><circle cx="6.4" cy="-10.6" r="2.4" fill="${eye}"/><path d="M-10 -13 L-2 -13" stroke="${skin}" stroke-width="3"/><path d="M2 -13 L10 -13" stroke="${skin}" stroke-width="3"/><path d="M-2 -3 L2 -3" stroke="#9a5b4e" stroke-width="1.4" stroke-linecap="round"/>`,
      wink: `<ellipse cx="-6" cy="-12" rx="4" ry="5.2" fill="#fff"/><circle cx="-5.4" cy="-11" r="2.7" fill="${eye}"/><circle cx="-6.6" cy="-12.6" r="1" fill="#fff"/><path d="M2 -11 Q6 -15 10 -11" stroke="#7a3a52" stroke-width="1.8" fill="none" stroke-linecap="round"/><path d="M-2 -3 Q1 -1 3 -4" stroke="#9a5b4e" stroke-width="1.4" fill="none" stroke-linecap="round"/><path d="M11 -19 l1.4 -2.6 l1.4 2.6 l-1.4 2.6 Z" fill="${c}"/>`,
      grin: `<ellipse cx="-6" cy="-12" rx="4" ry="4.6" fill="#fff"/><ellipse cx="6" cy="-12" rx="4" ry="4.6" fill="#fff"/><circle cx="-5.4" cy="-11.5" r="2.7" fill="${eye}"/><circle cx="6.6" cy="-11.5" r="2.7" fill="${eye}"/><circle cx="-6.6" cy="-13" r="1" fill="#fff"/><circle cx="5.4" cy="-13" r="1" fill="#fff"/><path d="M-4 -4 Q0 2 4 -4 Z" fill="#5a0f1a"/><path d="M-4 -4 L4 -4" stroke="#fff" stroke-width="1.6"/>`,
      keen: `${EW.replace(/ry="5\.2"/g, 'ry="4.4"')}<path d="M-10 -16 L-3 -15" stroke="#5a3a2a" stroke-width="1.8" stroke-linecap="round"/><path d="M10 -16 L3 -15" stroke="#5a3a2a" stroke-width="1.8" stroke-linecap="round"/><path d="M-2 -3 L2 -3" stroke="#9a5b4e" stroke-width="1.4" stroke-linecap="round"/>`,
      soft: `${EW}<path d="M-2 -4 Q0 -2 2 -4" stroke="#9a5b4e" stroke-width="1.3" fill="none" stroke-linecap="round"/>`,
      calm: `${EW}<path d="M-2 -3 L2 -3" stroke="#9a5b4e" stroke-width="1.3" stroke-linecap="round"/>`,
    }
    const HAIR: any = {
      spiky: `<path d="M-15 -13 L-11 -28 L-6 -18 L0 -31 L6 -18 L11 -28 L15 -13 Q12 -23 0 -23 Q-12 -23 -15 -13 Z" fill="${hair}"/>`,
      bob: `<path d="M-15 -9 Q-16 -29 0 -29 Q16 -29 15 -9 L15 -3 Q11 -17 0 -17 Q-11 -17 -15 -3 Z" fill="${hair}"/>`,
      pony: `<path d="M-14 -11 Q-14 -29 0 -29 Q14 -29 14 -11 L11 -13 Q11 -23 0 -23 Q-11 -23 -11 -13 Z" fill="${hair}"/>`,
      twin: `<path d="M-13 -12 Q-13 -29 0 -29 Q13 -29 13 -12 L10 -14 Q10 -23 0 -23 Q-10 -23 -10 -14 Z" fill="${hair}"/>`,
      long: `<path d="M-15 -9 Q-16 -30 0 -30 Q16 -30 15 -9 L15 -3 Q11 -17 0 -17 Q-11 -17 -15 -3 Z" fill="${hair}"/>`,
    }
    const GEAR: any = {
      horns: `<path d="M-9 -23 L-14 -36 L-3 -26 Z" fill="${c}"/><path d="M9 -23 L14 -36 L3 -26 Z" fill="${c}"/>`,
      headset: `<path d="M-14 -15 Q-14 -30 0 -30 Q14 -30 14 -15" stroke="${c}" stroke-width="2.4" fill="none"/><rect x="-18" y="-15" width="5" height="9" rx="2" fill="${c}"/><rect x="13" y="-15" width="5" height="9" rx="2" fill="${c}"/><path d="M-16 -7 Q-10 -3 -4 -5" stroke="${c}" stroke-width="1.4" fill="none"/>`,
      headband: `<rect x="-15" y="-21" width="30" height="4.6" rx="1.6" fill="${c}"/><path d="M14 -20 L26 -15 L23 -21 Z" fill="${c}"/><path d="M14 -18 L27 -9 L22 -18 Z" fill="${c}" opacity="0.7"/>`,
      catears: `<path d="M-13 -23 L-17 -37 L-5 -27 Z" fill="${hair}"/><path d="M13 -23 L17 -37 L5 -27 Z" fill="${hair}"/><path d="M-12 -25 L-14 -32 L-8 -27 Z" fill="${c}"/><path d="M12 -25 L14 -32 L8 -27 Z" fill="${c}"/>`,
      clip: `<path d="M9 -25 l1.6 -3 l1.6 3 l-1.6 3 Z" fill="${c}"/>`,
      none: '',
    }
    const blush = `<ellipse cx="-8" cy="-7" rx="3" ry="2" fill="${c}" opacity="0.3"/><ellipse cx="8" cy="-7" rx="3" ry="2" fill="${c}" opacity="0.3"/>`
    if (gear === 'hood')
      return `<ellipse cx="0" cy="-12" rx="15" ry="15" fill="${skin}"/>
        <path d="M-16 -6 Q-20 -34 0 -34 Q20 -34 16 -6 L16 -2 Q13 -19 0 -19 Q-13 -19 -16 -2 Z" fill="${c}" opacity="0.93"/>
        <path d="M-9 -12 L-3 -11 L-4 -8 L-9 -9 Z" fill="${eye}"/><path d="M9 -12 L3 -11 L4 -8 L9 -9 Z" fill="${eye}"/>
        <path d="M-9 -6 Q0 -3 9 -6 L9 0 Q0 4 -9 0 Z" fill="#15122b" stroke="${c}" stroke-width="1"/>`
    if (gear === 'visor')
      return `<ellipse cx="0" cy="-12" rx="15" ry="15" fill="${skin}"/>${HAIR[style]}
        <rect x="-13" y="-15" width="26" height="8.5" rx="3.5" fill="${c}" opacity="0.92"/>
        <rect x="-11" y="-13.5" width="9" height="2.6" rx="1" fill="#fff" opacity="0.75"/>
        <path d="M-4 -3 Q0 0 4 -3" stroke="#9a5b4e" stroke-width="1.4" fill="none" stroke-linecap="round"/>`
    return `<ellipse cx="0" cy="-12" rx="15" ry="15" fill="${skin}"/>${blush}${HAIR[style]}${FACE[expr] || FACE.cool}${GEAR[gear] || ''}`
  }
  // simplified torso-top (collar + V-neck + gem) matching the body, for the profile bust
  function chibiCollar(c: string) {
    return `<path d="M-13 0 Q-13 -3 -9 -3 L9 -3 Q13 -3 13 0 L13 9 L-13 9 Z" fill="#1b1838" stroke="${c}" stroke-width="2"/><path d="M-13 0 Q-13 -3 -9 -3 L0 -3 L0 9 L-13 9 Z" fill="${c}" opacity="0.7"/><path d="M-9 -3 L0 5 L9 -3 Z" fill="#0c0a1c"/><circle cx="0" cy="2" r="2.4" fill="${c}"/>`
  }

  function buildBase(t: any) {
    const g = el('g', { id: 'base-' + t.id, class: 'team-marker', transform: `translate(${t.x} ${t.y})` })
    g.innerHTML = `
      <title>${esc(t.name)}</title>
      <circle class="marker-ring" r="20" stroke="${t.color}"/>
      <text class="marker-index" y="4.5" text-anchor="middle">${t.idx + 1}</text>
      <g id="svc-${t.id}" transform="translate(0 27)"></g>
      <text class="node-label" x="0" y="54" text-anchor="middle">${esc(arenaTeamLabel(t.name))}</text>
      <text class="node-score" id="sc-${t.id}" x="0" y="71" text-anchor="middle">${fmtAdScore(dispScore(t))}</text>`
    return g
  }

  function updateSelection() {
    const selected = TEAMS.find((team) => team.id === selectedTeamId)
    if (!selected) selectedTeamId = null
    for (const team of TEAMS) {
      const active = team.id === selectedTeamId
      $('base-' + team.id)?.classList.toggle('selected', active)
      const row = $('rk-' + team.id)
      row?.classList.toggle('selected', active)
      row?.setAttribute('aria-pressed', String(active))
    }
    $('selectionName').textContent = selected ? selected.name : 'Select a team in the rankings to highlight it.'
    $('selectionScore').textContent = selected ? fmtAdScore(dispScore(selected)) + ' pts' : ''
    $('teamCount').textContent = String(TEAMS.length)
    $('serviceCount').textContent = String(SERVICES.length)
    $('hillCount').textContent = String(HILLS.length)
  }

  // service status → colour. def=Ok(green) vuln=Mumble(amber) down=Offline(grey)
  // error=InternalError(violet) none=never-checked(dim). pwned=transient red flash on capture.
  const SVC_COLOR: any = {
    def: '#3dffb0',
    vuln: '#ffb020',
    down: '#4f4a78',
    error: '#9d6bff',
    none: '#2f2c44',
    pwned: '#ff3b5b',
  }
  const MISS_COL = '#8c5663' // rejected-flag (wrong answer) tracer — muted blood-grey
  function renderSvc(t: any) {
    const g = $('svc-' + t.id)
    if (!g) return
    const now = Date.now()
    const n = t.svc.length,
      w = 11,
      gap = 4,
      tot = n * w + (n - 1) * gap,
      start = -tot / 2
    // Build the per-service rects ONCE; on later calls just patch the fill of the ones
    // that changed (a flag burst re-tints tiles without re-creating DOM each time).
    if (g.childElementCount !== n) {
      g.innerHTML = ''
      for (let i = 0; i < n; i++)
        g.appendChild(
          el('rect', {
            x: start + i * (w + gap),
            y: 0,
            width: w,
            height: 11,
            rx: 2,
            fill: SVC_COLOR.none,
            stroke: '#06050f',
            'stroke-width': 1,
          })
        )
    }
    const rects = g.children
    t.svc.forEach((s: any, i: number) => {
      // a freshly-pwned service flashes red for a few seconds; otherwise it shows its
      // SLA check verdict colour (Ok / Mumble / Offline / InternalError).
      const fill = s.pwnUntil && s.pwnUntil > now ? SVC_COLOR.pwned : SVC_COLOR[s.status] || SVC_COLOR.none
      const rc: any = rects[i]
      if (rc && rc.getAttribute('fill') !== fill) rc.setAttribute('fill', fill)
    })
  }
  function renderScore(t: any) {
    const e = $('sc-' + t.id)
    if (e) e.textContent = fmtAdScore(dispScore(t))
  }
  function pulseBase(t: any, col: string) {
    if (frozen || !motionEnabled) return
    const g = $('base-' + t.id)
    if (!g) return
    g.style.transition = 'none'
    g.style.filter = `drop-shadow(0 0 10px ${col})`
    requestAnimationFrame(() => {
      g.style.transition = 'filter .6s'
      g.style.filter = 'none'
    })
  }

  /* -------- FX canvas -------- */
  let SC = 1
  function sizeCanvas() {
    const r = arena.getBoundingClientRect()
    arenaRect = r
    const dpr = 1 // FX particle layers; the SVG stays vector-crisp regardless
    fx.width = r.width * dpr
    fx.height = r.height * dpr
    fxbg.width = r.width * dpr
    fxbg.height = r.height * dpr
    SC = (r.width / 1000) * dpr
    ctx.setTransform(SC, 0, 0, SC, 0, 0)
    ctxbg.setTransform(SC, 0, 0, SC, 0, 0)
    fxRenderer.resize(r.width, r.height) // keep the WebGL FX layer aligned to the arena
    fbRenderer.resize() // first-blood layer is viewport-sized; tracks the window
    fzRenderer.resize() // freeze frost is viewport-sized too
    winRenderer.resize() // victory confetti/rays viewport-sized too
    jeop.layout()
  }
  // rAF-coalesce the window resize: a drag-burst (30-60/s) collapses to one sizeCanvas per frame,
  // each of which does forced reflows + a full jeopardy relayout/SVG rebuild + two Pixi resizes.
  // (Direct sizeCanvas() calls for fullscreen/init stay synchronous so they aren't deferred.)
  let resizeRaf = 0
  const onResize = () => {
    if (resizeRaf) return
    resizeRaf = requestAnimationFrame(() => {
      resizeRaf = 0
      if (!killed) sizeCanvas()
    })
  }
  window.addEventListener('resize', onResize)
  // keep audio alive when the tab is backgrounded (some browsers suspend the context)
  const onLiveAvailability = () => {
    if (killed || preview) return
    if (!liveReadsAllowed()) {
      livePollController?.abort()
      clearTimeout(livePollTimer)
      livePollTimer = 0
      clearTimeout(reconnectTimer)
      reconnectTimer = 0
      return
    }
    if (livePollStarted) scheduleLivePoll(0)
    if (TEAMS.length) connectWS()
  }
  const onVis = () => {
    snd.resume()
    onLiveAvailability()
  }
  document.addEventListener('visibilitychange', onVis)
  window.addEventListener('online', onLiveAvailability)
  window.addEventListener('offline', onLiveAvailability)

  const shots: any[] = [],
    sparks: any[] = [],
    fxq: any[] = []

  // Ambient idle motion lives on the #fxbg background canvas (behind the SVG) instead
  // of animating the SVG DOM every frame: rotating recon rings and a soft breathing aura
  // behind each (static, crisp) SVG avatar. This is what keeps the
  // arena smooth — the SVG now only repaints on real events (scores, status, ownership).
  let fxClock = 0,
    ambientTick = 0
  // Pre-render each team-colour glow once and blit it, instead of building a radial
  // gradient every frame (gradient creation is the only pricey per-frame canvas op).
  const glowCache: Record<string, any> = {}
  function glowSprite(color: string) {
    let c = glowCache[color]
    if (!c) {
      c = document.createElement('canvas')
      c.width = c.height = 64
      const g = c.getContext('2d')
      const grd = g.createRadialGradient(32, 32, 1, 32, 32, 32)
      grd.addColorStop(0, color)
      grd.addColorStop(1, 'transparent')
      g.fillStyle = grd
      g.fillRect(0, 0, 64, 64)
      glowCache[color] = c
    }
    return c
  }
  function drawAmbient(T: number) {
    if (!motionEnabled || frozen || ambientTick++ % 2) return // ~30fps ambient; skip entirely while frozen (overlay covers it)
    const TAU = 6.2832
    ctxbg.clearRect(0, 0, 1000, 1000)
    // the colosseum ring is static SVG now (no rotating recon rings) — the ambient canvas
    // only carries the per-avatar breathing aura (replaces the per-avatar SVG bob)
    for (const t of TEAMS) {
      if (t.globeVisible === false) continue
      const p = (Math.sin((T * TAU) / 2.8 + t.idx * 0.7) + 1) / 2
      const rad = 24 + 5 * p
      ctxbg.globalAlpha = 0.18 + 0.13 * p
      ctxbg.drawImage(glowSprite(t.color), t.x - rad, t.y - 4 - rad, rad * 2, rad * 2)
    }
    ctxbg.globalAlpha = 1
  }

  function fireShot(from: any, to: any, col: string, miss = false) {
    if (!motionEnabled || frozen || document.hidden || shots.length > 220) return // cap: a huge flag burst can't unbound the queue
    const cx = (from.x + to.x) / 2,
      cy = (from.y + to.y) / 2
    const dx = to.x - from.x,
      dy = to.y - from.y
    const px = -dy,
      py = dx,
      len = Math.hypot(px, py) || 1
    const bow = rng(40, 90) * (Math.random() < 0.5 ? 1 : -1)
    shots.push({
      x: from.x,
      y: from.y,
      fx: from.x,
      fy: from.y,
      tx: to.x,
      ty: to.y,
      cx: cx + (px / len) * bow,
      cy: cy + (py / len) * bow,
      t: 0,
      sp: rng(0.018, 0.028) * speed,
      col,
      miss,
      trail: [],
    })
  }
  const bez = (a: number, c: number, b: number, t: number) => {
    const u = 1 - t
    return u * u * a + 2 * u * t * c + t * t * b
  }
  function addSpark(x: number, y: number, col: string) {
    if (!motionEnabled) return
    if (document.hidden || sparks.length > 600) return
    for (let i = 0; i < 16; i++) {
      const a = rng(0, 6.28),
        v = rng(60, 260)
      sparks.push({ x, y, vx: Math.cos(a) * v, vy: Math.sin(a) * v, life: 1, col })
    }
    sparks.push({ x, y, ring: true, r: 4, life: 1, col })
  }
  function hexPath(x: number, y: number, r: number) {
    ctx.beginPath()
    for (let i = 0; i < 6; i++) {
      const a = ((60 * i - 90) * Math.PI) / 180
      const px = x + r * Math.cos(a),
        py = y + r * Math.sin(a)
      if (i) ctx.lineTo(px, py)
      else ctx.moveTo(px, py)
    }
    ctx.closePath()
  }
  function spawnShield(x: number, y: number, col: string) {
    if (!motionEnabled) return
    if (frozen || document.hidden) return
    fxq.push({ kind: 'shield', x, y, col, t: 0, dur: 0.95 })
    for (let i = 0; i < 10; i++) {
      const a = -1.57 + rng(-1, 1)
      const v = rng(70, 150)
      sparks.push({ x: x + rng(-14, 14), y: y + 10, vx: Math.cos(a) * v * 0.4, vy: -Math.abs(v), life: 1, col })
    }
  }
  function spawnDown(x: number, y: number, col: string) {
    if (!motionEnabled) return
    if (frozen || document.hidden) return
    fxq.push({ kind: 'down', x, y, col, t: 0, dur: 1.0 })
    for (let i = 0; i < 14; i++) {
      const v = rng(60, 180)
      sparks.push({ x: x + rng(-12, 12), y: y - 6, vx: rng(-40, 40), vy: Math.abs(v), life: 1, col })
    }
  }
  function spawnBeam(from: any, to: any, col: string, big: boolean) {
    if (!motionEnabled) return
    if (frozen || document.hidden) return
    fxq.push({ kind: 'beam', fx: from.x, fy: from.y, tx: to.x, ty: to.y, col, t: 0, dur: big ? 0.6 : 0.42, big: !!big })
  }
  function spawnCapture(from: any, hill: any, col: string) {
    if (!motionEnabled) return
    if (frozen || document.hidden) return
    spawnBeam(from, hill, col, false)
    fxq.push({ kind: 'shield', x: hill.x, y: hill.y, col, t: 0, dur: 0.9 })
  }
  // replay a CSS class animation (toggle + forced reflow), then drop the class after ms
  function restartAnim(g: any, cls: string, ms: number) {
    if (!motionEnabled) return
    if (!g) return
    g.classList.remove(cls)
    void g.offsetWidth
    g.classList.add(cls)
    ownedTimeout(() => g.classList.remove(cls), ms)
  }

  function drawFX(dt: number) {
    fxClock += dt
    drawAmbient(fxClock)
    const pixi = fxRenderer.ready // WebGL path renders the FX; 2D draws below are the fallback
    if (!pixi) {
      ctx.clearRect(0, 0, 1000, 1000)
      ctx.lineCap = 'round'
    }
    for (let i = shots.length - 1; i >= 0; i--) {
      const s = shots[i]
      s.t += s.sp * dt * 60
      const x = bez(s.fx, s.cx, s.tx, Math.min(s.t, 1))
      const y = bez(s.fy, s.cy, s.ty, Math.min(s.t, 1))
      s.trail.push({ x, y })
      if (s.trail.length > 14) s.trail.shift()
      if (!pixi) {
        const tr = s.trail,
          n = tr.length,
          aMul = s.miss ? 0.32 : 0.9,
          wMul = s.miss ? 0.45 : 1
        ctx.strokeStyle = s.col
        if (n >= 2) {
          ctx.beginPath()
          ctx.moveTo(tr[0].x, tr[0].y)
          for (let j = 1; j < n; j++) ctx.lineTo(tr[j].x, tr[j].y)
          ctx.globalAlpha = 0.38 * aMul
          ctx.lineWidth = 3 * wMul
          ctx.stroke()
          ctx.beginPath()
          ctx.moveTo(tr[n - 2].x, tr[n - 2].y)
          ctx.lineTo(tr[n - 1].x, tr[n - 1].y)
          ctx.globalAlpha = 0.9 * aMul
          ctx.lineWidth = 6 * wMul
          ctx.stroke()
        }
        ctx.globalAlpha = s.miss ? 0.4 : 0.55
        ctx.fillStyle = s.col
        ctx.beginPath()
        ctx.arc(x, y, s.miss ? 5 : 9, 0, 6.28)
        ctx.fill()
        ctx.globalAlpha = s.miss ? 0.7 : 1
        ctx.fillStyle = s.miss ? s.col : '#fff'
        ctx.beginPath()
        ctx.arc(x, y, s.miss ? 2.5 : 4, 0, 6.28)
        ctx.fill()
        ctx.globalAlpha = 1
      }
      if (s.t >= 1) {
        if (!s.miss) addSpark(s.tx, s.ty, s.col)
        shots.splice(i, 1)
      }
    }
    for (let i = sparks.length - 1; i >= 0; i--) {
      const sp = sparks[i]
      if (sp.ring) {
        sp.r += 420 * dt
        sp.life -= 2.2 * dt
        if (!pixi) {
          ctx.globalAlpha = Math.max(sp.life, 0)
          ctx.strokeStyle = sp.col
          ctx.lineWidth = 3
          ctx.beginPath()
          ctx.arc(sp.x, sp.y, sp.r, 0, 6.28)
          ctx.stroke()
          for (let k = 0; k < 8; k++) {
            const a = (k / 8) * 6.28
            ctx.beginPath()
            ctx.moveTo(sp.x + Math.cos(a) * sp.r, sp.y + Math.sin(a) * sp.r)
            ctx.lineTo(sp.x + Math.cos(a) * (sp.r + 10), sp.y + Math.sin(a) * (sp.r + 10))
            ctx.stroke()
          }
          ctx.globalAlpha = 1
        }
      } else {
        sp.x += sp.vx * dt
        sp.y += sp.vy * dt
        sp.vx *= 0.92
        sp.vy *= 0.92
        sp.life -= 2.4 * dt
        if (!pixi) {
          ctx.globalAlpha = Math.max(sp.life, 0)
          ctx.fillStyle = sp.col
          ctx.fillRect(sp.x - 2, sp.y - 2, 4, 4)
          ctx.globalAlpha = 1
        }
      }
      if (sp.life <= 0) sparks.splice(i, 1)
    }
    for (let i = fxq.length - 1; i >= 0; i--) {
      const e = fxq[i]
      e.t += dt / e.dur
      const p = Math.min(e.t, 1)
      // beam-impact spark is a side-effect — must fire on the WebGL path too (extracted from the
      // draw). 0.4 matches the PLASMA LANCE head-arrival in all three beam renderers.
      if (e.kind === 'beam' && !e.hit && p / 0.4 >= 1) {
        e.hit = true
        addSpark(e.tx, e.ty, e.col)
      }
      if (!pixi) {
        if (e.kind === 'shield') {
          ctx.lineCap = 'round'
          const rIn = 70 - 58 * Math.min(p * 2, 1)
          ctx.globalAlpha = (1 - p) * 0.9
          ctx.strokeStyle = e.col
          ctx.lineWidth = 3
          hexPath(e.x, e.y, Math.max(rIn, 12))
          ctx.stroke()
          const rOut = 18 + 60 * p
          ctx.globalAlpha = (1 - p) * 0.6
          ctx.lineWidth = 2
          hexPath(e.x, e.y, rOut)
          ctx.stroke()
          ctx.globalAlpha = (1 - p) * 0.28
          ctx.fillStyle = e.col
          hexPath(e.x, e.y, Math.max(rIn, 12))
          ctx.fill()
          ctx.globalAlpha = 1
        } else if (e.kind === 'down') {
          const r = 70 * (1 - p)
          ctx.globalAlpha = (1 - p) * 0.85
          ctx.strokeStyle = e.col
          ctx.lineWidth = 3
          ctx.beginPath()
          ctx.arc(e.x, e.y, Math.max(r, 2), 0, 6.28)
          ctx.stroke()
          ctx.globalAlpha = (1 - p) * 0.5
          for (let k = 0; k < 3; k++) {
            const yy = e.y + rng(-26, 26)
            ctx.fillStyle = e.col
            ctx.fillRect(e.x - 30, yy, 60, 2)
          }
          ctx.globalAlpha = 1
        } else if (e.kind === 'beam') {
          // PLASMA LANCE (2D fallback before WebGL loads): thick flickering beam + white-hot core
          const tt = Math.min(p / 0.4, 1)
          const hx = e.fx + (e.tx - e.fx) * tt,
            hy = e.fy + (e.ty - e.fy) * tt
          const fade = p < 0.7 ? 1 : Math.max(0, 1 - (p - 0.7) / 0.3),
            fl = 0.82 + 0.18 * Math.sin(p * 50),
            b = e.big ? 1.5 : 1
          ctx.lineCap = 'round'
          const beam = (w: number, color: string, a: number) => {
            ctx.globalAlpha = a
            ctx.strokeStyle = color
            ctx.lineWidth = w
            ctx.beginPath()
            ctx.moveTo(e.fx, e.fy)
            ctx.lineTo(hx, hy)
            ctx.stroke()
          }
          ctx.shadowColor = e.col
          ctx.shadowBlur = e.big ? 30 : 20
          beam(22 * b * fl, e.col, 0.16 * fade)
          beam(11 * b, e.col, 0.45 * fade)
          ctx.shadowBlur = 0
          beam(5 * b, '#ffebff', 0.92 * fade)
          ctx.fillStyle = '#fff'
          ctx.globalAlpha = 0.9 * fade
          ctx.beginPath()
          ctx.arc(hx, hy, 6 * b, 0, 6.28)
          ctx.fill()
          ctx.globalAlpha = 1
        }
      }
      if (e.t >= 1) fxq.splice(i, 1)
    }
  }

  function floatText(wx: number, wy: number, txt: string, col: string) {
    if (!motionEnabled) return
    if (frozen || document.hidden) return
    const r = arenaRect || arena.getBoundingClientRect() // cached; only re-measured if not yet sized
    const px = (wx / 1000) * r.width,
      py = (wy / 1000) * r.height
    const d = document.createElement('div')
    d.className = 'float'
    d.style.left = px + 'px'
    d.style.top = py + 'px'
    d.style.color = col
    d.style.transform = 'translate(-50%,-50%)'
    d.textContent = txt
    arena.appendChild(d)
    ownedTimeout(() => d.remove(), 1100)
  }

  /* -------- log -------- */
  const clk = () => new Date(tNow).toUTCString().slice(17, 25)
  function addLog(tag: string, cls: string, html: string) {
    if (frozen && cls !== 'sys') return // public board redacted during freeze; keep system lines
    const row = document.createElement('div')
    row.className = 'lg'
    row.innerHTML = `<span class="ts">${clk()}</span><span class="tag ${cls}">${tag}</span>${html}`
    logEl.appendChild(row)
    while (logEl.children.length > 60) logEl.removeChild(logEl.firstChild)
    logDirty = true // scroll-to-bottom batched in the loop (avoids a forced reflow per event)
  }

  let pendingResolves = 0 // in-flight resolveFlag setTimeouts; burst signal for the overflow path
  // quiet=true (burst overflow): keep capture evidence + dirty-flag draws live, but skip the
  // per-event cosmetics (audio graph, float DOM nodes + their timers, battle-log innerHTML, pwn pulse)
  // so a flag flurry / 256-deep WS reconnect catch-up can't flood timers+audio+DOM. The bounded poll is
  // official board is the score source of truth, so skipped cosmetics never desync it.
  function resolveFlag(atkr: any, vic: any, svc: any, pts: number, isFB: boolean, quiet?: boolean) {
    // A&D capture → attack SFX at impact; jeopardy solve plays sfxSolve at the laser instead.
    // No SFX while frozen: the public freeze redacts scoring events, and an audible cue would
    // leak that a capture happened (the visual fireShot/laser are already frozen-gated).
    if (!isFB && vic && !quiet && !frozen) snd.sfxAttack()
    // An accepted A&D flag is evidence, not an immediate point award. Epoch scoring
    // settles it with defense and SLA evidence on the next official-board poll.
    if (vic) {
      atkr.captureEvidence = (atkr.captureEvidence || 0) + 1
      if (preview) atkr.offenseRate = boundedRate(atkr.captureEvidence / Math.max(TEAMS.length - 1, 1))
    } else {
      atkr.jpScore = (atkr.jpScore || 0) + pts
      atkr.jpSolved = (atkr.jpSolved || 0) + 1
    }
    if (!quiet) {
      if (vic && svc) {
        svc.pwnUntil = Date.now() + 5000
        ownedTimeout(() => {
          if (!killed) renderSvc(vic)
        }, 5200)
      }
      if (vic) {
        renderSvc(vic)
        pulseBase(vic, vic.color)
      }
      floatText(atkr.x, atkr.y - 66, vic ? 'CAPTURE ACCEPTED' : pts > 0 ? '+' + pts : 'SOLVED', atkr.color)
      if (vic) floatText(vic.x, vic.y - 66, isFB ? 'FIRST BLOOD' : 'PWNED', vic.color)
      if (isFB)
        addLog(
          'FIRST BLOOD',
          'fb',
          `<span class="who">${esc(atkr.name)}</span> drew first blood${vic ? ` on <span class="vic">${esc(vic.name)}</span> <span class="em">CAPTURE ACCEPTED</span>` : ''}`
        )
      else
        addLog(
          'FLAG',
          'flag',
          `<span class="who">${esc(atkr.name)}</span> &gt; <span class="vic">${vic ? esc(vic.name) : 'CORE'}</span> :: <span class="svc">${esc(svc ? svc.name : 'flag')}</span>${vic ? ' <span class="em">CAPTURE ACCEPTED</span>' : pts > 0 ? ` <span class="em">+${pts}</span>` : ''}`
        )
    }
    totalFlags++
    refreshRank()
  }

  // Jewel-band crown (fill=currentColor; tight viewBox so it fills its box). Used as the KotH
  // VS separator (purple) and the throne portrait (dark on a purple disc).
  const JEWEL_CROWN = `<svg viewBox="10 29 80 59" fill="currentColor"><path d="M10 70 L14 36 L30 50 L50 30 L70 50 L86 36 L90 70 Z"/><rect x="10" y="66" width="80" height="16" rx="3"/><circle cx="50" cy="73" r="4" fill="#fff" opacity=".55"/><circle cx="28" cy="73" r="3.2" fill="#fff" opacity=".45"/><circle cx="72" cy="73" r="3.2" fill="#fff" opacity=".45"/></svg>`
  // Per-kind first-blood theming. A&D = blood clash (attacker vs defender + challenge reticle),
  // Jeopardy = solo capture (attacker + big challenge reticle, no victim), KotH = coronation
  // ("FIRST CROWN", purple nova, crown separator + throne). retAccent/retGlow colour the reticle.
  const FB_THEME: any = {
    ad: {
      title: 'FIRST BLOOD',
      accent: '#ff3b5b',
      accent2: '#ff2350',
      sep: 'VS',
      tele: 'INCOMING STRIKE',
      palette: 'blood',
      atkTag: 'ATTACKER',
      oppTag: 'DEFENDER',
      retAccent: '#ffc637',
      retGlow: 'rgba(255,198,55,.6)',
      retLabel: '&#9635;',
    },
    jeopardy: {
      title: 'FIRST BLOOD',
      accent: '#ffc637',
      accent2: '#ff9a1f',
      sep: '',
      tele: 'INCOMING BREACH',
      palette: 'blood',
      atkTag: 'ATTACKER',
      oppTag: '',
      retAccent: '#ffc637',
      retGlow: 'rgba(255,198,55,.6)',
      retLabel: '&#9635; CHALLENGE',
    },
    koth: {
      title: 'FIRST CROWN',
      accent: '#9d6bff',
      accent2: '#b98bff',
      sep: 'CROWN',
      tele: 'INCOMING SIEGE',
      palette: 'crown',
      atkTag: 'CHALLENGER',
      oppTag: 'THE THRONE',
      retAccent: '#9d6bff',
      retGlow: 'rgba(157,107,255,.6)',
      retLabel: '',
    },
  }

  // opt: { kind, oppName, oppColor, oppPortrait(html), beamTo, onImpact }
  function fbCinematic(atkr: any, opt: any) {
    if (!motionEnabled) {
      opt.onImpact?.()
      return
    }
    cinema = true
    const th = FB_THEME[opt.kind] || FB_THEME.ad
    const oppName = opt.oppName || 'THE FIELD'
    const oppColor = opt.oppColor || th.accent
    const ov: any = $('fbOverlay')
    const ttl: any = root.querySelector('.fb-title')
    const tShadowW = th.palette === 'crown' ? 3 : 4
    if (ttl) {
      ttl.textContent = th.title
      ttl.style.textShadow = `${tShadowW}px 0 ${th.accent2}, -${tShadowW}px 0 #27e3ff, 0 0 26px ${th.accent}, 0 0 60px ${th.accent}`
    }
    // separator: "VS"/flag text, a crown SVG (KotH), or collapsed (jeopardy = solo attacker)
    const vsEl: any = $('fbVs')
    if (vsEl) vsEl.classList.toggle('solo', !!opt.solo)
    const vsx: any = $('fbVsx')
    if (vsx) {
      if (th.sep === 'CROWN') {
        vsx.innerHTML = JEWEL_CROWN
        vsx.style.color = th.accent
        vsx.style.textShadow = 'none'
      } else {
        vsx.innerHTML = ''
        vsx.textContent = th.sep
        vsx.style.color = '#fff'
        vsx.style.textShadow = `0 0 14px ${th.accent},2px 0 ${th.accent2},-2px 0 #27e3ff`
      }
    }
    root.querySelectorAll('.fb-fighter .por').forEach((p: any) => {
      p.style.boxShadow = `0 0 26px ${th.accent}`
    })
    $('fbSub').innerHTML = opt.sub || ''
    $('fbAtkPor').innerHTML = avatar(atkr.look, atkr.color)
    const vicPor: any = $('fbVicPor')
    if (vicPor) {
      vicPor.innerHTML = opt.oppPortrait || ''
      vicPor.classList.toggle('fb-throne', opt.oppPorClass === 'fb-throne')
    }
    const an: any = $('fbAtkNm')
    an.textContent = atkr.name
    an.style.color = atkr.color
    const vn: any = $('fbVicNm')
    vn.textContent = oppName
    vn.style.color = oppColor
    // role tags (team-coloured): who attacked / who defended / who holds the throne
    const setTag = (el: any, label: string, col: string) => {
      if (!el) return
      if (!label) {
        el.style.display = 'none'
        return
      }
      el.style.display = ''
      el.textContent = label
      el.style.color = col
      el.style.background = `${col}1f`
      el.style.border = `1px solid ${col}80`
    }
    setTag($('fbAtkTag'), th.atkTag, atkr.color)
    setTag($('fbVicTag'), th.oppTag, oppColor)
    // challenge reticle (A&D + Jeopardy show the challenge; KotH's "challenge" IS the throne)
    const chalEl: any = $('fbChal')
    if (chalEl) {
      if (opt.chal) {
        chalEl.innerHTML = `<div class="fb-brk${opt.chalBig ? ' big' : ''}" style="--rc:${th.retAccent};--rcg:${th.retGlow}"><i class="tl"></i><i class="tr"></i><i class="bl"></i><i class="br"></i><span class="lbl">${th.retLabel}</span> ${esc(opt.chal)}</div>`
        chalEl.style.display = ''
      } else {
        chalEl.innerHTML = ''
        chalEl.style.display = 'none'
      }
    }
    // ---- PHASE 1: telegraph (attention-seeking pre-roll) ----
    // The board stays FULLY VISIBLE while a warning builds (transparent edge
    // vignette + a pulsing "INCOMING …" banner), so the room
    // can read the scoreboard before the slam reveals FIRST BLOOD. The older
    // arena did this with an "INCOMING STRIKE" banner; this restores that beat.
    const teleTxt: any = root.querySelector('.fb-tele-txt')
    if (teleTxt) {
      teleTxt.textContent = th.tele
      teleTxt.style.textShadow = `0 0 16px ${th.accent}`
    }
    const teleVig: any = root.querySelector('.fb-tele-vig')
    if (teleVig) teleVig.style.background = `radial-gradient(circle at 50% 50%,transparent 40%,${th.accent}3a 100%)`
    ov.style.setProperty('--fbPre', FB.preroll + 'ms')
    ov.classList.remove('play', 'tele')
    void ov.offsetWidth
    ov.classList.add('tele')
    // A procedural alarm crescendos through the telegraph, then yields to the
    // reveal stinger. Keeping this in Web Audio avoids loading licensed samples.
    stopIncomingSound?.()
    stopFirstBloodSound?.()
    stopFirstBloodSound = null
    stopIncomingSound = snd.sfxIncoming(FB.preroll / 1000)
    // ---- PHASE 2: the slam cinematic, after the build-up ----
    ownedTimeout(() => {
      if (killed) return
      stopIncomingSound?.()
      stopIncomingSound = null
      ov.classList.remove('tele')
      void ov.offsetWidth
      ov.classList.add('play')
      // GPU slam graphics (dark+splash+flash), synced with the DOM text/bars. Burst behind the
      // hero title (its layout centre is stable under the scale animation), tinted per mode.
      const tr: any = ttl ? ttl.getBoundingClientRect() : null
      if (motionEnabled)
        fbRenderer.play(
          FB.total,
          tr ? { cx: tr.left + tr.width / 2, cy: tr.top + tr.height / 2, palette: th.palette } : { palette: th.palette }
        )
      slamCovering = true // the dark slam overlay covers the board — pause the arena draw underneath
      // The procedural first-blood stinger fires with the reveal so it
      // punctuates the slam rather than the build-up.
      ownedTimeout(() => {
        if (killed || !snd.isEnabled()) return
        stopFirstBloodSound = snd.sfxFirstBlood()
      }, FB.soundDelay)
      ownedTimeout(() => {
        if (killed) return
        const sh: any = root.querySelector('.shell')
        if (sh) {
          sh.classList.add('shake')
          ownedTimeout(() => sh.classList.remove('shake'), 520)
        }
        if (opt.onImpact) opt.onImpact()
      }, FB.slam)
      ownedTimeout(() => {
        if (opt.beamTo) {
          spawnBeam(atkr, opt.beamTo, th.accent, true)
          if (opt.beamTo.id && opt.beamTo.color) pulseBase(opt.beamTo, opt.beamTo.color)
        }
      }, FB.total - 680)
      ownedTimeout(() => {
        ov.classList.remove('play')
        cinema = false
        slamCovering = false
      }, FB.total)
    }, FB.preroll)
  }
  function fbAd(atkr: any, vic: any, chalName: string, onImpact: () => void) {
    const oppN = vic ? vic.name : 'THE FIELD',
      oppC = vic ? vic.color : '#ff3b5b'
    fbCinematic(atkr, {
      kind: 'ad',
      oppName: oppN,
      oppColor: oppC,
      oppPortrait: vic ? avatar(vic.look, vic.color) : '',
      sub: `<span style="color:${atkr.color}">${esc(atkr.name)}</span> &nbsp;&#9656;&nbsp; <span style="color:${oppC}">${esc(oppN)}</span>`,
      chal: chalName,
      beamTo: vic || { x: CX, y: CY },
      onImpact,
    })
  }
  function fbJeopardy(atkr: any, chalName: string, onImpact: () => void) {
    fbCinematic(atkr, {
      kind: 'jeopardy',
      solo: true,
      chal: chalName,
      chalBig: true,
      sub: `<span style="color:${atkr.color}">${esc(atkr.name)}</span> drew first blood`,
      beamTo: { x: CX, y: CY },
      onImpact,
    })
  }
  function fbKoth(atkr: any, hill: any, onImpact: () => void) {
    const hillN = hill ? hill.name : 'THE HILL'
    fbCinematic(atkr, {
      kind: 'koth',
      oppName: hillN,
      oppColor: '#9d6bff',
      oppPortrait: JEWEL_CROWN,
      oppPorClass: 'fb-throne',
      sub: `<span style="color:${atkr.color}">${esc(atkr.name)}</span> seized <span style="color:#9d6bff">${esc(hillN)}</span>`,
      beamTo: hill || { x: CX, y: CY },
      onImpact,
    })
  }

  /* -------- rank + stats -------- */
  let rankInit = false
  function rebuildRank() {
    rankEl.innerHTML = ''
    TEAMS.forEach((t) => {
      const div = document.createElement('button')
      div.type = 'button'
      div.dataset.teamId = t.id
      div.setAttribute('aria-pressed', String(selectedTeamId === t.id))
      div.onclick = () => {
        selectedTeamId = selectedTeamId === t.id ? null : t.id
        updateSelection()
        jeop.focusTeam(selectedTeamId)
      }
      div.id = 'rk-' + t.id
      div.className = 'rk'
      div.innerHTML = `<span class="pos"></span>
        <span class="av" aria-hidden="true" style="--team-color:${t.color}">${esc(arenaTeamInitials(t.name))}</span>
        <span class="body"><span class="nm">${esc(t.name)}</span>
          <span class="bars" id="bars-${t.id}"><i id="ba-${t.id}" title="Offense rate" style="background:#27e3ff"></i><i id="bd-${t.id}" title="Defense rate" style="background:#3dffb0"></i><i id="bf-${t.id}" title="SLA rate" style="background:#ffc637"></i></span></span>
        <span class="sc" id="rsc-${t.id}"></span>`
      rankEl.appendChild(div)
      const bars: any = div.querySelector('.bars')
      // cache node refs (kills the per-frame getElementById chains in drawRank) + last-rendered values
      t._rk = {
        div,
        pos: div.querySelector('.pos'),
        bars,
        ba: bars.children[0],
        bd: bars.children[1],
        bf: bars.children[2],
        sc: div.querySelector('.sc'),
        lastSc: '',
        lastPos: -1,
      }
    })
    rankInit = true
    rankEl.style.display = 'flex'
    rankEl.style.flexDirection = 'column'
  }
  function drawRank() {
    if (!rankInit) rebuildRank()
    const sorted = [...TEAMS].sort((a, b) => {
      if (rankMode === 'ad') {
        return (
          validRank(shownOr(a, 'shownRank', 'officialRank')) - validRank(shownOr(b, 'shownRank', 'officialRank')) ||
          stableTeamOrder(a, b)
        )
      }
      if (rankMode === 'koth') {
        return validRank(a.kothRank) - validRank(b.kothRank) || dispScore(b) - dispScore(a) || stableTeamOrder(a, b)
      }
      return dispScore(b) - dispScore(a) || stableTeamOrder(a, b)
    })
    const orderChanged = sorted.some((team, index) => rankEl.children[index] !== team._rk?.div)
    const focused = root.activeElement as HTMLElement | null
    sorted.forEach((t, i) => {
      const r = t._rk
      if (!r) return
      // only touch position/class when the rank actually moved
      if (r.lastPos !== i) {
        r.lastPos = i
        r.div.className = 'rk p' + (i + 1) + (t.id === selectedTeamId ? ' selected' : '')
        r.div.style.order = String(i)
        r.pos.textContent = (i + 1 < 10 ? '0' : '') + (i + 1)
      }
      let sc: string
      if (rankMode === 'ad') {
        r.bars.style.display = ''
        const offense = boundedRate(dispOffense(t)),
          defense = boundedRate(dispDefense(t)),
          sla = boundedRate(dispSla(t))
        r.ba.style.flex = String(Math.max(offense * 100, 1))
        r.ba.title = `Offense ${(offense * 100).toFixed(1)}%`
        r.bd.style.flex = String(Math.max(defense * 100, 1))
        r.bd.title = `Defense ${(defense * 100).toFixed(1)}%`
        r.bf.style.background = '#ffc637'
        r.bf.style.flex = String(Math.max(sla * 100, 1))
        r.bf.title = `SLA ${(sla * 100).toFixed(1)}%`
        sc = `${fmtAdScore(dispScore(t))}<small>LIVE ${fmtAdScore(dispProjected(t))} · FEED ${dispCaptures(t) || 0} CAP</small>`
      } else if (rankMode === 'koth') {
        r.bars.style.display = 'none'
        const held = HILLS.filter((h) => h.owner && h.owner.id === t.id).length
        sc = `${dispScore(t)}<small>${held} hill${held === 1 ? '' : 's'}</small>`
      } else {
        r.bars.style.display = 'none'
        sc = `${dispScore(t)}<small>${t.jpSolved || 0} solved</small>`
      }
      // only re-parse the score cell HTML when its rendered string changed
      if (r.lastSc !== sc) {
        r.lastSc = sc
        r.sc.innerHTML = sc
      }
    })
    // Keep keyboard/screen-reader order aligned with the visual ranking after live updates.
    if (orderChanged) {
      rankEl.append(...sorted.map((team) => team._rk.div))
      if (focused && rankEl.contains(focused)) focused.focus({ preventScroll: true })
    }
    updateSelection()
  }
  const renderAllScores = () => TEAMS.forEach(renderScore)

  /* -------- scoreboard freeze + match winner -------- */
  const secsLeft = () => (gameEndMs != null ? Math.max(0, Math.round((gameEndMs - Date.now()) / 1000)) : 0)
  function enterFreeze() {
    if (frozen) return
    frozen = true
    TEAMS.forEach((t) => {
      t.shown = t.score
      t.shownProjected = t.projectedScore
      t.shownRank = t.officialRank
      t.shownOffense = t.offenseRate
      t.shownDefense = t.defenseRate
      t.shownSla = t.slaRate
      t.shownCaptures = t.captureEvidence
    })
    const tag = $('freezeTag')
    if (tag) tag.classList.add('show')
    const rp = root.querySelector('.panel.rank')
    if (rp) rp.classList.add('frozen')
    const fb = $('freezeBtn')
    if (fb) {
      fb.classList.add('on')
      fb.setAttribute('aria-pressed', 'true')
    }
    addLog('FREEZE', 'sys', `<span class="em">SCOREBOARD FROZEN</span> :: public board locked, map redacted`)
    const ov = $('fzOverlay')
    if (ov) {
      ov.classList.remove('show')
      void ov.offsetWidth
      ov.classList.add('show')
      if (motionEnabled) fzRenderer.start()
    }
    const fc = $('fzCount')
    if (fc) fc.textContent = 'RESULTS IN T- ' + fmtMS(secsLeft())
    snd.sfxFreeze()
    refreshRank()
  }
  function unfreeze() {
    if (!frozen) return
    frozen = false
    snd.sfxUnfreeze()
    const tag = $('freezeTag')
    if (tag) tag.classList.remove('show')
    const rp = root.querySelector('.panel.rank')
    if (rp) rp.classList.remove('frozen')
    const fb = $('freezeBtn')
    if (fb) {
      fb.classList.remove('on')
      fb.setAttribute('aria-pressed', 'false')
    }
    const ov = $('fzOverlay')
    if (ov) ov.classList.remove('show')
    fzRenderer.stop()
    HILLS.forEach((h) => renderHill(h))
    renderAllScores()
    refreshRank()
  }
  async function endMatch() {
    if (matchOver || endingMatch) return
    if (!preview && Date.now() < nextEndCheckMs) return
    nextEndCheckMs = Date.now() + 5000
    endingMatch = true
    if (preview) {
      updatePreviewScores(true)
    } else {
      try {
        const finalBoard = await fetchJSONWithTimeout<AdScoreboardModel>(liveRoutes.adScoreboard)
        if (killed) return
        // Recheck a possible organizer extension before committing the podium.
        try {
          const game: any = await fetchJSONWithTimeout(liveRoutes.game)
          if (game?.end) gameEndMs = new Date(game.end).getTime()
        } catch {}
        if (gameEndMs != null && Date.now() < gameEndMs - 1500) return
        const pureKoth = SERVICES.length === 0 && HILLS.length > 0
        if (pureKoth) {
          const finalKoth: any = await fetchJSONWithTimeout(liveRoutes.kothScoreboard)
          if (!finalKoth?.fullySettled) {
            const settling = $('fzCount')
            if (settling) settling.textContent = 'FINAL EPOCH SETTLING'
            return
          }
          applyOfficialKothBoard(finalKoth)
        } else if (!finalBoard.fullySettled) {
          const settling = $('fzCount')
          if (settling) settling.textContent = 'FINAL EPOCH SETTLING'
          return
        } else if (finalBoard.started) {
          applyOfficialAdBoard(finalBoard)
        } else {
          const finalKoth: any = await fetchJSONWithTimeout(liveRoutes.kothScoreboard)
          const generatedAt = new Date(finalKoth?.generatedAt).getTime()
          if (!Number.isFinite(generatedAt) || generatedAt + 1000 < finalBoard.generatedAt) return
          applyOfficialKothBoard(finalKoth)
        }
      } catch {
        return // fail closed; tickClock retries and no stale podium is rendered
      } finally {
        endingMatch = false
      }
    }
    if (killed) {
      endingMatch = false
      return
    }
    const finalists = preview ? TEAMS : TEAMS.filter((t) => t.onOfficialBoard)
    const sorted = [...finalists].sort(
      (a, b) => validRank(a.officialRank) - validRank(b.officialRank) || stableTeamOrder(a, b)
    )
    const champ = sorted[0]
    if (!champ) {
      endingMatch = false
      return
    }
    endingMatch = false
    matchOver = true
    unfreeze()
    // PODIUM SPOTLIGHT: top 3 on tiered pedestals (1st centre/crowned/gold, 2nd left, 3rd right).
    // DOM order is 1·2·3; CSS `order` lays them out as 2·1·3 with the gold step tallest.
    const podium = $('podium')
    const title = $('winTitle')
    if (title) title.textContent = 'CHAMPIONS'
    const top = [sorted[0], sorted[1], sorted[2]],
      cls = ['p1', 'p2', 'p3'],
      lbl = ['01', '02', '03']
    podium.innerHTML = top
      .map((team: any, i: number) =>
        team
          ? `<div class="pod ${cls[i]}">${i === 0 ? `<div class="pcrown">${JEWEL_CROWN}</div>` : ''}` +
            `<div class="pav">${avatar(team.look, team.color)}</div>` +
            `<div class="pn">${esc(team.name)}</div>` +
            `<div class="ps">${fmtAdScore(team.score)}</div><div class="ped"><span class="rk">${lbl[i]}</span></div></div>`
          : ''
      )
      .join('')
    const ov = $('winOverlay')
    if (ov) ov.classList.add('show')
    if (motionEnabled) winRenderer.start()
    if (preview) {
      const rb = $('rematchBtn')
      if (rb) rb.style.display = ''
    }
    snd.sfxVictory()
    addLog(
      'MATCH',
      'sys',
      `<span class="em">MATCH OVER</span> :: <span class="who">${esc(champ.name)}</span> wins with official <span class="em">${fmtAdScore(champ.score)}</span>`
    )
  }
  // Live-only: an admin extending the game's EndTimeUtc past now after the podium showed
  // (a supported workflow) must resume the arena, not leave the champions screen stuck up.
  // Unlike resetMatch this preserves all scores/state — the match simply continues.
  function reopenMatch() {
    if (!matchOver) return
    matchOver = false
    endingMatch = false
    nextEndCheckMs = 0
    const ov = $('winOverlay')
    if (ov) ov.classList.remove('show')
    winRenderer.stop()
    addLog('MATCH', 'sys', `<span class="em">MATCH RESUMED</span> :: end time extended`)
  }
  function resetMatch() {
    matchOver = false
    endingMatch = false
    nextEndCheckMs = 0
    round = 1
    tickLeft = 30
    kothDir.reset()
    gameEndMs = Date.now() + MATCH_SECONDS * 1000
    TEAMS.forEach((t) => {
      t.score = 0
      t.projectedScore = 0
      t.officialRank = t.idx + 1
      t.offenseRate = 0
      t.defenseRate = rng(0.2, 0.95)
      t.slaRate = rng(0.88, 1)
      t.shown = t.shownProjected = t.shownRank = t.shownSla = t.shownOffense = t.shownDefense = t.shownCaptures = null
      t.captureEvidence = 0
      t.svc.forEach((s: any) => {
        s.status = Math.random() < 0.85 ? 'def' : 'vuln'
      })
      renderSvc(t)
    })
    updatePreviewScores(true)
    HILLS.forEach((h) => {
      h.owner = null
      renderHill(h)
    })
    totalFlags = 0
    renderAllScores()
    refreshRank()
    const ov = $('winOverlay')
    if (ov) ov.classList.remove('show')
    winRenderer.stop()
    addLog('SYS', 'sys', `<span class="em">REMATCH</span> :: arena reset`)
  }
  // Coalesce the heavy DOM rebuilds: events just mark dirty (refreshRank), and the rAF loop
  // flushes drawRank/log-scroll at most once per frame instead of rebuilding on every event.
  function refreshRank() {
    rankDirty = true
  }
  function updatePreviewScores(settle: boolean) {
    if (!preview) return
    TEAMS.forEach((t) => {
      const core =
        0.4 * boundedRate(t.offenseRate) +
        0.4 * boundedRate(t.defenseRate) +
        0.2 * Math.sqrt(boundedRate(t.offenseRate) * boundedRate(t.defenseRate))
      t.projectedScore = 100 * boundedRate(t.slaRate) * core
      if (settle) t.score = t.projectedScore
    })
    const ranked = [...TEAMS].sort((a, b) => b.score - a.score)
    ranked.forEach((t, i) => {
      t.officialRank = i + 1
    })
  }

  /* -------- loop / clock -------- */
  let lastTs = performance.now()
  function loop(ts: number) {
    if (killed) return
    const dt = Math.min((ts - lastTs) / 1000, 0.05)
    lastTs = ts
    // draw only when there's something to draw: skip while the slam overlay covers the
    // board, while the tab is hidden, and while frozen with no active FX (idle freeze).
    const fxActive = shots.length || sparks.length || fxq.length
    if (!slamCovering && !matchOver) jeop.tick(ts, dt)
    // !matchOver: after endMatch the opaque PODIUM win overlay (z97) covers the whole arena and
    // winRenderer runs its own loop on top — skip the ambient/jeop draw beneath it to save GPU.
    if (motionEnabled && !slamCovering && !matchOver && !document.hidden && (fxActive || !frozen)) {
      drawFX(dt) // advances FX physics + ambient; draws the 2D fallback only while !fxRenderer.ready
      if (fxRenderer.ready) fxRenderer.tick(dt, shots, sparks, fxq) // WebGL render of the same arrays
    }
    // a first-crown owed but deferred past a running cinematic — fire it once free. Also hold
    // through a freeze (the FB overlay z95 sits under the frost z96 — it would play invisibly)
    // and after match end (the podium is up; never fire a cinematic under it).
    const pc = kothDir.takePendingCrown(cinema || frozen || matchOver)
    if (pc) {
      const ph = HILLS.find((x) => x.id === pc.hill)
      const po = TEAMS.find((t) => t.id === pc.owner)
      if (ph && po) fbKoth(po, ph, () => {})
    }
    if (rankDirty) {
      rankDirty = false
      drawRank()
    }
    if (logDirty) {
      logDirty = false
      logEl.scrollTop = logEl.scrollHeight
    }
    raf = requestAnimationFrame(loop)
  }
  // Preview event generator runs on a self-rescheduling setTimeout (NOT the rAF loop):
  // background tabs pause requestAnimationFrame, which would silence the simulated
  // battle; setTimeout keeps firing (throttled to ~1s) so the SFX still play out of tab.
  let evTimer = 0
  function scheduleEvent() {
    if (killed || !preview) return
    if (!cinema && TEAMS.length) {
      const r = Math.random()
      if (r < 0.3) evFlag()
      else if (r < 0.46) evJeopardy()
      else if (r < 0.58) evMiss()
      else if (r < 0.7) evDef()
      else if (r < 0.8) evSla()
      else if (r < 0.9) evHill()
      else evPatch()
    }
    evTimer = window.setTimeout(scheduleEvent, rng(900, 1700) / Math.max(speed, 1))
  }
  // Upper-right pills follow the selected official scoring board. Pure KotH
  // events always use the KotH clock, even before the viewer selects a tab.
  const activeEpochClock = () => {
    const pureKoth = SERVICES.length === 0 && HILLS.length > 0
    const useKoth = !preview && kothEpochTicks > 0 && (rankMode === 'koth' || pureKoth)
    return useKoth
      ? { round: kothRound, startRound: kothStartRound, epochTicks: kothEpochTicks, endsAt: kothRoundEndsAt }
      : { round, startRound: adStartRound, epochTicks: adEpochTicks, endsAt: liveRoundEndsAt }
  }
  const setTickPill = () => {
    const clock = activeEpochClock()
    const progress = epochProgress(clock.round, clock.startRound, clock.epochTicks)
    const rp = $('roundPill')
    if (rp) {
      rp.textContent = progress
        ? `R${Math.max(clock.round, 0)} · E${progress.epoch} ${progress.tick}/${progress.totalTicks}`
        : 'TICK ' + Math.max(clock.round, 0)
      rp.title = progress
        ? `Round ${Math.max(clock.round, 0)} · Epoch ${progress.epoch} · Tick ${progress.tick}/${progress.totalTicks}`
        : `Round ${Math.max(clock.round, 0)}`
    }
    const cp = $('countPill')
    if (cp) cp.textContent = fmtMS(Math.max(tickLeft, 0))
  }

  function applyAdRoundClock(ad: AdScoreboardModel) {
    round = ad.latestRound
    liveRoundEndsAt = ad.currentRoundEndsAt ? new Date(ad.currentRoundEndsAt).getTime() : liveRoundEndsAt
    adEpochTicks = ad.epochTicks
    adStartRound = ad.startRound
    setTickPill()
  }

  function applyKothRoundClock(koth: any) {
    if (!koth) return
    kothRound = Number.isInteger(koth.latestRound) ? koth.latestRound : 0
    kothRoundEndsAt = koth.currentRoundEndsAt ? new Date(koth.currentRoundEndsAt).getTime() : null
    kothEpochTicks = Number.isInteger(koth.epochTicks) && koth.epochTicks > 0 ? koth.epochTicks : 0
    kothStartRound = Number.isInteger(koth.startRound) && koth.startRound > 0 ? koth.startRound : null
    setTickPill()
  }

  function tickClock() {
    tNow = Date.now()
    // match countdown to game end (live: real EndTimeUtc; preview: boot + MATCH_SECONDS)
    if (gameEndMs != null && !matchOver) {
      const left = secsLeft()
      if (preview && left <= FREEZE_SECONDS && !frozen) enterFreeze() // live freeze comes from the board's isFrozenView
      if (frozen) {
        const fc = $('fzCount')
        if (fc) fc.textContent = 'RESULTS IN T- ' + fmtMS(left)
      }
      if (left <= 0) {
        void endMatch()
        return
      }
    }
    if (preview) {
      tickLeft--
      if (tickLeft <= 0) {
        round++
        tickLeft = 30
        updatePreviewScores(round % 4 === 1)
        HILLS.forEach((h) => {
          if (h.owner) h.owner.kothScore = (h.owner.kothScore || 0) + Math.floor(rng(10, 20))
        })
        snd.sfxRound()
        addLog('ROUND', 'sys', `<span class="em">TICK ${round} START</span> :: epoch projection refreshed`)
        TEAMS.forEach(renderScore)
        refreshRank()
      }
      setTickPill()
      return
    }
    const activeRoundEndsAt = activeEpochClock().endsAt
    if (activeRoundEndsAt) {
      tickLeft = Math.max(0, Math.round((activeRoundEndsAt - Date.now()) / 1000))
      setTickPill()
    }
  }

  /* -------- live data -------- */
  async function fetchJSON<T = any>(url: string, signal?: AbortSignal): Promise<T> {
    const r = await eventVpnFetch(url, { headers: { Accept: 'application/json' }, signal })
    if (!r.ok) {
      throw new ArenaHttpError(url + ' -> ' + r.status, r.status, parseArenaRetryAfter(r.headers.get('Retry-After')))
    }
    return r.json()
  }

  async function fetchJSONWithTimeout<T = any>(url: string): Promise<T> {
    const controller = new AbortController()
    liveRequestControllers.add(controller)
    const timeout = window.setTimeout(() => controller.abort(), 8_000)
    try {
      return await fetchJSON<T>(url, controller.signal)
    } finally {
      clearTimeout(timeout)
      liveRequestControllers.delete(controller)
    }
  }

  // Jeopardy categories for the constellation overlay: every challenge on the standard
  // scoreboard that is NOT an A&D service or KotH hill, grouped by category, with the
  // live (dynamic) point value and the blood solvers (gold/silver/bronze, ordered).
  const CATEGORY_COLOR: any = {
    Misc: '#46e3a0',
    Crypto: '#ffc637',
    Pwn: '#ff4d6a',
    Web: '#34e3ff',
    Reverse: '#a06bff',
    Blockchain: '#ff8c42',
    Forensics: '#ff5bd0',
    Hardware: '#8bd450',
    Mobile: '#5b8cff',
    PPC: '#ff6f91',
    AI: '#2ee6c0',
    Pentest: '#e0b24a',
    OSINT: '#b07bff',
  }
  function buildJeopCats(ad: AdScoreboardModel, jp: any): JeopCategory[] {
    const adIds = new Set(((ad && ad.challenges) || []).map((c: any) => c.challengeId))
    const ch = (jp && jp.challenges) || {}
    const out: JeopCategory[] = []
    Object.keys(ch).forEach((catName) => {
      // Jeopardy stars only: drop A&D + KotH challenges. The jeopardy scoreboard
      // payload carries every enabled challenge, so without the type filter KotH
      // hills (which aren't in the A&D-board adIds set) leak in as jeopardy stars.
      const list = (ch[catName] || []).filter(
        (c: any) => !adIds.has(c.id) && c.type !== 'AttackDefense' && c.type !== 'KingOfTheHill'
      )
      if (!list.length) return
      out.push({
        id: catName,
        name: catName.toUpperCase(),
        color: CATEGORY_COLOR[catName] || '#7fd7ff',
        challenges: list.map((c: any) => ({
          id: c.id,
          name: c.title,
          base: Math.round(c.score || 0),
          solveCount: c.solved || 0,
          solvers: acceptedTerritorySolvers(
            c.id,
            jp?.items || [],
            c.bloods || [],
            (name) => teamByName(name)?.color || '#7fd7ff'
          ),
        })),
      })
    })
    return out
  }
  // Fold the KotH per-team totals (koth.teams, by participationId) and the standard
  // jeopardy scoreboard (jp.items, by team name) onto the arena teams, for the two
  // non-A&D ranking modes. A&D score stays t.score from the A&D board.
  function applyAuxScores(koth: any, jp: any) {
    const kById: any = {}
    ;((koth && koth.teams) || []).forEach((r: any) => {
      kById['p' + r.participationId] = r
    })
    const jByName: any = {}
    ;((jp && jp.items) || []).forEach((r: any) => {
      jByName[r.name] = r
    })
    TEAMS.forEach((t) => {
      const k = kById[t.id]
      if (k) {
        t.kothScore = Math.round(k.settledTotal || 0)
        t.kothRank = Number.isInteger(k.rank) && k.rank > 0 ? k.rank : null
      }
      const j = jByName[t.name]
      if (j) {
        t.jpScore = Math.round(j.score || 0)
        t.jpSolved = j.solvedCount || 0
      }
    })
  }
  function buildLiveModel(ad: AdScoreboardModel, koth: any, jp: any, title: string | null) {
    const previousTeams = new Map(TEAMS.map((team) => [team.id, team]))
    const kothHills = koth && koth.hills ? koth.hills : []
    const kothIds = new Set(kothHills.map((h: any) => h.challengeId))
    const svcDefs = (ad.challenges || []).filter((c: any) => !kothIds.has(c.challengeId))
    SERVICES = svcDefs.map((c: any) => c.title)
    const svcIds = svcDefs.map((c: any) => c.challengeId)

    const adRows = ad.teams || []
    const rosterRows = mergeArenaRoster(adRows, (koth && koth.teams) || [], (jp && jp.items) || [])

    TEAMS = rosterRows.map((seed, i: number) => {
      const row = seed.ad || seed.koth || seed.jeopardy
      const previous = previousTeams.get(seed.key)
      const color = PALETTE[i % PALETTE.length]
      const t: any = {
        id: seed.key,
        pid: seed.participationId,
        name: seed.teamName,
        color: previous?.color || color,
        hue: Math.round((i * 137.508) % 360),
        score: Number(seed.ad?.settledTotal) || 0,
        projectedScore: Number(seed.ad?.projectedTotal) || 0,
        officialRank: row.rank || i + 1,
        offenseRate: boundedRate(seed.ad?.offenseRate),
        defenseRate: boundedRate(seed.ad?.defenseRate),
        slaRate: boundedRate(seed.ad?.slaRate),
        captureEvidence: previous?.captureEvidence || 0,
        onOfficialBoard: Boolean(seed.ad || seed.koth || seed.jeopardy),
        kothScore: Math.round(seed.koth?.settledTotal || 0),
        kothRank: Number.isInteger(seed.koth?.rank) && seed.koth.rank > 0 ? seed.koth.rank : null,
        jpScore: Math.round(seed.jeopardy?.score || 0),
        jpSolved: seed.jeopardy?.solvedCount || 0,
      }
      // The official epoch board deliberately exposes no per-team service verdicts.
      // Keep challenge nodes neutral; the rank bars carry the official A/D/SLA rates.
      t.svc = svcIds.map((cid: any, j: number) => ({ name: SERVICES[j], cid, status: 'none' }))
      return t
    })

    TEAMS.forEach((t, i) => {
      t.idx = i
      const point = arenaTeamPosition(i, TEAMS.length)
      t.ang = point.angle
      t.x = point.x
      t.y = point.y
      t.look = makeLook(t, i)
    })

    HILLS = kothHills.map((h: any) => ({
      id: 'h' + h.challengeId,
      cid: h.challengeId,
      name: h.title,
      jp: '',
      status: statusFromCheck(h.lastCheckStatus),
      owner: h.currentHolderTeamName ? teamByName(h.currentHolderTeamName) || null : null,
    }))
    // seed the director so a hill already held when the viewer arrives is NOT mistaken
    // for a fresh capture on the first poll/WS frame (no spurious FIRST CROWN).
    HILLS.forEach((h) => kothDir.seed(h.id, h.owner ? h.owner.id : null))
    HILLS.forEach((h, i) => {
      const ang = ((-90 + (i + 0.5) * (360 / HILLS.length)) * Math.PI) / 180
      h.idx = i
      h.ang = ang
      h.x = CX + HILLR * Math.cos(ang)
      h.y = CY + HILLR * Math.sin(ang)
    })

    if (!preview && !rankModeChosen) {
      rankMode = initialArenaRanking(SERVICES.length, HILLS.length)
      const tabs: any = $('rankTabs')
      if (tabs)
        tabs.querySelectorAll('button').forEach((button: any) => {
          const selected = button.getAttribute('data-rm') === rankMode
          button.classList.toggle('on', selected)
          button.setAttribute('aria-pressed', String(selected))
        })
    }
    applyAuxScores(koth, jp)
    applyKothRoundClock(koth)
    totalFlags = Math.max(0, Number(ad.evidence?.acceptedCaptures) || 0)
    jeop.setData(buildJeopCats(ad, jp))
    applyAdRoundClock(ad)
    if (title) $('brandLogo').textContent = title
  }

  function applyOfficialAdBoard(ad: AdScoreboardModel) {
    const adById = new Map(ad.teams.map((row) => ['p' + row.participationId, row] as const))
    TEAMS.forEach((t) => {
      const row = adById.get(t.id)
      if (!row) return
      t.onOfficialBoard = true
      t.name = row.teamName
      t.score = Number(row.settledTotal) || 0
      t.projectedScore = Number(row.projectedTotal) || 0
      t.officialRank = row.rank || t.officialRank
      t.offenseRate = boundedRate(row.offenseRate)
      t.defenseRate = boundedRate(row.defenseRate)
      t.slaRate = boundedRate(row.slaRate)
      renderScore(t)
    })
    totalFlags = Math.max(0, Number(ad.evidence?.acceptedCaptures) || totalFlags)
    applyAdRoundClock(ad)
    refreshRank()
  }

  function applyOfficialKothBoard(koth: any) {
    const kothById = new Map(((koth && koth.teams) || []).map((row: any) => ['p' + row.participationId, row]))
    TEAMS.forEach((team) => {
      const row: any = kothById.get(team.id)
      if (!row) return
      team.onOfficialBoard = true
      team.name = row.teamName
      team.score = Number(row.settledTotal) || 0
      team.kothRank = Number.isInteger(row.rank) && row.rank > 0 ? row.rank : null
      team.officialRank = team.kothRank || team.officialRank
      team.kothScore = team.score
      renderScore(team)
    })
    applyKothRoundClock(koth)
    refreshRank()
  }

  function snapshotSignature(ad: AdScoreboardModel, koth: any, jp: any) {
    const roster = mergeArenaRoster(ad.teams || [], koth?.teams || [], jp?.items || [])
    return JSON.stringify({
      teams: roster.map((team) => `${team.key}:${team.teamName}`),
      services: (ad.challenges || []).map((challenge: any) => challenge.challengeId).sort((a, b) => a - b),
      hills: (koth?.hills || []).map((hill: any) => hill.challengeId).sort((a: number, b: number) => a - b),
    })
  }

  function reconcileLiveModel(ad: AdScoreboardModel, koth: any, jp: any, title: string | null) {
    const signature = snapshotSignature(ad, koth, jp)
    if (signature !== liveModelSignature) {
      buildLiveModel(ad, koth, jp, title)
      liveModelSignature = signature
      if (!TEAMS.length) {
        showNote('WAITING FOR THE OFFICIAL EVENT ROSTER')
        return
      }
      clearNote()
      buildArena()
      refreshRank()
      sizeCanvas()
    }
    applyLivePoll(ad, koth, jp)
    if (title) $('brandLogo').textContent = title
  }

  function applyLivePoll(ad: AdScoreboardModel, koth: any, jp: any) {
    applyAuxScores(koth, jp)
    applyKothRoundClock(koth)
    jeop.setData(buildJeopCats(ad, jp))
    applyOfficialAdBoard(ad)
    const kothHills = koth && koth.hills ? koth.hills : []
    kothHills.forEach((kh: any) => {
      const h = HILLS.find((x) => x.cid === kh.challengeId)
      if (!h) return
      const newOwner = kh.currentHolderTeamName ? teamByName(kh.currentHolderTeamName) || null : null
      const ns = statusFromCheck(kh.lastCheckStatus)
      // backstop: if the WS koth frame was missed, the director fires the capture/crown
      // here so the FIRST CROWN cinematic still plays instead of the holder silently
      // appearing. Deduped against the WS frame by the director's owner ledger.
      // cinema||frozen blocks the FIRST CROWN: during a freeze it's deferred (loop's
      // takePendingCrown holds it on the same || frozen ||) and plays once the freeze lifts.
      const res = kothDir.applyCapture(h.id, newOwner ? newOwner.id : null, cinema || frozen)
      if (!res.changed && h.status === ns) return
      if (res.changed) h.owner = newOwner
      h.status = ns
      renderHill(h)
      if (res.changed && !matchOver) onHillCapture(h, newOwner, res)
    })
    // public ICPC freeze drives the lock screen. !matchOver on BOTH branches: after endMatch
    // the board often stays isFrozenView until organizers unfreeze — re-entering freeze here
    // would start a permanent fzRenderer loop hidden under the win overlay (z96 < z97).
    if (ad.isFrozenView && !frozen && !matchOver) enterFreeze()
    else if (!ad.isFrozenView && frozen && !matchOver) unfreeze()
  }

  const liveReadsAllowed = () => document.visibilityState !== 'hidden' && navigator.onLine !== false

  function scheduleLivePoll(delay: number) {
    clearTimeout(livePollTimer)
    livePollTimer = 0
    if (killed || preview || !liveReadsAllowed()) return
    livePollTimer = window.setTimeout(() => void pollLive(), delay)
  }

  async function pollLive() {
    if (killed || livePollController || !liveReadsAllowed()) return
    const controller = new AbortController()
    livePollController = controller
    const timeout = window.setTimeout(() => controller.abort(), 8_000)
    let nextDelay = arenaPollDelay(livePollFailures, null)
    try {
      const ad = await fetchJSON<AdScoreboardModel>(liveRoutes.adScoreboard, controller.signal)
      const [koth, jp, gi] = await Promise.all([
        fetchJSON(liveRoutes.kothScoreboard, controller.signal).catch(() => null),
        fetchJSON(liveRoutes.scoreboard, controller.signal).catch(() => null),
        fetchJSON(liveRoutes.game, controller.signal).catch(() => null),
      ])
      // refresh the real end time: an admin extending EndTimeUtc mid-match must move the podium
      // trigger (and un-stick it if the champions screen already showed) — gameEndMs was otherwise
      // read once at load and never updated. Keep the old value if the field is missing.
      if (gi?.end) gameEndMs = new Date(gi.end).getTime()
      if (matchOver && gameEndMs != null && Date.now() < gameEndMs - 1500) reopenMatch()
      if (!killed && !controller.signal.aborted) {
        reconcileLiveModel(ad, koth, jp, gi?.title || null)
        if (!livePollStarted) livePollStarted = true
        connectWS()
      }
      livePollFailures = 0
      nextDelay = arenaPollDelay(0, null)
    } catch (error) {
      if (!controller.signal.aborted && !killed) {
        livePollFailures += 1
        nextDelay = arenaPollDelay(livePollFailures, error instanceof ArenaHttpError ? error.retryAfterMs : null)
      }
    } finally {
      clearTimeout(timeout)
      if (livePollController === controller) livePollController = null
      scheduleLivePoll(nextDelay)
    }
  }

  function liveAttack(f: any) {
    const atkr = teamByName(f.teamName)
    if (!atkr) return
    const vic = f.victimTeamName ? teamByName(f.victimTeamName) : null
    // Rejected flag (wrong answer): a soft "MISS" tracer (jeopardy aims at the CORE),
    // no score, no impact, not logged. Capped so a flag-spam burst can't flood.
    if (f.type === 'Unaccepted') {
      if (!frozen && shots.filter((s: any) => s.miss).length < 6) {
        fireShot(atkr, vic || { x: CX, y: CY }, MISS_COL, true)
        snd.sfxMiss()
      }
      return
    }
    let svc: any = null
    if (vic) svc = vic.svc.find((s: any) => s.name === f.challengeTitle) || pick(vic.svc)
    let pts = 0
    // A&D scores are epoch aggregates and never move directly from a feed event.
    // Jeopardy may include its authoritative post-solve team score; otherwise the
    // event is still shown without inventing a point value.
    if (!vic && f.teamScore != null) pts = Math.max(0, Math.round(f.teamScore) - (atkr.jpScore || 0))
    const isFB = f.type === 'FirstBlood'
    if (isFB) {
      if (!vic) jeop.solveByTitle(atkr.x, atkr.y, f.challengeTitle || '', { name: atkr.name, color: atkr.color })
      if (cinema) {
        resolveFlag(atkr, vic, svc, pts, true)
        return
      }
      if (vic)
        fbAd(atkr, vic, f.challengeTitle || (svc && svc.name) || 'a challenge', () =>
          resolveFlag(atkr, vic, svc, pts, true)
        )
      else fbJeopardy(atkr, f.challengeTitle || 'a challenge', () => resolveFlag(atkr, null, null, pts, true))
      return
    }
    // normal solve — A&D shoots the victim; a jeopardy solve lasers the actual
    // challenge star in the constellation (falls back to the CORE if not mapped).
    // Burst overflow (e.g. a 256-deep WS catch-up): resolve quietly & synchronously —
    // capture evidence stays live, but skip tracer/audio/timers so we don't flood.
    if (pendingResolves > 12) {
      resolveFlag(atkr, vic, svc, pts, false, true)
      return
    }
    if (vic) fireShot(atkr, vic, atkr.color)
    else {
      if (!jeop.solveByTitle(atkr.x, atkr.y, f.challengeTitle || '', { name: atkr.name, color: atkr.color }))
        fireShot(atkr, { x: CX, y: CY }, atkr.color)
      if (!frozen) snd.sfxSolve()
    }
    pendingResolves++
    ownedTimeout(
      () => {
        pendingResolves--
        if (!killed) resolveFlag(atkr, vic, svc, pts, false)
      },
      320 / Math.max(speed, 1) + 120
    )
  }
  // Hill ownership is driven by BOTH the WS koth frame (instant) and the completion-scheduled poll
  // (reliable backstop) — whichever sees the change first; the other dedups via the
  // director's owner ledger (kothCapture.ts). The FIRST CROWN cinematic fires once;
  // if an A&D cinematic is mid-play it's deferred and fired from loop().
  // Render the FX implied by a director CaptureResult (caller has already updated
  // h.owner). 'crown' plays the cinematic now; 'defer' lets loop() fire it once the
  // running cinematic clears; 'capture' is a normal seize; 'neutral' just logs.
  function onHillCapture(h: any, newOwner: any, res: CaptureResult) {
    // While frozen, suppress the audio (and the crown is deferred by the director's blocked
    // flag below, so res.kind is never 'crown' here during a freeze) — an audible neutral/
    // capture cue or the FIRST CROWN stinger would leak a scoring event past the public freeze.
    if (res.kind === 'neutral' || !newOwner) {
      if (!frozen) snd.sfxNeutral()
      addLog('HILL', 'hill', `<span class="svc">${esc(h.name)}</span> went <span class="em">NEUTRAL</span>`)
      return
    }
    if (res.kind === 'crown') fbKoth(newOwner, h, () => {})
    else if (res.kind === 'capture') {
      spawnCapture(newOwner, h, newOwner.color)
      if (!frozen) snd.sfxCapture()
    }
    // 'defer' → the crown is owed; loop() fires it when the cinematic clears.
    floatText(h.x, h.y - 30, res.contested ? 'SEIZED' : 'CAPTURED', newOwner.color)
    addLog(
      'HILL',
      'hill',
      `<span class="who">${esc(newOwner.name)}</span> ${res.contested ? 'seized' : 'captured'} <span class="svc">${esc(h.name)}</span>`
    )
  }
  function liveKoth(f: any) {
    const h = HILLS.find((x) => x.cid === f.challengeId)
    if (!h) return
    if (f.status) h.status = statusFromCheck(f.status)
    const newOwner = f.holderTeamName ? teamByName(f.holderTeamName) || null : null
    const res = kothDir.applyCapture(h.id, newOwner ? newOwner.id : null, cinema || frozen)
    if (res.changed) h.owner = newOwner
    renderHill(h)
    if (res.changed && !matchOver) onHillCapture(h, newOwner, res) // same gate as the poll path
  }
  // a team modified their service files — "patched". Cyan hardening pulse on their node.
  function patchEffect(t: any, challengeTitle: string, changeCount: number) {
    if (!t) return
    spawnShield(t.x, t.y, '#27e3ff')
    pulseBase(t, '#27e3ff')
    snd.sfxPatch()
    floatText(t.x, t.y - 66, '🔧 PATCH', '#27e3ff')
    const files = changeCount ? ` <span class="em">(${changeCount} file${changeCount === 1 ? '' : 's'})</span>` : ''
    addLog(
      'PATCH',
      'patch',
      `<span class="who">${esc(t.name)}</span> hardened <span class="svc">${esc(challengeTitle)}</span>${files}`
    )
  }
  function livePatch(f: any) {
    patchEffect(teamByName(f.teamName), f.challengeTitle, f.changeCount || 0)
  }

  function setConnection(label: string, state: string) {
    const badge = $('connectionStatus')
    if (badge.textContent !== label) badge.textContent = label
    badge.dataset.state = state
  }

  function connectWS() {
    if (killed || !liveReadsAllowed() || ws?.readyState === WebSocket.OPEN || ws?.readyState === WebSocket.CONNECTING)
      return
    clearTimeout(reconnectTimer)
    reconnectTimer = 0
    const proto = location.protocol === 'https:' ? 'wss' : 'ws'
    const socket = new WebSocket(`${proto}://${location.host}/hub/attack/ws?game=${gameId}`)
    ws = socket
    socket.onopen = () => {
      wsOpenedAt = Date.now()
      if (!killed && ws === socket) setConnection('Connected', 'connected')
    }
    socket.onmessage = (m) => {
      if (killed || ws !== socket) return
      let f: any
      try {
        f = JSON.parse(m.data)
      } catch {
        return
      }
      if (!f || !f.kind) return
      if (f.kind === 'attack') liveAttack(f)
      else if (f.kind === 'koth') liveKoth(f)
      else if (f.kind === 'patch') livePatch(f)
    }
    socket.onclose = () => {
      if (ws === socket) ws = null
      if (!killed) setConnection(navigator.onLine === false ? 'Offline' : 'Reconnecting', 'connecting')
      if (killed || !liveReadsAllowed()) return
      wsRetry = Date.now() - wsOpenedAt >= 30_000 ? 1 : Math.min(wsRetry + 1, 10)
      reconnectTimer = window.setTimeout(connectWS, arenaReconnectDelay(wsRetry))
    }
    socket.onerror = () => {
      try {
        socket.close()
      } catch {}
    }
  }

  function clearNote() {
    root.querySelectorAll('.arena-note').forEach((note) => note.remove())
  }
  function showNote(msg: string) {
    clearNote()
    const note = document.createElement('div')
    note.className = 'arena-note'
    note.innerHTML = msg
    arena.appendChild(note)
  }

  function ensureLiveLoops() {
    if (!liveClockStarted) {
      liveClockStarted = true
      timers.push(window.setInterval(tickClock, 1000))
    }
    if (!raf) raf = requestAnimationFrame(loop)
  }

  async function start() {
    let ad: AdScoreboardModel
    try {
      ad = await fetchJSONWithTimeout<AdScoreboardModel>(liveRoutes.adScoreboard)
    } catch {
      if (killed) return
      setConnection('Waiting for data', 'connecting')
      showNote('WAITING FOR LIVE EVENT DATA<br/>automatic recovery is active')
      addLog('SYS', 'sys', `<span class="em">LIVE DATA TEMPORARILY UNAVAILABLE</span> :: retrying`)
      tNow = Date.now()
      livePollStarted = true
      livePollFailures = 1
      scheduleLivePoll(arenaPollDelay(livePollFailures, null))
      ensureLiveLoops()
      return
    }
    let koth: any = null
    try {
      koth = await fetchJSONWithTimeout(liveRoutes.kothScoreboard)
    } catch {}
    let jp: any = null
    try {
      jp = await fetchJSONWithTimeout(liveRoutes.scoreboard)
    } catch {}
    let title: string | null = null
    try {
      const gi = await fetchJSONWithTimeout(liveRoutes.game)
      title = gi && gi.title
      if (gi && gi.end) gameEndMs = new Date(gi.end).getTime()
    } catch {}
    if (killed) return

    buildLiveModel(ad, koth, jp, title)
    liveModelSignature = snapshotSignature(ad, koth, jp)
    if (!TEAMS.length) {
      setConnection('Waiting for teams', 'connecting')
      showNote('WAITING FOR THE OFFICIAL EVENT ROSTER')
      tNow = Date.now()
      ensureLiveLoops()
      livePollStarted = true
      scheduleLivePoll(5_000)
      return
    }

    clearNote()
    buildArena()
    refreshRank()
    sizeCanvas()
    if (ad.isFrozenView) enterFreeze() // board already frozen when we connect
    addLog(
      'SYS',
      'sys',
      `<span class="em">ARENA ONLINE</span> :: ${TEAMS.length} teams, ${SERVICES.length} services, ${totalFlags} accepted captures`
    )

    livePollStarted = true
    connectWS()
    scheduleLivePoll(arenaPollDelay(0, null))
    ensureLiveLoops()
  }

  /* -------- preview: simulated battle (no WS / no poll) -------- */
  const DEMO_TEAMS = [
    { id: 'kpanic', name: 'KERNEL-PANIC', color: '#ff4d5e', hue: 354 },
    { id: 'nullb', name: 'NULLBYTE', color: '#27e3ff', hue: 190 },
    { id: 'segf', name: 'SEGFAULT', color: '#ffc637', hue: 44 },
    { id: 'bshock', name: 'BINARY-SHOCK', color: '#ff39a8', hue: 330 },
    { id: 'ronin', name: '0xRONIN', color: '#b9ff42', hue: 80 },
    { id: 'heap', name: 'HEAP-OVERFLOW', color: '#ff7a3a', hue: 20 },
    { id: 'ghost', name: 'GHOST-SHELL', color: '#9d6bff', hue: 262 },
    { id: 'ice', name: 'ICE-BREAKER', color: '#4d8bff', hue: 218 },
  ]
  // ---- procedural demo generators (driven by the preview count knobs) ----
  const SVC_POOL = [
    'neko-db',
    'torii-api',
    'sakura-web',
    'oni-auth',
    'kitsune-cache',
    'ronin-gw',
    'sake-queue',
    'tanuki-fs',
    'koi-mail',
    'yuki-ml',
    'hanabi-rng',
    'shoji-proxy',
  ]
  const JEOP_CAT_NAMES = ['Web', 'Pwn', 'Crypto', 'Reverse', 'Forensics', 'Misc', 'Blockchain', 'Hardware']
  function genDemoServices(n: number) {
    return Array.from({ length: Math.max(0, n) }, (_, i) => SVC_POOL[i] || 'svc-' + String(i + 1).padStart(2, '0'))
  }
  function genDemoHills(n: number) {
    return Array.from({ length: Math.max(0, n) }, (_, i) => ({
      id: 'h' + i,
      name: 'TORII-' + (i < 26 ? String.fromCharCode(65 + i) : 'X' + (i + 1)),
    }))
  }
  function genDemoTeams(n: number) {
    return Array.from(
      { length: Math.max(0, n) },
      (_, i) =>
        DEMO_TEAMS[i] || {
          id: 'demo' + i,
          name: 'TEAM-' + String(i + 1).padStart(2, '0'),
          color: PALETTE[i % PALETTE.length],
          hue: (i * 47) % 360,
        }
    )
  }
  // distribute n jeopardy challenges round-robin across categories (~5 per category)
  function genJeopCats(n: number): JeopCategory[] {
    if (n <= 0) return []
    const nCats = Math.min(JEOP_CAT_NAMES.length, Math.max(1, Math.round(n / 5)))
    const cats: any[] = JEOP_CAT_NAMES.slice(0, nCats).map((c) => ({
      id: c,
      name: c.toUpperCase(),
      color: CATEGORY_COLOR[c] || '#7fd7ff',
      challenges: [],
    }))
    let id = 9000
    for (let i = 0; i < n; i++) {
      const cat = cats[i % cats.length],
        k = cat.challenges.length
      cat.challenges.push({
        id: id++,
        name: cat.id.toLowerCase() + '-' + String(k + 1).padStart(2, '0'),
        base: [100, 150, 200, 300, 400, 500][k % 6],
        solveCount: 0,
        solvers: [],
      })
    }
    return cats.filter((c) => c.challenges.length)
  }
  function bootDemoModel() {
    SERVICES = genDemoServices(cfgAd)
    const teamDefs = genDemoTeams(cfgTeams)
    TEAMS = teamDefs.map((d: any, i: number) => {
      const point = arenaTeamPosition(i, teamDefs.length)
      const t: any = {
        ...d,
        idx: i,
        ang: point.angle,
        x: point.x,
        y: point.y,
        score: 0,
        projectedScore: 0,
        officialRank: i + 1,
        offenseRate: 0,
        defenseRate: rng(0.2, 0.95),
        slaRate: rng(0.88, 1),
        captureEvidence: 0,
        kothScore: Math.floor(rng(40, 220)),
        kothRank: null,
        jpScore: Math.floor(rng(150, 900)),
        jpSolved: Math.floor(rng(2, 14)),
      }
      t.svc = SERVICES.map((s: string) => ({
        name: s,
        status: Math.random() < 0.82 ? 'def' : Math.random() < 0.5 ? 'vuln' : 'down',
      }))
      t.look = makeLook(t, i)
      return t
    })
    updatePreviewScores(true)
    const hillDefs = genDemoHills(cfgKoth)
    HILLS = hillDefs.map((d: any, i: number) => {
      const ang = ((-90 + (i + 0.5) * (360 / Math.max(hillDefs.length, 1))) * Math.PI) / 180
      return { ...d, idx: i, ang, x: CX + HILLR * Math.cos(ang), y: CY + HILLR * Math.sin(ang), owner: null }
    })
    jeop.setData(genJeopCats(cfgJeop))
  }
  // rebuild the whole preview model + arena for the current count knobs
  function rebuildPreview() {
    if (!preview) return
    cinema = false
    matchOver = false
    if (frozen) unfreeze()
    const wo: any = $('winOverlay')
    if (wo) wo.classList.remove('show')
    winRenderer.stop() // symmetric with resetMatch — don't leave confetti drawing to a hidden canvas
    bootDemoModel()
    kothDir.reset()
    liveRoundEndsAt = null
    round = 1
    tickLeft = 30
    gameEndMs = Date.now() + MATCH_SECONDS * 1000
    totalFlags = 0
    buildArena()
    TEAMS.forEach((t) => renderSvc(t))
    rankInit = false
    refreshRank()
    sizeCanvas()
    addLog(
      'SYS',
      'sys',
      `<span class="em">PREVIEW REBUILT</span> :: ${TEAMS.length} teams · ${SERVICES.length} A&amp;D · ${HILLS.length} KotH · ${cfgJeop} jeopardy`
    )
  }
  // Preview flags are always normal hits — the demo no longer auto-plays a
  // first-blood cinematic at the start. Use the FB A&D / FB JEO / FB KOTH buttons
  // to showcase first blood on demand.
  function evFlag() {
    const atkr = pick(TEAMS)
    let vic = pick(TEAMS)
    let g = 0
    while (vic === atkr && g++ < 10) vic = pick(TEAMS)
    if (vic === atkr) return
    const svc = pick(vic.svc.filter((s: any) => s.status !== 'down')) || pick(vic.svc)
    fireShot(atkr, vic, atkr.color)
    ownedTimeout(
      () => {
        if (!killed) resolveFlag(atkr, vic, svc, 0, false)
      },
      380 / speed + 120
    )
  }
  function evDef() {
    const t = pick(TEAMS)
    if (!t || !t.svc.length) return
    const s = t.svc.find((x: any) => x.status === 'vuln') || t.svc.find((x: any) => x.status === 'down') || pick(t.svc)
    s.status = 'def'
    t.defenseRate = Math.min(1, (t.defenseRate || 0) + 0.025)
    renderSvc(t)
    renderScore(t)
    pulseBase(t, SVC_COLOR.def)
    spawnShield(t.x, t.y, SVC_COLOR.def)
    snd.sfxDefend()
    floatText(t.x, t.y - 66, 'PATCHED', SVC_COLOR.def)
    addLog('DEFEND', 'def', `<span class="who">${esc(t.name)}</span> shielded <span class="svc">${esc(s.name)}</span>`)
    refreshRank()
  }
  function evSla() {
    const t = pick(TEAMS)
    if (!t || !t.svc.length) return
    const s = pick(t.svc.filter((x: any) => x.status !== 'down')) || pick(t.svc)
    if (!s) return
    s.status = 'down'
    t.slaRate = Math.max(0.4, (t.slaRate || 0) - rng(0.02, 0.06))
    renderSvc(t)
    renderScore(t)
    spawnDown(t.x, t.y, '#ff3b5b')
    snd.sfxDown()
    restartAnim($('base-' + t.id), 'node-down', 1300)
    floatText(t.x, t.y - 66, '▼ DOWN', '#ff5b6e')
    addLog(
      'SLA',
      'sla',
      `<span class="who">${esc(t.name)}</span> :: <span class="svc">${esc(s.name)}</span> went <span class="em">DOWN</span>`
    )
    ownedTimeout(
      () => {
        if (s.status === 'down') {
          s.status = 'def'
          t.slaRate = Math.min(1, t.slaRate + 0.02)
          renderSvc(t)
          refreshRank()
        }
      },
      rng(4000, 9000)
    )
  }
  function evHill() {
    if (!HILLS.length) return
    const h = pick(HILLS)
    let atkr = pick(TEAMS)
    let g = 0
    while (h.owner === atkr && g++ < 8) atkr = pick(TEAMS)
    const contested = h.owner && h.owner !== atkr
    h.owner = atkr
    renderHill(h)
    spawnCapture(atkr, h, atkr.color)
    snd.sfxCapture()
    atkr.kothScore = (atkr.kothScore || 0) + Math.floor(rng(20, 45))
    floatText(h.x, h.y - 30, contested ? 'SEIZED' : 'CAPTURED', atkr.color)
    addLog(
      'HILL',
      'hill',
      `<span class="who">${esc(atkr.name)}</span> ${contested ? 'seized' : 'captured'} <span class="svc">${esc(h.name)}</span>`
    )
    refreshRank()
  }
  function evPatch() {
    if (!TEAMS.length) return
    const t = pick(TEAMS)
    const svc = pick(t.svc)
    patchEffect(t, svc ? svc.name : SERVICES[0] || 'service', Math.floor(rng(1, 9)))
  }
  // a jeopardy solve — no victim, tracer to the CORE, credits the jeopardy board
  const DEMO_JP = [
    'web-portal',
    'crypto-rng',
    'pwn-heap',
    'rev-vm',
    'forensics-01',
    'misc-jail',
    'osint-2',
    'blockchain-1',
  ]
  function evJeopardy() {
    const atkr = pick(TEAMS)
    if (!atkr) return
    // laser the actual constellation star if one is free; else a CORE tracer
    const hit = jeop.solveRandom(atkr.x, atkr.y, { name: atkr.name, color: atkr.color })
    const ch = hit ? hit.name : pick(DEMO_JP)
    const pts = hit ? hit.base : Math.floor(rng(50, 150))
    if (!hit) fireShot(atkr, { x: CX, y: CY }, atkr.color)
    snd.sfxSolve()
    ownedTimeout(
      () => {
        if (killed) return
        atkr.jpScore = (atkr.jpScore || 0) + pts
        atkr.jpSolved = (atkr.jpSolved || 0) + 1
        floatText(atkr.x, atkr.y - 66, '+' + pts, atkr.color)
        addLog(
          'SOLVE',
          'flag',
          `<span class="who">${esc(atkr.name)}</span> solved <span class="svc">${esc(ch)}</span> <span class="em">+${pts}</span>`
        )
        totalFlags++
        refreshRank()
      },
      380 / speed + 120
    )
  }
  // a rejected flag attempt — soft MISS tracer (jeopardy aims at the CORE, A&D at a rival)
  function evMiss() {
    const atkr = pick(TEAMS)
    if (!atkr) return
    let target: any = { x: CX, y: CY }
    if ((HILLS.length ? Math.random() < 0.5 : Math.random() < 0.7) && TEAMS.length > 1) {
      let v = pick(TEAMS)
      let g = 0
      while (v === atkr && g++ < 8) v = pick(TEAMS)
      if (v !== atkr) target = v
    }
    if (!frozen && shots.filter((s: any) => s.miss).length < 6) {
      fireShot(atkr, target, MISS_COL, true)
      snd.sfxMiss()
    }
  }
  async function startPreview() {
    // Preview is a fully simulated battle driven by the TEAMS / A&D / KOTH / JEOP
    // knobs — it does NOT mirror live game data (use the non-preview view for that),
    // so the on-screen counts always match the inputs from the start.
    let title: string | null = null
    try {
      const gi = await fetchJSONWithTimeout(liveRoutes.game)
      title = gi && gi.title
    } catch {}
    if (killed) return
    bootDemoModel()
    if (title) $('brandLogo').textContent = title
    liveRoundEndsAt = null
    round = 1
    tickLeft = 30
    gameEndMs = Date.now() + MATCH_SECONDS * 1000
    buildArena()
    const fbb: any = $('fbBtns')
    if (fbb) fbb.style.display = ''
    const cfg: any = $('cfgBtns')
    if (cfg) cfg.style.display = ''
    refreshRank()
    sizeCanvas()
    addLog('SYS', 'sys', `<span class="em">PREVIEW MODE</span> :: simulated battle — ${TEAMS.length} teams`)
    timers.push(window.setInterval(tickClock, 1000))
    raf = requestAnimationFrame(loop)
    timers.push(window.setTimeout(() => evFlag(), 1200))
    timers.push(window.setTimeout(() => evJeopardy(), 2600))
    timers.push(window.setTimeout(() => evHill(), 4200))
    timers.push(window.setTimeout(() => evDef(), 4800))
    timers.push(window.setTimeout(() => evHill(), 5400))
    timers.push(window.setTimeout(() => evFlag(), 6000))
    evTimer = window.setTimeout(scheduleEvent, 1800) // recurring generator (survives background tabs)
  }

  /* -------- viewer toggles -------- */
  const speedBtn: any = $('speedBtn')
  if (speedBtn)
    speedBtn.onclick = function () {
      speed = speed === 1 ? 2 : speed === 2 ? 4 : 1
      speedBtn.textContent = 'Speed ' + speed + '×'
      speedBtn.classList.toggle('on', speed !== 1)
      speedBtn.setAttribute('aria-pressed', String(speed !== 1))
    }
  // preview count knobs — clamp + rebuild the demo on change
  const cfgWire: [string, (v: number) => void, number, number][] = [
    ['cfgTeams', (v) => (cfgTeams = v), 2, 20],
    ['cfgAd', (v) => (cfgAd = v), 0, 10],
    ['cfgKoth', (v) => (cfgKoth = v), 0, 12],
    ['cfgJeop', (v) => (cfgJeop = v), 0, 40],
  ]
  cfgWire.forEach(([cid, set, lo, hi]) => {
    const inp: any = $(cid)
    if (!inp) return
    inp.onchange = () => {
      let v = Math.round(+inp.value || 0)
      v = Math.max(lo, Math.min(hi, v))
      inp.value = String(v)
      set(v)
      rebuildPreview()
    }
  })
  // fullscreen the battle map (recompute wheel + constellations for the new size)
  const fsWrap: any = root.querySelector('.arena-wrap')
  const onFsChange = () => {
    const fs = root.fullscreenElement === fsWrap
    const b = $('fsBtn')
    if (b) {
      b.textContent = fs ? '✕' : '⛶'
      b.setAttribute('aria-label', fs ? 'Exit fullscreen globe' : 'Fullscreen globe')
    }
    sizeCanvas()
  }
  const fsBtn: any = $('fsBtn')
  if (fsBtn && fsWrap) {
    fsBtn.onclick = () => {
      if (document.fullscreenElement) document.exitFullscreen?.()
      else (fsWrap.requestFullscreen || fsWrap.webkitRequestFullscreen || (() => {})).call(fsWrap)
    }
    document.addEventListener('fullscreenchange', onFsChange)
  }
  const rankTabs: any = $('rankTabs')
  if (rankTabs)
    rankTabs.querySelectorAll('button').forEach((b: any) => {
      b.onclick = () => {
        rankMode = b.getAttribute('data-rm')
        rankModeChosen = true
        rankTabs.querySelectorAll('button').forEach((x: any) => {
          x.classList.toggle('on', x === b)
          x.setAttribute('aria-pressed', String(x === b))
        })
        setTickPill()
        renderAllScores()
        refreshRank()
      }
    })

  // Browsers suspend Web Audio until the first user gesture. Any arena button
  // also counts as that gesture.
  const primeAudio = () => {
    snd.unlock()
    document.removeEventListener('pointerdown', primeAudio)
    document.removeEventListener('keydown', primeAudio)
  }
  document.addEventListener('pointerdown', primeAudio, { once: true })
  document.addEventListener('keydown', primeAudio, { once: true })

  const soundBtn: any = $('soundBtn')
  if (soundBtn)
    soundBtn.onclick = function () {
      const on = !snd.isEnabled()
      snd.setEnabled(on)
      soundBtn.textContent = on ? 'Sound on' : 'Sound off'
      soundBtn.classList.toggle('on', on)
      soundBtn.setAttribute('aria-pressed', String(on))
      if (on) snd.unlock()
      else {
        stopIncomingSound?.()
        stopIncomingSound = null
        stopFirstBloodSound?.()
        stopFirstBloodSound = null
        if ('speechSynthesis' in window) {
          try {
            speechSynthesis.cancel()
          } catch {}
        }
      }
    }

  // Preview-only: manually trigger each first-blood variant.
  const fbAdBtn: any = $('fbAdBtn')
  if (fbAdBtn)
    fbAdBtn.onclick = () => {
      if (cinema || !TEAMS.length) return
      const a = pick(TEAMS)
      let v = pick(TEAMS)
      let g = 0
      while (v === a && g++ < 8) v = pick(TEAMS)
      const svc = pick(v.svc)
      fbAd(a, v, svc && svc.name ? svc.name : 'a challenge', () => resolveFlag(a, v, svc, 0, true))
    }
  const fbJeoBtn: any = $('fbJeoBtn')
  if (fbJeoBtn)
    fbJeoBtn.onclick = () => {
      if (cinema || !TEAMS.length) return
      const a = pick(TEAMS)
      const ch = pick(SERVICES.length ? SERVICES : ['web-portal', 'crypto-rng', 'pwn-heap', 'rev-vm'])
      const pts = Math.floor(rng(60, 99))
      fbJeopardy(a, ch, () => {
        a.jpScore = (a.jpScore || 0) + pts
        a.jpSolved = (a.jpSolved || 0) + 1
        totalFlags++
        floatText(a.x, a.y - 66, '+' + pts, a.color)
        addLog(
          'FIRST BLOOD',
          'fb',
          `<span class="who">${esc(a.name)}</span> first-blooded <span class="svc">${esc(ch)}</span> <span class="em">+${pts}</span>`
        )
        refreshRank()
      })
    }
  const fbKothBtn: any = $('fbKothBtn')
  if (fbKothBtn)
    fbKothBtn.onclick = () => {
      if (cinema || !TEAMS.length) return
      const a = pick(TEAMS)
      const h = HILLS.length ? pick(HILLS) : null
      const pts = Math.floor(rng(30, 60))
      fbKoth(a, h, () => {
        if (h) {
          h.owner = a
          renderHill(h)
          spawnCapture(a, h, a.color)
        }
        a.kothScore = (a.kothScore || 0) + pts
        addLog(
          'HILL',
          'hill',
          `<span class="who">${esc(a.name)}</span> first crowned <span class="svc">${esc(h ? h.name : 'the hill')}</span>`
        )
        refreshRank()
      })
    }
  const patchBtn: any = $('patchBtn')
  if (patchBtn)
    patchBtn.onclick = () => {
      if (TEAMS.length) evPatch()
    }
  const freezeBtn: any = $('freezeBtn')
  if (freezeBtn)
    freezeBtn.onclick = () => {
      if (frozen) unfreeze()
      else enterFreeze()
    }
  const endBtn: any = $('endBtn')
  if (endBtn)
    endBtn.onclick = () => {
      void endMatch()
    }
  const rematchBtn: any = $('rematchBtn')
  if (rematchBtn)
    rematchBtn.onclick = () => {
      if (preview) resetMatch()
    }

  const motionBtn: HTMLButtonElement = $('motionBtn')
  const applyMotion = () => {
    motionEnabled = motionRequested && !motionQuery.matches
    root.host.setAttribute('data-motion', motionEnabled ? 'on' : 'off')
    motionBtn.textContent = motionEnabled ? 'Motion on' : 'Motion off'
    motionBtn.setAttribute('aria-pressed', String(motionEnabled))
    motionBtn.classList.toggle('on', motionEnabled)
    motionBtn.disabled = motionQuery.matches
    motionBtn.title = motionQuery.matches ? 'Your reduced-motion preference is active' : 'Toggle animated effects'
    if (!motionEnabled) {
      shots.forEach((shot) => {
        shot.t = 1
      })
      sparks.forEach((spark) => {
        spark.life = 0
      })
      shots.length = sparks.length = fxq.length = 0
      ctx.clearRect(0, 0, 1000, 1000)
      ctxbg.clearRect(0, 0, 1000, 1000)
      if (fxRenderer.ready) fxRenderer.tick(0, shots, sparks, fxq)
      fbRenderer.stop()
      fzRenderer.stop()
      winRenderer.stop()
    }
    jeop.refreshMotion()
  }
  motionBtn.onclick = () => {
    motionRequested = !motionRequested
    applyMotion()
  }
  motionQuery.addEventListener('change', applyMotion)
  applyMotion()

  if (preview) {
    setConnection('Preview', 'preview')
    startPreview()
  } else start()

  /* -------- teardown -------- */
  return () => {
    killed = true
    motionQuery.removeEventListener('change', applyMotion)
    timers.forEach((id) => clearInterval(id))
    timers.forEach((id) => clearTimeout(id))
    deferred.forEach((id) => clearTimeout(id))
    deferred.clear()
    clearTimeout(evTimer)
    clearTimeout(reconnectTimer) // cancel any pending WS reconnect so connectWS can't fire after killed
    clearTimeout(livePollTimer)
    livePollController?.abort()
    livePollController = null
    liveRequestControllers.forEach((controller) => controller.abort())
    liveRequestControllers.clear()
    if (raf) cancelAnimationFrame(raf)
    window.removeEventListener('resize', onResize)
    if (resizeRaf) cancelAnimationFrame(resizeRaf) // don't let a pending resize fire sizeCanvas into destroyed Pixi apps
    document.removeEventListener('visibilitychange', onVis)
    window.removeEventListener('online', onLiveAvailability)
    window.removeEventListener('offline', onLiveAvailability)
    document.removeEventListener('pointerdown', primeAudio)
    document.removeEventListener('keydown', primeAudio)
    if ('speechSynthesis' in window) {
      try {
        speechSynthesis.cancel()
      } catch {}
    }
    stopIncomingSound?.()
    stopIncomingSound = null
    stopFirstBloodSound?.()
    stopFirstBloodSound = null
    snd.close()
    document.removeEventListener('fullscreenchange', onFsChange)
    jeop.destroy()
    fxRenderer.destroy()
    fbRenderer.destroy()
    fzRenderer.destroy()
    winRenderer.destroy()
    if (ws) {
      try {
        ws.onclose = null
        ws.close()
      } catch {}
      ws = null
    }
  }
}

/* -------------------------------------------------------------------------- */

const Attack: FC = () => {
  const { id } = useParams()
  const scheme = useComputedColorScheme('dark')
  const [searchParams] = useSearchParams()
  const preview = searchParams.has('preview')
  const hostRef = useRef<HTMLDivElement>(null)
  const cleanupRef = useRef<null | (() => void)>(null)

  useEffect(() => {
    const host = hostRef.current
    if (!host || !id) return

    const shadow = host.shadowRoot ?? host.attachShadow({ mode: 'open' })
    shadow.innerHTML = `<style>${ARENA_CSS}</style>${ARENA_BODY}`
    shadow.getElementById('eventLink')?.setAttribute('href', `/games/${encodeURIComponent(id)}`)
    shadow.getElementById('scoreboardLink')?.setAttribute('href', `/games/${encodeURIComponent(id)}/scoreboard`)
    cleanupRef.current = runArena(shadow, id, preview)

    return () => {
      cleanupRef.current?.()
      cleanupRef.current = null
    }
  }, [id, preview])

  return (
    <WithNavBar competition width="1600px">
      <Stack gap="md">
        <PageHeader
          title="Live arena"
          eyebrow="Competition workspace"
          description="Explore the event as a living world of teams and challenge islands."
          actions={
            <Group gap="xs">
              <Button component={Link} to={`/games/${id}/challenges`} variant="default">
                Challenges
              </Button>
              <Button component={Link} to={`/games/${id}/scoreboard`} variant="default">
                Scoreboard
              </Button>
            </Group>
          }
        />
        <div
          ref={hostRef}
          role="region"
          aria-label="Live competition arena"
          data-arena-theme="globe"
          data-arena-scheme={scheme}
          style={{ minWidth: 0, colorScheme: scheme }}
        />
      </Stack>
    </WithNavBar>
  )
}

export default Attack
