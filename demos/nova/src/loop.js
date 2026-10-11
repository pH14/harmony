// SPDX-License-Identifier: AGPL-3.0-or-later
export class SearchLoop {
  constructor(
    step,
    {
      schedule = (callback, delay) => setTimeout(callback, delay),
      cancel = (id) => clearTimeout(id),
      delay = 20,
    } = {},
  ) {
    this.step = step;
    this.schedule = schedule;
    this.cancel = cancel;
    this.delay = delay;
    this.active = false;
    this.epoch = 0;
    this.timer = null;
  }
  pause() {
    this.active = false;
    this.epoch++;
    if (this.timer !== null) this.cancel(this.timer);
    this.timer = null;
  }
  resume() {
    if (this.active) return;
    this.active = true;
    this.tick(++this.epoch);
  }
  tick(epoch) {
    if (!this.active || epoch !== this.epoch) return;
    if (!this.step()) {
      this.pause();
      return;
    }
    if (!this.active || epoch !== this.epoch) return;
    this.timer = this.schedule(() => {
      if (epoch !== this.epoch) return;
      this.timer = null;
      this.tick(epoch);
    }, this.delay);
  }
}
