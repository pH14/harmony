// SPDX-License-Identifier: AGPL-3.0-or-later
export const QUICKNES_REVISION = "26bb785c9deddb66a17717b21bb4e328f03ade32";

export function coreURL(name, base, version) {
  const url = new URL("engine/" + name, base);
  url.searchParams.set("v", version);
  return url.href;
}

export async function loadQuickNES(base, rom, inputs = {}) {
  const version = "four-screen-1";
  const factory = (await import(/* @vite-ignore */ coreURL("quicknes.js", base, version))).default;
  const mod = await factory({
    ...(inputs.wasmBinary ? { wasmBinary: inputs.wasmBinary } : {}),
    locateFile: (name) => coreURL(name, base, version),
    print: () => {},
    printErr: () => {},
  });
  const rp = mod._malloc(rom.length);
  mod.HEAPU8.set(rom, rp);
  if (inputs.cartridgeMemory) mod.HEAPU8[rp + 6] |= 2;
  const loaded = mod._nes_load(rp, rom.length);
  mod._free(rp);
  if (!loaded) throw new Error("QuickNES could not load the ROM");
  const size = mod._nes_state_size(),
    scratch = mod._malloc(size);
  if (size <= 0 || size > 1048576) throw new Error("Invalid QuickNES snapshot size");
  if (mod._nes_ram_size() !== 2048) throw new Error("QuickNES memory layout mismatch");
  return {
    mod,
    revision: QUICKNES_REVISION,
    stateSize: size,
    ramPointer: inputs.cartridgeMemory ? () => mod._nes_memory() : () => mod._nes_ram(),
    commitMemory: () => mod._nes_memory_commit(),
    step: (buttons, frames) => mod._nes_run(buttons, frames, 0),
    runRendered: (buttons, frames, render) => mod._nes_run(buttons, frames, render ? 1 : 0),
    enableAudio(enabled) {
      mod._nes_audio_enable(enabled ? 1 : 0);
    },
    audio() {
      const count = mod._nes_audio_count(),
        ptr = mod._nes_audio_samples();
      const samples = new Int16Array(mod.HEAPU8.buffer, ptr, count).slice();
      mod._nes_audio_clear();
      return samples;
    },
    capture() {
      if (!mod._nes_save(scratch, size)) throw new Error("Snapshot capture failed");
      return canonicalize(mod.HEAPU8.slice(scratch, scratch + size));
    },
    captureInto(target) {
      if (!mod._nes_save(scratch, size)) throw new Error("Snapshot capture failed");
      target.set(mod.HEAPU8.subarray(scratch, scratch + size));
      canonicalize(target);
    },
    restore(bytes) {
      if (bytes.length !== size) throw new Error("Snapshot size mismatch");
      mod.HEAPU8.set(bytes, scratch);
      if (!mod._nes_restore(scratch, size)) throw new Error("Snapshot restore failed");
    },
    pixels() {
      const width = mod._nes_width(),
        height = mod._nes_height();
      if (width < 1 || width > 256 || height < 1 || height > 240)
        throw new Error("Invalid video geometry");
      const ptr = mod._nes_pixels();
      return { width, height, data: new Uint8ClampedArray(mod.HEAPU8.slice(ptr, ptr + width * height * 4)) };
    },
    pixelView() {
      const ptr = mod._nes_pixels();
      return mod.HEAPU8.subarray(ptr, ptr + mod._nes_width() * mod._nes_height() * 4);
    },
  };
}

export function canonicalize(bytes) {
  // QuickNES leaves three unused PPU bytes undefined. This is the same
  // canonical field used by the native adapter; no execution state is removed.
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let offset = 8,
    found = false;
  while (offset + 8 <= bytes.length) {
    const tag = String.fromCharCode(...bytes.subarray(offset, offset + 4)),
      length = view.getUint32(offset + 4, true);
    if (length > bytes.length - offset - 8) throw new Error("Invalid snapshot block");
    if (tag === "PPUR") {
      if (length !== 52) throw new Error("Unexpected PPU state");
      bytes.fill(0, offset + 8 + 49, offset + 8 + 52);
      found = true;
    }
    offset += 8 + length;
  }
  if (!found) throw new Error("Missing PPU snapshot block");
  return bytes;
}
