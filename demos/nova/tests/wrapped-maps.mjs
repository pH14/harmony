// SPDX-License-Identifier: AGPL-3.0-or-later
import { chromium } from '@playwright/test';
import assert from 'node:assert/strict';
import { mkdir } from 'node:fs/promises';
const browser=await chromium.launch({headless:true,...(process.env.CHROME_CHANNEL?{channel:process.env.CHROME_CHANNEL}:{})}),errors=[];
await mkdir('test-results',{recursive:true});
try {
  for(const phone of [false,true]) {
    const page=await browser.newPage({viewport:phone?{width:390,height:844}:{width:1440,height:1000},hasTouch:phone,isMobile:phone});
    page.on('pageerror',e=>errors.push(e.message));
    await page.addInitScript(()=>{
      try {localStorage.setItem('harmony.nova.tour.v1','seen');} catch {}
      const NativeWorker=Worker;
      window.Worker=class extends NativeWorker {
        constructor(...args) {super(...args);this.addEventListener('message',({data})=>{
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
      const room=await page.evaluate(()=>window.root.observation.level),canvases=page.locator(`.map-row[data-map="${room}"] canvas`);
      assert.ok(await canvases.count()>1);
      const geometry=await canvases.evaluateAll(cs=>cs.map(c=>({x:+c.dataset.panelX,y:+c.dataset.panelY,w:+c.dataset.mapWidth,h:+c.dataset.mapHeight,width:c.clientWidth,roomWidth:+c.dataset.roomWidth,roomHeight:+c.dataset.roomHeight})));
      for(const p of geometry)assert.ok(16*p.width/p.w>=8,'Nova remains readable instead of shrinking the whole room');
      assert.equal(geometry.reduce((sum,p)=>sum+p.w*p.h,0),geometry[0].roomWidth*geometry[0].roomHeight);
      await canvases.first().scrollIntoViewIfNeeded();await page.screenshot({path:`test-results/wrapped-${level}-${phone?'phone':'desktop'}.png`});
      const art=await page.evaluate(async room=>{
        const catalog=await(await fetch(new URL('maps.json',location.href))).json(),image=new Image();
        image.src=new URL(catalog.maps.find(m=>m.id===room).file,location.href);await image.decode();let equal=0,total=0;
        for(const c of document.querySelectorAll(`.map-row[data-map="${room}"] canvas`)){
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
      await page.waitForFunction(room=>[...document.querySelectorAll(`.map-row[data-map="${room}"] canvas`)].every(c=>+c.dataset.zoom===2),room);
      await page.locator(`.area-zoom[data-map="${room}"]`).click();await page.locator(`.area-zoom[data-map="${room}"]`).click();
      await page.waitForFunction(room=>[...document.querySelectorAll(`.map-row[data-map="${room}"] canvas`)].every(c=>+c.dataset.zoom===1),room);
      if(level===8){
        await page.locator('#pause').click();
        await page.waitForFunction(room=>window.points.some(p=>p.observation.level===room&&p.observation.x>=Number(document.querySelector(`.map-row[data-map="${room}"] canvas`).dataset.mapWidth)),room,{timeout:60000});
        await page.locator('#pause').click();
        const target=await page.evaluate(room=>{
          const cs=[...document.querySelectorAll(`.map-row[data-map="${room}"] canvas`)];
          const p=window.points.find(p=>p.observation.level===room&&p.observation.x>=+cs[0].dataset.mapWidth),x=p.observation.x,y=p.observation.y;
          const c=cs.find(c=>x>=+c.dataset.panelX&&x<+c.dataset.panelX+c.width&&y-8>=+c.dataset.panelY&&y-8<+c.dataset.panelY+c.height);
          return{id:p.retained,panel:+c.dataset.panel,x:Math.floor(x/32)*32+16,y:Math.floor(y/32)*32+8};
        },room),canvas=canvases.nth(target.panel);
        await canvas.scrollIntoViewIfNeeded();
        const point=await canvas.evaluate((c,t)=>{const r=c.getBoundingClientRect();return{x:r.left+(t.x-+c.dataset.panelX)*r.width/c.width,y:r.top+(t.y-+c.dataset.panelY)*r.height/c.height};},target);
        if(phone)await page.touchscreen.tap(point.x,point.y);else await page.mouse.click(point.x,point.y);
        await page.waitForFunction(()=>!document.querySelector('#inspector').hidden&&!document.querySelector('#take-control').disabled);
        assert.ok(await page.evaluate(id=>window.lastStateRequest.includes(id),target.id),'A later section selects its authentic retained history');
        assert.equal(await page.locator('#visualization').getAttribute('data-value'),'movement');
        await page.waitForFunction(([room,target])=>[...document.querySelectorAll(`.map-row[data-map="${room}"] canvas`)].some(c=>target.x>=+c.dataset.panelX&&target.x<+c.dataset.panelX+c.width&&target.y>=+c.dataset.panelY&&target.y<+c.dataset.panelY+c.height&&+c.dataset.tracePoints>0),[room,target]);
        await page.locator('#close-inspector').click();
      }
    }
    if(phone) {
      await page.setViewportSize({width:844,height:390});
      await page.waitForTimeout(200);
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,'Rotation never creates page-wide horizontal scrolling');
      const coverage=await page.locator('.area-map').evaluateAll(cs=>cs.reduce((n,c)=>n+c.width*c.height,0));
      assert.equal(coverage,1024*896,'Responsive reflow retains every pixel of the tall room');
    }
    await page.goto('about:blank');await page.close();
  }
  assert.deepEqual(errors,[]);console.log('Horizontal/vertical wraps, readable original-art crops, section navigation, zoom and authentic retained tile hits across seams passed on desktop and phone.');
}finally{await browser.close();}
