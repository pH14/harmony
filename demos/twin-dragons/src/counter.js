// SPDX-License-Identifier: AGPL-3.0-or-later
const MIN_SPAN = 50, MAX_SPAN = 400, OVERLAP = 1.25;

export class SmoothCounter {
  constructor() {
    this.reset();
  }
  reset() {
    Object.assign(this, { from: 0, to: 0, start: 0, span: 100, last: null });
  }
  push(value, now) {
    if (this.last === null || value < this.to) {
      Object.assign(this, { from: value, to: value, start: now, last: now });
      return;
    }
    const interval = Math.max(MIN_SPAN, Math.min(MAX_SPAN, now - this.last));
    Object.assign(this, { from: this.read(now), to: value, start: now, last: now, span: interval * OVERLAP });
  }
  read(now) {
    return this.from + (this.to - this.from) * Math.max(0, Math.min(1, (now - this.start) / this.span));
  }
}
