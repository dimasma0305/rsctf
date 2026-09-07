// Read-only browser fixtures. Exercise real SPA links with live mixed-mode status.
import assert from 'node:assert/strict'
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs'
import {resolve} from 'node:path'
import {launchBrowser} from './cdp.mjs'

const target = process.env.RSCTF_WORKSPACE_PREVIEW || 'http://127.0.0.1:63017'
assert.equal(new URL(target).hostname, '127.0.0.1')
const output = resolve(process.env.RSCTF_WORKSPACE_OUTPUT || '../visual-audit-output/event-navigation')
mkdirSync(output, {recursive:true})
const now = Date.now()
const profile = {userId:'11111111-1111-4111-8111-111111111111', role:'Admin', userName:'Dimas', email:'test@example.invalid'}
const game = {id:901, title:'INTECHFEST 2026 Main Event', start:now-3600000, end:now+86400000, serverTime:now, status:'Accepted', joined:true, practiceMode:false, divisions:[], teamCount:50, writeupRequired:true}
const items = Array.from({length:12}, (_,i) => ({id:9001+i,title:['Parcel Panic','Serpent Circuit','Tower of Babel'][i] || `Challenge ${i+1}`,category:i%2?'Web':'Pwn',type:i===0?'AttackDefense':i===1?'KingOfTheHill':'StaticAttachment',score:500,solved:2,bloods:[],disableBloodBonus:true}))
const challenges = Object.groupBy(items, c=>c.category)
const rank = {id:7,name:'HIB — Stargazers',rank:7,score:3250,solvedCount:2,solvedChallenges:[]}
const state = {currentRound:178,startRound:42,epochTicks:8,roundEndsAt:now-5000,flagsReady:true,flagDeliveryFailures:17,scoringPaused:false,services:[]}
const responses = {
  '/api/account/profile':profile,
  '/api/config':{title:'INTECHFEST',slogan:'Capture the flag',portMapping:'Default',allowRegister:true,enableBrowserFingerprint:false},
  '/api/captcha':{type:'None'},
  '/api/game/901':game,
  '/api/game/901/details':{challenges,challengeCount:items.length,rank,teamToken:'fixture-only-not-a-credential'},
  '/api/game/901/details/participant':{rank},
  '/api/game/901/notices':[],
  '/api/game/901/scoreboard':{updateTimeUtc:now,bloodBonus:0,challenges,challengeCount:items.length,items:[rank],timelines:[],divisions:[]},
  '/api/game/901/ad/state':state,
  '/api/game/901/ad/token':{exists:false,hint:'',canManage:false,revision:1,participationId:7,teamId:7},
  '/api/game/901/events/page':[],
  '/api/game/901/events/backfill':{events:[],nextCursor:0,hasMore:false},
}
const {cdp, close} = await launchBrowser()
const reports=[], transitions=[], errors=[], unknown=new Set()
const evaluate = async expression => {
  const r=await cdp.send('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true})
  if(r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text)
  return r.result?.value
}
const waitFor = async expression => {
  for(let i=0;i<120;i++) {
    if(await evaluate(`Boolean(${expression})`)) return
    await new Promise(r=>setTimeout(r,200))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('document.body.innerText.slice(-2000)')}`)
}
const geometry = `(() => {
 const rect = selector => { const e=document.querySelector(selector); if(!e)return null; const r=e.getBoundingClientRect(); return {x:r.x,y:r.y,w:r.width,h:r.height}; };
 const surfaces = [...document.querySelectorAll('[data-motion="page"]')];
 const motion = surfaces.map(e => { const s = getComputedStyle(e); return { opacity:Number(s.opacity), transform:s.transform, animation:s.animationName }; });
 return {header:rect('[data-event-workspace-header]'), title:rect('[data-event-workspace-header] h1'), nav:rect('[data-event-tabs]'), track:rect('[data-event-tabs] nav'), rail:rect('#primary-navigation-rail'), mobileHeader:rect('header[data-guide-boundary="top-shell"]'), motion};
})()`
const inspect = async name => {
 await evaluate(readFileSync('node_modules/axe-core/axe.min.js','utf8'))
 const issues=await evaluate(`(async()=>({overflow:document.documentElement.scrollWidth>innerWidth+1, violations:(await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>({target:n.target,summary:n.failureSummary}))}))}))()`)
 const shot=await cdp.send('Page.captureScreenshot',{format:'png',captureBeyondViewport:false})
 writeFileSync(`${output}/${name}.png`,Buffer.from(shot.data,'base64'))
 reports.push({name,...issues,errors:[...errors]})
 console.log(name,JSON.stringify(issues))
}
try {
 await cdp.send('Page.enable'); await cdp.send('Runtime.enable')
 cdp.on('Runtime.exceptionThrown',e=>errors.push(e.exceptionDetails.exception?.description || e.exceptionDetails.text))
 await cdp.send('Page.addScriptToEvaluateOnNewDocument',{source:`if(location.origin===${JSON.stringify(target)}) {
 localStorage.setItem('language',JSON.stringify('en-US'));
 localStorage.setItem('mantine-color-scheme-value','dark');
 localStorage.setItem('challenge-explorer-view',JSON.stringify('list'));
 localStorage.setItem('scoreboard-tab-901',JSON.stringify('jeopardy'));
 localStorage.setItem('rsctf-player-guide:${profile.userId}',JSON.stringify({interactiveEnabled:false,completedVersion:1,seenFeatures:[]}));
 }`})
 await cdp.send('Fetch.enable',{patterns:[{urlPattern:`${target}/api/*`},{urlPattern:`${target}/hub*`}]})
 cdp.on('Fetch.requestPaused', async ({requestId,request})=>{
  const path=new URL(request.url).pathname.toLowerCase()
  let value=responses[path],status=200
  if(!['GET','HEAD'].includes(request.method)) {status=405;value={status,title:'Fixture mutation blocked'}}
  else if(value===undefined) {unknown.add(path);value=[]}
  await cdp.send('Fetch.fulfillRequest',{requestId,responseCode:status,responseHeaders:[{name:'Content-Type',value:'application/json'}],body:Buffer.from(JSON.stringify(value)).toString('base64')})
 })
 for(const [name,width,height,reduced=false] of [['desktop',1600,1000],['wide',1920,1080],['laptop',1024,768],['mobile',390,844],['compact',320,568],['tablet-reduced',768,1024,true]]) {
  await cdp.send('Emulation.setDeviceMetricsOverride',{width,height,deviceScaleFactor:1,mobile:false})
  await cdp.send('Emulation.setEmulatedMedia',{features:[{name:'prefers-reduced-motion',value:reduced?'reduce':'no-preference'}]})
  await cdp.send('Page.navigate',{url:`${target}/games/901/challenges`})
  await waitFor(`document.querySelector('[data-round-messages]')?.textContent.includes('17 flag deliveries')`)
  assert.match(await evaluate(`document.querySelector('[data-competition-status]').innerText`),/178/)
  assert.equal(await evaluate(`document.querySelector('[data-round-deadline]').textContent`),'Awaiting round update')
  assert.ok(await evaluate(`document.querySelector('[data-competition-status]').getBoundingClientRect().height < ${width>=1600?105:200}`),'round status must be a compact toolbar, not a tall card')
  await inspect(`${name}-live-challenges`)
  const baseline=await evaluate(geometry)
  for(const destination of ['scoreboard','monitor/events','challenges']) {
   const before=await evaluate(geometry)
   const alreadyVisible=await evaluate(`(() => {const item=document.querySelector('[data-event-workspace-header] a[href="/games/901/${destination}"]').getBoundingClientRect(); const view=document.querySelector('[data-event-tabs]').getBoundingClientRect(); return item.left>=view.left && item.right<=view.right;})()`)
   const frames=await evaluate(`new Promise(resolve=>{
    const frames=[]; const until=performance.now()+1000;
    const tick=()=>{frames.push(${geometry});if(performance.now()<until)requestAnimationFrame(tick);else resolve(frames)};
    requestAnimationFrame(tick);
    document.querySelector('[data-event-workspace-header] a[href="/games/901/${destination}"]').click();
   })`)
   await waitFor(`location.pathname.endsWith('/${destination}') && document.querySelector('[data-event-workspace-header] nav')`)
   if(destination === 'monitor/events') {
    await waitFor(`document.body.innerText.includes('No matching events')`)
    assert.equal(await evaluate(`document.querySelector('[role="tablist"][aria-label="Monitoring"]').getAttribute('aria-orientation')`),'horizontal')
   }
   transitions.push({name,destination,baseline,frames})
   assert.ok(frames.some(frame => frame.motion.length > 0), 'route content has a shared entrance surface')
   if(!reduced) assert.ok(frames.some(frame => frame.motion.some(m => m.opacity < 1 && m.animation === 'rsctf-content-in')), `${name}: new route content must ease in`)
   for(const frame of frames) {
    assert.ok(frame.header && frame.nav,`${name}: shared event header disappeared during navigation to ${destination}`)
    for(const part of ['header','title','nav']) for(const key of ['x','y','w','h']) assert.ok(Math.abs(frame[part][key]-baseline[part][key])<=1,`${name}: ${part}.${key} shifted from ${baseline[part][key]} to ${frame[part][key]} going to ${destination}`)
    if(alreadyVisible) assert.ok(Math.abs(frame.track.x-before.track.x)<=1,`${name}: an already-visible tab caused the navigation track to pan`)
    assert.equal(!!frame.rail,width>768,`${name}: wrong sidebar on initial route paint`)
    for(const motion of frame.motion) {
     assert.ok(motion.opacity >= 0.7 && motion.opacity <= 1, 'navigation never hides content')
     assert.equal(motion.transform,'none','page entrances must not move fixed/sticky descendants')
     if(reduced) assert.equal(motion.animation,'none','reduced motion disables content entrances')
    }
   }
   await inspect(`${name}-${destination.replaceAll('/','-')}`)
  }
  // A second navigation must not wait for the first route's entrance to finish.
  await evaluate(`(() => {
    document.querySelector('[data-event-workspace-header] a[href="/games/901/scoreboard"]').click();
    requestAnimationFrame(() => document.querySelector('[data-event-workspace-header] a[href="/games/901/challenges"]')?.click());
  })()`)
  await waitFor(`location.pathname.endsWith('/challenges') && document.querySelector('[data-challenge-list]')`)
  await evaluate(`Promise.all(document.getAnimations().filter(a => a.effect?.getComputedTiming().endTime !== Infinity).map(a => a.finished.catch(() => {})))`)
  assert.equal(await evaluate(`document.querySelectorAll('#main-content').length`),1,'rapid navigation never retains an outgoing page')
  assert.equal(await evaluate(`[...document.querySelectorAll('[data-motion]')].every(e=>getComputedStyle(e).opacity==='1' && getComputedStyle(e).transform==='none')`),true,'interrupted entrances settle into visible content')
 }
 // A paused round retains the frozen remaining time; a warmup never looks live.
 for(const phase of ['paused','warmup']) {
  Object.assign(state,phase==='paused'?{scoringPaused:true,scoringPausedAt:now,roundEndsAt:now+40000}:{currentRound:0,startRound:null,scoringPaused:false,roundEndsAt:null,flagDeliveryFailures:0})
  await cdp.send('Page.navigate',{url:`${target}/games/901/challenges`})
  await waitFor(`document.querySelector('[data-competition-status]')?.innerText.includes('${phase==='paused'?'40s':'Warmup'}')`)
  await inspect(`compact-${phase}`)
 }
} finally {
 writeFileSync(`${output}/report.json`,JSON.stringify({reports,transitions,unknown:[...unknown]},null,2))
 await close()
}
assert.deepEqual([...unknown],[])
assert.deepEqual(reports.filter(r=>r.overflow||r.violations.length||r.errors.length),[])
console.log(`PASS: ${reports.length} browser/Axe views and ${transitions.length} stable SPA transitions`)
