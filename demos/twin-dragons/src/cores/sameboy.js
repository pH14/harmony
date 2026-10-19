// SPDX-License-Identifier: AGPL-3.0-or-later
import { coreURL } from "./quicknes.js";

export const SAMEBOY_REVISION = "98da08c3117a7bf39c7329fc662c0d18cb54707d";

export async function loadSameBoy(base, rom, inputs = {}) {
  const version = "halt-skip-1";
  const factory = (await import(/* @vite-ignore */ coreURL("sameboy.js", base, version))).default;
  const mod = await factory({
    ...(inputs.wasmBinary ? { wasmBinary: inputs.wasmBinary } : {}),
    locateFile: (name) => coreURL(name, base, version),
    print: () => {},
    printErr: () => {},
  });
  const rp = mod._malloc(rom.length);
  mod.HEAPU8.set(rom, rp);
  const loaded = mod._gb_load(rp, rom.length);
  mod._free(rp);
  if (!loaded) throw new Error("SameBoy could not load the ROM");
  const size = mod._gb_state_size(),
    scratch = mod._malloc(size);
  if (size <= 0 || size > 1048576) throw new Error("Invalid SameBoy snapshot size");
  return {
    mod,
    revision: SAMEBOY_REVISION,
    stateSize: size,
    ramPointer: () => mod._gb_wram(),
    step: (buttons, frames) => mod._gb_run(buttons, frames, 0),
    runRendered: (buttons, frames, render) => mod._gb_run(buttons, frames, render ? 1 : 0),
    enableAudio(enabled) {
      mod._gb_audio_enable(enabled ? 1 : 0);
    },
    audio() {
      const count = mod._gb_audio_count(),
        ptr = mod._gb_audio_samples();
      const samples = new Int16Array(mod.HEAPU8.buffer, ptr, count).slice();
      mod._gb_audio_clear();
      return samples;
    },
    capture() {
      if (!mod._gb_save(scratch, size)) throw new Error("Snapshot capture failed");
      return mod.HEAPU8.slice(scratch, scratch + size);
    },
    captureInto(target) {
      if (!mod._gb_save(scratch, size)) throw new Error("Snapshot capture failed");
      target.set(mod.HEAPU8.subarray(scratch, scratch + size));
    },
    restore(bytes) {
      if (bytes.length !== size) throw new Error("Snapshot size mismatch");
      mod.HEAPU8.set(bytes, scratch);
      if (!mod._gb_restore(scratch, size)) throw new Error("Snapshot restore failed");
    },
    pixels() {
      const width = mod._gb_width(),
        height = mod._gb_height(),
        ptr = mod._gb_pixels();
      return { width, height, data: new Uint8ClampedArray(mod.HEAPU8.slice(ptr, ptr + width * height * 4)) };
    },
  };
}
