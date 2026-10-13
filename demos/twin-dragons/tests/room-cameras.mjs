// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium, webkit } from '@playwright/test';
import twinDragons from "../src/games/twin-dragons.js";
import assert from 'node:assert/strict';
import { mkdir, readFile } from 'node:fs/promises';
import { TOUR_KEY } from '../src/tour.js';
import { createEngine } from '../src/emulator.js';
import { snapshotHash } from '../src/media.js';
const recorded=JSON.parse(await readFile(new URL('./fixtures/level-one.json',import.meta.url)));
// Fork from a grounded point far into Level 1 but short of its goal, so the forked search stays in Level 1.
const base=new URL('../public/',import.meta.url);
const engine=await createEngine(base, twinDragons, {rom:await readFile(new URL('twin-dragons.nes',base)),wasmBinary:await readFile(new URL('engine/quicknes.wasm',base))});
engine.boot(0);
let tape=null;
for(const [i,action] of recorded.actions.entries()){
  engine.run(action.buttons,action.frames);
  const o=engine.observation();
  if(o.level===0&&o.on_ground&&o.health>0&&o.x>=1200&&o.x<=1600){const actions=recorded.actions.slice(0,i+1);tape={...recorded,actions,frames:actions.reduce((n,a)=>n+a.frames,0),observation:o,endpoint_sha256:await snapshotHash(engine.capture())};}
}
assert.ok(tape,'The Level 1 history passes a grounded point far into the level');
const browserType=process.env.BROWSER==='webkit'?webkit:chromium;
const browser=await browserType.launch({headless:true,...(browserType===chromium&&process.env.CHROME_CHANNEL?{channel:process.env.CHROME_CHANNEL}:{})}),errors=[];
await mkdir('test-results',{recursive:true});
const camera=async(page,phone,mode)=>{const b=page.locator(`#camera-toggle [data-camera="${mode}"]`);await(phone?b.tap():b.click());};
const cameraMode=page=>page.locator('#camera-toggle').getAttribute('data-value');
try {
  for(const phone of [false,true]) {
    const page=await browser.newPage({viewport:phone?{width:390,height:844}:{width:1440,height:1000},hasTouch:phone,isMobile:phone});
    page.on('pageerror',e=>errors.push(e.message));
    await page.addInitScript(key=>{
      try {localStorage.setItem(key,'seen');} catch {}
      const NativeWorker=Worker;
      window.Worker=class extends NativeWorker {
        constructor(...args) {super(...args);window.testWorker=this;this.addEventListener('message',({data})=>{
          if(data.type==='ready'){window.root=data.state;window.points=[];if(data.state.id==='1:0')this.postMessage({type:'pause'});}
          if(data.type==='batch')window.points.push(...data.points.filter(p=>p.retained!==null));
        });}
        postMessage(data,...rest){if(data.type==='states'&&data.request>0)window.lastStateRequest=data.ids;super.postMessage(data,...rest);}
      };
    },TOUR_KEY);
    await page.goto(`${(process.env.DEMO_URL||'http://127.0.0.1:4173').replace(/\/$/,'')}/?pace=0`);
    await page.waitForFunction(()=>!document.querySelector('#new-search').disabled);
    let panned=0;
    for(const level of [0,1,2]) {
      await page.locator('#new-search').click();await page.locator(`.level-card[data-level="${level}"]`).click();
      await page.waitForFunction(level=>window.root?.boot_level===level&&!document.querySelector('#pause').disabled,level);
      await page.waitForFunction(()=>Number(document.querySelector('#attempts').textContent.replaceAll(',',''))>=12);
      await page.locator('#pause').click();
      const room=await page.evaluate(()=>window.root.observation.level),canvases=page.locator(`.map-row[data-map="${room}"] .area-map`);
      assert.equal(await canvases.count(),1,'Each room shows through one camera viewport');
      const view=await canvases.first().evaluate(c=>({viewport:c.dataset.viewport,w:+c.dataset.mapWidth,h:+c.dataset.mapHeight,rw:+c.dataset.roomWidth,rh:+c.dataset.roomHeight,visible:c.getBoundingClientRect().height>0,zoom:+c.dataset.zoom,ratio:c.width/c.height}));
      assert.ok(view.viewport==='true'&&view.w===view.rw&&view.h===view.rh&&view.visible,`The camera covers the whole room: ${JSON.stringify(view)}`);
      assert.equal(view.ratio,phone?2:4,'Phones use a 2:1 viewport and desktops a 4:1 strip');
      assert.ok(view.zoom>=1,'The camera opens on a room-height close-up');
      assert.equal(await page.locator('.area-zoom,.room-continuation').count(),0,'Rooms no longer wrap into sections or offer zoom buttons');
      const part=page.locator(`.map-row[data-map="${room}"] .room-part`);
      if(await part.evaluate(p=>p.dataset.panLeft==='true'||p.dataset.panRight==='true')){
        panned++;
        const dir=await part.evaluate(p=>p.dataset.panRight==='true'?1:-1),before=+await canvases.first().getAttribute('data-center-x');
        assert.equal(await part.locator('.room-track').count(),1,'Each room camera shows where it sits within the room');
        const edge=part.locator(`.pan-edge[data-dir="${dir}"]`);
        if(phone)await edge.tap();else{await canvases.first().hover();await edge.hover();}
        await page.waitForFunction(([room,before,dir])=>dir*(+document.querySelector(`.map-row[data-map="${room}"] .area-map`).dataset.centerX-before)>8,[room,before,dir]);
        assert.equal(await cameraMode(page),'follow','Panning from a room edge keeps Auto view, which resumes after a pause');
        if(!phone)await page.mouse.move(0,0);
        await camera(page,phone,'follow');
      }
      await camera(page,phone,'room');
      await page.waitForFunction(room=>+document.querySelector(`.map-row[data-map="${room}"] .area-map`).dataset.zoom===1,room);
      assert.equal(await cameraMode(page),'room');
      await camera(page,phone,'follow');
      assert.equal(await cameraMode(page),'follow');
      assert.equal(await page.locator('#camera-toggle [data-camera="follow"]').getAttribute('aria-pressed'),'true');
      await canvases.first().scrollIntoViewIfNeeded();await page.screenshot({path:`test-results/room-camera-${level}-${phone?'phone':'desktop'}.png`});
    }
    assert.ok(panned>0,'At least one long room offers edge panning');
    if(phone) {
      await page.setViewportSize({width:844,height:390});
      await page.waitForTimeout(200);
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,'Rotation never creates page-wide horizontal scrolling');
      assert.ok(await page.locator('.area-map').evaluateAll(cs=>cs.every(c=>c.dataset.viewport==='true')),'Rotation keeps one camera viewport per room');
      await page.setViewportSize({width:390,height:844});
      await page.waitForTimeout(200);
    }
    // A recorded, hash-verified native history makes the selected cell independent of search speed. The
    // forked search pauses as soon as it starts, so later states cannot push the root out of its cell's list.
    await page.locator('#new-search').click();await page.locator('.level-card[data-level="0"]').click();
    await page.waitForFunction(()=>window.root?.boot_level===0&&!document.querySelector('#pause').disabled);
    await page.evaluate(tape=>window.testWorker.postMessage({type:'fork',tape,seed:3}),tape);
    await page.waitForFunction(()=>window.root?.id==='1:0'&&!document.querySelector('#pause').disabled);
    await page.locator('#pause').click();
    await page.waitForFunction(()=>document.querySelector('.map-wrap').dataset.camera!=='focus',null,{timeout:10000});
    await camera(page,phone,'room');
    const target=await page.evaluate(()=>{const o=window.root.observation;return{id:window.root.id,room:o.level,x:Math.floor((o.x+8)/32)*32+16,y:Math.floor(o.y/32)*32+24};});
    if(phone&&await page.locator(`#room-tabs button[data-map="${target.room}"]`).getAttribute('aria-pressed')!=='true')await page.locator(`#room-tabs button[data-map="${target.room}"]`).tap();
    await camera(page,phone,'room');
    const canvas=page.locator(`.map-row[data-map="${target.room}"] .area-map`);
    await canvas.scrollIntoViewIfNeeded();
    const point=await canvas.evaluate((c,t)=>{const r=c.getBoundingClientRect(),scale=Math.min(c.width/+c.dataset.mapWidth,c.height/+c.dataset.mapHeight)*+c.dataset.zoom;return{x:r.left+((t.x-+c.dataset.centerX)*scale+c.width/2)*r.width/c.width,y:r.top+((t.y-+c.dataset.centerY)*scale+c.height/2)*r.height/c.height};},target);
    if(phone)await page.touchscreen.tap(point.x,point.y);else await page.mouse.click(point.x,point.y);
    await page.waitForFunction(()=>!document.querySelector('#inspector').hidden&&!document.querySelector('#take-control').disabled);
    assert.ok(await page.evaluate(id=>window.lastStateRequest.includes(id),target.id),'Selecting the far end of a long room opens its authentic retained history');
    assert.equal(await page.locator('#visualization').getAttribute('data-value'),'movement');
    await page.waitForFunction(room=>+document.querySelector(`.map-row[data-map="${room}"] .area-map`).dataset.tracePoints>0,target.room);
    await page.locator('#close-inspector').click();
    await page.goto('about:blank');await page.close();
  }
  assert.deepEqual(errors,[]);console.log('Room cameras: one viewport per room on desktop and phone, whole-room and follow modes, edge panning, rotation, and authentic retained hits at the far end of a long room passed.');
}finally{await browser.close();}
