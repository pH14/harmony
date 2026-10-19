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
