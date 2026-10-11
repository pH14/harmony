// SPDX-License-Identifier: AGPL-3.0-or-later
export const CREDIT = {
  title: "Nova the Squirrel",
  author: "NovaSquirrel",
  license: "CC BY-NC-SA 4.0",
  license_url: "https://creativecommons.org/licenses/by-nc-sa/4.0/",
  source:
    "https://github.com/NovaSquirrel/NovaTheSquirrel/tree/e9e79ae59b188348bd6a87117a2d5c86a34ba433",
  notice:
    "Original gameplay imagery. Noncommercial Harmony demonstration. Upstream character/cameo restrictions apply; see source README.",
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
export function snapshotDigest(bytes) {
  let a = 0x811c9dc5, b = 0x01c93a75;
  for (let i = 0; i < bytes.length; i++) {
    a = Math.imul(a ^ bytes[i], 0x01000193) >>> 0;
    const c = Math.imul(b ^ bytes[i], 0x01000193) >>> 0;
    b = ((c << 5) | (c >>> 27)) >>> 0;
  }
  return a.toString(16).padStart(8, "0") + b.toString(16).padStart(8, "0");
}
