// SPDX-License-Identifier: AGPL-3.0-or-later
import {webkit} from '@playwright/test';
import {mkdir} from 'node:fs/promises';
import assert from 'node:assert/strict';
const browser=await webkit.launch({headless:true});
try {
 const page=await browser.newPage({viewport:{width:390,height:744},isMobile:true,hasTouch:true,reducedMotion:'reduce'});
 await page.addInitScript(()=>localStorage.setItem('harmony.nova.theme','dark'));
 await mkdir('test-results',{recursive:true});
 await page.goto(process.env.DEMO_URL || 'http://127.0.0.1:4173/');
 const ready=step=>page.waitForFunction(s=>document.querySelector('#guided-tour')?.dataset.step===String(s)&&document.querySelector('.tour-card').getAttribute('aria-busy')==='false'&&document.querySelector('#tour-next').textContent!=='Try again',step,{timeout:60000});
 await ready(0);
 for(let step=1;step<=4;step++){await page.locator('#tour-next').tap();await ready(step);}
 await page.locator('#take-control').tap();
 await page.waitForFunction(()=>document.querySelector('#inspector').classList.contains('controlling'));
 await page.waitForTimeout(150);
 await page.locator('#tour-next').tap();await ready(5);
 for(const size of [{width:390,height:744},{width:390,height:664},{width:744,height:390}]) {
  await page.setViewportSize(size);await page.waitForTimeout(150);
  assert.equal(await page.locator('#tour-rings rect').count(),1);
  const lit=await page.locator('.branch-actions').screenshot();
  await page.locator('.tour-shade').evaluate(e=>e.style.visibility='hidden');
  assert.deepEqual(await page.locator('.branch-actions').screenshot(),lit,'WebKit action pixels must be fully lit');
  await page.locator('.tour-shade').evaluate(e=>e.style.visibility='');
  for(const id of ['search-here','discard-branch'])assert.equal(await page.locator('#'+id).evaluate(e=>{const r=e.getBoundingClientRect();return r.top>=0&&r.bottom<=visualViewport.height&&document.elementFromPoint(r.left+r.width/2,r.top+r.height/2)?.closest('button')===e;}),true);
  await page.screenshot({path:`test-results/tour-webkit-${size.width}-${size.height}.png`});
 }
 await page.locator('#search-here').tap();await ready(6);
 await page.waitForFunction(()=>document.querySelector('#inspector').hidden && document.querySelector('button[data-search="1"][aria-pressed="true"]'));
 for(const size of [{width:390,height:744},{width:390,height:664},{width:744,height:390}]) {
  await page.setViewportSize(size);await page.waitForTimeout(200);
  await page.screenshot({path:`test-results/tour-webkit-searches-${size.width}-${size.height}.png`});
  const geometry=await page.locator('#branches').evaluate(p=>{const nav=p.querySelector('nav').getBoundingClientRect(), heading=p.querySelector('h2').getBoundingClientRect(),r=p.getBoundingClientRect();return {navTop:nav.top,navBottom:nav.bottom,top:r.top,bottom:r.bottom,headingTop:heading.top,headingBottom:heading.bottom};});
  assert.ok(geometry.navTop>=geometry.top && geometry.navBottom<=geometry.bottom,`Searches list fits inside its fixed pane: ${JSON.stringify(geometry)}`);
  assert.ok(geometry.headingTop>=geometry.top && geometry.headingBottom<=geometry.bottom);
  const alignment=await page.locator('#tour-rings rect').evaluate(r=>{const a=r.getBoundingClientRect(),b=document.querySelector('#branches').getBoundingClientRect();return Math.max(Math.abs(a.top-b.top),Math.abs(a.bottom-b.bottom),Math.abs(a.left-b.left),Math.abs(a.right-b.right));});
  assert.ok(alignment<1,'Searches spotlight follows the actual pane without exposing map pixels');
  assert.equal(await page.locator('#tour-rings rect').getAttribute('rx'),'14');
  for(const selector of ['#branches h2','#branch-tree [aria-pressed="true"]']) {
   const lit=await page.locator(selector).screenshot();
   await page.locator('.tour-shade').evaluate(e=>e.style.visibility='hidden');
   assert.deepEqual(await page.locator(selector).screenshot(),lit,'Searches heading and selected branch are fully lit');
   await page.locator('.tour-shade').evaluate(e=>e.style.visibility='');
  }
 }
 assert.equal(await page.locator('#tour-rings rect').count(),1);
 await page.locator('button[data-search="0"]').tap();
 await page.waitForFunction(()=>document.querySelector('button[data-search="0"][aria-pressed="true"]'));
 console.log('WebKit phone: dark-mode Branch/Discard pixels fully lit, one focused outline, real tap targets, toolbar-sized resize, landscape, two-search pane alignment, lit selected branch and actual search switching passed.');
 await page.close();
}finally{await browser.close();}
