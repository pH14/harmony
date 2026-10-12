// SPDX-License-Identifier: AGPL-3.0-or-later
export function memoryBudget(deviceMemory, coarsePointer) {
  const small = coarsePointer || (deviceMemory > 0 && deviceMemory <= 4);
  return { snapshotsMiB: small ? 32 : 128, searchMiB: small ? 96 : 192 };
}
export class StateCache {
  constructor(maxBytes = 2 * 1024 * 1024, maxStates = 32) {
    this.maxBytes = maxBytes;
    this.maxStates = maxStates;
    this.bytes = 0;
    this.entries = new Map();
  }
  get(id) {
    const entry = this.entries.get(id);
    if (!entry) return undefined;
    this.entries.delete(id);
    this.entries.set(id, entry);
    return entry.state;
  }
  set(id, state) {
    const previous = this.entries.get(id);
    if (previous) this.bytes -= previous.bytes;
    this.entries.delete(id);
    const bytes =
      (state.snapshot?.byteLength || 0) + state.actions.length * 64 + 512;
    if (bytes > this.maxBytes) return;
    this.entries.set(id, { state, bytes });
    this.bytes += bytes;
    while (this.bytes > this.maxBytes || this.entries.size > this.maxStates) {
      const oldest = this.entries.keys().next().value;
      this.bytes -= this.entries.get(oldest).bytes;
      this.entries.delete(oldest);
    }
  }
}
