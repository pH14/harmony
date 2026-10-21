// SPDX-License-Identifier: AGPL-3.0-or-later
import test from "node:test";
import assert from "node:assert/strict";
import { ViewRecording, composeView } from "../src/recording.js";

function fixture(options = {}) {
  const track = { frames: 0, requestFrame() { this.frames++; }, stop() { this.stopped = true; } };
  class Recorder {
    static isTypeSupported(type) { return type === "video/webm;codecs=vp8"; }
    constructor(stream, options) { this.mimeType = options.mimeType; this.options = options; this.state = "inactive"; Recorder.last = this; }
    start() { this.state = "recording"; }
    stop() { this.state = "inactive"; this.ondataavailable?.({ data: new Blob(["trailer"]) }); this.onstop?.(); }
    pause() { this.state = "paused"; }
    resume() { this.state = "recording"; }
    data(value) { this.ondataavailable?.({ data: new Blob([value]) }); }
  }
  const completions = [], errors = [], states = [];
  const recording = new ViewRecording({ Recorder, onComplete: (...args) => completions.push(args), onError: error => errors.push(error), onChange: state => states.push(state), ...options });
  const canvas = { captureStream(fps) { assert.equal(fps, 0); return { getTracks: () => [track], getVideoTracks: () => [track] }; } };
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
  assert.equal(recording.track, null);
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
  const writeReady = new Promise(resolve => { release = resolve; });
  const writable = { async write(blob) { events.push(await blob.text()); await writeReady; }, async close() { events.push("close"); }, async abort() {} };
  const f = fixture({ maxPendingBytes: 6 });
  f.recording.start(f.canvas, { writable });
  assert.equal(f.recording.frame(), true);
  f.Recorder.last.data("abc");
  f.Recorder.last.data("def");
  assert.equal(f.recording.frame(), false);
  assert.equal(f.track.frames, 1);
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
  assert.equal(f.recording.frame(), true);
  assert.equal(f.track.frames, 2);
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

test("recording requests the full HD bitrate independently of display cadence", () => {
  const f = fixture();
  f.recording.start(f.canvas);
  assert.equal(f.Recorder.last.options.videoBitsPerSecond, 20_000_000);
  f.recording.stop();
});

test("full HD composition scales text and map together without losing layout", () => {
  const calls = [];
  const ctx = Object.fromEntries(["save", "restore", "scale", "fillRect", "fillText", "drawImage"].map(name => [name, (...args) => calls.push([name, ...args])]));
  const map = { width: 1280, height: 320 }, overview = { width: 1280, height: 96 };
  composeView({ width: 1920, height: 1080, getContext: () => ctx }, { map, overview, title: "Harmony Plays Famidash | 128 Workers", detail: "Stereo Madness", progress: "500,000 timelines tried · 6,780,374 game frames", attribution: "Famidash by TFDSoft, MIT / github.com/pH14/harmony" });
  assert.deepEqual(calls[0], ["save"]);
  assert.deepEqual(calls[1], ["scale", 1.5, 1.5]);
  assert.deepEqual(calls.at(-1), ["restore"]);
  assert.equal(calls.filter(([name]) => name === "drawImage").length, 2);
  assert.equal(ctx.imageSmoothingEnabled, false);
  assert.deepEqual(calls.filter(([name]) => name === "fillText").map(([, text]) => text), ["Harmony Plays Famidash | 128 Workers", "Stereo Madness", "500,000 timelines tried · 6,780,374 game frames", "Famidash by TFDSoft, MIT / github.com/pH14/harmony"]);
});


test("supported explicit H264 is preferred and unsupported codecs fall back in order", () => {
  const high = "video/mp4;codecs=avc1.640034";
  for (const [supported, expected] of [
    [[high, "video/webm;codecs=vp9"], high],
    [["video/mp4;codecs=avc1.640028", "video/mp4;codecs=avc1.42E01E", "video/webm;codecs=vp9"], "video/webm;codecs=vp9"],
    [["video/webm;codecs=vp9", "video/webm;codecs=vp8"], "video/webm;codecs=vp9"],
    [["video/webm;codecs=vp8"], "video/webm;codecs=vp8"],
    [["video/webm"], "video/webm"],
    [["video/mp4"], "video/mp4"],
  ]) {
    const f = fixture();
    f.Recorder.isTypeSupported = type => supported.includes(type);
    assert.equal(f.recording.mimeType, expected);
    f.recording.start(f.canvas);
    assert.equal(f.Recorder.last.mimeType, expected);
    f.recording.stop();
    assert.equal(f.completions[0][0].type, expected.toLowerCase());
  }
});

test("manual frame requests follow composition calls without a fixed-rate elapsed gate", () => {
  const f = fixture();
  assert.equal(f.recording.frame(), false);
  f.recording.start(f.canvas);
  assert.equal(f.track.frames, 0);
  for (let i = 0; i < 5; i++) assert.equal(f.recording.frame(), true);
  assert.equal(f.track.frames, 5);
  f.Recorder.last.pause();
  assert.equal(f.recording.frame(), false);
  f.Recorder.last.resume();
  assert.equal(f.recording.frame(), true);
  f.Recorder.last.state = "inactive";
  f.recording.stop();
  assert.equal(f.recording.state, "stopping");
  assert.equal(f.recording.frame(), false);
  f.Recorder.last.data("final frame");
  f.Recorder.last.onstop();
  assert.equal(f.recording.frame(), false);
  assert.equal(f.track.frames, 6);
  assert.equal(f.recording.track, null);
});

test("manual capture requires a requestFrame-capable video track and releases unsupported streams", () => {
  const f = fixture();
  delete f.track.requestFrame;
  assert.throws(() => f.recording.start(f.canvas), /Display-paced.*unavailable/);
  assert.equal(f.track.stopped, true);
  assert.equal(f.recording.track, null);
  assert.equal(f.recording.state, "idle");
  assert.deepEqual(f.completions, []);
});

test("frame request failure aborts file capture once and releases its track", async () => {
  let aborts = 0;
  const f = fixture();
  f.recording.start(f.canvas, { writable: { async write() {}, async close() {}, async abort() { aborts++; } } });
  f.track.requestFrame = () => { throw new Error("capture failed"); };
  assert.equal(f.recording.frame(), false);
  assert.equal(f.recording.frame(), false);
  await tick();
  assert.equal(aborts, 1);
  assert.equal(f.errors.length, 1);
  assert.equal(f.errors[0].message, "capture failed");
  assert.equal(f.track.stopped, true);
  assert.equal(f.recording.track, null);
  assert.deepEqual(f.completions, []);
});
