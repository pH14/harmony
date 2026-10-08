// SPDX-License-Identifier: AGPL-3.0-or-later
export class GameAudio {
  constructor(
    factory,
    Context = globalThis.AudioContext || globalThis.webkitAudioContext,
  ) {
    this.factory = factory;
    this.Context = Context;
    this.sources = new Set();
    this.generation = 0;
    this.samples = 0;
    this.muted = false;
  }
  unlock() {
    if (!this.Context) return Promise.resolve();
    this.context ||= new this.Context({ sampleRate: 48000 });
    if (!this.gain) {
      this.gain = this.context.createGain();
      this.gain.connect(this.context.destination);
    }
    this.gain.gain.value = this.muted ? 0 : 0.4;
    return this.context.resume();
  }
  async start(snapshot) {
    this.stop();
    const generation = ++this.generation;
    this.active = true;
    if (!this.Context) return;
    try {
      const resume = this.unlock();
      this.pending ||= this.factory();
      const [engine] = await Promise.all([this.pending, resume]);
      if (generation !== this.generation || !this.active) return;
      this.engine = engine;
      engine.restore(snapshot());
      engine.enableAudio(true);
      this.nextTime = this.context.currentTime + 0.025;
    } catch (e) {
      if (generation !== this.generation) return;
      this.stop();
      this.pending = null;
      throw e;
    }
  }
  run(buttons, frames = 1) {
    if (this.active && this.engine) this.engine.run(buttons, frames, false);
  }
  flush(rate = 1) {
    if (!this.active || !this.engine || this.context.state !== "running")
      return;
    const pcm = this.engine.audio();
    if (!pcm.length) return;
    const frames = pcm.length / 2,
      buffer = this.context.createBuffer(2, frames, 48000);
    for (let c = 0; c < 2; c++) {
      const data = buffer.getChannelData(c);
      for (let i = 0; i < frames; i++) data[i] = pcm[2 * i + c] / 32768;
    }
    const now = this.context.currentTime;
    if (this.nextTime > now + 0.18) {
      for (const s of this.sources) s.stop();
      this.sources.clear();
      this.nextTime = now + 0.025;
    }
    const source = this.context.createBufferSource();
    source.buffer = buffer;
    source.playbackRate.value = rate;
    source.connect(this.gain);
    source.onended = () => {
      this.sources.delete(source);
      source.disconnect();
    };
    this.sources.add(source);
    this.nextTime = Math.max(this.nextTime, now + 0.025);
    source.start(this.nextTime);
    this.nextTime += frames / 48000 / rate;
    this.samples += frames;
  }
  mute() {
    this.muted = !this.muted;
    if (this.gain) this.gain.gain.value = this.muted ? 0 : 0.4;
    return this.muted;
  }
  stop() {
    this.generation++;
    this.active = false;
    for (const s of this.sources) s.stop();
    this.sources.clear();
    this.engine?.enableAudio(false);
    this.engine = null;
  }
}
