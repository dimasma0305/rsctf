// Fixed-input-rate browser A/B. Run against a production frontend build on loopback.
import assert from 'node:assert/strict'
import { mkdirSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'
import { arenaBrowserSetup, createArenaFixture } from './attack-arena-fixtures.mjs'

const target = process.env.RSCTF_ARENA_PREVIEW || 'http://127.0.0.1:18080'
assert.equal(new URL(target).hostname, '127.0.0.1')
const output = resolve(process.env.RSCTF_ANIMATION_OUTPUT || 'visual-audit-output/arena-animation')
mkdirSync(output, { recursive: true })
const browser = await launchBrowser(), { cdp } = browser
const data = createArenaFixture(), errors = [], requests = [], results = []
let documentGeneration = 0, closing = false, retiredReads = 0
const evaluate = async expression => {
  const response = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (response.exceptionDetails) throw Error(response.exceptionDetails.exception?.description || response.exceptionDetails.text)
  return response.result.value
}
try {
  await cdp.send('Page.enable'); await cdp.send('Runtime.enable'); await cdp.send('Performance.enable')
  cdp.on('Runtime.exceptionThrown', ({exceptionDetails}) => errors.push(exceptionDetails.text))
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', {source: arenaBrowserSetup})
  await cdp.send('Fetch.enable', {patterns:[{urlPattern:`${target}/api/*`},{urlPattern:`${target}/hub*`}]})
  cdp.on('Fetch.requestPaused', async ({requestId, request}) => {
    const generation = documentGeneration
    requests.push({url:request.url,method:request.method})
    const response = data.fixture(request.url, request.method)
    try {
      await cdp.send('Fetch.fulfillRequest', {requestId,responseCode:response.status,responseHeaders:[{name:'Content-Type',value:'application/json'}],body:Buffer.from(JSON.stringify(response.body)).toString('base64')})
    } catch (error) {
      // Navigating to the next viewport retires pending old-document reads.
      // Do not turn that expected cancellation into an unhandled rejection.
      if (error.message.includes('Invalid InterceptionId') && (generation !== documentGeneration || closing)) retiredReads++
      else errors.push(String(error))
    }
  })
  for (const width of [1600,390]) {
    await cdp.send('Emulation.setDeviceMetricsOverride', {width,height:1100,deviceScaleFactor:1,mobile:false})
    documentGeneration++
    await cdp.send('Page.navigate', {url:`${target}/games/901/attack`})
    await evaluate(`new Promise((resolve,reject)=>{let n=0;const timer=setInterval(()=>{const root=document.querySelector('[data-arena-theme]')?.shadowRoot;if(root?.querySelectorAll('.territory-choice').length===12){clearInterval(timer);resolve()}else if(++n>150){clearInterval(timer);reject(Error('arena did not load'))}},100)})`)
    for (const mode of ['rotation','focus']) {
      const before = (await cdp.send('Performance.getMetrics')).metrics
      const measurements = await evaluate(`new Promise(resolve=>{
        const root=document.querySelector('[data-arena-theme]').shadowRoot;
        const stage=root.getElementById('arena'); stage.scrollIntoView({block:'center'});
        const intervals=[], frames=[], jumps=[], longTasks=[];
        const start=performance.now();let previous=start,lastPaint=start,lastYaw=Number(stage.dataset.globeYaw),arrivals=0,changes=0;
        const observer=new PerformanceObserver(list=>longTasks.push(...list.getEntries().map(e=>e.duration)));observer.observe({type:'longtask'});
        const mutations=new MutationObserver(records=>changes+=records.filter(r=>r.type==='childList').length);mutations.observe(root.getElementById('conquestRoutes'),{childList:true,subtree:true});
        const timer=${JSON.stringify(mode)}==='focus'?setInterval(()=>{root.querySelectorAll('.territory-choice')[(arrivals++*5)%12].click()},500):null;
        function frame(now){
          intervals.push(now-previous);previous=now;
          const yaw=Number(stage.dataset.globeYaw);if(yaw!==lastYaw){frames.push(now-lastPaint);jumps.push(Math.abs(Math.atan2(Math.sin(yaw-lastYaw),Math.cos(yaw-lastYaw))));lastPaint=now;lastYaw=yaw}
          if(now-start<12000){requestAnimationFrame(frame);return}
          if(timer)clearInterval(timer);observer.disconnect();mutations.disconnect();
          const stats=values=>{const a=values.toSorted((a,b)=>a-b);const p=n=>a[Math.max(0,Math.ceil(a.length*n)-1)]||0;return {samples:a.length,avg:a.reduce((s,n)=>s+n,0)/(a.length||1),p50:p(.5),p90:p(.9),p95:p(.95),p99:p(.99),max:p(1)}};
          resolve({durationMs:now-start,arrivals,frameMs:stats(intervals),paintIntervalMs:stats(frames),yawStep:stats(jumps),longTaskMs:stats(longTasks),routeChildMutations:changes,teams:root.querySelectorAll('#ranklist .rk').length,islands:root.querySelectorAll('.island').length});
        }requestAnimationFrame(frame);
      })`)
      const after = (await cdp.send('Performance.getMetrics')).metrics
      const metric = (list,name) => list.find(m=>m.name===name)?.value || 0
      const row = {width,mode,...measurements,taskMs:(metric(after,'TaskDuration')-metric(before,'TaskDuration'))*1000,scriptMs:(metric(after,'ScriptDuration')-metric(before,'ScriptDuration'))*1000,heapBytes:metric(after,'JSHeapUsedSize')}
      results.push(row); console.log(JSON.stringify(row))
      assert.equal(row.teams,49);assert.equal(row.islands,12)
      if(mode==='focus') assert.ok(row.arrivals>=23&&row.arrivals<=25)
    }
  }
  assert.deepEqual(errors,[])
  assert.equal(requests.some(r=>!['GET','HEAD'].includes(r.method)&&!r.url.includes('/hub/user/negotiate')),false)
} finally {
  closing = true
  writeFileSync(`${output}/report.json`,JSON.stringify({scenario:{teams:49,islands:12,durationMs:12000,focusRatePerSecond:2},results,errors,requests,retiredReads},null,2))
  await browser.close()
}
