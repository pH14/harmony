// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium, webkit } from '@playwright/test';
import assert from 'node:assert/strict';
import { mkdir, readFile } from 'node:fs/promises';
const tape=JSON.parse(await readFile(new URL('./fixtures/main-exit.json',import.meta.url)));
const browserType=process.env.BROWSER==='webkit'?webkit:chromium;
const browser=await browserType.launch({headless:true,...(browserType===chromium&&process.env.CHROME_CHANNEL?{channel:process.env.CHROME_CHANNEL}:{})}),errors=[];
await mkdir('test-results',{recursive:true});
const toggle=async(page,phone)=>phone?page.locator('#camera-toggle').tap():page.locator('#camera-toggle').click();
try {
  for(const phone of [false,true]) {
    const page=await browser.newPage({viewport:phone?{width:390,height:844}:{width:1440,height:1000},hasTouch:phone,isMobile:phone});
    page.on('pageerror',e=>errors.push(e.message));
    await page.addInitScript(()=>{
      try {localStorage.setItem('harmony.nova.tour.v1','seen');} catch {}
      const NativeWorker=Worker;
      window.Worker=class extends NativeWorker {
        constructor(...args) {super(...args);window.testWorker=this;this.addEventListener('message',({data})=>{
          if(data.type==='ready'){window.root=data.state;window.points=[];}
          if(data.type==='batch')window.points.push(...data.points.filter(p=>p.retained!==null));
        });}
        postMessage(data,...rest){if(data.type==='states'&&data.request>0)window.lastStateRequest=data.ids;super.postMessage(data,...rest);}
      };
    });
    await page.goto(process.env.DEMO_URL||'http://127.0.0.1:4173/');
    await page.waitForFunction(()=>!document.querySelector('#goal-title').disabled);
    let panned=0;
    for(const level of [8,13,26]) {
      await page.locator('#goal-title').click();await page.locator(`.level-card[data-level="${level}"]`).click();
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
        assert.equal(await page.locator('#camera-toggle').innerText(),'Follow the search','Panning from a room edge hands the camera to the visitor');
        if(!phone)await page.mouse.move(0,0);
        await toggle(page,phone);
      }
      if(await page.locator('#camera-toggle').getAttribute('aria-pressed')==='true')await toggle(page,phone);
      await page.waitForFunction(room=>+document.querySelector(`.map-row[data-map="${room}"] .area-map`).dataset.zoom===1,room);
      assert.equal(await page.locator('#camera-toggle').innerText(),'Follow the search');
      await toggle(page,phone);
      assert.equal(await page.locator('#camera-toggle').getAttribute('aria-pressed'),'true');
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
    // A recorded, hash-verified native history makes the selected cell independent of search speed.
    await page.locator('#goal-title').click();await page.locator('.level-card[data-level="0"]').click();
    await page.waitForFunction(()=>window.root?.boot_level===0&&!document.querySelector('#pause').disabled);
    await page.evaluate(tape=>window.testWorker.postMessage({type:'fork',tape,seed:3}),tape);
    await page.waitForFunction(()=>window.root?.id==='1:0'&&!document.querySelector('#pause').disabled);
    await page.locator('#pause').click();
    await page.waitForFunction(()=>document.querySelector('.map-wrap').dataset.camera!=='focus',null,{timeout:10000});
    if(await page.locator('#camera-toggle').getAttribute('aria-pressed')==='true')await toggle(page,phone);
    const target=await page.evaluate(()=>{const o=window.root.observation;return{id:window.root.id,room:o.level,x:Math.floor(o.x/32)*32+16,y:Math.floor(o.y/32)*32+8};});
    if(phone&&await page.locator(`#room-tabs button[data-map="${target.room}"]`).getAttribute('aria-pressed')!=='true')await page.locator(`#room-tabs button[data-map="${target.room}"]`).tap();
    if(await page.locator('#camera-toggle').getAttribute('aria-pressed')==='true')await toggle(page,phone);
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
