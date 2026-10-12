// SPDX-License-Identifier: AGPL-3.0-or-later
import { deflateSync } from "node:zlib";
function crc(buf) {
  let c = 0xffffffff;
  for (const b of buf) {
    c ^= b;
    for (let i = 0; i < 8; i++) c = c & 1 ? (c >>> 1) ^ 0xedb88320 : c >>> 1;
  }
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(tag, data) {
  const t = Buffer.from(tag),
    b = Buffer.alloc(12 + data.length);
  b.writeUInt32BE(data.length);
  t.copy(b, 4);
  data.copy(b, 8);
  b.writeUInt32BE(crc(Buffer.concat([t, data])), 8 + data.length);
  return b;
}
export function png(width, height, data) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  const scan = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++)
    Buffer.from(data.buffer, data.byteOffset + y * width * 4, width * 4).copy(
      scan,
      y * (width * 4 + 1) + 1,
    );
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(scan)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}
