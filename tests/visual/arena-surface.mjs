// Independent ray/spherical-polygon oracle for the horizon clipping regression.
// Uses real browser SVG hit testing, not another copy of the candidate clipper.
import assert from 'node:assert/strict'
import { build } from '../../web/node_modules/esbuild/lib/main.js'
import { mkdirSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { launchBrowser } from './cdp.mjs'

const output = resolve(process.env.RSCTF_SURFACE_OUTPUT || 'visual-audit-output/settlement-surface-proof')
mkdirSync(output, {recursive:true})
const bundle = await build({stdin:{contents:`export { buildArenaGeography } from './src/pages/games/[id]/arenaGeography'; export { projectSurface } from './src/pages/games/[id]/arenaProjection';`,resolveDir:resolve('web')}, bundle:true,write:false,platform:'browser',format:'iife',globalName:'surfaceProof',tsconfig:'web/tsconfig.app.json',define:{'import.meta.env':'{}'}})
const browser = await launchBrowser(), {cdp} = browser
try {
  await cdp.send('Runtime.enable')
  await cdp.send('Runtime.evaluate',{expression:bundle.outputFiles[0].text})
  const result = await cdp.send('Runtime.evaluate',{returnByValue:true,expression:`(() => {
    const {buildArenaGeography,projectSurface}=surfaceProof;
    const ns='http://www.w3.org/2000/svg', svg=document.createElementNS(ns,'svg');
    svg.setAttribute('viewBox','0 0 1000 1000');document.body.append(svg);
    const make=()=>{const p=document.createElementNS(ns,'path');svg.append(p);return p};
    const candidate=make(),edge=make(),old=make();edge.setAttribute('stroke-width','1.5');
    const dot=(a,b)=>a.x*b.x+a.y*b.y+a.z*b.z;
    const rotate=(p,yaw,pitch)=>{const x=p.x*Math.cos(yaw)+p.z*Math.sin(yaw),z=p.z*Math.cos(yaw)-p.x*Math.sin(yaw);return{x,y:p.y*Math.cos(pitch)-z*Math.sin(pitch),z:p.y*Math.sin(pitch)+z*Math.cos(pitch)}};
    const points=[];
    for(const r of [0,.3,.6,.85,.96,.985,.999])for(let a=0;a<48;a++){const angle=a*Math.PI/24;points.push({x:r*Math.cos(angle),y:r*Math.sin(angle),z:Math.sqrt(1-r*r)})}
    let checks=0,oldMismatches=0,toleranceExclusions=0;const mismatches=[];
    const categories=['Web','Pwn','Misc','Crypto'].map((id,i)=>({id,challenges:Array.from({length:3},(_,j)=>({id:i*3+j}))}));
    const geography=[...buildArenaGeography(categories),...buildArenaGeography([{id:'Single large continent',challenges:[{id:100}]}])];
    for(const continent of geography)for(const country of continent.countries){
      const center=continent.location,east={x:center.z,y:0,z:-center.x},north={x:center.y*east.z,y:center.z*east.x-center.x*east.z,z:-center.y*east.x};
      const gnomonic=p=>({x:dot(p,east)/dot(p,center),y:dot(p,north)/dot(p,center)}),vertices=country.coast.map(gnomonic);
      const contains=p=>{if(dot(p,center)<=0)return false;const q=gnomonic(p);let inside=false;for(let i=0,j=vertices.length-1;i<vertices.length;j=i++){const a=vertices[i],b=vertices[j];if(a.y>q.y!==b.y>q.y&&q.x<(b.x-a.x)*(q.y-a.y)/(b.y-a.y)+a.x)inside=!inside}return inside};
      for(const pitch of [-.6,0,.6])for(let turn=0;turn<24;turn++){
        const yaw=turn*Math.PI/12,paths=projectSurface(country.coast,yaw,pitch);
        candidate.setAttribute('d',paths.fill);edge.setAttribute('d',paths.edge);
        // Preserve the old straight-chord behavior as a negative control.
        const rotated=country.coast.map(p=>rotate(p,yaw,pitch)),visible=[];
        for(let i=0;i<rotated.length;i++){const a=rotated[i],b=rotated[(i+1)%rotated.length];if(a.z>=.015)visible.push(a);if(a.z>=.015!==b.z>=.015){const t=(.015-a.z)/(b.z-a.z);visible.push({x:a.x+(b.x-a.x)*t,y:a.y+(b.y-a.y)*t})}}
        old.setAttribute('d',visible.length<3?'':visible.map((p,i)=>(i?'L':'M')+(500+p.x*435)+','+(500+p.y*435)).join('')+'Z');
        for(const view of points){
          const y=view.y*Math.cos(pitch)+view.z*Math.sin(pitch),z=view.z*Math.cos(pitch)-view.y*Math.sin(pitch);
          const world={x:view.x*Math.cos(yaw)-z*Math.sin(yaw),y,z:view.x*Math.sin(yaw)+z*Math.cos(yaw)};
          const sample=new DOMPoint(500+view.x*435,500+view.y*435),expected=contains(world);
          if(edge.isPointInStroke(sample)){toleranceExclusions++;continue}
          checks++;
          if(old.isPointInFill(sample)!==expected)oldMismatches++;
          if(candidate.isPointInFill(sample)!==expected&&mismatches.length<20)mismatches.push({category:continent.id,country:country.id,yaw,pitch,sample:{x:sample.x,y:sample.y},expected});
        }
      }
    }
    return {checks,oldMismatches,toleranceExclusions,mismatches};
  })()`})
  assert.equal(result.exceptionDetails,undefined)
  const report=result.result.value
  writeFileSync(`${output}/report.json`,JSON.stringify(report,null,2))
  console.log(JSON.stringify(report,null,2))
  assert.ok(report.checks>100000)
  assert.ok(report.oldMismatches>100,'negative control reproduces the original sunken-land bug')
  assert.deepEqual(report.mismatches,[],'every checked ray agrees with the independent spherical polygon oracle')
} finally { await browser.close() }
