// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium, webkit } from '@playwright/test';
import assert from 'node:assert/strict';
import { mkdir, readFile } from 'node:fs/promises';
const tape=JSON.parse(await readFile(new URL('./fixtures/main-exit.json',import.meta.url)));
const browserType=process.env.BROWSER==='webkit'?webkit:chromium;
const browser=await browserType.launch({headless:true,...(browserType===chromium&&process.env.CHROME_CHANNEL?{channel:process.env.CHROME_CHANNEL}:{})}),errors=[];
await mkdir('test-results',{recursive:true});
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
    for(const level of [8,13,26]) {
      await page.locator('#goal-title').click();await page.locator(`.level-card[data-level="${level}"]`).click();
      await page.waitForFunction(level=>window.root?.boot_level===level&&!document.querySelector('#pause').disabled,level);
      await page.waitForFunction(()=>Number(document.querySelector('#attempts').textContent.replaceAll(',',''))>=12);
      await page.locator('#pause').click();
      const room=await page.evaluate(()=>window.root.observation.level),canvases=page.locator(`.map-row[data-map="${room}"] .area-map`);
      if(phone){
        assert.equal(await canvases.count(),1,'Phones show each room through one camera viewport');
        const view=await canvases.first().evaluate(c=>({viewport:c.dataset.viewport,w:+c.dataset.mapWidth,h:+c.dataset.mapHeight,rw:+c.dataset.roomWidth,rh:+c.dataset.roomHeight,visible:c.getBoundingClientRect().height>0}));
        assert.ok(view.viewport==='true'&&view.w===view.rw&&view.h===view.rh&&view.visible,`The phone camera covers the whole room: ${JSON.stringify(view)}`);
        if(await page.locator('#camera-toggle').getAttribute('aria-pressed')==='true')await page.locator('#camera-toggle').tap();
        await page.waitForFunction(room=>+document.querySelector(`.map-row[data-map="${room}"] .area-map`).dataset.zoom===1,room);
        assert.equal(await page.locator('#camera-toggle').innerText(),'Follow the search');
        await page.locator('#camera-toggle').tap();
        assert.equal(await page.locator('#camera-toggle').getAttribute('aria-pressed'),'true');
        await canvases.first().scrollIntoViewIfNeeded();await page.screenshot({path:`test-results/wrapped-${level}-phone.png`});
        continue;
      }
      assert.ok(await canvases.count()>1);
      const geometry=await canvases.evaluateAll(cs=>cs.map(c=>({x:+c.dataset.panelX,y:+c.dataset.panelY,w:+c.dataset.mapWidth,h:+c.dataset.mapHeight,width:c.clientWidth,roomWidth:+c.dataset.roomWidth,roomHeight:+c.dataset.roomHeight})));
      for(const p of geometry)assert.ok(16*p.width/p.w >= (phone ? 4 : 8),'Room sections keep Nova readable');
      assert.equal(geometry.reduce((sum,p)=>sum+p.w*p.h,0),geometry[0].roomWidth*geometry[0].roomHeight);
      await canvases.first().scrollIntoViewIfNeeded();await page.screenshot({path:`test-results/wrapped-${level}-${phone?'phone':'desktop'}.png`});
      const art=await page.evaluate(async room=>{
        const catalog=await(await fetch(new URL('maps.json',location.href))).json(),image=new Image();
        image.src=new URL(catalog.maps.find(m=>m.id===room).file,location.href);await image.decode();let equal=0,total=0;
        for(const c of document.querySelectorAll(`.map-row[data-map="${room}"] .area-map`)){
          const expected=document.createElement('canvas');expected.width=c.width;expected.height=c.height;
          const ctx=expected.getContext('2d');ctx.drawImage(image,-Number(c.dataset.panelX),-Number(c.dataset.panelY));ctx.fillStyle='rgba(12,17,20,.24)';ctx.fillRect(0,0,c.width,c.height);
          const a=ctx.getImageData(0,0,c.width,c.height).data,b=c.getContext('2d').getImageData(0,0,c.width,c.height).data;
          for(let y=8;y<c.height;y+=17)for(let x=8;x<c.width;x+=17){const i=(y*c.width+x)*4;total++;if(Math.abs(a[i]-b[i])+Math.abs(a[i+1]-b[i+1])+Math.abs(a[i+2]-b[i+2])<=4)equal++;}
        }return equal/total;
      },room);
      assert.ok(art>.85,`Original map crops align (${art}); moving Novas may differ`);
      await page.locator(`.map-row[data-map="${room}"] .room-continuation button`).nth(1).click();await page.waitForTimeout(400);
      assert.ok((await canvases.nth(1).boundingBox()).y>=0);
      await page.locator(`.area-zoom[data-map="${room}"]`).click();
      await page.waitForFunction(room=>[...document.querySelectorAll(`.map-row[data-map="${room}"] .area-map`)].every(c=>+c.dataset.zoom===2),room);
      await page.locator(`.area-zoom[data-map="${room}"]`).click();await page.locator(`.area-zoom[data-map="${room}"]`).click();
      await page.waitForFunction(room=>[...document.querySelectorAll(`.map-row[data-map="${room}"] .area-map`)].every(c=>+c.dataset.zoom===1),room);

    }
    if(phone) {
      await page.setViewportSize({width:844,height:390});
      await page.waitForTimeout(200);
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,'Rotation never creates page-wide horizontal scrolling');
      const coverage=await page.locator('.area-map').evaluateAll(cs=>cs.reduce((n,c)=>n+c.width*c.height,0));
      assert.equal(coverage,1024*896,'Responsive reflow retains every pixel of the tall room');
      await page.setViewportSize({width:390,height:844});
      await page.waitForTimeout(200);
    }
    // Use a recorded, hash-verified native history so selection beyond the broader
    // seam does not depend on how quickly the random search crosses this room.
    await page.locator('#goal-title').click();await page.locator('.level-card[data-level="0"]').click();
    await page.waitForFunction(()=>window.root?.boot_level===0&&!document.querySelector('#pause').disabled);
    await page.evaluate(tape=>window.testWorker.postMessage({type:'fork',tape,seed:3}),tape);
    await page.waitForFunction(()=>window.root?.id==='1:0'&&!document.querySelector('#pause').disabled);
    await page.locator('#pause').click();
    if(phone&&await page.locator('#camera-toggle').getAttribute('aria-pressed')==='true')await page.locator('#camera-toggle').tap();
    const target=await page.evaluate(()=>{
      const root=window.root,o=root.observation,cs=[...document.querySelectorAll(`.map-row[data-map="${o.level}"] .area-map`)];
      const c=cs.find(c=>o.x>=+c.dataset.panelX&&o.x<+c.dataset.panelX+ +c.dataset.mapWidth&&o.y-8>=+c.dataset.panelY&&o.y-8<+c.dataset.panelY+ +c.dataset.mapHeight);
      return{id:root.id,room:o.level,panel:+c.dataset.panel,x:Math.floor(o.x/32)*32+16,y:Math.floor(o.y/32)*32+8};
    });
    if(!phone)assert.ok(target.panel>0,'The native fixture lies beyond a section seam');
    const canvas=page.locator(`.map-row[data-map="${target.room}"] .area-map`).nth(target.panel);
    await canvas.scrollIntoViewIfNeeded();
    const point=await canvas.evaluate((c,t)=>{const r=c.getBoundingClientRect(),scale=Math.min(c.width/+c.dataset.mapWidth,c.height/+c.dataset.mapHeight)*+c.dataset.zoom;return{x:r.left+((t.x-+c.dataset.centerX)*scale+c.width/2)*r.width/c.width,y:r.top+((t.y-+c.dataset.centerY)*scale+c.height/2)*r.height/c.height};},target);
    if(phone)await page.touchscreen.tap(point.x,point.y);else await page.mouse.click(point.x,point.y);
    await page.waitForFunction(()=>!document.querySelector('#inspector').hidden&&!document.querySelector('#take-control').disabled);
    assert.ok(await page.evaluate(id=>window.lastStateRequest.includes(id),target.id),'A later section selects its authentic retained history');
    assert.equal(await page.locator('#visualization').getAttribute('data-value'),'movement');
    await page.waitForFunction(target=>[...document.querySelectorAll(`.map-row[data-map="${target.room}"] .area-map`)].some(c=>target.x>=+c.dataset.panelX&&target.x<+c.dataset.panelX+ +c.dataset.mapWidth&&+c.dataset.tracePoints>0),target);
    await page.locator('#close-inspector').click();
    await page.goto('about:blank');await page.close();
  }
  assert.deepEqual(errors,[]);console.log('Desktop horizontal wrapping, section navigation, original-art crops, vertical rooms and zoom; phone camera viewports with whole-room and follow modes; authentic retained hits across seams on desktop and phone passed.');
}finally{await browser.close();}
