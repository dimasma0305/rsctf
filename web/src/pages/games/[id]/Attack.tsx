/**
 * Public per-game live Attack & Defense / King of the Hill battle arena.
 *
 * Route: /games/{id}/attack  (no authentication).
 *
 * Arcade / anime "cyber arena" design & animation by lawbyte
 * (https://github.com/lawbyte). Ported into the platform here and wired to the
 * live plain-WebSocket attack feed (/hub/attack/ws?game={id}) plus the public
 * A&D / KotH scoreboards (/api/Game/{id}/Ad/Scoreboard, .../Ad/Koth/Scoreboard).
 *
 * The whole piece is a self-contained imperative SVG + canvas scene with its
 * own full-page CSS, so it is mounted into a Shadow DOM: that isolates its
 * styles and DOM from the React app shell completely (and a ShadowRoot still
 * supports getElementById, which the engine relies on). The engine runs in a
 * useEffect and tears itself down (WebSocket, timers, rAF) on unmount.
 */
import { FC, useEffect, useRef } from 'react'
import { useParams, useSearchParams } from 'react-router'
import { epochProgress } from '@Utils/epochProgress'
import type { AdScoreboardModel } from '@Api'
import { createJeopardy, type JeopCategory } from './arenaJeopardy'
import { createSoundEngine } from './audio'
import { createFbRenderer } from './fbRenderer'
import { createFxRenderer } from './fxRenderer'
import { createFzRenderer } from './fzRenderer'
import { createJeopRenderer } from './jeopRenderer'
import { KothDirector, statusFromCheck, type CaptureResult } from './kothCapture'
import { createWinRenderer } from './winRenderer'

const FONTS_HREF =
  'https://fonts.googleapis.com/css2?family=Press+Start+2P&family=VT323&family=DotGothic16&display=swap'

/* -------------------------------------------------------------------------- */
/* Scene CSS (lawbyte). `body` is remapped to `:host` for the shadow root.    */
/* -------------------------------------------------------------------------- */
const ARENA_CSS = `
  :host{
    --bg:#06050f; --bg2:#0b0918; --panel:rgba(13,10,28,0.9);
    --line:rgba(132,98,238,0.16); --line2:rgba(132,98,238,0.32);
    --text:#e9e6ff; --dim:#7d78ad; --dimmer:#4f4a78;
    --cyan:#27e3ff; --magenta:#ff39a8; --lime:#b9ff42; --amber:#ffc637;
    --violet:#9d6bff; --orange:#ff7a3a; --blue:#4d8bff; --red:#ff4d5e;
    --good:#3dffb0; --warn:#ffd23a; --bad:#ff3b5b;
    --glow:0 0 18px;
    display:block; position:absolute; inset:0; overflow:hidden;
    color:var(--text); font-family:'VT323',monospace; -webkit-font-smoothing:none;
    background:
      radial-gradient(1200px 700px at 50% 42%, #15102e 0%, rgba(8,6,18,0) 60%),
      radial-gradient(900px 600px at 8% 12%, rgba(255,57,168,0.10), transparent 55%),
      radial-gradient(900px 600px at 92% 16%, rgba(39,227,255,0.10), transparent 55%),
      var(--bg);
  }
  *{box-sizing:border-box;margin:0;padding:0}
  .circuit{position:absolute;inset:0;z-index:0;opacity:.5;pointer-events:none;
    background-image:
      linear-gradient(var(--line) 1px,transparent 1px),
      linear-gradient(90deg,var(--line) 1px,transparent 1px),
      linear-gradient(var(--line) 1px,transparent 1px),
      linear-gradient(90deg,var(--line) 1px,transparent 1px);
    background-size:96px 96px,96px 96px,24px 24px,24px 24px;
    background-position:-1px -1px,-1px -1px,-1px -1px,-1px -1px;
    mask-image:radial-gradient(1100px 700px at 50% 45%,#000 30%,transparent 85%);
  }
  .grain{position:absolute;inset:0;z-index:61;pointer-events:none;opacity:.05;
    background-image:url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='120' height='120'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.9' numOctaves='2'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)'/%3E%3C/svg%3E")}

  .shell{position:relative;z-index:5;height:100vh;display:grid;
    grid-template-rows:auto 1fr auto;gap:10px;padding:12px}
  .midrow{display:grid;grid-template-columns:clamp(200px,20vw,300px) minmax(0,1fr) clamp(210px,21vw,320px);gap:12px;min-height:0;min-width:0}

  .topbar{display:flex;align-items:center;justify-content:space-between;
    padding:8px 16px;border:1px solid var(--line2);background:var(--panel);
    clip-path:polygon(0 0,calc(100% - 14px) 0,100% 14px,100% 100%,14px 100%,0 calc(100% - 14px))}
  .brand{display:flex;align-items:baseline;gap:14px}
  .brand .logo{font-family:'Press Start 2P';font-size:15px;letter-spacing:1px;
    color:#fff;text-shadow:var(--glow) var(--magenta),0 0 4px var(--magenta)}
  .brand .logo b{color:var(--cyan);text-shadow:var(--glow) var(--cyan)}
  .brand .mode{font-family:'Press Start 2P';font-size:9px;color:var(--dim);
    border:1px solid var(--line2);padding:5px 8px}
  .topright{display:flex;align-items:center;gap:18px;font-size:19px}
  .roundpill{font-family:'Press Start 2P';font-size:9px;color:var(--amber);
    border:1px solid rgba(255,198,55,.4);padding:6px 9px;
    text-shadow:0 0 6px rgba(255,198,55,.6)}
  .countpill{font-family:'Press Start 2P';font-size:9px;color:var(--cyan);
    border:1px solid rgba(39,227,255,.4);padding:6px 9px;text-shadow:0 0 6px rgba(39,227,255,.6)}

  .panel{position:relative;border:1px solid var(--line2);background:var(--panel);
    display:flex;flex-direction:column;min-height:0;min-width:0;
    clip-path:polygon(0 0,calc(100% - 16px) 0,100% 16px,100% 100%,16px 100%,0 calc(100% - 16px))}
  .panel::before{content:"";position:absolute;inset:0;pointer-events:none;
    border-top:1px solid rgba(255,255,255,.04)}
  .phead{display:flex;align-items:center;justify-content:space-between;
    padding:7px 12px;border-bottom:1px solid var(--line);
    background:linear-gradient(90deg,rgba(157,107,255,.10),transparent)}
  .phead .t{font-family:'Press Start 2P';font-size:9px;letter-spacing:1px;color:#fff}
  .rank-tabs{display:inline-flex;gap:3px}
  .rank-tabs button{font-family:'Press Start 2P';font-size:7px;color:var(--dim);background:transparent;
    border:1px solid var(--line2);padding:4px 5px;border-radius:3px;cursor:pointer;line-height:1}
  .rank-tabs button:hover{color:#fff;border-color:var(--cyan)}
  .rank-tabs button.on{color:#06050f;background:var(--cyan);border-color:var(--cyan)}
  .accent-c{box-shadow:inset 3px 0 0 var(--cyan)}
  .accent-m{box-shadow:inset 3px 0 0 var(--magenta)}
  .accent-v{box-shadow:inset 3px 0 0 var(--violet)}

  #log{flex:1;overflow-y:auto;overflow-x:auto;padding:8px 10px;display:flex;flex-direction:column;
    align-items:flex-start;gap:3px;font-size:15px;line-height:1.25;justify-content:flex-start;scrollbar-width:thin;scrollbar-color:var(--line2) transparent}
  #log::-webkit-scrollbar{width:6px;height:6px}#log::-webkit-scrollbar-thumb{background:var(--line2);border-radius:3px}
  /* rows keep their full width (no ellipsis) so long lines stay readable via horizontal scroll */
  .lg{flex:0 0 auto;white-space:nowrap;opacity:.92;animation:logIn .25s ease-out}
  @keyframes logIn{from{opacity:0;transform:translateX(-8px)}}
  .lg .ts{color:var(--dimmer);margin-right:5px}
  .lg .tag{font-family:'Press Start 2P';font-size:8px;padding:1px 4px;margin-right:6px;
    vertical-align:middle}
  .tag.flag{color:#fff;background:rgba(255,77,94,.18);border:1px solid var(--red)}
  .tag.def{color:#fff;background:rgba(61,255,176,.14);border:1px solid var(--good)}
  .tag.patch{color:#fff;background:rgba(39,227,255,.16);border:1px solid var(--cyan)}
  .tag.sla{color:#fff;background:rgba(255,210,58,.14);border:1px solid var(--warn)}
  .tag.fb{color:#fff;background:rgba(255,57,168,.2);border:1px solid var(--magenta)}
  .tag.sys{color:var(--dim);border:1px solid var(--line2)}
  .tag.hill{color:#fff;background:rgba(157,107,255,.2);border:1px solid var(--violet)}
  .lg .who{color:var(--cyan)}
  .lg .vic{color:var(--magenta)}
  .lg .svc{color:var(--amber)}
  .lg .em{color:#fff}

  .arena-wrap{position:relative;display:flex;align-items:center;justify-content:center;
    min-height:0;min-width:0;overflow:hidden}
  /* z-index:5 lifts the wheel (and its edge team-name labels that overflow the square) ABOVE the
     jeopardy constellation canvas (z-index:4) so the names aren't cropped/covered by the "space". */
  .arena{position:relative;aspect-ratio:1/1;height:100%;max-height:100%;max-width:100%;z-index:5}
  #fxbg{position:absolute;inset:0;width:100%;height:100%;pointer-events:none}
  /* overflow:visible so the outer team-name labels (offset past the 1000 viewBox edge) aren't clipped */
  #svg{position:absolute;inset:0;width:100%;height:100%;overflow:visible}
  #fx{position:absolute;inset:0;width:100%;height:100%;pointer-events:none}
  .arena-note{position:absolute;inset:0;display:flex;align-items:center;justify-content:center;
    text-align:center;font-family:'Press Start 2P';font-size:11px;color:var(--dim);
    line-height:2;padding:20px;z-index:8}
  /* ---- jeopardy constellation overlay (side bands beside the square wheel) ---- */
  #jeop{position:absolute;inset:0;width:100%;height:100%;pointer-events:none;z-index:4}
  #jeopSpace{display:none}
  @keyframes jtwk{0%,100%{opacity:var(--o,1)}50%{opacity:var(--o2,.6)}}
  #jeop .twk{animation:jtwk var(--d,3s) ease-in-out infinite;animation-delay:var(--dl,0s)}
  #jeop .chhit{pointer-events:all;cursor:pointer}
  .jtip{position:absolute;z-index:9;pointer-events:none;opacity:0;transform:translateY(4px);
    transition:opacity .12s ease,transform .12s ease;min-width:140px;max-width:230px;padding:8px 10px;border-radius:7px;
    background:rgba(8,10,22,.96);border:1px solid rgba(120,140,200,.35);box-shadow:0 8px 28px rgba(0,0,0,.6);font-family:'VT323',monospace}
  .jtip.show{opacity:1;transform:translateY(0)}
  .jtip .jt-name{font-size:17px;color:#e7ebf7;line-height:1.1}
  .jtip .jt-meta{font-size:14px;color:#8b93b4;margin:1px 0 6px}
  .jtip .jt-row{display:flex;align-items:center;gap:6px;font-size:15px;color:#d4dcef;margin:2px 0}
  .jtip .jt-dot{width:9px;height:9px;border-radius:50%;flex:0 0 auto;box-shadow:0 0 5px currentColor}
  .jtip .jt-rank{margin-left:auto;font-size:12px;font-family:'Press Start 2P';letter-spacing:.4px}
  .jtip .jt-none{font-size:15px;color:#6f7794}
  /* ---- fullscreen battle-map button ---- */
  .fs-btn{position:absolute;top:6px;right:8px;z-index:9;width:28px;height:28px;display:flex;
    align-items:center;justify-content:center;font-size:14px;line-height:1;color:#b6c0df;
    background:rgba(12,16,30,.72);border:1px solid rgba(120,140,200,.42);border-radius:7px;cursor:pointer;
    opacity:.55;box-shadow:0 2px 10px rgba(0,0,0,.45);transition:opacity .18s,background .15s,border-color .15s,color .15s,transform .1s}
  .arena-wrap:hover .fs-btn{opacity:1}
  .fs-btn:hover{background:rgba(46,60,108,.96);border-color:rgba(150,180,255,.85);color:#fff}
  .fs-btn:active{transform:scale(.92)}
  .arena-wrap:fullscreen{background:radial-gradient(120% 120% at 50% 40%,#0b0f1e 0%,#06070f 70%,#04050b 100%);padding:0}
  .arena-wrap:fullscreen .arena{height:100%}

  .rightcol{display:flex;flex-direction:column;gap:12px;min-height:0;min-width:0}
  .panel.rank{flex:1;min-height:0}
  #ranklist{flex:1;min-height:0;overflow-y:auto;overflow-x:hidden;padding:6px;
    scrollbar-width:thin;scrollbar-color:var(--line2) transparent}
  #ranklist::-webkit-scrollbar{width:6px}#ranklist::-webkit-scrollbar-thumb{background:var(--line2);border-radius:3px}
  .rk{flex:0 0 auto;display:flex;align-items:center;gap:9px;padding:6px 7px;margin-bottom:5px;
    border:1px solid var(--line);position:relative;
    background:linear-gradient(90deg,rgba(255,255,255,.02),transparent);
    transition:transform .35s cubic-bezier(.2,.9,.2,1)}
  .rk .pos{font-family:'Press Start 2P';font-size:12px;width:22px;text-align:center;color:var(--dim)}
  .rk.p1 .pos{color:var(--amber);text-shadow:0 0 8px var(--amber)}
  .rk.p2 .pos{color:#d8e0ff}
  .rk.p3 .pos{color:var(--orange)}
  .rk .av{width:38px;height:38px;flex:none;border:1px solid var(--line2);
    border-radius:5px;overflow:hidden;background:#0a0818}
  .rk .av svg{display:block;width:100%;height:100%}
  .rk .body{flex:1;min-width:0}
  .rk .nm{font-family:'Press Start 2P';font-size:8px;letter-spacing:.5px;
    white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .rk .bars{display:flex;gap:3px;margin-top:4px;height:5px}
  .rk .bars i{display:block;height:100%;border-radius:1px;opacity:.9}
  .rk .sc{font-size:22px;color:#fff;line-height:1;text-align:right;min-width:46px;
    text-shadow:0 0 8px rgba(255,255,255,.25)}
  .rk .sc small{display:block;font-size:11px;color:var(--good);margin-top:1px}
  .rk .sc small.dn{color:var(--bad)}

  .strow{display:flex;justify-content:space-between;padding:3px 0;
    border-bottom:1px dashed var(--line)}
  .strow:last-child{border-bottom:0}
  .strow .k{color:var(--dim);font-size:16px;font-family:'DotGothic16'}
  .strow .v{color:#fff;letter-spacing:1px}
  .strow .v.acc{color:var(--cyan);text-shadow:0 0 6px rgba(39,227,255,.5)}
  .legend{display:flex;gap:12px;padding:8px 12px;border-top:1px solid var(--line);
    font-family:'Press Start 2P';font-size:7px;color:var(--dim);flex-wrap:wrap}
  .legend span{display:inline-flex;align-items:center;gap:5px}
  .legend i{width:9px;height:9px;display:inline-block}
  .slegend{display:flex;flex-wrap:wrap;gap:6px 10px;padding:8px 0 2px;margin-top:4px;
    border-top:1px dashed var(--line);font-family:'Press Start 2P';font-size:7px;color:var(--dim)}
  .slegend span{display:inline-flex;align-items:center;gap:5px}
  .slegend i{width:8px;height:8px;display:inline-block;border-radius:2px;border:1px solid #06050f}

  .devbar{display:flex;flex-wrap:wrap;align-items:center;gap:10px;padding:8px 14px;
    border:1px solid var(--line2);background:var(--panel);
    clip-path:polygon(14px 0,100% 0,100% calc(100% - 14px),calc(100% - 14px) 100%,0 100%,0 14px)}
  .devbar .label{font-family:'Press Start 2P';font-size:8px;color:var(--violet);
    letter-spacing:1px;margin-right:4px}
  #cfgBtns{display:inline-flex;gap:7px;flex-wrap:wrap}
  .cfg{display:inline-flex;align-items:center;gap:5px;font-family:'Press Start 2P';font-size:7px;
    color:var(--dim);border:1px solid var(--line2);padding:4px 6px;border-radius:4px}
  .cfg input{width:38px;background:#0a0818;border:1px solid var(--line2);color:#fff;
    font-family:'VT323';font-size:15px;padding:2px 4px;border-radius:3px;text-align:center}
  .cfg input:focus{outline:none;border-color:var(--cyan)}
  .btn{font-family:'Press Start 2P';font-size:8px;letter-spacing:.5px;color:#0a0612;
    border:0;padding:8px 11px;cursor:pointer;position:relative;
    clip-path:polygon(6px 0,100% 0,100% calc(100% - 6px),calc(100% - 6px) 100%,0 100%,0 6px);
    transition:transform .08s,filter .15s}
  .btn:active{transform:translateY(2px)}
  .btn:hover{filter:brightness(1.15)}
  .btn.ghost{background:transparent;color:var(--dim);border:1px solid var(--line2);box-shadow:none}
  .btn.ghost.on{color:#0a0612;background:var(--cyan);border-color:var(--cyan)}
  .btn.fb-ad{background:var(--red);box-shadow:0 0 14px rgba(255,77,94,.5)}
  .btn.fb-jeo{background:var(--amber);box-shadow:0 0 14px rgba(255,198,55,.5)}
  .btn.fb-koth{background:#9d6bff;color:#0a0612;box-shadow:0 0 14px rgba(157,107,255,.5)}
  .btn.patch{background:#27e3ff;color:#06121a;box-shadow:0 0 14px rgba(39,227,255,.5)}
  #fbBtns{display:inline-flex;gap:10px}
  .sp{flex:1}

  .float{position:absolute;font-family:'Press Start 2P';font-size:9px;pointer-events:none;
    z-index:7;text-shadow:0 0 6px currentColor;animation:floatUp 1.1s ease-out forwards}
  @keyframes floatUp{0%{opacity:0;transform:translateY(4px) scale(.7)}
    20%{opacity:1;transform:translateY(-2px) scale(1.1)}
    100%{opacity:0;transform:translateY(-30px) scale(1)}}

  /* avatar idle motion lives on the #fxbg canvas now (see drawAmbient) — the SVG stays static */

  @keyframes shake{
    0%,100%{transform:translate(0,0)}
    10%{transform:translate(-5px,3px)}25%{transform:translate(6px,-4px)}
    40%{transform:translate(-7px,2px)}55%{transform:translate(5px,4px)}
    70%{transform:translate(-4px,-3px)}85%{transform:translate(3px,2px)}}
  .shell.shake{animation:shake .5s cubic-bezier(.36,.07,.19,.97)}

  @keyframes nodeGlitch{0%,100%{opacity:1;filter:none}18%{opacity:.35;filter:brightness(1.7) hue-rotate(-18deg)}40%{opacity:.9}58%{opacity:.25;filter:brightness(.5) saturate(2.2)}78%{opacity:.7}}
  .node-down{animation:nodeGlitch .42s steps(2) 3}

  /* ===== FIRST BLOOD CINEMATIC ===== */
  .fb-overlay{position:fixed;inset:0;z-index:95;pointer-events:none;visibility:hidden;overflow:hidden}
  .fb-overlay.play,.fb-overlay.tele{visibility:visible}
  .fb-overlay>div{position:absolute;opacity:0}
  .fb-dark{inset:0;background:radial-gradient(circle at 50% 45%,rgba(40,2,12,.72),rgba(2,1,6,.97))}
  .fb-bar{left:0;width:100%;height:11vh;background:#040308;border-color:#ff3b5b}
  .fb-bar.t{top:0;border-bottom:2px solid #ff3b5b}
  .fb-bar.b{bottom:0;border-top:2px solid #ff3b5b}
  .fb-rays{inset:-25%;
    background:repeating-conic-gradient(from 0deg at 50% 47%,rgba(255,255,255,0) 0deg 3.4deg,rgba(255,90,110,.24) 3.4deg 4deg);
    -webkit-mask:radial-gradient(circle at 50% 47%,transparent 11%,#000 40%,transparent 78%);
            mask:radial-gradient(circle at 50% 47%,transparent 11%,#000 40%,transparent 78%)}
  /* Artistic radial RED-LIQUID splash from center — a bright core with tapering liquid tendrils
     + flung droplets, revealed center-out by a growing clip-path. No drips/gore. Composited only. */
  .fb-splat{left:50%;top:47%;width:108vmin;height:108vmin;transform:translate(-50%,-50%) scale(.55);
    background:url("data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20viewBox='0%200%20600%20600'%3E%3Cdefs%3E%3CradialGradient%20id='g'%20cx='50%25'%20cy='50%25'%20r='58%25'%3E%3Cstop%20offset='0'%20stop-color='%23ff5a6e'/%3E%3Cstop%20offset='.35'%20stop-color='%23e8122f'/%3E%3Cstop%20offset='.72'%20stop-color='%23bf0f24'/%3E%3Cstop%20offset='1'%20stop-color='%238c0c1d'/%3E%3C/radialGradient%3E%3C/defs%3E%3Cg%20fill='url(%23g)'%3E%3Cellipse%20cx='300'%20cy='300'%20rx='52'%20ry='46'/%3E%3Cellipse%20cx='286'%20cy='310'%20rx='30'%20ry='26'/%3E%3Cellipse%20cx='316'%20cy='292'%20rx='26'%20ry='22'/%3E%3Cpath%20d='M340.2%20313.9%20Q450.7%20304.6%20541.1%20297%20Q450.6%20291.7%20339.8%20285.1%20Z'/%3E%3Cpath%20d='M333.8%20324.6%20Q436.8%20353.6%20521.1%20377.4%20Q440.4%20343.4%20341.7%20301.9%20Z'/%3E%3Cpath%20d='M321.4%20336.1%20Q423.2%20418.1%20506.5%20485.3%20Q430.7%20409.7%20338.2%20317.4%20Z'/%3E%3Cpath%20d='M302.9%20340.7%20Q338.4%20450.3%20367.5%20539.9%20Q345.6%20448.3%20318.8%20336.3%20Z'/%3E%3Cpath%20d='M291.8%20339.8%20Q293.7%20425.2%20295.3%20495%20Q300.2%20425.3%20306.3%20340.2%20Z'/%3E%3Cpath%20d='M270.3%20329%20Q216.5%20433.1%20172.5%20518.3%20Q225.1%20438.1%20289.3%20340.1%20Z'/%3E%3Cpath%20d='M258.3%20307.1%20Q134.2%20383.3%2032.7%20445.6%20Q140.1%20394.1%20271.4%20331.2%20Z'/%3E%3Cpath%20d='M258%20297.8%20Q190.7%20323.4%20135.7%20344.5%20Q193.8%20334.9%20264.8%20323.1%20Z'/%3E%3Cpath%20d='M264.4%20280.5%20Q136.7%20241%2032.3%20208.7%20Q134.7%20247%20259.9%20293.7%20Z'/%3E%3Cpath%20d='M276%20267.1%20Q201.9%20203.3%20141.2%20151.1%20Q197.2%20208.3%20265.6%20278.2%20Z'/%3E%3Cpath%20d='M288.3%20259%20Q227.2%20189.1%20177.2%20131.9%20Q216.5%20196.9%20264.5%20276.4%20Z'/%3E%3Cpath%20d='M304.8%20258.1%20Q283%20190.4%20265.1%20135%20Q271.2%20192.9%20278.6%20263.6%20Z'/%3E%3Cpath%20d='M317.1%20263%20Q341.6%20140.9%20361.6%2041.1%20Q334.5%20139.3%20301.4%20259.2%20Z'/%3E%3Cpath%20d='M336.2%20277.4%20Q383.3%20203.2%20421.8%20142.5%20Q372.8%20195.1%20312.8%20259.3%20Z'/%3E%3Cpath%20d='M341.8%20293.2%20Q424%20240.3%20491.2%20197%20Q418.1%20229.3%20328.7%20268.9%20Z'/%3E%3C/g%3E%3Cg%20fill='%23ff3b5b'%3E%3Ccircle%20cx='547.8'%20cy='296.9'%20r='6.9'/%3E%3Ccircle%20cx='530.4'%20cy='378.4'%20r='5.5'/%3E%3Ccircle%20cx='508.3'%20cy='493.2'%20r='8.8'/%3E%3Ccircle%20cx='368.1'%20cy='547'%20r='4.3'/%3E%3Ccircle%20cx='295.1'%20cy='501.4'%20r='8.2'/%3E%3Ccircle%20cx='168.1'%20cy='524'%20r='5.4'/%3E%3Ccircle%20cx='24.8'%20cy='448.1'%20r='5.1'/%3E%3Ccircle%20cx='129.9'%20cy='347.2'%20r='5.9'/%3E%3Ccircle%20cx='28.4'%20cy='205.2'%20r='6.3'/%3E%3Ccircle%20cx='135.2'%20cy='144.5'%20r='5.3'/%3E%3Ccircle%20cx='171.5'%20cy='129.3'%20r='5.2'/%3E%3Ccircle%20cx='264.3'%20cy='127.5'%20r='6.2'/%3E%3Ccircle%20cx='363.6'%20cy='38'%20r='6.1'/%3E%3Ccircle%20cx='428'%20cy='138.5'%20r='8.4'/%3E%3Ccircle%20cx='498.6'%20cy='195.6'%20r='8.9'/%3E%3Ccircle%20cx='333'%20cy='440.2'%20r='5.1'/%3E%3Ccircle%20cx='228'%20cy='433.1'%20r='2.3'/%3E%3Ccircle%20cx='473.6'%20cy='410.4'%20r='3'/%3E%3Ccircle%20cx='166.8'%20cy='201.6'%20r='3.8'/%3E%3Ccircle%20cx='480.8'%20cy='252.5'%20r='4.3'/%3E%3Ccircle%20cx='386.7'%20cy='203.5'%20r='2.6'/%3E%3Ccircle%20cx='510.1'%20cy='163.8'%20r='3'/%3E%3Ccircle%20cx='387'%20cy='518.8'%20r='5.8'/%3E%3Ccircle%20cx='390.7'%20cy='560.2'%20r='5.5'/%3E%3Ccircle%20cx='160.7'%20cy='194'%20r='2.4'/%3E%3Ccircle%20cx='569.7'%20cy='366.9'%20r='3'/%3E%3Ccircle%20cx='259.5'%20cy='162'%20r='5.3'/%3E%3Ccircle%20cx='176.1'%20cy='214.1'%20r='2.7'/%3E%3Ccircle%20cx='280'%20cy='193.8'%20r='2.9'/%3E%3Ccircle%20cx='60.7'%20cy='206.4'%20r='4.5'/%3E%3Ccircle%20cx='249.2'%20cy='564.5'%20r='2.8'/%3E%3Ccircle%20cx='445.4'%20cy='315.2'%20r='3.8'/%3E%3Ccircle%20cx='419.3'%20cy='347.6'%20r='3.5'/%3E%3Ccircle%20cx='192.1'%20cy='247.4'%20r='3.4'/%3E%3Ccircle%20cx='517.8'%20cy='122'%20r='4.6'/%3E%3Ccircle%20cx='225.6'%20cy='107.8'%20r='2.6'/%3E%3Ccircle%20cx='396'%20cy='321.5'%20r='5.6'/%3E%3Ccircle%20cx='215.7'%20cy='35.2'%20r='2.1'/%3E%3Ccircle%20cx='177.6'%20cy='159.1'%20r='4.9'/%3E%3Ccircle%20cx='180.5'%20cy='558.6'%20r='2.3'/%3E%3Ccircle%20cx='74.8'%20cy='232.9'%20r='5.6'/%3E%3C/g%3E%3Ccircle%20cx='300'%20cy='300'%20r='20'%20fill='%23ff7283'/%3E%3C/svg%3E") center/contain no-repeat;
    will-change:transform,opacity}
  .fb-splat2{left:50%;top:47%;width:126vmin;height:126vmin;transform:translate(-50%,-50%) scale(.7);
    background:url("data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20viewBox='0%200%20600%20600'%3E%3Cg%20fill='%23e8122f'%3E%3Ccircle%20cx='107.3'%20cy='239.9'%20r='6.3'/%3E%3Ccircle%20cx='245.6'%20cy='519.9'%20r='4'/%3E%3Ccircle%20cx='44.7'%20cy='449.9'%20r='3'/%3E%3Ccircle%20cx='127.3'%20cy='362.6'%20r='4.1'/%3E%3Ccircle%20cx='499.4'%20cy='298.4'%20r='5.2'/%3E%3Ccircle%20cx='72.8'%20cy='425.6'%20r='4.5'/%3E%3Ccircle%20cx='403.1'%20cy='488.8'%20r='4.3'/%3E%3Ccircle%20cx='362.2'%20cy='74'%20r='3.4'/%3E%3Ccircle%20cx='210.3'%20cy='127.1'%20r='4.8'/%3E%3Ccircle%20cx='339.8'%20cy='507.8'%20r='6'/%3E%3Ccircle%20cx='38.4'%20cy='274.1'%20r='5.9'/%3E%3Ccircle%20cx='89.3'%20cy='171.6'%20r='4.3'/%3E%3Ccircle%20cx='103.3'%20cy='120'%20r='2.9'/%3E%3Ccircle%20cx='489.5'%20cy='221.7'%20r='3.1'/%3E%3Ccircle%20cx='418.2'%20cy='462.7'%20r='2.8'/%3E%3Ccircle%20cx='130'%20cy='427.1'%20r='5.4'/%3E%3Ccircle%20cx='16.1'%20cy='360.5'%20r='5.5'/%3E%3Ccircle%20cx='178.6'%20cy='553.7'%20r='5.6'/%3E%3Ccircle%20cx='554.7'%20cy='247'%20r='6.8'/%3E%3Ccircle%20cx='353.2'%20cy='484.2'%20r='6.9'/%3E%3Ccircle%20cx='585.8'%20cy='339.5'%20r='6.6'/%3E%3Ccircle%20cx='413.4'%20cy='430.1'%20r='6.6'/%3E%3C/g%3E%3Cg%20fill='%23ff5a6e'%3E%3Ccircle%20cx='534'%20cy='239.5'%20r='3.8'/%3E%3Ccircle%20cx='310'%20cy='137.2'%20r='2'/%3E%3Ccircle%20cx='131'%20cy='124.7'%20r='4.4'/%3E%3Ccircle%20cx='179.4'%20cy='288.7'%20r='2.4'/%3E%3Ccircle%20cx='155.4'%20cy='347.9'%20r='2.1'/%3E%3Ccircle%20cx='366.9'%20cy='477.8'%20r='2.3'/%3E%3Ccircle%20cx='408.7'%20cy='393.2'%20r='2.1'/%3E%3Ccircle%20cx='350.4'%20cy='64.1'%20r='2.5'/%3E%3Ccircle%20cx='138.4'%20cy='119'%20r='2.6'/%3E%3Ccircle%20cx='178.2'%20cy='510.7'%20r='3.9'/%3E%3Ccircle%20cx='132.7'%20cy='140.5'%20r='3.4'/%3E%3Ccircle%20cx='214.4'%20cy='498.3'%20r='4'/%3E%3C/g%3E%3C/svg%3E") center/contain no-repeat;
    will-change:transform,opacity}
  .fb-flash{inset:0;background:#fff}
  .fb-overlay .fb-core{inset:0;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:1.4vh;text-align:center;opacity:1;padding-bottom:8vh}
  .fb-title{font-family:'Press Start 2P';font-size:clamp(26px,7vw,86px);color:#fff;line-height:1;opacity:0;
    text-shadow:4px 0 #ff2350,-4px 0 #27e3ff,0 0 26px rgba(255,40,80,.9),0 0 60px rgba(255,40,80,.6)}
  .fb-sub{font-family:'Press Start 2P';font-size:clamp(9px,1.5vw,15px);color:#ffd0d6;opacity:0;
    letter-spacing:1px;text-shadow:0 0 10px rgba(255,80,110,.7)}
  .fb-overlay.play .fb-dark{animation:fbDark 5s ease-out forwards}
  .fb-overlay.play .fb-bar.t{animation:fbBarT 5s ease-out forwards}
  .fb-overlay.play .fb-bar.b{animation:fbBarB 5s ease-out forwards}
  .fb-overlay.play .fb-rays{animation:fbRays 5s ease-out forwards}
  .fb-overlay.play .fb-splat{animation:fbSplat 5s cubic-bezier(.18,.9,.25,1) forwards}
  .fb-overlay.play .fb-splat2{animation:fbSplat2 5s cubic-bezier(.2,.85,.3,1) forwards}
  .fb-overlay.play .fb-flash{animation:fbFlash 5s linear forwards}
  .fb-overlay.play .fb-title{animation:fbTitle 5s cubic-bezier(.2,1.5,.3,1) forwards}
  .fb-overlay.play .fb-sub{animation:fbSub 5s ease-out forwards}
  @keyframes fbDark{0%{opacity:0}5%{opacity:.95}82%{opacity:.95}100%{opacity:0}}
  @keyframes fbBarT{0%{opacity:1;transform:translateY(-100%)}9%{transform:translateY(0)}84%{opacity:1;transform:translateY(0)}100%{opacity:1;transform:translateY(-100%)}}
  @keyframes fbBarB{0%{opacity:1;transform:translateY(100%)}9%{transform:translateY(0)}84%{opacity:1;transform:translateY(0)}100%{opacity:1;transform:translateY(100%)}}
  @keyframes fbRays{0%,12%{opacity:0}16%{opacity:.7}82%{opacity:.45}100%{opacity:0}}
  /* radial splash: bursts outward from center on a scale punch, settles, holds, then fades
     drifting a touch wider (the splash dissipating). transform/opacity ONLY — no clip-path,
     so it stays GPU-composited (animating clip-path on this screen-sized filtered element
     repainted every frame and was the source of the first-blood lag). */
  @keyframes fbSplat{
    0%,11%{opacity:0;transform:translate(-50%,-50%) scale(0)}
    16%{opacity:1;transform:translate(-50%,-50%) scale(1.12)}
    24%{transform:translate(-50%,-50%) scale(.97)}
    30%{transform:translate(-50%,-50%) scale(1)}
    78%{opacity:1;transform:translate(-50%,-50%) scale(1.03)}
    100%{opacity:0;transform:translate(-50%,-50%) scale(1.14)}}
  /* secondary spray: a beat later, splashes a touch wider/lighter, holds, fades. */
  @keyframes fbSplat2{
    0%,15%{opacity:0;transform:translate(-50%,-50%) scale(0)}
    21%{opacity:.9;transform:translate(-50%,-50%) scale(1.08)}
    30%{opacity:.85;transform:translate(-50%,-50%) scale(1)}
    78%{opacity:.8;transform:translate(-50%,-50%) scale(1.04)}
    100%{opacity:0;transform:translate(-50%,-50%) scale(1.16)}}
  @keyframes fbFlash{0%,11%{opacity:0}13%{opacity:.95}16%{opacity:0}18%{opacity:.5}21%{opacity:0}100%{opacity:0}}
  @keyframes fbTitle{0%{opacity:0;transform:scale(4.2)}11%{opacity:.25}15%{opacity:1;transform:scale(.92)}19%{transform:scale(1.05)}24%{transform:scale(1)}45%{transform:scale(1.015)}65%{transform:scale(1)}80%{opacity:1}100%{opacity:0;transform:scale(1.7)}}
  @keyframes fbSub{0%,21%{opacity:0;transform:translateY(16px)}29%{opacity:1;transform:translateY(0)}82%{opacity:1}100%{opacity:0}}
  /* symmetric 3-col grid: equal side columns keep the separator dead-centre even when
     the two team names differ in width; fighters hug the centre (atk->end, vic->start).
     .solo (jeopardy: no victim) collapses to a single centred attacker. */
  .fb-overlay .fb-vs{display:grid;grid-template-columns:1fr auto 1fr;align-items:start;justify-items:center;column-gap:clamp(18px,7vw,90px);opacity:1}
  .fb-overlay .fb-vs .fb-fighter.atk{justify-self:end}
  .fb-overlay .fb-vs .fb-fighter.vic{justify-self:start}
  .fb-overlay .fb-vs.solo{display:flex;justify-content:center}
  .fb-overlay .fb-vs.solo .fb-vs-x,.fb-overlay .fb-vs.solo .fb-fighter.vic{display:none}
  .fb-fighter{display:flex;flex-direction:column;align-items:center;gap:6px;opacity:0}
  .fb-rtag{font-family:'Press Start 2P';font-size:clamp(6px,.82vw,9px);letter-spacing:.1em;padding:.45em .7em;border-radius:5px;opacity:0;white-space:nowrap}
  .fb-fighter .por.fb-throne{display:grid;place-items:center;background:radial-gradient(circle at 38% 30%,#d8c4ff,#9d6bff)}
  .fb-fighter .por.fb-throne svg{width:76%;height:76%;color:#220a3a;display:block}
  /* RETICLE BRACKETS challenge container — four amber corner brackets, no fill. --rc/--rcg set inline. */
  .fb-chal{opacity:0;display:inline-flex;align-items:center;justify-content:center}
  .fb-brk{position:relative;font-family:'Press Start 2P';font-size:clamp(9px,1.3vw,15px);color:#fff;padding:.7em 1.3em;text-shadow:0 0 10px var(--rcg,rgba(255,198,55,.6))}
  .fb-brk.big{font-size:clamp(11px,1.7vw,20px);padding:.85em 1.7em}
  .fb-brk .lbl{color:var(--rc,#ffc637)}
  .fb-brk i{position:absolute;width:13px;height:13px;border:2px solid var(--rc,#ffc637);box-shadow:0 0 8px var(--rcg,rgba(255,198,55,.6))}
  .fb-brk.big i{width:17px;height:17px}
  .fb-brk .tl{top:0;left:0;border-right:0;border-bottom:0}
  .fb-brk .tr{top:0;right:0;border-left:0;border-bottom:0}
  .fb-brk .bl{bottom:0;left:0;border-right:0;border-top:0}
  .fb-brk .br{bottom:0;right:0;border-left:0;border-top:0}
  .fb-fighter .por{width:clamp(64px,11vw,124px);height:clamp(64px,11vw,124px);border-radius:50%;
    overflow:hidden;border:3px solid rgba(255,40,80,.55);box-shadow:0 0 26px rgba(255,40,80,.7);background:#0a0818}
  .fb-fighter .por svg{display:block;width:100%;height:100%}
  .fb-fighter .nm{font-family:'Press Start 2P';font-size:clamp(8px,1.1vw,13px);color:#fff;text-shadow:0 0 8px currentColor}
  /* separator sits in its own cell, height = portrait height so it centres on the portrait row
     (not the taller name+tag column). Holds "VS"/flag text or a crown SVG (KotH). */
  .fb-vs-x{display:grid;place-items:center;height:clamp(64px,11vw,124px);font-family:'Press Start 2P';font-size:clamp(16px,2.4vw,30px);color:#fff;opacity:0;
    text-shadow:0 0 14px #ff3b5b,2px 0 #ff2350,-2px 0 #27e3ff}
  .fb-vs-x svg{width:74%;height:74%;display:block;filter:drop-shadow(0 0 12px rgba(157,107,255,.85))}
  .fb-fighter.atk{transform:translateX(-130vw)}
  .fb-fighter.vic{transform:translateX(130vw)}
  .fb-overlay.play .fb-vs .fb-fighter.atk{animation:fbAtk 5s cubic-bezier(.2,1.3,.3,1) forwards}
  .fb-overlay.play .fb-vs .fb-fighter.vic{animation:fbVic 5s cubic-bezier(.2,1.3,.3,1) forwards}
  .fb-overlay.play .fb-vs-x{animation:fbVsx 5s ease-out forwards}
  .fb-overlay.play .fb-rtag{animation:fbRtag 5s ease-out forwards}
  .fb-overlay.play .fb-chal{animation:fbChal 5s cubic-bezier(.2,1.4,.4,1) forwards}
  @keyframes fbRtag{0%,18%{opacity:0;transform:translateY(10px)}24%{opacity:1;transform:translateY(0)}80%{opacity:1}100%{opacity:0}}
  @keyframes fbChal{0%,22%{opacity:0;transform:translateY(12px) scale(.9)}28%{opacity:1;transform:translateY(0) scale(1.05)}32%{transform:translateY(0) scale(1)}80%{opacity:1}100%{opacity:0}}
  @keyframes fbAtk{0%{opacity:0;transform:translateX(-130vw)}9%{opacity:1;transform:translateX(-14px)}
    14%{transform:translateX(14px)}16%{transform:translateX(0)}80%{opacity:1;transform:translateX(0)}100%{opacity:0;transform:translateX(-22vw)}}
  @keyframes fbVic{0%{opacity:0;transform:translateX(130vw)}9%{opacity:1;transform:translateX(14px)}
    14%{transform:translateX(26px) rotate(6deg)}16%{transform:translateX(20px) rotate(4deg)}80%{opacity:1;transform:translateX(20px) rotate(4deg)}100%{opacity:0;transform:translateX(22vw)}}
  @keyframes fbVsx{0%,12%{opacity:0;transform:scale(2.4)}16%{opacity:1;transform:scale(1)}80%{opacity:1}100%{opacity:0}}

  /* ===== FIRST-BLOOD TELEGRAPH (attention-seeking pre-roll; board stays visible) =====
     Plays before the slam: a transparent edge-vignette + a pulsing "INCOMING …"
     banner — the centre stays clear so the scoreboard reads through. Durations track
     the JS FB.preroll via the --fbPre custom property. */
  .fb-tele{inset:0;overflow:hidden}
  .fb-overlay.tele .fb-tele{animation:fbTele var(--fbPre,5000ms) ease-out forwards}
  .fb-tele-vig{position:absolute;inset:0;background:radial-gradient(circle at 50% 50%,transparent 40%,rgba(255,40,80,.34) 100%);opacity:0}
  .fb-overlay.tele .fb-tele-vig{animation:fbTeleVig var(--fbPre,5000ms) ease-in-out forwards}
  .fb-tele-ban{position:absolute;left:50%;top:15vh;transform:translateX(-50%);display:flex;align-items:center;gap:14px;white-space:nowrap;opacity:0}
  .fb-tele-ban .fb-tele-txt{font-family:'Press Start 2P';font-size:clamp(13px,2.4vw,28px);color:#fff;letter-spacing:2px;text-shadow:0 0 16px rgba(255,59,91,.95)}
  .fb-overlay.tele .fb-tele-ban{animation:fbTeleBan var(--fbPre,5000ms) ease-in-out forwards}
  @keyframes fbTele{0%{opacity:0}10%{opacity:1}100%{opacity:1}}
  @keyframes fbTeleVig{0%{opacity:0}30%{opacity:.45}100%{opacity:1}}
  @keyframes fbTeleBan{0%{opacity:0;transform:translateX(-50%) scale(1.25);letter-spacing:8px}
    18%{opacity:1;transform:translateX(-50%) scale(1);letter-spacing:2px}
    55%{opacity:.74}82%{opacity:1}100%{opacity:1}}

  /* match countdown + freeze pills */
  .freezepill{display:none;font-family:'Press Start 2P';font-size:9px;color:#bfe9ff;
    border:1px solid rgba(120,200,255,.5);padding:6px 9px;background:rgba(120,200,255,.1);
    text-shadow:0 0 8px rgba(120,200,255,.8);animation:freezePulse 1.6s ease-in-out infinite}
  .freezepill.show{display:block}
  @keyframes freezePulse{50%{box-shadow:0 0 14px rgba(120,200,255,.5)}}
  .panel.rank.frozen{box-shadow:inset 0 0 34px rgba(120,200,255,.16);border-color:rgba(120,200,255,.45)}
  .panel.rank.frozen .phead .t{color:#bfe9ff}
  .panel.rank.frozen .rk{filter:saturate(.85)}
  .btn.end{background:#ff5b6e;color:#1a0508;box-shadow:0 0 14px rgba(255,91,110,.5)}
  .btn.frz{background:#7fd7ff;color:#06121a;box-shadow:0 0 14px rgba(127,215,255,.5)}

  /* ===== SCOREBOARD FREEZE CINEMATIC ===== */
  /* ===== WINDOW FROST: dark wash + 2D-canvas corner frost + frosted-glass panel ===== */
  .fz-overlay{position:fixed;inset:0;z-index:96;pointer-events:none;visibility:hidden;overflow:hidden}
  .fz-overlay.show{visibility:visible}
  .fz-dark{position:absolute;inset:0;opacity:0;transition:opacity .5s;background:radial-gradient(circle at 50% 44%,#103257,#0a1f3c 55%,#040b18)}
  .fz-overlay.show .fz-dark{opacity:.97}
  .fz-cv{position:absolute;inset:0;width:100%;height:100%}
  .fz-vig{position:absolute;inset:0;background:radial-gradient(circle at 50% 46%,transparent 42%,rgba(2,8,20,.6) 100%)}
  .fz-overlay .fz-core{position:absolute;inset:0;display:flex;align-items:center;justify-content:center;text-align:center}
  .fz-panel{position:relative;display:flex;flex-direction:column;align-items:center;gap:clamp(8px,1.6vh,16px);
    padding:clamp(22px,3.4vw,40px) clamp(30px,5vw,66px);border-radius:18px;opacity:0;overflow:hidden;
    background:linear-gradient(160deg,rgba(180,220,255,.10),rgba(120,180,255,.04));border:1px solid rgba(180,225,255,.28);
    box-shadow:0 24px 70px rgba(0,18,45,.55),inset 0 1px 0 rgba(255,255,255,.22),inset 0 0 48px rgba(150,210,255,.10);
    -webkit-backdrop-filter:blur(7px) saturate(1.15);backdrop-filter:blur(7px) saturate(1.15)}
  .fz-overlay.show .fz-panel{animation:fzPanelIn .8s .15s cubic-bezier(.2,1.1,.3,1) forwards}
  .fz-panel::after{content:'';position:absolute;top:0;bottom:0;width:55%;left:-70%;pointer-events:none;
    background:linear-gradient(105deg,transparent,rgba(230,245,255,.30),transparent);transform:skewX(-18deg)}
  .fz-overlay.show .fz-panel::after{animation:fzSweep 2.4s 1.1s ease-in-out}
  .fz-brk{position:absolute;width:18px;height:18px;border:2px solid #8fdcff;box-shadow:0 0 8px rgba(140,220,255,.7);opacity:.9}
  .fz-brk.tl{top:10px;left:10px;border-right:0;border-bottom:0}.fz-brk.tr{top:10px;right:10px;border-left:0;border-bottom:0}
  .fz-brk.bl{bottom:10px;left:10px;border-right:0;border-top:0}.fz-brk.br{bottom:10px;right:10px;border-left:0;border-top:0}
  .fz-badge{color:#cdeaff;filter:drop-shadow(0 0 18px rgba(150,210,255,.9));opacity:0;width:clamp(64px,9vw,92px)}
  .fz-badge svg{display:block;width:100%;height:auto}
  .fz-overlay.show .fz-badge{animation:fzIcoIn .9s .25s cubic-bezier(.2,1.3,.3,1) forwards}
  .fz-title{font-family:'Press Start 2P';font-size:clamp(18px,4.4vw,46px);color:#eef8ff;line-height:1.14;letter-spacing:2px;
    text-shadow:0 0 22px rgba(140,210,255,.95),0 0 6px rgba(180,230,255,.8),0 2px 0 rgba(20,50,90,.5);opacity:0}
  .fz-overlay.show .fz-title{animation:fzTitleIn .8s .3s cubic-bezier(.2,1.3,.3,1) forwards}
  .fz-bar{width:min(280px,66vw);height:9px;border:1px solid rgba(150,205,255,.34);border-radius:6px;overflow:hidden;background:rgba(10,26,50,.5);opacity:0}
  .fz-overlay.show .fz-bar{animation:fzFade .4s .6s forwards}
  .fz-fill{display:block;height:100%;width:0;background:linear-gradient(90deg,#5fc4ff,#bdf0ff);box-shadow:0 0 14px rgba(150,220,255,.8)}
  .fz-overlay.show .fz-fill{animation:fzFill 1.1s .65s cubic-bezier(.3,.8,.3,1) forwards}
  .fz-secured{font-family:'VT323';font-size:clamp(12px,1.6vw,17px);color:#8fdcff;letter-spacing:2px;opacity:0;text-shadow:0 0 10px rgba(140,210,255,.6)}
  .fz-overlay.show .fz-secured{animation:fzFade .5s 1.05s forwards}
  .fz-count{font-family:'VT323';font-size:clamp(20px,3vw,34px);color:#eef8ff;letter-spacing:3px;text-shadow:0 0 14px rgba(150,210,255,.9);opacity:0}
  .fz-overlay.show .fz-count{animation:fzFade .6s 1.2s forwards}
  @keyframes fzPanelIn{0%{opacity:0;transform:translateY(16px) scale(.96)}100%{opacity:1;transform:translateY(0) scale(1)}}
  @keyframes fzSweep{0%{left:-70%}60%,100%{left:160%}}
  @keyframes fzIcoIn{0%{opacity:0;transform:scale(1.7) rotate(-10deg);filter:blur(6px)}60%{opacity:1;transform:scale(1) rotate(0);filter:blur(0)}100%{opacity:1}}
  @keyframes fzTitleIn{0%{opacity:0;transform:scale(1.5);filter:blur(8px)}60%{opacity:1;transform:scale(1);filter:blur(0)}100%{opacity:1}}
  @keyframes fzFill{to{width:100%}}
  @keyframes fzFade{to{opacity:1}}

  /* ===== MATCH WINNER SCREEN ===== */
  /* ===== MATCH COMPLETE — PODIUM SPOTLIGHT (gold god-rays canvas + tiered podium) ===== */
  .win-overlay{position:fixed;inset:0;z-index:97;pointer-events:none;opacity:0;visibility:hidden;overflow:hidden;
    background:radial-gradient(circle at 50% 40%,rgba(60,44,8,.72),rgba(3,2,8,.97));transition:opacity .5s}
  .win-overlay.show{opacity:1;visibility:visible;pointer-events:auto}
  .win-cv{position:absolute;inset:0;width:100%;height:100%;z-index:0}
  .win-core{position:absolute;inset:0;z-index:1;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:clamp(6px,1.4vh,14px);text-align:center;padding:18px}
  .win-overlay.show .win-core{animation:winIn .7s cubic-bezier(.2,1.3,.3,1) both}
  @keyframes winIn{0%{opacity:0;transform:translateY(24px) scale(.92)}100%{opacity:1;transform:none}}
  .win-eyebrow{font-family:'Press Start 2P';font-size:clamp(8px,1.2vw,12px);color:var(--amber);letter-spacing:2px;text-shadow:0 0 10px rgba(255,198,55,.6)}
  .win-title{font-family:'Press Start 2P';font-size:clamp(18px,3.6vw,38px);color:#fff;line-height:1;letter-spacing:2px;
    text-shadow:0 0 24px rgba(255,198,55,.8),3px 0 var(--amber),-3px 0 #ff7a3a}
  /* tiered podium: 1st tall/centre/gold, 2nd left/silver, 3rd right/bronze */
  .podium{display:flex;gap:clamp(8px,1.6vw,22px);align-items:flex-end;justify-content:center;margin-top:clamp(8px,1.6vh,18px)}
  .pod{display:flex;flex-direction:column;align-items:center;gap:6px;opacity:0}
  .pod .pcrown{width:clamp(34px,4.4vw,58px);color:var(--amber);filter:drop-shadow(0 0 12px rgba(255,198,55,.9));animation:crownBob 2.6s ease-in-out infinite}
  .pod .pcrown svg{display:block;width:100%;height:auto}
  @keyframes crownBob{50%{transform:translateY(-6px)}}
  .pod .pav{aspect-ratio:1;border-radius:50%;overflow:hidden;border:3px solid var(--c);background:#0a0818;box-shadow:0 0 26px -4px var(--c)}
  .pod .pav svg{display:block;width:100%;height:100%}
  .pod .pn{font-family:'Press Start 2P';font-size:clamp(7px,.9vw,10px);max-width:clamp(70px,12vw,160px);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .pod .ps{font-family:'VT323';font-size:clamp(16px,2.2vw,28px);color:#fff;line-height:.9}
  .pod .ped{display:flex;align-items:flex-start;justify-content:center;width:clamp(66px,10vw,116px);border-radius:6px 6px 0 0;padding-top:6px;
    background:linear-gradient(180deg,rgba(255,210,120,.20),rgba(255,180,70,.05));border:1px solid rgba(255,200,90,.3);border-bottom:0;box-shadow:inset 0 1px 0 rgba(255,255,255,.12)}
  .pod .rk{font-family:'Press Start 2P';font-size:clamp(9px,1.3vw,15px);color:var(--c);text-shadow:0 0 8px currentColor}
  .pod.p1{order:2;--c:var(--amber)}.pod.p2{order:1;--c:#cfe0ee}.pod.p3{order:3;--c:#e0975a}
  .pod.p1 .pav{width:clamp(74px,10.5vw,132px);border-width:4px}
  .pod.p2 .pav{width:clamp(52px,7vw,90px)}.pod.p3 .pav{width:clamp(46px,6vw,78px)}
  .pod.p1 .ped{height:clamp(40px,6vw,82px)}
  .pod.p2 .ped{height:clamp(28px,4.4vw,58px)}
  .pod.p3 .ped{height:clamp(20px,3.4vw,44px)}
  .win-overlay.show .pod.p2{animation:podrise .6s .85s ease-out forwards}
  .win-overlay.show .pod.p1{animation:podrise .6s .98s cubic-bezier(.2,1.3,.3,1) forwards}
  .win-overlay.show .pod.p3{animation:podrise .6s 1.1s ease-out forwards}
  @keyframes podrise{0%{opacity:0;transform:translateY(28px)}100%{opacity:1;transform:translateY(0)}}
  .btn.rematch{margin-top:16px;pointer-events:auto;font-size:11px;padding:12px 22px;background:var(--amber);color:#1c1400;box-shadow:0 0 20px rgba(255,198,55,.6)}

  @media (max-width:900px){
    :host{overflow-y:auto;position:absolute}
    .shell{height:auto;min-height:100vh}
    .midrow{display:flex;flex-direction:column;gap:12px}
    /* stack the wheel + the jeopardy constellations in normal flow and let the page scroll;
       no fullscreen on mobile (the square wheel + below-stack don't fit a phone fullscreen) */
    .arena-wrap{order:-1;height:auto;padding:8px;flex-direction:column;justify-content:flex-start;overflow:visible}
    .arena{width:min(92vw,560px);height:auto;max-height:none;flex:0 0 auto}
    #jeopSpace{display:block;width:100%}
    .fs-btn{display:none}
    .panel.log-panel,.rightcol{display:flex}
    .panel.log-panel{order:1}.rightcol{order:2}
    #log{height:30vh;flex:none}
    .panel.rank{flex:none}
    #ranklist{max-height:48vh;overflow-y:auto}
  }
  @media (max-width:680px){
    .shell{padding:8px;gap:8px}
    .topbar{flex-wrap:wrap;gap:8px;padding:8px 12px}
    .brand{gap:9px}.brand .logo{font-size:12px}
    .topright{gap:11px}
    .devbar{flex-wrap:wrap;gap:7px;justify-content:center}
    .btn{font-size:7px;padding:7px 8px}
    .arena{width:min(96vw,100%)}
    #log{font-size:13px;height:25vh}
    .sp{display:none}
  }
  :where(button,input):focus-visible{outline:3px solid var(--cyan);outline-offset:3px}
  @media (prefers-reduced-motion:reduce){
    *,*::before,*::after{scroll-behavior:auto!important;animation-duration:.01ms!important;animation-iteration-count:1!important;transition-duration:.01ms!important}
    .grain{display:none}
  }
`

/* -------------------------------------------------------------------------- */
/* Scene markup. Dynamic regions (#svg / #log / #ranklist / #stats / FB       */
/* portraits) are populated by the engine.                                    */
/* -------------------------------------------------------------------------- */
const ARENA_BODY = `
  <div class="circuit"></div>
  <div class="shell">
    <div class="topbar">
      <div class="brand">
        <div class="logo" id="brandLogo">CYBER<b>A/D</b>.ARENA</div>
      </div>
      <div class="topright">
        <div class="freezepill" id="freezeTag">&#10052; FROZEN</div>
        <div class="countpill" id="countPill" role="timer" aria-label="Time remaining">0:00</div>
        <div class="roundpill" id="roundPill">TICK</div>
      </div>
    </div>
    <div class="midrow">
      <div class="panel log-panel">
        <div class="phead accent-m"><span class="t">BATTLE LOG</span></div>
        <div id="log" role="log" aria-live="polite" aria-label="Battle event log"></div>
      </div>
      <div class="panel arena-wrap accent-v">
        <button id="fsBtn" class="fs-btn" title="Fullscreen battle map" aria-label="Fullscreen">⛶</button>
        <svg id="jeop" preserveAspectRatio="none" aria-label="Jeopardy challenge constellation"></svg>
        <div class="arena" id="arena">
          <canvas id="fxbg" width="870" height="870" aria-hidden="true"></canvas>
          <svg id="svg" viewBox="0 0 1000 1000" preserveAspectRatio="xMidYMid meet" aria-label="Live attack and defense map"></svg>
          <canvas id="fx" width="870" height="870" aria-hidden="true"></canvas>
        </div>
        <div id="jeopSpace"></div>
        <div id="jtip" class="jtip"></div>
      </div>
      <div class="rightcol">
        <div class="panel rank">
          <div class="phead accent-c"><span class="t">RANKING</span><span class="rank-tabs" id="rankTabs"><button data-rm="ad" class="on" aria-pressed="true">A&amp;D</button><button data-rm="koth" aria-pressed="false">KOTH</button><button data-rm="jeopardy" aria-pressed="false">JEO</button></span></div>
          <div id="ranklist" aria-label="Live team ranking"></div>
        </div>
      </div>
    </div>
    <div class="devbar">
      <span class="label">VIEW</span>
      <button class="btn ghost" id="speedBtn" aria-pressed="false">SPEED 1X</button>
      <button class="btn ghost on" id="soundBtn" aria-pressed="true">SOUND</button>
      <span id="cfgBtns" style="display:none">
        <label class="cfg">TEAMS<input id="cfgTeams" type="number" min="2" max="20" value="8"></label>
        <label class="cfg">A&amp;D<input id="cfgAd" type="number" min="0" max="10" value="4"></label>
        <label class="cfg">KOTH<input id="cfgKoth" type="number" min="0" max="12" value="3"></label>
        <label class="cfg">JEOP<input id="cfgJeop" type="number" min="0" max="40" value="24"></label>
      </span>
      <span id="fbBtns" style="display:none">
        <button class="btn fb-ad" id="fbAdBtn">FB A&amp;D</button>
        <button class="btn fb-jeo" id="fbJeoBtn">FB JEO</button>
        <button class="btn fb-koth" id="fbKothBtn">FB KOTH</button>
        <button class="btn patch" id="patchBtn">PATCH</button>
        <button class="btn frz" id="freezeBtn">FREEZE</button>
        <button class="btn end" id="endBtn">END</button>
      </span>
      <span class="sp"></span>
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
  const timers: number[] = []
  let raf = 0
  let liveClockStarted = false,
    livePollStarted = false
  let ws: WebSocket | null = null
  let wsRetry = 0,
    reconnectTimer = 0 // single reconnect handle (never >1 pending) — don't accumulate in timers[]

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
    RING = 362,
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
  const isTouch = 'ontouchstart' in window || navigator.maxTouchPoints > 0
  // Pixi v8 WebGL renderer for the jeopardy constellation layer (its own overlay canvas on
  // .arena-wrap, wrap-pixel space). When ready it takes over the star twinkle + lasers from the
  // SVG (which kept 40 infinite CSS animations); SVG stays as the static text + hit-test layer.
  const wrapEl: any = root.querySelector('.arena-wrap')
  const jeopRenderer = createJeopRenderer(wrapEl, { onReady: () => jeop.syncPixi() })
  const jeop = createJeopardy({
    root,
    arena,
    isFrozen: () => frozen,
    isTouch,
    pixiReady: () => jeopRenderer.ready,
    onStars: (cats, dense) => jeopRenderer.setStars(cats, dense),
    onBeam: (tx, ty, sx, sy, sr, col) => jeopRenderer.beam(tx, ty, sx, sy, sr, col),
    onFlash: (x, y, r, col) => jeopRenderer.flash(x, y, r, col),
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

    // COLOSSEUM SECTORS — each team gets a donut wedge "seat" (alternating fill) with a
    // divider at every boundary; teams ring the rim, the central pit (hills + open core)
    // stays clear. RIN = inner edge of the seating band; bases sit on the cyan ring at RING.
    const step = 360 / TEAMS.length,
      R = 470,
      RIN = 224
    const polar = (r: number, deg: number): [number, number] => {
      const a = (deg * Math.PI) / 180
      return [CX + r * Math.cos(a), CY + r * Math.sin(a)]
    }
    TEAMS.forEach((t, i) => {
      const d0 = -90 + i * step - step / 2,
        d1 = -90 + i * step + step / 2
      const [ox0, oy0] = polar(R, d0),
        [ox1, oy1] = polar(R, d1)
      const [ix0, iy0] = polar(RIN, d0),
        [ix1, iy1] = polar(RIN, d1)
      svg.appendChild(
        el('path', {
          d: `M${ix0.toFixed(1)} ${iy0.toFixed(1)} L${ox0.toFixed(1)} ${oy0.toFixed(1)} A${R} ${R} 0 0 1 ${ox1.toFixed(1)} ${oy1.toFixed(1)} L${ix1.toFixed(1)} ${iy1.toFixed(1)} A${RIN} ${RIN} 0 0 0 ${ix0.toFixed(1)} ${iy0.toFixed(1)} Z`,
          fill: t.color,
          opacity: i % 2 ? 0.05 : 0.1,
        })
      )
    })

    const ringG = el('g', {})
    // bold outer rim + inner seating-band ring; clean cyan ring where the bases stand
    ringG.appendChild(el('circle', { cx: CX, cy: CY, r: R, fill: 'none', stroke: 'var(--line2)', 'stroke-width': 1.6 }))
    ringG.appendChild(
      el('circle', {
        cx: CX,
        cy: CY,
        r: RIN,
        fill: 'none',
        stroke: 'var(--line)',
        'stroke-width': 1,
        'stroke-opacity': 0.5,
      })
    )
    // NO ring at the avatar radius (RING) — that circle ran right through all the bases and read
    // as a line connecting the avatars. NO radial dividers either; the seats read from the
    // alternating colour fill alone.
    svg.appendChild(ringG)

    // arena center (the pit) is intentionally left open — hills sit just inside the inner ring

    HILLS.forEach((h) => svg.appendChild(buildHill(h)))
    TEAMS.forEach((t) => svg.appendChild(buildBase(t)))
    TEAMS.forEach((t) => renderSvc(t))
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

  function labelOffset(t: any) {
    const c = Math.cos(t.ang),
      s = Math.sin(t.ang)
    let x = 0,
      y = -58,
      anc = 'middle'
    if (s < -0.5) {
      y = -60
    } else if (s > 0.5) {
      y = 72
    }
    if (c > 0.5) {
      x = 58
      anc = 'start'
      y = -4
    } else if (c < -0.5) {
      x = -58
      anc = 'end'
      y = -4
    }
    if (Math.abs(s) > 0.85) {
      x = 0
      anc = 'middle'
      y = s < 0 ? -62 : 72
    }
    return { x, y, anc }
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
    const c = t.color,
      L = t.look,
      idx = t.idx,
      hair = L.hair
    const g = el('g', { id: 'base-' + t.id, transform: `translate(${t.x} ${t.y})` })
    const body = `
      <rect x="-7" y="14" width="6" height="13" rx="3" fill="#14122e" stroke="${c}" stroke-width="1.2"/>
      <rect x="1" y="14" width="6" height="13" rx="3" fill="#14122e" stroke="${c}" stroke-width="1.2"/>
      <path d="M-13 0 Q-13 -3 -9 -3 L9 -3 Q13 -3 13 0 L13 14 Q13 18 9 18 L-9 18 Q-13 18 -13 14 Z" fill="#1b1838" stroke="${c}" stroke-width="2"/>
      <path d="M-13 0 Q-13 -3 -9 -3 L0 -3 L0 18 L-9 18 Q-13 18 -13 14 Z" fill="${c}" opacity="0.8"/>
      <path d="M-9 -3 L0 5 L9 -3 Z" fill="#0c0a1c"/>
      <circle cx="0" cy="8" r="3" fill="${c}"/>
      <rect x="-17" y="2" width="5" height="12" rx="2.5" fill="#14122e" stroke="${c}" stroke-width="1.2"/>
      <rect x="12" y="2" width="5" height="12" rx="2.5" fill="#14122e" stroke="${c}" stroke-width="1.2"/>`
    let back = ''
    if (L.style === 'pony') back += `<path d="M11 -16 Q28 -8 22 12 Q19 0 9 -4 Z" fill="${hair}"/>`
    if (L.style === 'long')
      back += `<path d="M-15 -12 Q-18 -30 0 -30 Q18 -30 15 -12 L16 12 L11 12 Q12 -14 0 -14 Q-12 -14 -11 12 L-16 12 Z" fill="${hair}" opacity="0.95"/>`
    if (L.style === 'twin')
      back += `<path d="M-14 -14 Q-22 -2 -18 12 Q-15 2 -10 -4 Z" fill="${hair}"/><path d="M14 -14 Q22 -2 18 12 Q15 2 10 -4 Z" fill="${hair}"/>`
    if (L.prop === 'katana')
      back += `<g transform="rotate(-26)"><rect x="13" y="-32" width="2.6" height="34" rx="1.2" fill="#e6ecff"/><rect x="11" y="0" width="7" height="3" rx="1" fill="${c}"/><rect x="13.4" y="3" width="2" height="9" rx="1" fill="#2a2740"/></g>`
    let props = ''
    if (L.prop === 'orb')
      props += `<circle cx="21" cy="7" r="8" fill="none" stroke="${c}" stroke-width="0.9" opacity="0.5"/><circle cx="21" cy="7" r="4.6" fill="${c}"/><circle cx="19.4" cy="5.6" r="1.4" fill="#fff" opacity="0.8"/>`
    if (L.prop === 'gaunt')
      props += `<rect x="13" y="9" width="11" height="10" rx="2.5" fill="#1b1838" stroke="${c}" stroke-width="1.6"/><rect x="14.5" y="10.5" width="8" height="2.4" fill="${c}"/>`
    if (L.prop === 'kunai')
      props += `<g transform="rotate(28 20 8)"><path d="M20 0 L24 7 L20 9 L16 7 Z" fill="#dfe6ff"/><rect x="19" y="9" width="2" height="6" fill="#2a2740"/><circle cx="20" cy="16" r="2.2" fill="none" stroke="#dfe6ff" stroke-width="1.2"/></g>`
    if (L.prop === 'shield')
      props += `<g transform="translate(-20 4)"><path d="M0 -7 L8 -4 Q8 7 0 13 Q-8 7 -8 -4 Z" fill="#1b1838" stroke="${c}" stroke-width="1.6"/><circle cx="0" cy="1" r="2.4" fill="${c}"/></g>`

    const head = chibiHead(L, c)
    const flag = `<line x1="-17" y1="-2" x2="-17" y2="-25" stroke="#cfd2ee" stroke-width="1.6"/><path d="M-17 -25 L-33 -21 L-17 -18 Z" fill="${c}" stroke="#0a0818" stroke-width="0.8"/>`
    const lo = labelOffset(t)
    g.innerHTML = `
      <ellipse cx="0" cy="30" rx="40" ry="13" fill="${c}" opacity="0.13" filter="url(#soft)"/>
      <ellipse cx="0" cy="31" rx="22" ry="6" fill="#000" opacity="0.42"/>
      <ellipse cx="0" cy="28" rx="24" ry="7" fill="#0b0a1c" stroke="${c}" stroke-width="1.6" stroke-opacity="0.75"/>
      <ellipse cx="0" cy="28" rx="14" ry="3.6" fill="none" stroke="${c}" stroke-width="1" stroke-opacity="0.4"/>
      <g class="u-float" style="animation-delay:${(idx * 0.34).toFixed(2)}s">
        ${back}${body}${props}${head}${flag}
      </g>
      <g id="svc-${t.id}" transform="translate(0 41)"></g>
      <text x="${lo.x}" y="${lo.y}" text-anchor="${lo.anc}" fill="#fff" font-family="'Press Start 2P'" font-size="11" paint-order="stroke" stroke="#06050f" stroke-width="4">${esc(t.name)}</text>
      <text id="sc-${t.id}" x="${lo.x}" y="${lo.y + 18}" text-anchor="${lo.anc}" fill="${c}" font-family="'VT323'" font-size="22" paint-order="stroke" stroke="#06050f" stroke-width="4">${t.score}</text>`
    return g
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
    if (e) e.textContent = fmtAdScore(adScore(t))
  }
  function pulseBase(t: any, col: string) {
    if (frozen) return
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
    jeop.layout() // re-place the jeopardy constellations (and hand the laid-out stars to jeopRenderer via onStars)
    // size the wrap-space Pixi jeopardy canvas AFTER layout() (which may grow the wrap via #jeopSpace).
    const wr = wrapEl.getBoundingClientRect()
    if (wr.width > 1) jeopRenderer.resize(wr.width, wr.height, r.left - wr.left, r.top - wr.top, r.width)
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
  const onVis = () => snd.resume()
  document.addEventListener('visibilitychange', onVis)

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
    if (frozen || ambientTick++ % 2) return // ~30fps ambient; skip entirely while frozen (overlay covers it)
    const TAU = 6.2832
    ctxbg.clearRect(0, 0, 1000, 1000)
    // the colosseum ring is static SVG now (no rotating recon rings) — the ambient canvas
    // only carries the per-avatar breathing aura (replaces the per-avatar SVG bob)
    for (const t of TEAMS) {
      const p = (Math.sin((T * TAU) / 2.8 + t.idx * 0.7) + 1) / 2
      const rad = 24 + 5 * p
      ctxbg.globalAlpha = 0.18 + 0.13 * p
      ctxbg.drawImage(glowSprite(t.color), t.x - rad, t.y - 4 - rad, rad * 2, rad * 2)
    }
    ctxbg.globalAlpha = 1
  }

  function fireShot(from: any, to: any, col: string, miss = false) {
    if (frozen || document.hidden || shots.length > 220) return // cap: a huge flag burst can't unbound the queue
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
      i ? ctx.lineTo(px, py) : ctx.moveTo(px, py)
    }
    ctx.closePath()
  }
  function spawnShield(x: number, y: number, col: string) {
    if (frozen || document.hidden) return
    fxq.push({ kind: 'shield', x, y, col, t: 0, dur: 0.95 })
    for (let i = 0; i < 10; i++) {
      const a = -1.57 + rng(-1, 1)
      const v = rng(70, 150)
      sparks.push({ x: x + rng(-14, 14), y: y + 10, vx: Math.cos(a) * v * 0.4, vy: -Math.abs(v), life: 1, col })
    }
  }
  function spawnDown(x: number, y: number, col: string) {
    if (frozen || document.hidden) return
    fxq.push({ kind: 'down', x, y, col, t: 0, dur: 1.0 })
    for (let i = 0; i < 14; i++) {
      const v = rng(60, 180)
      sparks.push({ x: x + rng(-12, 12), y: y - 6, vx: rng(-40, 40), vy: Math.abs(v), life: 1, col })
    }
  }
  function spawnBeam(from: any, to: any, col: string, big: boolean) {
    if (frozen || document.hidden) return
    fxq.push({ kind: 'beam', fx: from.x, fy: from.y, tx: to.x, ty: to.y, col, t: 0, dur: big ? 0.6 : 0.42, big: !!big })
  }
  function spawnCapture(from: any, hill: any, col: string) {
    if (frozen || document.hidden) return
    spawnBeam(from, hill, col, false)
    fxq.push({ kind: 'shield', x: hill.x, y: hill.y, col, t: 0, dur: 0.9 })
  }
  // replay a CSS class animation (toggle + forced reflow), then drop the class after ms
  function restartAnim(g: any, cls: string, ms: number) {
    if (!g) return
    g.classList.remove(cls)
    void g.offsetWidth
    g.classList.add(cls)
    setTimeout(() => g.classList.remove(cls), ms)
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
    setTimeout(() => d.remove(), 1100)
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
  // so a flag flurry / 256-deep WS reconnect catch-up can't flood timers+audio+DOM. The 15s poll is
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
        setTimeout(() => {
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
    setTimeout(() => {
      if (killed) return
      stopIncomingSound?.()
      stopIncomingSound = null
      ov.classList.remove('tele')
      void ov.offsetWidth
      ov.classList.add('play')
      // GPU slam graphics (dark+splash+flash), synced with the DOM text/bars. Burst behind the
      // hero title (its layout centre is stable under the scale animation), tinted per mode.
      const tr: any = ttl ? ttl.getBoundingClientRect() : null
      fbRenderer.play(
        FB.total,
        tr ? { cx: tr.left + tr.width / 2, cy: tr.top + tr.height / 2, palette: th.palette } : { palette: th.palette }
      )
      slamCovering = true // the dark slam overlay covers the board — pause the arena draw underneath
      // The procedural first-blood stinger fires with the reveal so it
      // punctuates the slam rather than the build-up.
      setTimeout(() => {
        if (killed || !snd.isEnabled()) return
        stopFirstBloodSound = snd.sfxFirstBlood()
      }, FB.soundDelay)
      setTimeout(() => {
        if (killed) return
        const sh: any = root.querySelector('.shell')
        if (sh) {
          sh.classList.add('shake')
          setTimeout(() => sh.classList.remove('shake'), 520)
        }
        if (opt.onImpact) opt.onImpact()
      }, FB.slam)
      setTimeout(() => {
        if (opt.beamTo) {
          spawnBeam(atkr, opt.beamTo, th.accent, true)
          if (opt.beamTo.id && opt.beamTo.color) pulseBase(opt.beamTo, opt.beamTo.color)
        }
      }, FB.total - 680)
      setTimeout(() => {
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
      const div = document.createElement('div')
      div.id = 'rk-' + t.id
      div.className = 'rk'
      div.innerHTML = `<div class="pos"></div>
        <div class="av">${avatar(t.look, t.color)}</div>
        <div class="body"><div class="nm" style="color:${t.color}">${esc(t.name)}</div>
          <div class="bars" id="bars-${t.id}"><i id="ba-${t.id}" title="Offense rate" style="background:#27e3ff"></i><i id="bd-${t.id}" title="Defense rate" style="background:#3dffb0"></i><i id="bf-${t.id}" title="SLA rate" style="background:#ffc637"></i></div></div>
        <div class="sc" id="rsc-${t.id}"></div>`
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
    sorted.forEach((t, i) => {
      const r = t._rk
      if (!r) return
      // only touch position/class when the rank actually moved
      if (r.lastPos !== i) {
        r.lastPos = i
        r.div.className = 'rk p' + (i + 1)
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
      fzRenderer.start()
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
        const finalBoard = await fetchJSON<AdScoreboardModel>(`/api/Game/${gameId}/Ad/Scoreboard`)
        if (killed) return
        // Recheck a possible organizer extension before committing the podium.
        try {
          const game: any = await fetchJSON(`/api/Game/${gameId}`)
          if (game?.end) gameEndMs = new Date(game.end).getTime()
        } catch (e) {}
        if (gameEndMs != null && Date.now() < gameEndMs - 1500) return
        const pureKoth = SERVICES.length === 0 && HILLS.length > 0
        if (pureKoth) {
          const finalKoth: any = await fetchJSON(`/api/Game/${gameId}/Ad/Koth/Scoreboard`)
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
          const finalKoth: any = await fetchJSON(`/api/Game/${gameId}/Ad/Koth/Scoreboard`)
          const generatedAt = new Date(finalKoth?.generatedAt).getTime()
          if (!Number.isFinite(generatedAt) || generatedAt + 1000 < finalBoard.generatedAt) return
          applyOfficialKothBoard(finalKoth)
        }
      } catch (e) {
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
            `<div class="pn" style="color:${team.color}">${esc(team.name)}</div>` +
            `<div class="ps">${fmtAdScore(team.score)}</div><div class="ped"><span class="rk">${lbl[i]}</span></div></div>`
          : ''
      )
      .join('')
    const ov = $('winOverlay')
    if (ov) ov.classList.add('show')
    winRenderer.start()
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
    const jeopActive = jeopRenderer.active() // GPU jeopardy stars twinkling / lasers in flight
    // !matchOver: after endMatch the opaque PODIUM win overlay (z97) covers the whole arena and
    // winRenderer runs its own loop on top — skip the ambient/jeop draw beneath it to save GPU.
    if (!slamCovering && !matchOver && !document.hidden && (fxActive || jeopActive || !frozen)) {
      drawFX(dt) // advances FX physics + ambient; draws the 2D fallback only while !fxRenderer.ready
      if (fxRenderer.ready) fxRenderer.tick(dt, shots, sparks, fxq) // WebGL render of the same arrays
      jeopRenderer.render(ts, frozen) // WebGL jeopardy star twinkle (30fps) + lasers; skips while frozen
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
  async function fetchJSON<T = any>(url: string): Promise<T> {
    const r = await fetch(url, { headers: { Accept: 'application/json' } })
    if (!r.ok) throw new Error(url + ' -> ' + r.status)
    return r.json()
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
          solvers: (c.bloods || []).map((b: any) => {
            const tm = teamByName(b.name)
            return { name: b.name || '', color: tm ? tm.color : '#7fd7ff' }
          }),
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
    const kothHills = koth && koth.hills ? koth.hills : []
    const kothIds = new Set(kothHills.map((h: any) => h.challengeId))
    const svcDefs = (ad.challenges || []).filter((c: any) => !kothIds.has(c.challengeId))
    SERVICES = svcDefs.map((c: any) => c.title)
    const svcIds = svcDefs.map((c: any) => c.challengeId)

    const adRows = ad.teams || []
    const hasOfficialAdRoster = adRows.length > 0
    const rosterRows: any[] = hasOfficialAdRoster
      ? adRows
      : ((koth && koth.teams) || []).map((row: any) => ({
          ...row,
          settledTotal: 0,
          projectedTotal: 0,
          offenseRate: 0,
          defenseRate: 0,
          slaRate: 0,
        }))

    TEAMS = rosterRows.map((row, i: number) => {
      const color = PALETTE[i % PALETTE.length]
      const t: any = {
        id: 'p' + row.participationId,
        pid: row.participationId,
        name: row.teamName,
        color,
        hue: Math.round((i * 137.508) % 360),
        score: Number(row.settledTotal) || 0,
        projectedScore: Number(row.projectedTotal) || 0,
        officialRank: row.rank || i + 1,
        offenseRate: boundedRate(row.offenseRate),
        defenseRate: boundedRate(row.defenseRate),
        slaRate: boundedRate(row.slaRate),
        captureEvidence: 0,
        onOfficialBoard: hasOfficialAdRoster,
        kothScore: 0,
        kothRank: null,
        jpScore: 0,
        jpSolved: 0,
      }
      // The official epoch board deliberately exposes no per-team service verdicts.
      // Keep challenge nodes neutral; the rank bars carry the official A/D/SLA rates.
      t.svc = svcIds.map((cid: any, j: number) => ({ name: SERVICES[j], cid, status: 'none' }))
      return t
    })

    TEAMS.forEach((t, i) => {
      const ang = ((-90 + i * (360 / TEAMS.length)) * Math.PI) / 180
      t.idx = i
      t.ang = ang
      t.x = CX + RING * Math.cos(ang)
      t.y = CY + RING * Math.sin(ang)
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

    if (!preview && SERVICES.length === 0 && HILLS.length > 0 && rankMode === 'ad') {
      rankMode = 'koth'
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
    jeop.initHover()
    applyAdRoundClock(ad)
    if (title) $('brandLogo').textContent = title.toUpperCase().slice(0, 22)
  }

  function applyOfficialAdBoard(ad: AdScoreboardModel) {
    const adById = new Map(ad.teams.map((row) => ['p' + row.participationId, row] as const))
    TEAMS.forEach((t) => {
      const row = adById.get(t.id)
      t.onOfficialBoard = Boolean(row)
      if (!row) return
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
      team.onOfficialBoard = Boolean(row)
      if (!row) return
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

  async function pollLive() {
    if (killed) return
    try {
      const ad = await fetchJSON<AdScoreboardModel>(`/api/Game/${gameId}/Ad/Scoreboard`)
      let koth: any = null
      try {
        koth = await fetchJSON(`/api/Game/${gameId}/Ad/Koth/Scoreboard`)
      } catch (e) {}
      let jp: any = null
      try {
        jp = await fetchJSON(`/api/Game/${gameId}/Scoreboard`)
      } catch (e) {}
      // refresh the real end time: an admin extending EndTimeUtc mid-match must move the podium
      // trigger (and un-stick it if the champions screen already showed) — gameEndMs was otherwise
      // read once at load and never updated. Keep the old value if the field is missing.
      try {
        const gi = await fetchJSON(`/api/Game/${gameId}`)
        if (gi && gi.end) gameEndMs = new Date(gi.end).getTime()
      } catch (e) {}
      if (matchOver && gameEndMs != null && Date.now() < gameEndMs - 1500) reopenMatch()
      if (!killed) applyLivePoll(ad, koth, jp)
    } catch (e) {
      /* transient */
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
    setTimeout(
      () => {
        pendingResolves--
        if (!killed) resolveFlag(atkr, vic, svc, pts, false)
      },
      320 / Math.max(speed, 1) + 120
    )
  }
  // Hill ownership is driven by BOTH the WS koth frame (instant) and the 15s poll
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

  function connectWS() {
    if (killed) return
    const proto = location.protocol === 'https:' ? 'wss' : 'ws'
    ws = new WebSocket(`${proto}://${location.host}/hub/attack/ws?game=${gameId}`)
    ws.onopen = () => {
      wsRetry = 0
    }
    ws.onmessage = (m) => {
      if (killed) return
      let f: any
      try {
        f = JSON.parse(m.data)
      } catch (e) {
        return
      }
      if (!f || !f.kind) return
      if (f.kind === 'attack') liveAttack(f)
      else if (f.kind === 'koth') liveKoth(f)
      else if (f.kind === 'patch') livePatch(f)
    }
    ws.onclose = () => {
      if (killed) return
      wsRetry = Math.min(wsRetry + 1, 6)
      reconnectTimer = window.setTimeout(connectWS, 1000 * wsRetry)
    }
    ws.onerror = () => {
      try {
        if (ws) ws.close()
      } catch (e) {}
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

  // Backfill the battle log with recent attacks so a refresh doesn't start empty.
  // History only — scores come from the 15s poll, and we deliberately skip the map
  // cinematics (replaying ~50 events would be a flurry of noise). Oldest-first from the
  // server; the backend returns [] for Hidden/frozen games (matching the live gate).
  async function seedLog() {
    let evts: any[]
    try {
      evts = await fetchJSON(`/api/Game/${gameId}/AttackFeed?limit=50`)
    } catch (e) {
      return
    }
    if (!Array.isArray(evts) || !evts.length) return
    for (const f of evts) {
      if (!f || f.type === 'Unaccepted') continue
      const who = esc(f.teamName || '???')
      const svc = esc(f.challengeTitle || 'flag')
      const vic = f.victimTeamName ? esc(f.victimTeamName) : 'CORE'
      const capture = Boolean(f.victimTeamName)
      if (capture) {
        const team = teamByName(f.teamName)
        if (team) team.captureEvidence = (team.captureEvidence || 0) + 1
      }
      if (f.type === 'FirstBlood')
        addLog(
          'FIRST BLOOD',
          'fb',
          `<span class="who">${who}</span> drew first blood${capture ? ` on <span class="vic">${vic}</span> <span class="em">CAPTURE ACCEPTED</span>` : ''} :: <span class="svc">${svc}</span>`
        )
      else
        addLog(
          'FLAG',
          'flag',
          `<span class="who">${who}</span> &gt; <span class="vic">${vic}</span> :: <span class="svc">${svc}</span>${capture ? ' <span class="em">CAPTURE ACCEPTED</span>' : ''}`
        )
    }
    refreshRank()
  }

  async function start() {
    let ad: AdScoreboardModel
    try {
      ad = await fetchJSON<AdScoreboardModel>(`/api/Game/${gameId}/Ad/Scoreboard`)
    } catch (e) {
      if (killed) return
      showNote('NO LIVE A&amp;D DATA<br/>this game has no Attack &amp; Defense<br/>or King of the Hill challenges')
      addLog('SYS', 'sys', `<span class="em">NO A&amp;D / KOTH SCOREBOARD</span> for this game`)
      tNow = Date.now()
      ensureLiveLoops()
      return
    }
    let koth: any = null
    try {
      koth = await fetchJSON(`/api/Game/${gameId}/Ad/Koth/Scoreboard`)
    } catch (e) {}
    let jp: any = null
    try {
      jp = await fetchJSON(`/api/Game/${gameId}/Scoreboard`)
    } catch (e) {}
    let title: string | null = null
    try {
      const gi = await fetchJSON(`/api/Game/${gameId}`)
      title = gi && gi.title
      if (gi && gi.end) gameEndMs = new Date(gi.end).getTime()
    } catch (e) {}
    if (killed) return

    buildLiveModel(ad, koth, jp, title)
    if (!TEAMS.length) {
      showNote('WAITING FOR THE OFFICIAL A&amp;D ROSTER')
      tNow = Date.now()
      ensureLiveLoops()
      timers.push(
        window.setTimeout(() => {
          if (!killed) void start()
        }, 5000)
      )
      return
    }

    clearNote()
    buildArena()
    refreshRank()
    sizeCanvas()
    if (ad.isFrozenView) enterFreeze() // board already frozen when we connect
    await seedLog() // replay recent attacks first so the log survives a refresh
    if (killed) return
    addLog(
      'SYS',
      'sys',
      `<span class="em">ARENA ONLINE</span> :: ${TEAMS.length} teams, ${SERVICES.length} services, ${totalFlags} accepted captures`
    )

    if (!livePollStarted) {
      livePollStarted = true
      connectWS()
      timers.push(window.setInterval(pollLive, 15000))
    }
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
      const ang = ((-90 + i * (360 / Math.max(teamDefs.length, 1))) * Math.PI) / 180
      const t: any = {
        ...d,
        idx: i,
        ang,
        x: CX + RING * Math.cos(ang),
        y: CY + RING * Math.sin(ang),
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
    jeop.initHover()
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
    setTimeout(
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
    setTimeout(
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
    setTimeout(
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
      const gi = await fetchJSON(`/api/Game/${gameId}`)
      title = gi && gi.title
    } catch (e) {}
    if (killed) return
    bootDemoModel()
    if (title) $('brandLogo').textContent = title.toUpperCase().slice(0, 22)
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
      speedBtn.textContent = 'SPEED ' + speed + 'X'
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
    const fs = document.fullscreenElement === fsWrap
    const b = $('fsBtn')
    if (b) b.textContent = fs ? '✕' : '⛶'
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
        rankTabs.querySelectorAll('button').forEach((x: any) => {
          x.classList.toggle('on', x === b)
          x.setAttribute('aria-pressed', String(x === b))
        })
        setTickPill()
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
          } catch (e) {}
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

  if (preview) startPreview()
  else start()

  /* -------- teardown -------- */
  return () => {
    killed = true
    timers.forEach((id) => clearInterval(id))
    timers.forEach((id) => clearTimeout(id))
    clearTimeout(evTimer)
    clearTimeout(reconnectTimer) // cancel any pending WS reconnect so connectWS can't fire after killed
    if (raf) cancelAnimationFrame(raf)
    window.removeEventListener('resize', onResize)
    if (resizeRaf) cancelAnimationFrame(resizeRaf) // don't let a pending resize fire sizeCanvas into destroyed Pixi apps
    document.removeEventListener('visibilitychange', onVis)
    document.removeEventListener('pointerdown', primeAudio)
    document.removeEventListener('keydown', primeAudio)
    if ('speechSynthesis' in window) {
      try {
        speechSynthesis.cancel()
      } catch (e) {}
    }
    stopIncomingSound?.()
    stopIncomingSound = null
    stopFirstBloodSound?.()
    stopFirstBloodSound = null
    snd.close()
    document.removeEventListener('fullscreenchange', onFsChange)
    jeop.destroy()
    fxRenderer.destroy()
    jeopRenderer.destroy()
    fbRenderer.destroy()
    fzRenderer.destroy()
    winRenderer.destroy()
    if (ws) {
      try {
        ws.onclose = null
        ws.close()
      } catch (e) {}
      ws = null
    }
  }
}

/* -------------------------------------------------------------------------- */

const Attack: FC = () => {
  const { id } = useParams()
  const [searchParams] = useSearchParams()
  const preview = searchParams.has('preview')
  const hostRef = useRef<HTMLDivElement>(null)
  const cleanupRef = useRef<null | (() => void)>(null)

  useEffect(() => {
    const host = hostRef.current
    if (!host || !id) return

    // Google Fonts must live at document level so @font-face resolves inside
    // the shadow root (font-family references in a shadow root match
    // document-level @font-face rules).
    if (!document.getElementById('cyber-arena-fonts')) {
      const link = document.createElement('link')
      link.id = 'cyber-arena-fonts'
      link.rel = 'stylesheet'
      link.href = FONTS_HREF
      document.head.appendChild(link)
    }

    const shadow = host.shadowRoot ?? host.attachShadow({ mode: 'open' })
    shadow.innerHTML = `<style>${ARENA_CSS}</style>${ARENA_BODY}`
    cleanupRef.current = runArena(shadow, id, preview)

    return () => {
      cleanupRef.current?.()
      cleanupRef.current = null
    }
  }, [id, preview])

  return (
    <div
      ref={hostRef}
      id="main-content"
      role="main"
      tabIndex={-1}
      aria-label="Live attack and defense arena"
      style={{ position: 'fixed', inset: 0, zIndex: 100 }}
    />
  )
}

export default Attack
