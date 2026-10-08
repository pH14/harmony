// SPDX-License-Identifier: AGPL-3.0-or-later
import { test } from "node:test";
import assert from "node:assert/strict";
import { GameAudio } from "../src/audio.js";
class Context {
  constructor() {
    this.currentTime = 0;
    this.state = "running";
    this.nodes = [];
  }
  resume() {
    return Promise.resolve();
  }
  createGain() {
    return { gain: { value: 1 }, connect() {} };
  }
  createBuffer(channels, n) {
    const data = Array.from({ length: channels }, () => new Float32Array(n));
    return { getChannelData: (c) => data[c] };
  }
  createBufferSource() {
    const n = {
      connect() {},
      disconnect() {},
      start(at) {
        this.at = at;
      },
      stop() {
        this.stopped = true;
      },
    };
    this.nodes.push(n);
    return n;
  }
}
test("audio comes from the shadow engine and queued sound is stopped on release", async () => {
  let restored, enabled;
  const e = {
    restore: (s) => (restored = s),
    enableAudio: (v) => (enabled = v),
    run() {},
    audio: () => new Int16Array([32767, -32768, 100, -100]),
  };
  const a = new GameAudio(async () => e, Context);
  await a.start(() => "current snapshot");
  assert.equal(restored, "current snapshot");
  assert.equal(enabled, true);
  a.flush();
  assert.equal(a.samples, 2);
  assert.equal(a.sources.size, 1);
  assert.equal(a.context.nodes[0].buffer.getChannelData(1)[0], -1);
  assert.equal(a.mute(), true);
  assert.equal(a.gain.gain.value, 0);
  a.stop();
  assert.equal(enabled, false);
  assert.equal(a.context.nodes[0].stopped, true);
});
test("a late audio load cannot start after takeover has stopped", async () => {
  let resolve,
    restored = false;
  const a = new GameAudio(() => new Promise((r) => (resolve = r)), Context);
  const start = a.start(() => "stale");
  a.stop();
  resolve({
    restore() {
      restored = true;
    },
    enableAudio() {},
  });
  await start;
  assert.equal(restored, false);
  assert.equal(a.engine, null);
});

test("failed audio initialization can retry without reviving a stopped session", async () => {
  let calls = 0;
  const e = { restore() {}, enableAudio() {} };
  const a = new GameAudio(async () => {
    if (++calls === 1) throw new Error("offline");
    return e;
  }, Context);
  await assert.rejects(
    a.start(() => "root"),
    /offline/,
  );
  assert.equal(a.active, false);
  await a.start(() => "root");
  assert.equal(a.engine, e);
  assert.equal(calls, 2);
  a.stop();
  let reject;
  const b = new GameAudio(() => new Promise((_, r) => (reject = r)), Context);
  const loading = b.start(() => "stale");
  b.stop();
  reject(new Error("late failure"));
  await loading;
  assert.equal(b.active, false);
});
