// SPDX-License-Identifier: AGPL-3.0-or-later
export const CREDIT = {
  title: "Twin Dragons",
  author: "Garydos",
  source:
    "https://github.com/Garydos/twin-dragons-nes/tree/bcf27d0c5117a2aac19e6c33bdb3c776ada069fb",
  code_license: "Zlib",
  artwork:
    "Some artwork by surt, https://opengameart.org/content/twin-dragons, CC BY 3.0 (https://creativecommons.org/licenses/by/3.0/). Modified: rendered with the game palette and composed into level maps and sprite sheets.",
  music: "Music by Garydos, CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/)",
  notice:
    "Altered build: the sound-effect data is zeroed. Sound effects by Shiru are not included.",
};
export async function creditPNG(blob, frame) {
  const bytes = new Uint8Array(await blob.arrayBuffer()),
    text = new TextEncoder().encode(
      "Description\0" + JSON.stringify({ ...CREDIT, frame }),
    ),
    tag = new TextEncoder().encode("tEXt"),
    chunk = new Uint8Array(text.length + 12),
    view = new DataView(chunk.buffer);
  view.setUint32(0, text.length);
  chunk.set(tag, 4);
  chunk.set(text, 8);
  let crc = 0xffffffff;
  for (const b of chunk.subarray(4, -4)) {
    crc ^= b;
    for (let i = 0; i < 8; i++)
      crc = crc & 1 ? (crc >>> 1) ^ 0xedb88320 : crc >>> 1;
  }
  view.setUint32(chunk.length - 4, (crc ^ 0xffffffff) >>> 0);
  return new Blob([bytes.subarray(0, -12), chunk, bytes.subarray(-12)], {
    type: "image/png",
  });
}
export async function snapshotHash(bytes) {
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}
