// Local browser fixtures only. No team changes, service starts or real submissions.
import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'

const target = process.env.RSCTF_GUIDE_PREVIEW || 'http://127.0.0.1:63017'
assert.equal(new URL(target).hostname, '127.0.0.1')
const output = resolve(process.env.RSCTF_GUIDE_OUTPUT || '../visual-audit-output/guide-hub')
mkdirSync(output, { recursive: true })
const { cdp, close } = await launchBrowser()
const errors = [], reports = [], requests = [], unknown = new Set()
const userId = '11111111-1111-4111-8111-111111111111'
const key = `rsctf-player-guide:${userId}`
const now = Date.now()
const config = { title:'RSCTF', portMapping:'Default', allowRegister:true, allowPasswordRegistration:true, allowTeamCreation:true, emailConfirmationRequired:false, enableBrowserFingerprint:false }
const challenges = Array.from({ length:10 }, (_, i) => ({ id:9001+i, title:`Guide challenge ${i+1}`, category:i%2 ? 'Web' : 'Pwn', type:'StaticAttachment', score:100, solved:0, bloods:[], disableBloodBonus:true }))
const fixtures = {
  '/api/config':config, '/api/captcha':{ type:'None' },
  '/api/account/profile':{ userId, userName:'Guide tester', role:'User', email:'guide@example.invalid' },
  '/api/game/901':{ id:901, title:'Guide test event', start:now-60000, end:now+3600000, serverTime:now, status:'Accepted', joined:true, divisions:[], teamCount:2, userCount:6 },
  '/api/game/901/details':{ challenges:Object.groupBy(challenges, item=>item.category), challengeCount:10, rank:{ id:1, name:'Fixture team', rank:1, score:0, solvedCount:0, solvedChallenges:[] } },
  '/api/game/901/details/participant':{ rank:{ id:1, name:'Fixture team', rank:1, score:0, solvedCount:0, solvedChallenges:[] } },
  '/api/game/901/notices':[],
}
for (const challenge of challenges) {
  fixtures[`/api/game/901/challenges/${challenge.id}`] = { ...challenge, content:'Read the fixture material. No actual flag is required for this guide test.', attempts:0, hints:[] }
  fixtures[`/api/game/901/challenges/${challenge.id}/solvers/page`] = { data:[], total:0 }
}
const evaluate = async expression => {
  const result = await cdp.send('Runtime.evaluate', { expression, awaitPromise:true, returnByValue:true })
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text)
  return result.result?.value
}
const wait = async expression => {
  for (let i=0; i<100; i++) {
    if (await evaluate(`Boolean(${expression})`)) return
    await new Promise(resolve=>setTimeout(resolve,100))
  }
  throw new Error(`Timed out: ${expression}; ${await evaluate('document.body.innerText.slice(-1800)')}`)
}
const press = async (key, code) => {
  for (const type of ['keyDown','keyUp']) await cdp.send('Input.dispatchKeyEvent', {type,key,windowsVirtualKeyCode:code,...(type==='keyDown'&&key==='Enter'?{text:'\r',unmodifiedText:'\r'}:{})})
}
const audit = async name => {
  await evaluate(`Promise.all(document.getAnimations().filter(a=>a.effect?.getComputedTiming().endTime!==Infinity).map(a=>a.finished.catch(()=>{})))`)
  await evaluate(readFileSync('node_modules/axe-core/axe.min.js','utf8'))
  const result = await evaluate(`(async()=>({ overflow:document.documentElement.scrollWidth>innerWidth+1, violations:(await axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}})).violations.map(v=>({id:v.id,nodes:v.nodes.map(n=>({target:n.target,summary:n.failureSummary}))})) }))()`)
  reports.push({ name,...result,errors:[...errors] })
  const shot = await cdp.send('Page.captureScreenshot',{format:'png',captureBeyondViewport:false})
  writeFileSync(`${output}/${name}.png`,Buffer.from(shot.data,'base64'))
  console.log(name, JSON.stringify(result))
}
const setPreferences = async (preferences, view='globe') => {
  await cdp.send('Page.addScriptToEvaluateOnNewDocument', {source:`if(location.origin===${JSON.stringify(target)}) {localStorage.setItem(${JSON.stringify(key)},JSON.stringify(${JSON.stringify(preferences)}));localStorage.setItem('challenge-explorer-view',JSON.stringify(${JSON.stringify(view)}));}`})
}
const paused = { interactiveEnabled:false, completedVersion:0, seenFeatures:[], activeTourStep:'challenges', tourPaused:true }
try {
  await cdp.send('Page.enable'); await cdp.send('Runtime.enable')
  cdp.on('Runtime.exceptionThrown', ({exceptionDetails})=>errors.push(exceptionDetails.exception?.description || exceptionDetails.text))
  await cdp.send('Fetch.enable',{patterns:[{urlPattern:`${target}/api/*`},{urlPattern:`${target}/hub*`}]})
  cdp.on('Fetch.requestPaused',async ({requestId,request})=>{
    const path = new URL(request.url).pathname.toLowerCase()
    requests.push({path,method:request.method})
    let value=fixtures[path], status=200
    if (!['GET','HEAD'].includes(request.method)) {value={title:'Fixture mutation blocked',status:405};status=405}
    else if (value===undefined) {unknown.add(path);value=[]}
    await cdp.send('Fetch.fulfillRequest',{requestId,responseCode:status,responseHeaders:[{name:'Content-Type',value:'application/json'}],body:Buffer.from(JSON.stringify(value)).toString('base64')})
  })
  await setPreferences(paused)
  for (const [name,width,height,language,scheme] of [['desktop',1600,1100,'en-US','dark'],['compact',320,568,'en-US','dark'],['tablet',768,1024,'en-US','dark'],['mobile-id',390,844,'id-ID','light']]) {
    if (process.env.RSCTF_GUIDE_TOUR_ONLY === '1') continue
    await cdp.send('Emulation.setDeviceMetricsOverride',{width,height,deviceScaleFactor:1,mobile:false})
    await cdp.send('Emulation.setEmulatedMedia',{features:[{name:'prefers-reduced-motion',value:name==='mobile-id'?'reduce':'no-preference'}]})
    await cdp.send('Page.addScriptToEvaluateOnNewDocument',{source:`localStorage.setItem('language',JSON.stringify(${JSON.stringify(language)}));localStorage.setItem('mantine-color-scheme-value',${JSON.stringify(scheme)});`})
    await cdp.send('Page.navigate',{url:`${target}/guide?audit=${name}`})
    await wait(`document.querySelectorAll('[data-guide-topic]').length===8 && !document.querySelector('[data-guide-start]').disabled`)
    assert.equal(await evaluate(`document.querySelectorAll('h1').length`),1)
    await audit(`${name}-hub`)
    for (const id of ['play-challenge','connections','scoring','troubleshooting']) {
      await evaluate(`document.querySelector('[data-guide-topic="${id}"]').focus()`)
      await press('Enter',13)
      await wait(`document.querySelector('[data-guide-article]').dataset.guideArticle==='${id}' && document.activeElement.id==='guide-topic-title'`)
      assert.equal(await evaluate(`document.querySelectorAll('[data-guide-topic][aria-current="page"]').length`),1)
      await evaluate(`document.querySelector('[data-guide-article] .mantine-Accordion-control').click()`)
      await audit(`${name}-${id}`)
    }
    await evaluate(`document.querySelector('[data-guide-search]').focus()`)
    await cdp.send('Input.insertText',{text:'wsrx'})
    await wait(`document.querySelectorAll('[data-guide-topic]').length<8`)
    assert.equal(await evaluate(`!!document.querySelector('[data-guide-topic="connections"]')`),true)
    await cdp.send('Input.insertText',{text:' no-match-123'})
    await wait(`document.querySelectorAll('[data-guide-topic]').length===0`)
    await audit(`${name}-no-results`)
    await evaluate(`[...document.querySelectorAll('[data-guide-hub] button')].find(button=>['Clear search','Hapus pencarian'].includes(button.textContent.trim())).click()`)
    await wait(`document.querySelectorAll('[data-guide-topic]').length===8`)
    // A legacy deep link and browser history both select a real, readable topic.
    await cdp.send('Page.navigate',{url:`${target}/guide#play-challenge`})
    await wait(`document.querySelector('[data-guide-article]')?.dataset.guideArticle==='play-challenge'`)
    await evaluate(`document.querySelector('[data-guide-topic="connections"]').click()`)
    await wait(`document.querySelector('[data-guide-article]').dataset.guideArticle==='connections'`)
    await evaluate(`history.back()`)
    await wait(`document.querySelector('[data-guide-article]').dataset.guideArticle==='play-challenge'`)
    // Contextual entry starts this checkpoint, not a forced replay from welcome.
    await evaluate(`document.querySelector('[data-guide-article] button.mantine-Button-root').click()`)
    await wait(`document.querySelector('[data-guide-surface="coachmark"]')`)
    assert.equal(await evaluate(`JSON.parse(localStorage.getItem(${JSON.stringify(key)})).activeTourStep`),'challenges')
    await audit(`${name}-contextual-tour`)
    await press('Escape',27)
    await wait(`!document.querySelector('[data-guide-surface="coachmark"]')`)
    assert.equal(await evaluate(`JSON.parse(localStorage.getItem(${JSON.stringify(key)})).tourPaused`),true)
  }
  for (const [name,width,view] of [['desktop',1600,'globe'],['compact',320,'globe'],['mobile-list',390,'list']]) {
    await cdp.send('Emulation.setDeviceMetricsOverride',{width,height:width===320?568:1100,deviceScaleFactor:1,mobile:false})
    await setPreferences({...paused,interactiveEnabled:true,tourPaused:false},view)
    await cdp.send('Page.navigate',{url:`${target}/games/901/challenges?audit=${name}`})
    if(view==='globe') {
      await wait(`document.querySelector('[data-guide-surface="coachmark"]')?.dataset.guideTarget==='challenge-category'`)
      await audit(`${name}-tour-category`)
      await evaluate(`document.querySelector('[data-guide="challenge-category"]').click()`)
      assert.equal(await evaluate(`JSON.parse(localStorage.getItem(${JSON.stringify(key)})).activeTourStep`),'challenges')
    }
    await wait(`document.querySelector('[data-guide-surface="coachmark"]')?.dataset.guideTarget==='challenge-card'`)
    await audit(`${name}-tour-challenge`)
    await evaluate(`document.querySelector('[data-guide="challenge-card"]').click()`)
    await wait(`JSON.parse(localStorage.getItem(${JSON.stringify(key)})).activeTourStep==='connection' && document.querySelector('[data-guide="challenge-material"]')`)
    await audit(`${name}-tour-material`)
    // An attachment-only task can skip connection setup without starting or submitting anything.
    await evaluate(`[...document.querySelectorAll('[data-guide-surface="coachmark"] button')].find(button=>['Skip step','Lewati langkah'].includes(button.textContent.trim())).click()`)
    await wait(`JSON.parse(localStorage.getItem(${JSON.stringify(key)})).activeTourStep==='submit'`)
  }
  assert.deepEqual([...unknown],[])
  assert.equal(requests.filter(r=>r.path.includes('/challenges/')&&r.method!=='GET').length,0)
} finally {
  writeFileSync(`${output}/report.json`,JSON.stringify({reports,requests,unknown:[...unknown]},null,2))
  await close()
}
assert.deepEqual(reports.filter(r=>r.overflow||r.violations.length||r.errors.length),[])
console.log(`PASS: ${reports.length} guide browser/Axe views`)
