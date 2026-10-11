// SPDX-License-Identifier: AGPL-3.0-or-later
export const ROM_SHA256 =
  "9107be62a08a0ae51a01f900bd18a95a52fe043f53e9cae4a64d1e8e73114b08";
export const CORE_REVISION = "26bb785c9deddb66a17717b21bb4e328f03ade32";
export const BOOT = [
  [0, 60],
  [8, 6],
  [0, 114],
  [8, 6],
  [0, 54],
  [1, 6],
  [0, 54],
  [16, 6],
  [0, 6],
  [16, 6],
  [0, 6],
  [16, 6],
  [0, 54],
  [1, 6],
  [0, 60],
];
const runtimeURL = (name, base) => {
  const url = new URL("engine/" + name, base);
  url.searchParams.set("v", "audio-replay-2");
  return url.href;
};
export async function createEngine(base, inputs = {}) {
  const factory = (
    await import(/* @vite-ignore */ runtimeURL("quicknes.js", base))
  ).default;
  const rom =
    inputs.rom ||
    new Uint8Array(
      await (await checkedFetch(new URL("nova.nes", base))).arrayBuffer(),
    );
  if (rom.byteLength !== 262160) throw new Error("Unexpected Nova ROM size");
  const hash = [...new Uint8Array(await crypto.subtle.digest("SHA-256", rom))]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  if (hash !== ROM_SHA256) throw new Error("Nova ROM checksum mismatch");
  const mod = await factory({
    ...(inputs.wasmBinary ? { wasmBinary: inputs.wasmBinary } : {}),
    locateFile: (name) => runtimeURL(name, base),
    print: () => {},
    printErr: () => {},
  });
  const rp = mod._malloc(rom.length);
  mod.HEAPU8.set(rom, rp);
  const loaded = mod._nova_load(rp, rom.length);
  mod._free(rp);
  if (!loaded) throw new Error("QuickNES could not load Nova");
  const size = mod._nova_state_size(),
    scratch = mod._malloc(size);
  if (size <= 0 || size > 1048576)
    throw new Error("Invalid QuickNES snapshot size");
  const engine = {
    mod,
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
      mod._nova_run(buttons, frames, render ? 1 : 0);
    },
    enableAudio(enabled) {
      mod._nova_audio_enable(enabled ? 1 : 0);
    },
    audio() {
      const count = mod._nova_audio_count(),
        ptr = mod._nova_audio_samples();
      const samples = new Int16Array(mod.HEAPU8.buffer, ptr, count).slice();
      mod._nova_audio_clear();
      return samples;
    },
    capture() {
      if (!mod._nova_save(scratch, size))
        throw new Error("Snapshot capture failed");
      return canonicalize(mod.HEAPU8.slice(scratch, scratch + size));
    },
    restore(bytes) {
      if (bytes.length !== size) throw new Error("Snapshot size mismatch");
      mod.HEAPU8.set(bytes, scratch);
      if (!mod._nova_restore(scratch, size))
        throw new Error("Snapshot restore failed");
    },
    memory() {
      if (mod._nova_ram_size() !== 2048 || mod._nova_sram_size() !== 8192)
        throw new Error("Nova memory layout mismatch");
      const ram = new Uint8Array(10240);
      ram.set(mod.HEAPU8.subarray(mod._nova_ram(), mod._nova_ram() + 2048));
      ram.set(
        mod.HEAPU8.subarray(mod._nova_sram(), mod._nova_sram() + 8192),
        2048,
      );
      return ram;
    },
    observation() {
      return decode(this.memory());
    },
    pixels() {
      const width = mod._nova_width(),
        height = mod._nova_height();
      if (width < 1 || width > 256 || height < 1 || height > 240)
        throw new Error("Invalid video geometry");
      return {
        width,
        height,
        data: new Uint8ClampedArray(
          mod.HEAPU8.slice(
            mod._nova_pixels(),
            mod._nova_pixels() + width * height * 4,
          ),
        ),
      };
    },
    boot(level = 0) {
      if (!Number.isInteger(level) || level < 0 || level >= 44)
        throw new Error("Unknown Nova starting level");
      this.restore(powerOn);
      for (let i = 0; i < BOOT.length; i++) {
        if (i === 3) {
          const ptr = mod._nova_sram();
          mod.HEAPU8.fill(0, ptr + 0x1f1f, ptr + 0x1f2f);
          const world = Math.floor(level / 8);
          for (let w = 0; w < world; w++) mod.HEAPU8[ptr + 0x1f27 + w] = 1;
          mod.HEAPU8[ptr + 0x1f27 + world] = 1 << (level % 8);
        }
        this.run(...BOOT[i], true);
      }
      const o = this.observation();
      if (!o.health || !o.x || !o.y || o.selected_level !== level || o.checkpoint_level !== level || o.program_bank !== 9 || o.reload)
        throw new Error("Setup did not reach the selected Nova level");
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
export function decode(ram) {
  if (ram.length !== 10240) throw new Error("Invalid Nova memory");
  const count = (offset) =>
    Array.from(ram.slice(offset, offset + 8)).reduce(
      (n, b) => n + b.toString(2).replaceAll("0", "").length,
      0,
    );
  return {
    x: ram[0x26] * 16 + Math.floor(ram[0x25] / 16),
    y: ram[0x27] * 16 + Math.floor(ram[0x28] / 16),
    health: ram[0x4b],
    level: ram[0xa7],
    selected_level: ram[0xa8],
    checkpoint_level: ram[0x1a59],
    program_bank: ram[0x39e],
    chips: ram[0x508],
    chips_needed: ram[0x509],
    cleared_levels: Array.from(ram.slice(0x271f, 0x2724)),
    available_levels: Array.from(ram.slice(0x2727, 0x272c)),
    ability: ram[0x1a00],
    cleared: count(0x271f),
    available: count(0x2727),
    collectibles: count(0x272f),
    reload: ram[0xa9] !== 0,
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
