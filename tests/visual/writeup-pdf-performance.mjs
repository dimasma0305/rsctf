import assert from 'node:assert/strict'
import { mkdirSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'
import { gradingFixture, pdfFixture } from './writeup-grading-fixtures.mjs'

// Fixed workload: 60 portrait pages, 2x display, 12 tab changes at 750 ms intervals.
// API calls are synthetic even on the deployed origin; never modifies real grades.
const target = process.env.RSCTF_WRITEUP_TARGET || 'http://127.0.0.1:63017'
assert.ok(['http://127.0.0.1:63017', 'https://intechfest.1pc.tf', 'https://tcp.1pc.tf'].includes(target))
const output = resolve(process.env.RSCTF_WRITEUP_OUTPUT || '../visual-audit-output/writeup-pdf-performance')
mkdirSync(output, { recursive: true })
const baseline = process.argv.includes('--baseline')
const fixture = gradingFixture()
const pdf = pdfFixture({ pages: 60, height: 560 }).toString('base64')
const errors = [], unknown = [], arrivals = []
let pdfRequests = 0, report
const { cdp, close } = await launchBrowser()
const evaluate = async (expression) => {
  const r = await cdp.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true })
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text)
  return r.result?.value
}
const sleep = ms => new Promise(resolve => setTimeout(resolve, Math.max(0, ms)))
const wait = async expression => {
  for (let i = 0; i < 150; i++) {
    if (await evaluate(`Boolean(${expression})`)) return
    await sleep(100)
  }
  throw new Error('Timed out: ' + expression)
}
const distribution = values => {
  const sorted = [...values].sort((a, b) => a - b)
  const percentile = p => sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))] || 0
  return { count: values.length, avg: values.reduce((a, b) => a + b, 0) / (values.length || 1), p50: percentile(.5), p90: percentile(.9), p95: percentile(.95), p99: percentile(.99), max: percentile(1) }
}
try {
  await cdp.send('Page.enable'); await cdp.send('Runtime.enable'); await cdp.send('Performance.enable')
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1100, deviceScaleFactor: 2, mobile: false })
  await cdp.send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] })
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  cdp.on('Runtime.consoleAPICalled', ({ type, args }) => {
    const message = args.map(a => a.description || a.value || '').join(' ')
    if (['warning', 'error'].includes(type) && /TypeError|sendWithPromise|sendWithStream/.test(message)) errors.push(message)
  })
  await cdp.send('Fetch.enable', { patterns: [{ urlPattern: target + '/api/*' }, { urlPattern: target + '/hub*' }, { urlPattern: target + '/assets/writeup-fixture.pdf' }] })
  cdp.on('Fetch.requestPaused', async ({ requestId, request }) => {
    const path = new URL(request.url).pathname
    if (path === '/assets/writeup-fixture.pdf') {
      pdfRequests++
      return cdp.send('Fetch.fulfillRequest', { requestId, responseCode: 200, responseHeaders: [{ name: 'Content-Type', value: 'application/pdf' }], body: pdf })
    }
    const r = fixture(path, request.method, request.postData)
    if (r.unknown || !['GET', 'HEAD'].includes(request.method)) unknown.push(path)
    await cdp.send('Fetch.fulfillRequest', { requestId, responseCode: r.status || 200, responseHeaders: [{ name: 'Content-Type', value: 'application/json' }], body: Buffer.from(JSON.stringify(r.body)).toString('base64') })
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', { source: `
    if (window === window.top && location.origin === ${JSON.stringify(new URL(target).origin)}) {
    localStorage.setItem('language', JSON.stringify('en-US'));
    localStorage.setItem('mantine-color-scheme-value', 'dark');
    for(const id of ['guest','11111111-1111-4111-8111-111111111111']) localStorage.setItem('rsctf-player-guide:'+id, JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true}));
    window.pdfProfile={peakCanvases:0,peakCanvasBytes:0,canvasResizes:0,longTasks:[],timerDelays:[]};
    new PerformanceObserver(list=>{for(const e of list.getEntries()) if(pdfProfile.longTasks.length<1000) pdfProfile.longTasks.push(e.duration)}).observe({type:'longtask',buffered:true});
    new MutationObserver(records=>{for(const r of records) if(r.target.matches?.('.react-pdf__Page__canvas')) pdfProfile.canvasResizes++}).observe(document,{subtree:true,attributes:true,attributeFilter:['width','height']});
    let previous=performance.now();
    setInterval(()=>{
      const now=performance.now();
      if(pdfProfile.timerDelays.length<1000) pdfProfile.timerDelays.push(Math.max(0,now-previous-50));
      previous=now;
      const canvases=Array.from(document.querySelectorAll('.react-pdf__Page__canvas'));
      pdfProfile.peakCanvases=Math.max(pdfProfile.peakCanvases,canvases.length);
      pdfProfile.peakCanvasBytes=Math.max(pdfProfile.peakCanvasBytes,canvases.reduce((sum,c)=>sum+c.width*c.height*4,0));
    },50);
    }
  ` })
  const started = performance.now()
  await cdp.send('Page.navigate', { url: target + '/admin/games/19/writeups' })
  await wait(`document.querySelector('.react-pdf__Page__canvas')?.width > 0`)
  const firstCanvasMs = performance.now() - started
  // Allow initial render work to settle; fixed duration for both candidates.
  await sleep(3000)
  const initial = await evaluate('({...pdfProfile, canvases:document.querySelectorAll(".react-pdf__Page__canvas").length})')
  const actionStart = performance.now()
  for (let i = 0; i < 12; i++) {
    const scheduled = actionStart + i * 750
    await sleep(scheduled - performance.now())
    const started = performance.now()
    const name = i % 2 === 0 ? 'Projected scoreboard' : 'Review a team'
    await evaluate(`Array.from(document.querySelectorAll('[role="tab"]')).find(b=>b.textContent===${JSON.stringify(name)}).click()`)
    await wait(`document.querySelector('[role="tab"][aria-selected="true"]').textContent===${JSON.stringify(name)}`)
    arrivals.push({ lagMs: started - scheduled, responseMs: performance.now() - started })
  }
  await sleep(1000)
  const profile = await evaluate('pdfProfile')
  const metrics = Object.fromEntries((await cdp.send('Performance.getMetrics')).metrics.map(m => [m.name, m.value]))
  report = { target, baseline, pages: 60, deviceScaleFactor: 2, tabChanges: 12, intervalMs: 750, firstCanvasMs, initialCanvases: initial.canvases, peakCanvases: profile.peakCanvases, peakCanvasBytes: profile.peakCanvasBytes, canvasResizes: profile.canvasResizes, rendererTaskSeconds: metrics.TaskDuration, jsHeapUsedBytes: metrics.JSHeapUsedSize, longTasksMs: distribution(profile.longTasks), timerDelayMs: distribution(profile.timerDelays), tabResponseMs: distribution(arrivals.map(a => a.responseMs)), arrivalLagMs: distribution(arrivals.map(a => a.lagMs)), pdfRequests, errors, unknown }
  console.log(JSON.stringify(report, null, 2))
  assert.deepEqual(errors, []); assert.deepEqual(unknown, [])
  assert.equal(pdfRequests, 1, 'Tab changes must reuse the loaded document')
  if (!baseline) {
    assert.ok(profile.peakCanvases <= 1, 'Only the selected PDF page may be rendered')
    assert.ok(profile.peakCanvasBytes < 16 * 1024 * 1024, 'Canvas backing stores must remain bounded')
    assert.ok(report.tabResponseMs.max < 1000, 'Tab navigation must stay responsive')
    assert.ok(report.arrivalLagMs.max < 750, 'The browser must keep up with the fixed input rate')
  }
} finally {
  writeFileSync(output + '/report.json', JSON.stringify(report || { errors, unknown, arrivals }, null, 2))
  await close()
}
