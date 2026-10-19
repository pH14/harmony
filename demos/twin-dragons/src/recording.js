// SPDX-License-Identifier: AGPL-3.0-or-later
const TYPES = ["video/webm;codecs=vp9", "video/webm;codecs=vp8", "video/webm", "video/mp4"];

export class ViewRecording {
  constructor({ Recorder = globalThis.MediaRecorder, maxBytes = 256 * 1024 * 1024, maxDuration = 10 * 60 * 1000, onChange = () => {}, onComplete = () => {}, onError = () => {} } = {}) {
    Object.assign(this, { Recorder, maxBytes, maxDuration, onChange, onComplete, onError });
    this.state = "idle";
  }
  supported(canvas) {
    return !!canvas.captureStream && !!this.Recorder && TYPES.some(type => this.Recorder.isTypeSupported(type));
  }
  start(canvas) {
    if (this.state !== "idle") throw new Error("A recording is already in progress.");
    if (!this.supported(canvas)) throw new Error("Video recording is unavailable in this browser. Try Chrome or Firefox.");
    this.chunks = [];
    this.bytes = 0;
    this.reason = "";
    try {
      this.stream = canvas.captureStream(30);
      this.recorder = new this.Recorder(this.stream, {
        mimeType: TYPES.find(type => this.Recorder.isTypeSupported(type)), videoBitsPerSecond: 5_000_000,
      });
      this.recorder.ondataavailable = ({ data }) => {
        if (this.state === "idle" || !data.size) return;
        this.chunks.push(data);
        this.bytes += data.size;
        if (this.bytes >= this.maxBytes && this.state === "recording") this.stop("Size limit reached");
      };
      this.recorder.onerror = (event) => this.fail(event.error || new Error("Video encoder failed."));
      this.recorder.onstop = () => this.finish();
      this.stream.getTracks().forEach(track => { track.onended = () => this.stop("Capture ended"); });
      this.recorder.start(1000);
      this.started = performance.now();
      this.state = "recording";
      this.timer = setTimeout(() => this.stop("Recording duration limit reached"), this.maxDuration);
      this.onChange(this.state);
    } catch (error) {
      this.cleanup();
      throw error;
    }
  }
  stop(reason = "") {
    if (this.state !== "recording") return;
    this.reason = reason;
    this.state = "stopping";
    this.onChange(this.state);
    try { if (this.recorder.state !== "inactive") this.recorder.stop(); } catch (error) { this.fail(error); }
  }
  finish() {
    if (this.state === "idle") return;
    const blob = new Blob(this.chunks, { type: this.recorder.mimeType });
    const reason = this.reason;
    this.cleanup();
    if (blob.size) this.onComplete(blob, reason);
    else this.onError(new Error("The browser produced an empty recording. Try recording for longer."));
  }
  fail(error) {
    if (this.state === "idle") return;
    this.cleanup();
    this.onError(error);
  }
  cleanup() {
    clearTimeout(this.timer);
    if (this.recorder) {
      this.recorder.ondataavailable = this.recorder.onstop = this.recorder.onerror = null;
      if (this.recorder.state !== "inactive") {
        try { this.recorder.stop(); } catch { /* The encoder may already have failed. */ }
      }
    }
    this.stream?.getTracks().forEach(track => { track.onended = null; track.stop(); });
    this.recorder = this.stream = null;
    this.chunks = [];
    this.state = "idle";
    this.onChange(this.state);
  }
}

function fit(ctx, canvas, x, y, width, height) {
  const scale = Math.min(width / canvas.width, height / canvas.height);
  const w = canvas.width * scale, h = canvas.height * scale;
  ctx.drawImage(canvas, x + (width - w) / 2, y + (height - h) / 2, w, h);
}

export function composeView(target, { map, overview, title, detail, progress, attribution }) {
  const ctx = target.getContext("2d");
  ctx.fillStyle = "#081727";
  ctx.fillRect(0, 0, target.width, target.height);
  ctx.fillStyle = "#eff5ff";
  ctx.font = "bold 24px sans-serif";
  ctx.fillText(title, 24, 38, target.width - 48);
  ctx.font = "16px sans-serif";
  ctx.fillStyle = "#a8c6df";
  ctx.fillText(detail, 24, 66, target.width - 48);
  ctx.imageSmoothingEnabled = false;
  const vertical = map.height > map.width;
  if (vertical) {
    fit(ctx, map, 24, 88, 950, 560);
    if (overview) fit(ctx, overview, 1000, 88, 256, 560);
  } else {
    fit(ctx, map, 24, 88, 1232, 438);
    if (overview) fit(ctx, overview, 24, 548, 1232, 92);
  }
  ctx.fillStyle = "#eff5ff";
  ctx.fillText(progress, 24, 680, target.width - 48);
  ctx.font = "12px sans-serif";
  ctx.fillStyle = "#a8c6df";
  ctx.fillText(attribution, 24, 706, target.width - 48);
}
