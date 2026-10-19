// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { ViewRecording } from "../src/recording.js";

function fixture(options = {}) {
  const track = { stop() { this.stopped = true; } };
  class Recorder {
    static isTypeSupported(type) { return type === "video/webm;codecs=vp8"; }
    constructor(stream, options) { this.mimeType = options.mimeType; this.state = "inactive"; Recorder.last = this; }
    start() { this.state = "recording"; }
    stop() { this.state = "inactive"; this.ondataavailable?.({ data: new Blob(["trailer"]) }); this.onstop?.(); }
    pause() { this.state = "paused"; }
    resume() { this.state = "recording"; }
    data(value) { this.ondataavailable?.({ data: new Blob([value]) }); }
  }
  const completions = [], errors = [], states = [];
  const recording = new ViewRecording({ Recorder, onComplete: (...args) => completions.push(args), onError: error => errors.push(error), onChange: state => states.push(state), ...options });
  const canvas = { captureStream(fps) { assert.equal(fps, 30); return { getTracks: () => [track] }; } };
  return { recording, Recorder, canvas, track, completions, errors, states };
}

test("recording includes final encoder chunk and releases every track", async () => {
  const { recording, Recorder, canvas, track, completions, states } = fixture();
  recording.start(canvas);
  assert.throws(() => recording.start(canvas), /already/);
  Recorder.last.data("frame");
  recording.stop();
  recording.stop();
  assert.equal(await completions[0][0].text(), "frametrailer");
  assert.equal(completions[0][0].type, "video/webm;codecs=vp8");
  assert.equal(track.stopped, true);
  assert.deepEqual(states, ["recording", "stopping", "idle"]);
  assert.equal(recording.stream, null);
  recording.start(canvas);
  recording.stop();
  assert.equal(completions.length, 2);
});

test("size and time limits finalize a downloadable recording", async () => {
  const f = fixture({ maxBytes: 3 });
  f.recording.start(f.canvas);
  f.Recorder.last.data("large");
  assert.equal(f.recording.state, "idle");
  assert.match(f.completions[0][1], /Size limit/);
  const timed = fixture({ maxDuration: 5 });
  timed.recording.start(timed.canvas);
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(timed.recording.state, "idle");
  assert.match(timed.completions[0][1], /duration limit/);
});

test("encoder failure discards incomplete chunks and permits retry", () => {
  const f = fixture();
  f.recording.start(f.canvas);
  f.Recorder.last.data("partial");
  f.Recorder.last.onerror({ error: new Error("encoder broke") });
  assert.equal(f.errors[0].message, "encoder broke");
  assert.equal(f.track.stopped, true);
  assert.equal(f.recording.state, "idle");
  assert.deepEqual(f.completions, []);
  f.recording.start(f.canvas);
  f.recording.stop();
  assert.equal(f.completions.length, 1);
});

test("startup failure releases the stream and unsupported browsers never capture", () => {
  const f = fixture();
  f.Recorder.prototype.start = () => { throw new Error("start failed"); };
  assert.throws(() => f.recording.start(f.canvas), /start failed/);
  assert.equal(f.track.stopped, true);
  assert.equal(f.recording.state, "idle");
  f.Recorder.isTypeSupported = () => false;
  assert.equal(f.recording.supported(f.canvas), false);
  assert.throws(() => f.recording.start(f.canvas), /unavailable/);
});

test("ended track finalizes; empty output reports a recoverable error", () => {
  const f = fixture();
  f.recording.start(f.canvas);
  f.track.onended();
  assert.match(f.completions[0][1], /Capture ended/);
  f.recording.start(f.canvas);
  f.Recorder.last.onstop();
  assert.match(f.errors[0].message, /empty recording/);
  assert.equal(f.track.stopped, true);
});

test("a source that already stopped the encoder still keeps its final data", async () => {
  const f = fixture();
  f.recording.start(f.canvas);
  f.Recorder.last.state = "inactive";
  f.track.onended();
  assert.equal(f.recording.state, "stopping");
  f.Recorder.last.data("final frame");
  f.Recorder.last.onstop();
  assert.equal(await f.completions[0][0].text(), "final frame");
  assert.deepEqual(f.errors, []);
});

const tick = () => new Promise(resolve => setImmediate(resolve));

test("file capture writes chunks in order, closes after final data, and bypasses clip limits", async () => {
  const events = [];
  const writable = { async write(blob) { events.push(await blob.text()); }, async close() { events.push("close"); }, async abort() { events.push("abort"); } };
  const f = fixture({ maxBytes: 1, maxDuration: 1 });
  f.recording.start(f.canvas, { writable });
  f.Recorder.last.data("first");
  f.Recorder.last.data("second");
  await new Promise(resolve => setTimeout(resolve, 10));
  assert.equal(f.recording.state, "recording", "File capture has no clip time or size cap");
  f.recording.stop();
  await tick();
  assert.deepEqual(events, ["first", "second", "trailer", "close"]);
  assert.equal(f.completions[0][0], null);
  assert.deepEqual(f.completions[0][2], { file: true, bytes: 18 });
  assert.equal(f.track.stopped, true);
});

test("file backpressure pauses capture until ordered writes drain, with bounded queued chunks", async () => {
  let release;
  const events = [];
  const gate = new Promise(resolve => { release = resolve; });
  const writable = { async write(blob) { events.push(await blob.text()); await gate; }, async close() { events.push("close"); }, async abort() {} };
  const f = fixture({ maxPendingBytes: 6 });
  f.recording.start(f.canvas, { writable });
  f.Recorder.last.data("abc");
  f.Recorder.last.data("def");
  assert.equal(f.Recorder.last.state, "paused");
  assert.equal(f.recording.pendingBytes, 6);
  assert.equal(f.recording.backpressured, true);
  await tick();
  assert.deepEqual(events, ["abc"], "Only one ordered disk write runs at once");
  release();
  await tick();
  assert.equal(f.Recorder.last.state, "recording");
  assert.equal(f.recording.pendingBytes, 0);
  assert.equal(f.recording.backpressured, false);
  f.recording.stop();
  await tick();
  assert.deepEqual(events, ["abc", "def", "trailer", "close"]);
});

test("file write and close failures abort the uncommitted file and release capture", async () => {
  for (const failed of ["write", "close"]) {
    let aborted = false;
    const writable = { async write() { if (failed === "write") throw new Error("disk write failed"); }, async close() { throw new Error("disk close failed"); }, async abort() { aborted = true; } };
    const f = fixture();
    f.recording.start(f.canvas, { writable });
    f.Recorder.last.data("frame");
    f.recording.stop();
    await tick();
    assert.equal(f.errors.length, 1);
    assert.match(f.errors[0].message, /disk/);
    assert.equal(aborted, true);
    assert.equal(f.track.stopped, true);
    assert.equal(f.recording.state, "idle");
    assert.deepEqual(f.completions, []);
  }
});

test("aborting a pending file write cannot later cancel a new recording", async () => {
  let reject;
  let aborted = false;
  const writable = { write() { return new Promise((_, fail) => { reject = fail; }); }, async close() {}, async abort() { aborted = true; } };
  const f = fixture();
  f.recording.start(f.canvas, { writable });
  f.Recorder.last.data("frame");
  await tick();
  f.recording.cleanup();
  assert.equal(aborted, true);
  f.recording.start(f.canvas);
  reject(new Error("old write was canceled"));
  await tick();
  assert.equal(f.recording.state, "recording");
  assert.deepEqual(f.errors, []);
  f.recording.stop();
});
