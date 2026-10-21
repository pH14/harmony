// SPDX-License-Identifier: AGPL-3.0-or-later
const TYPES = ["video/webm;codecs=vp9", "video/webm;codecs=vp8", "video/webm", "video/mp4"];

export class ViewRecording {
  constructor({ Recorder = globalThis.MediaRecorder, maxBytes = 256 * 1024 * 1024, maxDuration = 10 * 60 * 1000, maxPendingBytes = 8 * 1024 * 1024, onChange = () => {}, onComplete = () => {}, onError = () => {} } = {}) {
    Object.assign(this, { Recorder, maxBytes, maxDuration, maxPendingBytes, onChange, onComplete, onError });
    this.state = "idle";
  }
  get mimeType() {
    return this.Recorder && TYPES.find(type => this.Recorder.isTypeSupported(type));
  }
  supported(canvas) { return !!canvas.captureStream && !!this.mimeType; }
  start(canvas, { writable = null } = {}) {
    if (this.state !== "idle") throw new Error("A recording is already in progress.");
    if (!this.supported(canvas)) throw new Error("Video recording is unavailable in this browser. Try Chrome or Firefox.");
    this.chunks = [];
    this.bytes = this.pendingBytes = 0;
    this.reason = "";
    this.backpressured = false;
    this.writable = writable;
    this.writes = Promise.resolve();
    try {
      this.stream = canvas.captureStream(60);
      this.recorder = new this.Recorder(this.stream, { mimeType: this.mimeType, videoBitsPerSecond: 20_000_000 });
      this.recorder.ondataavailable = ({ data }) => {
        if (this.state === "idle" || !data.size) return;
        this.bytes += data.size;
        if (this.writable) this.write(data);
        else {
          this.chunks.push(data);
          if (this.bytes >= this.maxBytes && this.state === "recording") this.stop("Size limit reached");
        }
      };
      this.recorder.onerror = event => this.fail(event.error || new Error("Video encoder failed."));
      this.recorder.onstop = () => this.finish();
      this.stream.getTracks().forEach(track => { track.onended = () => this.stop("Capture ended"); });
      this.recorder.start(1000);
      this.started = performance.now();
      this.state = "recording";
      if (!writable) this.timer = setTimeout(() => this.stop("Recording duration limit reached"), this.maxDuration);
      this.onChange(this.state);
    } catch (error) { this.cleanup(); throw error; }
  }
  write(data) {
    const writable = this.writable;
    this.pendingBytes += data.size;
    this.writes = this.writes.then(async () => {
      if (this.writable !== writable) return;
      await writable.write(data);
      if (this.writable !== writable) return;
      this.pendingBytes -= data.size;
      if (this.backpressured && this.pendingBytes <= this.maxPendingBytes / 2 && this.state === "recording") {
        this.recorder.resume();
        this.backpressured = false;
        this.onChange(this.state);
      }
    }).catch(error => { if (this.writable === writable) this.fail(error); });
    if (this.pendingBytes >= this.maxPendingBytes && this.state === "recording" && !this.backpressured) {
      try {
        this.recorder.pause();
        this.backpressured = true;
        this.onChange(this.state);
      } catch (error) { this.fail(error); }
    }
  }
  stop(reason = "") {
    if (this.state !== "recording") return;
    this.reason = reason;
    this.state = "stopping";
    this.onChange(this.state);
    try { if (this.recorder.state !== "inactive") this.recorder.stop(); } catch (error) { this.fail(error); }
  }
  async finish() {
    if (this.state === "idle") return;
    if (this.state === "recording") { this.state = "stopping"; this.onChange(this.state); }
    const reason = this.reason;
    if (this.writable) {
      const writable = this.writable;
      try {
        await this.writes;
        if (this.writable !== writable) return;
        if (!this.bytes) throw new Error("The browser produced an empty recording. Try recording for longer.");
        await writable.close();
        if (this.writable !== writable) return;
        const bytes = this.bytes;
        this.writable = null;
        this.cleanup();
        this.onComplete(null, reason, { file: true, bytes });
      } catch (error) { if (this.writable === writable) this.fail(error); }
    } else {
      const blob = new Blob(this.chunks, { type: this.recorder.mimeType });
      this.cleanup();
      if (blob.size) this.onComplete(blob, reason);
      else this.onError(new Error("The browser produced an empty recording. Try recording for longer."));
    }
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
    const writable = this.writable;
    this.writable = null;
    if (writable) {
      try { Promise.resolve(writable.abort()).catch(() => {}); } catch { /* Preserve the original capture error. */ }
    }
    this.recorder = this.stream = null;
    this.chunks = [];
    this.backpressured = false;
    this.pendingBytes = 0;
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
  const width = 1280, height = 720;
  ctx.save();
  ctx.scale(target.width / width, target.height / height);
  ctx.fillStyle = "#081727";
  ctx.fillRect(0, 0, width, height);
  ctx.fillStyle = "#eff5ff";
  ctx.font = "bold 24px sans-serif";
  ctx.fillText(title, 24, 38, width - 48);
  ctx.font = "16px sans-serif";
  ctx.fillStyle = "#a8c6df";
  ctx.fillText(detail, 24, 66, width - 48);
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
  ctx.fillText(progress, 24, 680, width - 48);
  ctx.font = "12px sans-serif";
  ctx.fillStyle = "#a8c6df";
  ctx.fillText(attribution, 24, 706, width - 48);
  ctx.restore();
}
