import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { gradingFixture, pdfFixture } from './writeup-grading-fixtures.mjs'
import { launchBrowser } from './cdp.mjs'

const target = process.env.RSCTF_WRITEUP_TARGET || 'http://127.0.0.1:63017'
assert.ok(['http://127.0.0.1:63017', 'https://intechfest.1pc.tf'].includes(target))
const output = resolve(process.env.RSCTF_WRITEUP_OUTPUT || '../visual-audit-output/writeup-local')
mkdirSync(output, { recursive: true })
const fixture = gradingFixture(), reports = [], writes = [], unknown = [], errors = []
let failSave = false, failLoad = false
const { cdp, close } = await launchBrowser()
const evaluate = async (expression) => {
  const r = await cdp.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text)
  return r.result?.value
}
const wait = async (expression) => {
  for (let i = 0; i < 100; i++) {
    if (await evaluate(`Boolean(document.body && (${expression}))`)) return
    await new Promise((r) => setTimeout(r, 100))
  }
  throw new Error('Timed out: ' + expression + '; ' + await evaluate('document.body.innerText.slice(-1500)'))
}
const clickText = (text) => evaluate(`Array.from(document.querySelectorAll('button')).find(b=>b.textContent===${JSON.stringify(text)}).click()`)
const visit = async () => {
  await evaluate('window.__writeupOldDocument=true')
  await cdp.send('Page.navigate', { url: target + '/admin/games/19/writeups' })
  await wait(`!window.__writeupOldDocument && document.querySelector('[data-writeup-grading]') && document.body.innerText.includes('Scored challenges')`)
  if (await evaluate(`!!document.querySelector('a[href="/assets/writeup-fixture.pdf"]')`)) {
    await wait(`document.querySelector('.react-pdf__Page__canvas')`)
  }
}
const audit = async (name) => {
  await evaluate(readFileSync('node_modules/axe-core/axe.min.js', 'utf8'))
  const r = await evaluate(`(async()=>({overflow:document.documentElement.scrollWidth>innerWidth+1,h1:document.querySelectorAll('h1').length,violations:(await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>({target:n.target,summary:n.failureSummary}))}))}))()`)
  const shot = await cdp.send('Page.captureScreenshot', { format:'png',captureBeyondViewport:false })
  writeFileSync(output+'/'+name+'.png',Buffer.from(shot.data,'base64'))
  reports.push({name,...r}); console.log(name,JSON.stringify(r))
}
try {
  await cdp.send('Page.enable'); await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails }) => errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Fetch.enable', { patterns: [{urlPattern:target+'/api/*'}, {urlPattern:target+'/hub*'}, {urlPattern:target+'/assets/writeup-fixture.pdf'}] })
  cdp.on('Fetch.requestPaused', async ({requestId,request}) => {
    const path = new URL(request.url).pathname
    if(path==='/assets/writeup-fixture.pdf') return cdp.send('Fetch.fulfillRequest',{requestId,responseCode:200,responseHeaders:[{name:'Content-Type',value:'application/pdf'}],body:pdfFixture().toString('base64')})
    if(!['GET','HEAD'].includes(request.method)) writes.push({path,method:request.method,body:JSON.parse(request.postData || '{}')})
    const r = failSave && request.method==='PUT' ? {status:409,body:{title:'Grade changed'}} : failLoad && path.endsWith('/grading') ? {status:503,body:{title:'Unavailable'}} : fixture(path,request.method,request.postData)
    if(r.unknown) unknown.push(r.unknown)
    await cdp.send('Fetch.fulfillRequest',{requestId,responseCode:r.status||200,responseHeaders:[{name:'Content-Type',value:'application/json'}],body:Buffer.from(JSON.stringify(r.body)).toString('base64')})
  })
  await cdp.send('Page.addScriptToEvaluateOnNewDocument',{source:`localStorage.setItem('language',JSON.stringify('en-US'));for(const id of ['guest','11111111-1111-4111-8111-111111111111'])localStorage.setItem('rsctf-player-guide:'+id,JSON.stringify({interactiveEnabled:false,completedVersion:5,seenFeatures:[],activeTourStep:null,tourPaused:true}));`})
  for(const [name,width,height,scheme] of [['desktop',1440,1100,'dark'],['tablet',768,1024,'dark'],['mobile',390,844,'dark'],['compact',320,568,'dark'],['light',390,844,'light']]) {
    await cdp.send('Emulation.setDeviceMetricsOverride',{width,height,deviceScaleFactor:1,mobile:false})
    await cdp.send('Emulation.setEmulatedMedia',{features:[{name:'prefers-reduced-motion',value:name==='light'?'reduce':'no-preference'}]})
    await cdp.send('Page.addScriptToEvaluateOnNewDocument',{source:`localStorage.setItem('mantine-color-scheme-value',${JSON.stringify(scheme)})`})
    await visit(); await audit(name+'-review')
    await clickText('Projected scoreboard'); await audit(name+'-ranking')
  }
  await cdp.send('Emulation.setDeviceMetricsOverride',{width:1440,height:1100,deviceScaleFactor:1,mobile:false})
  await visit()
  const input = `document.querySelector('input[aria-label^="Writeup grade (%) for"]')`
  const setGrade = async(value) => {
    await evaluate(`${input}.focus();${input}.select()`)
    await cdp.send('Input.insertText',{text:String(value)})
  }
  await setGrade(50); await clickText('Save grade')
  await wait(`document.body.innerText.includes('Saved grade: 50%') && document.body.innerText.includes('Projected rank #2')`)
  assert.ok(await evaluate(`document.body.innerText.includes('275 points retained')`))
  await audit('saved-grade-reorders-ranking')
  await visit()
  await clickText('Projected scoreboard')
  await clickText('Stargazers')
  await wait(`document.body.innerText.includes('Saved grade: 50%')`)
  await setGrade(101)
  assert.ok(await evaluate(`Array.from(document.querySelectorAll('button')).find(b=>b.textContent==='Save grade').disabled`))
  await setGrade(0); failSave=true; await clickText('Save grade')
  await wait(`document.body.innerText.includes('Grade not confirmed')`)
  assert.ok(await evaluate(`document.body.innerText.includes('Saved grade: 50%')`))
  await audit('conflict-does-not-change-grade')
  failSave=false; await clickText('Save grade')
  await wait(`document.body.innerText.includes('Saved grade: 0%')`)
  assert.equal(writes.at(-1).body.operationId,writes.at(-2).body.operationId)
  await clickText('Mark ungraded')
  await wait(`document.body.innerText.includes('Ungraded · retains 100%')`)
  failLoad=true; await clickText('Refresh scores & grades')
  await wait(`document.body.innerText.includes('Writeup grading could not be loaded')`)
  await audit('load-error')
  failLoad=false; await clickText('Refresh scores & grades')
  await wait(`document.body.innerText.includes('Scored challenges')`)
  assert.deepEqual(unknown,[]); assert.deepEqual(errors,[])
  assert.ok(writes.every(w=>w.method==='PUT' && /^\/api\/admin\/writeups\/19\/grading\/\d+\/\d+$/.test(w.path)))
  assert.deepEqual(reports.filter(r=>r.overflow || r.h1!==1 || r.violations.length),[])
} finally {
  writeFileSync(output+'/report.json',JSON.stringify({target,fixtures:true,reports,writes,unknown,errors},null,2))
  await close()
}
