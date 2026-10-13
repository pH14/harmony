// SPDX-License-Identifier: AGPL-3.0-or-later
export const ROM_SHA256 =
  "7b64267068caab5379b833336e836d6de49c7d37cc5a4341b83d07cbccf0ef73";
export const CORE_REVISION = "26bb785c9deddb66a17717b21bb4e328f03ade32";
export const LEVEL_COUNT = 3;
export const BOOT = [
  [0, 60],
  [8, 2],
  [0, 240],
];
const runtimeURL = (name, base) => {
  const url = new URL("engine/" + name, base);
  url.searchParams.set("v", "idle-loop-1");
  return url.href;
};
export async function createEngine(base, inputs = {}) {
  const factory = (
    await import(/* @vite-ignore */ runtimeURL("quicknes.js", base))
  ).default;
  const rom =
    inputs.rom ||
    new Uint8Array(
      await (await checkedFetch(new URL("twin-dragons.nes", base))).arrayBuffer(),
    );
  if (rom.byteLength !== 40976) throw new Error("Unexpected Twin Dragons ROM size");
  const hash = [...new Uint8Array(await crypto.subtle.digest("SHA-256", rom))]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  if (hash !== ROM_SHA256) throw new Error("Twin Dragons ROM checksum mismatch");
  const mod = await factory({
    ...(inputs.wasmBinary ? { wasmBinary: inputs.wasmBinary } : {}),
    locateFile: (name) => runtimeURL(name, base),
    print: () => {},
    printErr: () => {},
  });
  const rp = mod._malloc(rom.length);
  mod.HEAPU8.set(rom, rp);
  const loaded = mod._nes_load(rp, rom.length);
  mod._free(rp);
  if (!loaded) throw new Error("QuickNES could not load Twin Dragons");
  const size = mod._nes_state_size(),
    scratch = mod._malloc(size);
  if (size <= 0 || size > 1048576)
    throw new Error("Invalid QuickNES snapshot size");
  const engine = {
    mod,
    stateSize: size,
    run(buttons, frames, render = false) {
      if (
        !Number.isInteger(frames) ||
        frames < 0 ||
        frames > 200000 ||
        !Number.isInteger(buttons) ||
        buttons < 0 ||
        buttons > 255
      )
        throw new Error("Invalid emulator action");
      mod._nes_run(buttons, frames, render ? 1 : 0);
    },
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
      if (!mod._nes_save(scratch, size))
        throw new Error("Snapshot capture failed");
      return canonicalize(mod.HEAPU8.slice(scratch, scratch + size));
    },
    captureInto(target) {
      if (!mod._nes_save(scratch, size))
        throw new Error("Snapshot capture failed");
      target.set(mod.HEAPU8.subarray(scratch, scratch + size));
      canonicalize(target);
    },
    restore(bytes) {
      if (bytes.length !== size) throw new Error("Snapshot size mismatch");
      mod.HEAPU8.set(bytes, scratch);
      if (!mod._nes_restore(scratch, size))
        throw new Error("Snapshot restore failed");
    },
    memory() {
      if (mod._nes_ram_size() !== 2048)
        throw new Error("Twin Dragons memory layout mismatch");
      return mod.HEAPU8.slice(mod._nes_ram(), mod._nes_ram() + 2048);
    },
    observation() {
      return decode(this.memory());
    },
    pixels() {
      const width = mod._nes_width(),
        height = mod._nes_height();
      if (width < 1 || width > 256 || height < 1 || height > 240)
        throw new Error("Invalid video geometry");
      return {
        width,
        height,
        data: new Uint8ClampedArray(
          mod.HEAPU8.slice(
            mod._nes_pixels(),
            mod._nes_pixels() + width * height * 4,
          ),
        ),
      };
    },
    boot(level = 0) {
      if (!Number.isInteger(level) || level < 0 || level >= LEVEL_COUNT)
        throw new Error("Unknown Twin Dragons starting level");
      this.restore(powerOn);
      for (const step of BOOT) this.run(...step, true);
      if (level > 0) {
        const ram = mod._nes_ram();
        mod.HEAPU8[ram + NEXT_LEVEL] = level;
        mod.HEAPU8[ram + TOUCHED_GOAL] = 1;
        for (let frames = 0; frames < 600; frames++) {
          this.run(0, 1, true);
          const o = this.observation();
          if (!o.loading && o.level === level && !o.goal && o.on_ground) break;
        }
      } else
        for (let frames = 0; frames < 120 && !this.observation().on_ground; frames++)
          this.run(0, 1, true);
      const o = this.observation();
      if (o.loading || o.level !== level || o.health !== 3 || o.goal || !o.on_ground)
        throw new Error("Setup did not reach the selected Twin Dragons level");
      return this.capture();
    },
  };
  const powerOn = engine.capture();
  return engine;
}
async function checkedFetch(url) {
  const r = await fetch(url);
  if (!r.ok)
    throw new Error("Could not load " + url.pathname + " (" + r.status + ")");
  return r;
}
const NEXT_LEVEL = 0x35,
  TOUCHED_GOAL = 0x4e3;
export function levelsCleared(o) {
  if (o.gamestate === 1 && !o.loading && o.goal) return o.level + 1;
  return o.gamestate === 1 || o.gamestate === 2 ? o.level : 0;
}
export function settled(gamestate, level, next) {
  return gamestate === 1 && (next === level + 1 || (level === LEVEL_COUNT - 1 && next === 0xff));
}
export function decode(ram) {
  if (ram.length !== 2048) throw new Error("Invalid Twin Dragons memory");
  const o = {
    x: ram[0x52] * 256 + ram[0x57] + ram[0x62],
    y: ram[0x63],
    health: ram[0x75],
    level: ram[0x42],
    next_level: ram[NEXT_LEVEL],
    gamestate: ram[0x9e],
    goal: ram[TOUCHED_GOAL] !== 0,
    on_ground: ram[0x6b] !== 0,
    facing: ram[0x74],
  };
  o.loading = !settled(o.gamestate, o.level, o.next_level);
  o.cleared = levelsCleared(o);
  return o;
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
    if (length > bytes.length - offset - 8)
      throw new Error("Invalid snapshot block");
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
