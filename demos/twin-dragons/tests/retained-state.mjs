// SPDX-License-Identifier: AGPL-3.0-or-later

export function installRetainedCollector(scope = window) {
  const candidates = scope.tdTestNestedCandidates = [];
  scope.tdTestCollectNested = points => {
    for (const point of points) {
      const id = point.retained;
      if (typeof id !== 'string' || !id.startsWith('2:') || id === '2:0' || candidates.includes(id)) continue;
      if (candidates.length >= 200) {
        candidates.overflow = true;
        throw new Error('Nested retained candidate pool exceeds 200 unique states');
      }
      candidates.push(id);
    }
  };
}

export async function retainedDescendant({ worker, candidates, branch }) {
  if (!Array.isArray(candidates) || candidates.length > 200 || candidates.overflow || !Number.isSafeInteger(branch) || branch < 1)
    throw new Error('Invalid retained descendant candidates');
  const prefix = `${branch}:`, root = `${prefix}0`;
  const deadline = Date.now() + 30000;
  let next = 0;
  const exchange = (message, accepts) => new Promise((resolve, reject) => {
    let timer;
    const cleanup = () => {
      clearTimeout(timer);
      worker.removeEventListener('message', receive);
      worker.removeEventListener('error', failed);
      worker.removeEventListener('messageerror', failed);
    };
    const failed = () => {
      cleanup();
      reject(new Error('Worker failed during retained descendant query'));
    };
    const receive = ({ data }) => {
      if (!accepts(data)) return;
      if (Date.now() >= deadline) {
        cleanup();
        reject(new Error('Retained descendant query timed out'));
        return;
      }
      cleanup();
      resolve(data);
    };
    try {
      worker.addEventListener('message', receive);
      worker.addEventListener('error', failed);
      worker.addEventListener('messageerror', failed);
      timer = setTimeout(() => {
        cleanup();
        reject(new Error('Retained descendant query timed out'));
      }, Math.max(0, deadline - Date.now()));
      worker.postMessage(message);
    } catch (error) {
      cleanup();
      reject(error);
    }
  });
  await exchange({ type: 'pause' }, data => data?.type === 'paused');
  if (candidates.length > 200 || candidates.overflow) throw new Error('Invalid retained descendant candidates');
  const ids = [...new Set(candidates)].filter(id => typeof id === 'string' && id.startsWith(prefix) && id !== root).reverse();
  const observed = [];
  for (let start = 0; start < ids.length; start += 12) {
    const requested = ids.slice(start, start + 12);
    const request = `retained-descendant-${branch}-${++next}`;
    const reply = await exchange({ type: 'states', ids: requested, request }, data => data?.type === 'states' && data.request === request);
    if (!Array.isArray(reply.states) || !Array.isArray(reply.missing))
      throw new Error('Invalid retained descendant reply');
    const answered = [...reply.states.map(state => state.id), ...reply.missing];
    if (answered.length !== requested.length || new Set(answered).size !== answered.length || answered.some(id => !requested.includes(id)))
      throw new Error('Retained descendant reply names different states');
    for (const id of requested) {
      const state = reply.states.find(state => state.id === id);
      const live = state && ArrayBuffer.isView(state.snapshot) && state.snapshot.byteLength > 0;
      observed.push({ id, snapshot: !!live, missing: reply.missing.includes(id) });
      if (live) return state;
    }
  }
  throw new Error(`No currently retained nested descendant: ${JSON.stringify(observed)}`);
}
