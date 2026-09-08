const test = require('node:test');
const assert = require('node:assert/strict');
const report = require('./benchmark-report.cjs');

test('reports failed matrix cases and links evidence without trusting names as HTML', async () => {
  const calls = [];
  const summary = new Proxy({}, {get: (_, method) => method === 'then' ? undefined : (...args) => {calls.push([method,...args]); return summary;}});
  const jobs = [{name:'Nova <failed>',html_url:'https://example.test/job',conclusion:'failure',started_at:'2026-01-01T00:00:00Z',completed_at:'2026-01-01T00:00:12Z'}, {name:'report'}];
  const github = {rest:{actions:{listJobsForWorkflowRun:'jobs',listWorkflowRunArtifacts:'artifacts'}},paginate:async method => method==='jobs' ? jobs : [{id:7,name:'evidence',expired:false}]};
  await report({github,context:{repo:{owner:'owner',repo:'repo'},runId:1,serverUrl:'https://github.com'},core:{summary}});
  const table=calls.find(c=>c[0]==='addTable')[1];
  assert.equal(table.length,2);
  assert.equal(table[1][1],'failure');
  assert.equal(table[1][2],'12 s');
  assert.match(table[1][0],/Nova &lt;failed&gt;/);
  assert.match(calls.find(c=>c[0]==='addList')[1][0],/actions\/runs\/1\/artifacts\/7/);
});

test('missing artifacts are explicit even when every case fails before upload', async () => {
  const text=[];const summary=new Proxy({}, {get:(_,method)=>method === 'then' ? undefined : (...args)=>{if(method==='addRaw')text.push(args[0]);return summary;}});
  await report({github:{rest:{actions:{}},paginate:async()=>[]},context:{repo:{},runId:1},core:{summary}});
  assert.match(text.join(''),/No artifacts were published/);
});
