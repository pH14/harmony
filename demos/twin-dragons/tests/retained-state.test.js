// SPDX-License-Identifier: AGPL-3.0-or-later

import test from 'node:test';
import assert from 'node:assert/strict';
import { installRetainedCollector, retainedDescendant } from './retained-state.mjs';

class Worker {
  listeners = new Map();
  messages = [];
  constructor(handle) { this.handle = handle; }
  addEventListener(type, listener) {
    if (!this.listeners.has(type)) this.listeners.set(type, new Set());
    this.listeners.get(type).add(listener);
  }
  removeEventListener(type, listener) { this.listeners.get(type)?.delete(listener); }
  emit(data) { for (const listener of [...(this.listeners.get('message') ?? [])]) listener({ data }); }
  postMessage(message) { this.messages.push(message); this.handle(message, this); }
  assertReleased() { assert.equal([...this.listeners.values()].reduce((n, listeners) => n + listeners.size, 0), 0); }
}
const state = id => ({ id, snapshot: new Uint8Array([7, 12, 99]), actions: [{ buttons: 128, frames: 12 }], frames: 125 });

test('queries all recorded candidates and captures an older live descendant after the latest was evicted', async () => {
  let paused = false;
  const worker = new Worker((message, target) => {
    if (message.type === 'pause') {
      queueMicrotask(() => { paused = true; target.emit({ type: 'paused' }); });
      return;
    }
    assert.ok(paused, 'states requests must follow the observed pause acknowledgment');
    assert.ok(message.ids.length <= 12);
    const live = message.ids.includes('2:1') ? [state('2:1')] : [];
    target.emit({ type: 'states', request: message.request, states: live, missing: message.ids.filter(id => id !== '2:1') });
  });
  const candidates = Array.from({ length: 25 }, (_, i) => `2:${i + 1}`);
  const found = await retainedDescendant({ worker, candidates, branch: 2 });
  assert.deepEqual(found, state('2:1'));
  assert.deepEqual(worker.messages.slice(1).map(message => message.ids.length), [12, 12, 1]);
  assert.equal(new Set(worker.messages.slice(1).map(message => message.request)).size, 3);
  worker.assertReleased();
});

test('unrelated and stale responses cannot supply a live snapshot for the current request', async () => {
  const worker = new Worker((message, target) => {
    if (message.type === 'pause') return target.emit({ type: 'paused' });
    target.emit({ type: 'states', request: 'earlier-request', states: [state('2:9')], missing: [] });
    target.emit({ type: 'ready', request: message.request, states: [state('2:9')], missing: [] });
    queueMicrotask(() => target.emit({ type: 'states', request: message.request, states: [{ ...state('2:9'), snapshot: null }], missing: [] }));
  });
  await assert.rejects(retainedDescendant({ worker, candidates: ['2:9'], branch: 2 }), /No currently retained nested descendant/);
  worker.assertReleased();
});

test('a null snapshot does not hide another actual live response in the same batch', async () => {
  const worker = new Worker((message, target) => {
    if (message.type === 'pause') return target.emit({ type: 'paused' });
    target.emit({ type: 'states', request: message.request, states: [{ ...state('2:4'), snapshot: null }, state('2:3')], missing: [] });
  });
  assert.deepEqual(await retainedDescendant({ worker, candidates: ['2:3', '2:4'], branch: 2 }), state('2:3'));
  worker.assertReleased();
});

test('missing, null and root-only candidates cannot certify a descendant', async () => {
  for (const kind of ['missing', 'null', 'root']) {
    const worker = new Worker((message, target) => {
      if (message.type === 'pause') return target.emit({ type: 'paused' });
      target.emit({ type: 'states', request: message.request, states: kind === 'null' ? [{ ...state('2:8'), snapshot: null }] : [], missing: kind === 'missing' ? message.ids : [] });
    });
    await assert.rejects(retainedDescendant({ worker, candidates: kind === 'root' ? ['2:0', '1:8'] : ['2:8'], branch: 2 }), /No currently retained nested descendant/);
    worker.assertReleased();
  }
});

test('matching request with a foreign state refuses instead of accepting its snapshot', async () => {
  const worker = new Worker((message, target) => {
    if (message.type === 'pause') return target.emit({ type: 'paused' });
    target.emit({ type: 'states', request: message.request, states: [state('1:9')], missing: [] });
  });
  await assert.rejects(retainedDescendant({ worker, candidates: ['2:9'], branch: 2 }), /names different states/);
  worker.assertReleased();
});


test('includes actual candidates delivered before the pause acknowledgment without assuming a drained archive', async () => {
  const candidates = ['2:9'];
  const worker = new Worker((message, target) => {
    if (message.type === 'pause') {
      candidates.push('2:10');
      return target.emit({ type: 'paused' });
    }
    assert.deepEqual(message.ids, ['2:10', '2:9']);
    target.emit({ type: 'states', request: message.request, states: [state('2:10')], missing: ['2:9'] });
  });
  assert.equal((await retainedDescendant({ worker, candidates, branch: 2 })).id, '2:10');
  worker.assertReleased();
});


test('complete candidate pool preserves every announced unique descendant and refuses overflow before growing', async () => {
  const scope = {};
  installRetainedCollector(scope);
  const points = Array.from({ length: 4096 }, (_, i) => ({ retained: `2:${i + 1}` }));
  scope.tdTestCollectNested([{ retained: null }, { retained: '2:0' }, { retained: '1:8' }, ...points]);
  scope.tdTestCollectNested(points);
  assert.deepEqual([...scope.tdTestNestedCandidates], points.map(point => point.retained));
  assert.throws(() => scope.tdTestCollectNested([{ retained: '2:4097' }]), /exceeds 4096 unique states/);
  assert.equal(scope.tdTestNestedCandidates.length, 4096);
  assert.deepEqual([...scope.tdTestNestedCandidates], points.map(point => point.retained));
  assert.equal(scope.tdTestNestedCandidates.overflow, true);
  const worker = new Worker(() => assert.fail('an overflowed candidate pool must never certify a descendant'));
  await assert.rejects(retainedDescendant({ worker, candidates: scope.tdTestNestedCandidates, branch: 2 }), /Invalid retained descendant candidates/);
  assert.equal(worker.messages.length, 0);
  worker.assertReleased();
});

test('candidate overflow delivered before pause acknowledgment also refuses instead of certifying a truncated pool', async () => {
  const scope = {};
  installRetainedCollector(scope);
  scope.tdTestCollectNested(Array.from({ length: 4096 }, (_, i) => ({ retained: `2:${i + 1}` })));
  const worker = new Worker((message, target) => {
    assert.equal(message.type, 'pause');
    assert.throws(() => scope.tdTestCollectNested([{ retained: '2:4097' }]), /exceeds 4096/);
    target.emit({ type: 'paused' });
  });
  await assert.rejects(retainedDescendant({ worker, candidates: scope.tdTestNestedCandidates, branch: 2 }), /Invalid retained descendant candidates/);
  assert.equal(worker.messages.length, 1);
  worker.assertReleased();
});


test('selection ends collection before queries and later resumed branch batches cannot grow or overflow it', async () => {
  const scope = {};
  installRetainedCollector(scope);
  scope.tdTestCollectNested([{ retained: '2:1' }]);
  const worker = new Worker((message, target) => {
    if (message.type === 'pause') return target.emit({ type: 'paused' });
    assert.equal(scope.tdTestNestedCandidates.collecting, false);
    scope.tdTestCollectNested(Array.from({ length: 5000 }, (_, i) => ({ retained: `2:${i + 2}` })));
    target.emit({ type: 'states', request: message.request, states: [state('2:1')], missing: [] });
  });
  assert.equal((await retainedDescendant({ worker, candidates: scope.tdTestNestedCandidates, branch: 2 })).id, '2:1');
  scope.tdTestCollectNested(Array.from({ length: 5000 }, (_, i) => ({ retained: `2:${i + 5002}` })));
  assert.deepEqual([...scope.tdTestNestedCandidates], ['2:1']);
  assert.equal(scope.tdTestNestedCandidates.overflow, undefined);
  assert.equal(scope.tdTestNestedCandidates.collecting, false);
  worker.assertReleased();
});

test('failed acquisition also disarms collection without accepting missing evidence', async () => {
  const scope = {};
  installRetainedCollector(scope);
  scope.tdTestCollectNested([{ retained: '2:1' }]);
  const worker = new Worker((message, target) => {
    if (message.type === 'pause') return target.emit({ type: 'paused' });
    target.emit({ type: 'states', request: message.request, states: [], missing: message.ids });
  });
  await assert.rejects(retainedDescendant({ worker, candidates: scope.tdTestNestedCandidates, branch: 2 }), /No currently retained nested descendant/);
  scope.tdTestCollectNested(Array.from({ length: 5000 }, (_, i) => ({ retained: `2:${i + 2}` })));
  assert.deepEqual([...scope.tdTestNestedCandidates], ['2:1']);
  assert.equal(scope.tdTestNestedCandidates.collecting, false);
  worker.assertReleased();
});
