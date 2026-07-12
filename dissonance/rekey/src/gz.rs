// SPDX-License-Identifier: AGPL-3.0-or-later
//! A dependency-free reader for the committed `traces.tar.gz` corpus: gzip
//! (RFC 1952) framing, DEFLATE (RFC 1951) decompression, and `ustar` member
//! extraction.
//!
//! ## Why this exists
//!
//! The retained traces are committed as `.tar.gz`, and neither `flate2` nor
//! `tar` is on the conventions' dependency whitelist. Rather than spend the
//! justification bar on two crates, or put an external `tar` binary into the
//! gates, the ~300 lines below decode the archives in-process. Nothing here is
//! trusted on faith: **every extracted member is content-checked against the
//! corpus manifest's sha256** (`crate::manifest`), so a decoder bug cannot
//! silently produce plausible-but-wrong bytes — it fails the hash. The gzip
//! trailer's CRC-32 and length are checked too, as a first line of defence.
//!
//! Total, panic-free, and allocation-bounded on untrusted input (conventions
//! rule 4): every read is bounds-checked, and the LZ77 back-reference is
//! rejected when it points before the output start.

use crate::error::{Error, Result};

/// Decode one gzip member and return the decompressed bytes.
///
/// Verifies the magic, the (deflate) compression method, the trailing CRC-32,
/// and the trailing uncompressed length. `archive` names the artifact in any
/// error.
pub fn gunzip(archive: &str, input: &[u8]) -> Result<Vec<u8>> {
    let bad = |why: &str| Error::Archive {
        archive: archive.to_string(),
        why: why.to_string(),
    };

    // Header: magic(2) method(1) flags(1) mtime(4) xfl(1) os(1) = 10 bytes.
    if input.len() < 18 {
        return Err(bad("shorter than a gzip header plus trailer"));
    }
    if input[0] != 0x1f || input[1] != 0x8b {
        return Err(bad("not a gzip stream (bad magic)"));
    }
    if input[2] != 8 {
        return Err(bad("unsupported compression method (not deflate)"));
    }
    let flags = input[3];
    let mut at = 10usize;

    // FEXTRA: a little-endian u16 length, then that many bytes. Decoded with
    // `from_le_bytes` rather than a hand-rolled `hi << 8 | lo`: the shift-or is
    // indistinguishable from a shift-xor for any `lo < 256`, so that spelling
    // carries a mutant no test could ever kill.
    if flags & 0x04 != 0 {
        // Read a fixed `[u8; 2]`, not a slice we index into: `try_into` requires
        // the slice to be *exactly* two bytes, so `at..at + 2` cannot be widened
        // (an `at * 2` / `at - 2` misread of the range fails the length check and
        // errors, rather than silently reading the same first two bytes).
        let field: [u8; 2] = input
            .get(at..at + 2)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| bad("truncated FEXTRA"))?;
        let xlen = usize::from(u16::from_le_bytes(field));
        at = at
            .checked_add(2 + xlen)
            .ok_or_else(|| bad("FEXTRA length overflows"))?;
    }
    // FNAME / FCOMMENT: NUL-terminated strings.
    for (bit, what) in [(0x08u8, "FNAME"), (0x10u8, "FCOMMENT")] {
        if flags & bit != 0 {
            loop {
                let b = *input.get(at).ok_or_else(|| bad(what))?;
                at += 1;
                if b == 0 {
                    break;
                }
            }
        }
    }
    // FHCRC: a 2-byte header CRC we do not check (the sha256 pin is stronger).
    if flags & 0x02 != 0 {
        at = at.checked_add(2).ok_or_else(|| bad("truncated FHCRC"))?;
    }
    if at >= input.len() {
        return Err(bad("no deflate payload"));
    }

    let (out, consumed) = inflate(&input[at..]).map_err(|why| Error::Archive {
        archive: archive.to_string(),
        why,
    })?;

    // Trailer: CRC-32 then ISIZE (the uncompressed length mod 2^32).
    let tail = at
        .checked_add(consumed)
        .ok_or_else(|| bad("deflate stream overruns the archive"))?;
    let trailer = input
        .get(tail..tail + 8)
        .ok_or_else(|| bad("missing gzip trailer"))?;
    let want_crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let want_len = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
    if crc32(&out) != want_crc {
        return Err(bad("gzip CRC-32 mismatch"));
    }
    if (out.len() as u32) != want_len {
        return Err(bad("gzip length mismatch"));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// DEFLATE (RFC 1951). The decoder follows Mark Adler's `puff` reference shape:
// canonical Huffman codes decoded bit-by-bit against per-length counts.
// ---------------------------------------------------------------------------

/// Length-code bases for symbols 257..=285 (RFC 1951 §3.2.5).
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits per length code.
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Distance-code bases for symbols 0..=29.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits per distance code.
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// The largest output any corpus archive may inflate to (256 MiB). The committed
/// trace archives expand to ~27 MB each; a crafted stream must not be able to
/// exhaust memory before the CRC-32 at the *end* of the stream can reject it
/// (the module's untrusted-input contract).
const MAX_INFLATED: usize = 256 << 20;

/// The order code-length code lengths arrive in, for a dynamic block.
const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// An LSB-first bit reader over a byte slice.
struct BitReader<'a> {
    data: &'a [u8],
    /// The next byte to draw bits from.
    byte: usize,
    /// How many bits of `data[byte]` are already consumed (0..8).
    bit: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte: 0,
            bit: 0,
        }
    }

    /// One bit, LSB first.
    fn bit(&mut self) -> std::result::Result<u32, String> {
        let b = *self
            .data
            .get(self.byte)
            .ok_or_else(|| "deflate stream truncated".to_string())?;
        let v = u32::from(b >> self.bit) & 1;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.byte += 1;
        }
        Ok(v)
    }

    /// `n` bits (n <= 16), LSB first.
    fn bits(&mut self, n: u32) -> std::result::Result<u32, String> {
        let mut v = 0u32;
        for i in 0..n {
            v |= self.bit()? << i;
        }
        Ok(v)
    }

    /// Discard the remainder of the current byte.
    fn align(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.byte += 1;
        }
    }

    /// How many whole bytes of the input the reader has consumed.
    fn consumed(&self) -> usize {
        self.byte + usize::from(self.bit != 0)
    }
}

/// A canonical Huffman code: how many symbols use each bit length, and the
/// symbols themselves in canonical (length, symbol) order.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    /// Build from per-symbol code lengths (`0` = symbol absent). Rejects an
    /// over-subscribed code; an *incomplete* code is allowed (a dynamic block
    /// may legally carry a single distance code, and no symbol of an unused
    /// tree is ever decoded).
    fn build(lengths: &[u8]) -> std::result::Result<Huffman, String> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            if l as usize > 15 {
                return Err("code length exceeds 15".into());
            }
            counts[l as usize] += 1;
        }
        // Kraft: every length must leave a non-negative number of free codes.
        let mut left = 1i32;
        for &count in &counts[1..] {
            left <<= 1;
            left -= i32::from(count);
            if left < 0 {
                return Err("over-subscribed Huffman code".into());
            }
        }
        // The first symbol index of each code length.
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + counts[len];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huffman { counts, symbols })
    }

    /// Decode one symbol, walking lengths 1..=15 and comparing against the
    /// canonical first-code/count of each length.
    fn decode(&self, br: &mut BitReader) -> std::result::Result<u16, String> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..16 {
            code |= br.bit()? as i32;
            let count = i32::from(self.counts[len]);
            if code - count < first {
                let at = (index + (code - first)) as usize;
                return self
                    .symbols
                    .get(at)
                    .copied()
                    .ok_or_else(|| "Huffman symbol out of range".to_string());
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("invalid Huffman code".into())
    }
}

/// The RFC 1951 fixed literal/length code.
fn fixed_literal() -> std::result::Result<Huffman, String> {
    let mut lengths = [0u8; 288];
    lengths[0..144].fill(8);
    lengths[144..256].fill(9);
    lengths[256..280].fill(7);
    lengths[280..288].fill(8);
    Huffman::build(&lengths)
}

/// The RFC 1951 fixed distance code (32 five-bit codes; 30 and 31 are invalid
/// and are rejected at use, not at build).
fn fixed_distance() -> std::result::Result<Huffman, String> {
    Huffman::build(&[5u8; 32])
}

/// The error a dynamic header fails with when it declares more literal/length
/// codes than RFC 1951 defines. Pinned as a constant so the tests can assert
/// *which* check rejected a header, not merely that something did.
const TOO_MANY_CODES: &str = "dynamic block declares too many codes";

/// Read a dynamic block's literal/length and distance codes.
///
/// `HLIT` is a 5-bit field read as `+257`, so it can declare up to 288
/// literal/length codes where RFC 1951 defines only 286 — 287 and 288 do not
/// exist, and a header over that bound is refused before a Huffman code is built
/// over symbols with no meaning. `HDIST` (`+1`) spans `1..=32`, and **every one
/// of those counts is legal**: RFC 1951 permits `HDIST+1 ∈ 1..=32`, with distance
/// symbols 30 and 31 merely *reserved* rather than absent — caught at decode time
/// by the `dsym >= 30` guard if a stream actually uses one. So there is no HDIST
/// count bound to check here (a 5-bit field cannot exceed 32 in any case); only
/// the literal/length overrun is a real rejection.
fn dynamic_codes(br: &mut BitReader) -> std::result::Result<(Huffman, Huffman), String> {
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;
    if hlit > 286 {
        return Err(TOO_MANY_CODES.into());
    }

    let mut clen = [0u8; 19];
    for &slot in CLEN_ORDER.iter().take(hclen) {
        clen[slot] = br.bits(3)? as u8;
    }
    let clen_code = Huffman::build(&clen)?;

    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0usize;
    while i < lengths.len() {
        let sym = clen_code.decode(br)?;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                // Repeat the previous length 3..=6 times.
                let prev = if i == 0 {
                    return Err("code-length repeat with no previous length".into());
                } else {
                    lengths[i - 1]
                };
                let n = 3 + br.bits(2)? as usize;
                if i + n > lengths.len() {
                    return Err("code-length repeat overruns".into());
                }
                lengths[i..i + n].fill(prev);
                i += n;
            }
            17 | 18 => {
                // Repeat a zero length 3..=10 (17) or 11..=138 (18) times.
                let n = if sym == 17 {
                    3 + br.bits(3)? as usize
                } else {
                    11 + br.bits(7)? as usize
                };
                if i + n > lengths.len() {
                    return Err("zero-length repeat overruns".into());
                }
                i += n;
            }
            _ => return Err("invalid code-length symbol".into()),
        }
    }
    if lengths[256] == 0 {
        return Err("dynamic block has no end-of-block code".into());
    }
    let lit = Huffman::build(&lengths[..hlit])?;
    let dist = Huffman::build(&lengths[hlit..])?;
    Ok((lit, dist))
}

/// Inflate a raw DEFLATE stream; returns the output and how many input bytes
/// the stream occupied (so the caller can find the gzip trailer).
fn inflate(input: &[u8]) -> std::result::Result<(Vec<u8>, usize), String> {
    let mut br = BitReader::new(input);
    let mut out: Vec<u8> = Vec::new();

    loop {
        let last = br.bit()?;
        match br.bits(2)? {
            // Stored: align, then a length and its one's complement.
            0 => {
                br.align();
                let hdr = input
                    .get(br.byte..br.byte + 4)
                    .ok_or_else(|| "truncated stored block header".to_string())?;
                let len = u16::from_le_bytes([hdr[0], hdr[1]]) as usize;
                let nlen = u16::from_le_bytes([hdr[2], hdr[3]]) as usize;
                if len ^ 0xFFFF != nlen {
                    return Err("stored block length check failed".into());
                }
                br.byte += 4;
                let body = input
                    .get(br.byte..br.byte + len)
                    .ok_or_else(|| "truncated stored block".to_string())?;
                grow(out.len(), len)?;
                out.extend_from_slice(body);
                br.byte += len;
            }
            // Fixed and dynamic Huffman blocks share the symbol loop.
            mode @ (1 | 2) => {
                let (lit, dist) = if mode == 1 {
                    (fixed_literal()?, fixed_distance()?)
                } else {
                    dynamic_codes(&mut br)?
                };
                loop {
                    let sym = lit.decode(&mut br)?;
                    match sym {
                        0..=255 => {
                            grow(out.len(), 1)?;
                            out.push(sym as u8);
                        }
                        256 => break,
                        257..=285 => {
                            let i = sym as usize - 257;
                            let len =
                                LEN_BASE[i] as usize + br.bits(u32::from(LEN_EXTRA[i]))? as usize;
                            let dsym = dist.decode(&mut br)? as usize;
                            if dsym >= 30 {
                                return Err("invalid distance symbol".into());
                            }
                            let d = DIST_BASE[dsym] as usize
                                + br.bits(u32::from(DIST_EXTRA[dsym]))? as usize;
                            if d > out.len() {
                                return Err("distance points before the output start".into());
                            }
                            grow(out.len(), len)?;
                            // Byte-at-a-time, so an overlapping copy (d < len)
                            // repeats as DEFLATE specifies.
                            let start = out.len() - d;
                            for k in 0..len {
                                out.push(out[start + k]);
                            }
                        }
                        _ => return Err("invalid literal/length symbol".into()),
                    }
                }
            }
            _ => return Err("reserved deflate block type".into()),
        }
        if last == 1 {
            break;
        }
    }
    Ok((out, br.consumed()))
}

/// Refuse an output that would exceed [`MAX_INFLATED`]. A gzip stream's CRC and
/// length live in its *trailer*, so they cannot bound the allocation the body
/// drives — only this can.
fn grow(have: usize, by: usize) -> std::result::Result<(), String> {
    if have.saturating_add(by) > MAX_INFLATED {
        return Err(format!(
            "inflated output exceeds the {MAX_INFLATED}-byte cap; refusing to allocate"
        ));
    }
    Ok(())
}

/// The CRC-32 of `data` (the gzip / zlib polynomial, reflected).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// ---------------------------------------------------------------------------
// ustar
// ---------------------------------------------------------------------------

/// One regular file inside a tar archive.
pub struct TarEntry {
    /// The member name exactly as the archive records it (e.g. `./x.json`).
    pub name: String,
    /// The member's bytes.
    pub data: Vec<u8>,
}

/// Read the regular-file members of an uncompressed `ustar` archive, in
/// archive order. Directories and metadata members are skipped.
pub fn untar(archive: &str, bytes: &[u8]) -> Result<Vec<TarEntry>> {
    let bad = |why: String| Error::Archive {
        archive: archive.to_string(),
        why,
    };
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 512 <= bytes.len() {
        let header = &bytes[at..at + 512];
        // Two consecutive zero blocks end the archive; one is enough to stop.
        if header.iter().all(|&b| b == 0) {
            break;
        }
        if &header[257..262] != b"ustar" {
            return Err(bad(format!("member at offset {at} is not ustar")));
        }
        let name = cstr(&header[0..100]);
        let size = octal(&header[124..136])
            .ok_or_else(|| bad(format!("member {name} has a malformed size field")))?;
        let typeflag = header[156];
        at += 512;
        let size = size as usize;
        let padded = size.div_ceil(512) * 512;
        let body = bytes
            .get(at..at + size)
            .ok_or_else(|| bad(format!("member {name} is truncated")))?;
        // '0' and NUL are regular files; everything else (dirs, PAX headers,
        // links) carries no member content the corpus needs.
        if typeflag == b'0' || typeflag == 0 {
            out.push(TarEntry {
                name,
                data: body.to_vec(),
            });
        }
        at += padded;
    }
    Ok(out)
}

/// A NUL-terminated (or field-filling) ASCII field.
fn cstr(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// A NUL/space-terminated octal field; `None` on a non-octal digit.
fn octal(field: &[u8]) -> Option<u64> {
    let mut v = 0u64;
    let mut seen = false;
    for &b in field {
        match b {
            b'0'..=b'7' => {
                v = v.checked_mul(8)?.checked_add(u64::from(b - b'0'))?;
                seen = true;
            }
            0 | b' ' => {
                if seen {
                    break;
                }
            }
            _ => return None,
        }
    }
    seen.then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored (uncompressed) deflate block round-trips: the simplest block
    /// type, hand-assembled.
    #[test]
    fn inflate_reads_a_stored_block() {
        let payload = b"hello harmony";
        let mut raw = vec![0x01]; // BFINAL=1, BTYPE=00
        raw.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        raw.extend_from_slice(&(!(payload.len() as u16)).to_le_bytes());
        raw.extend_from_slice(payload);
        let (out, consumed) = inflate(&raw).expect("stored block");
        assert_eq!(out, payload);
        assert_eq!(consumed, raw.len());
    }

    /// The fixed-Huffman path, including an overlapping back-reference: the
    /// byte-at-a-time copy must repeat, not read past the window.
    ///
    /// The fixed literal/length code assigns symbols 256..=279 the 7-bit codes
    /// 0..=23, and literals 0..=143 the 8-bit codes 0x30..=0xBF (RFC 1951
    /// §3.2.6). Length 4 is symbol 258 (no extra bits) and distance 1 is
    /// distance symbol 0.
    #[test]
    fn inflate_repeats_an_overlapping_match() {
        let mut w = BitWriter::default();
        w.bits(1, 1); // BFINAL
        w.bits(1, 2); // BTYPE = fixed
        w.code(0x30 + 0x61, 8); // literal 'a'
        w.code(258 - 256, 7); // length symbol 258 → length 4
        w.code(0, 5); // distance symbol 0 → distance 1
        w.code(256 - 256, 7); // end of block
        let (out, _) = inflate(&w.finish()).expect("fixed block");
        assert_eq!(out, b"aaaaa", "one literal then a 4-byte self-overlap");
    }

    /// The first 17 bits of a dynamic block: `BFINAL`, `BTYPE = 10`, then the
    /// `HLIT` / `HDIST` / `HCLEN` counts. Enough to reach the header check; what
    /// follows is deliberately absent.
    fn dynamic_header(hlit: usize, hdist: usize, hclen: usize) -> Vec<u8> {
        let mut w = BitWriter::default();
        w.bits(1, 1); // BFINAL
        w.bits(2, 2); // BTYPE = dynamic
        w.bits((hlit - 257) as u32, 5);
        w.bits((hdist - 1) as u32, 5);
        w.bits((hclen - 4) as u32, 4);
        w.finish()
    }

    /// Whichever error `inflate` returned on this header.
    fn header_error(hlit: usize, hdist: usize) -> String {
        inflate(&dynamic_header(hlit, hdist, 4)).expect_err("a bare header cannot decode")
    }

    /// A dynamic header declaring more literal/length codes than RFC 1951 defines
    /// is refused — at the first value past the limit, not only at the field's
    /// maximum. `HLIT` is a 5-bit `+257` field, so it can encode 287 and 288
    /// literal/length codes where the format defines only 286; without this check
    /// the decoder would build a Huffman code over symbols that have no meaning.
    ///
    /// `HDIST` is deliberately *not* rejected on count — its whole `1..=32` range
    /// is legal (see [`every_legal_code_count_passes_the_header_check`]) — so the
    /// distance count never contributes to this rejection: the cases below pin
    /// HLIT as the sole cause, once with the maximum legal distance count.
    #[test]
    fn a_dynamic_header_declaring_too_many_codes_is_refused() {
        assert_eq!(header_error(288, 1), TOO_MANY_CODES, "HLIT at its maximum");
        assert_eq!(
            header_error(287, 1),
            TOO_MANY_CODES,
            "HLIT one past the limit"
        );
        // HLIT over the limit even with the maximum *legal* distance count: the
        // rejection is HLIT's alone, never a distance overrun.
        assert_eq!(
            header_error(288, 32),
            TOO_MANY_CODES,
            "HLIT over, HDIST at its legal maximum"
        );
    }

    /// Every legal code count passes the header check and fails only later, on the
    /// truncated code-length table. RFC 1951 permits `HLIT+257 ∈ 257..=286` and
    /// `HDIST+1 ∈ 1..=32` — so the distance counts **31 and 32**, which the earlier
    /// `hdist > 30` bound wrongly rejected, must get past this check (distance
    /// symbols 30/31 are reserved *when used*, which the decode-time `dsym >= 30`
    /// guard handles, not the declaration).
    #[test]
    fn every_legal_code_count_passes_the_header_check() {
        for (hlit, hdist) in [(286, 1), (286, 30), (286, 31), (286, 32), (257, 32)] {
            let err = header_error(hlit, hdist);
            assert_ne!(
                err, TOO_MANY_CODES,
                "HLIT {hlit} / HDIST {hdist} are legal counts"
            );
            assert!(err.contains("truncated"), "it fails later, not here: {err}");
        }
    }

    /// A dynamic block whose code-length table is a complete 2-bit code over the
    /// symbols `{0, 8, 16, 18}` (canonical codes `00`, `01`, `10`, `11`), with
    /// `HLIT = 257` / `HDIST = 1` — so the code-length table it decodes into holds
    /// exactly 258 entries. `body` writes the code-length symbol stream.
    fn dynamic_block_with_clen_stream(body: impl Fn(&mut BitWriter)) -> Vec<u8> {
        // Slots, in CLEN_ORDER: 16, 17, 18, 0, 8 — the first five, so HCLEN = 5.
        let clen: [u8; 5] = [2, 0, 2, 2, 2];
        let mut w = BitWriter::default();
        w.bits(1, 1); // BFINAL
        w.bits(2, 2); // BTYPE = dynamic
        w.bits(0, 5); // HLIT  = 257
        w.bits(0, 5); // HDIST = 1
        w.bits(1, 4); // HCLEN = 5
        for len in clen {
            w.bits(u32::from(len), 3);
        }
        body(&mut w);
        w.finish()
    }

    /// The code-length symbols of [`dynamic_block_with_clen_stream`]'s table,
    /// MSB-first as DEFLATE packs Huffman codes.
    const CLEN_SYM_0: u32 = 0b00;
    const CLEN_SYM_8: u32 = 0b01;
    const CLEN_SYM_16: u32 = 0b10;
    const CLEN_SYM_18: u32 = 0b11;

    /// A code-length repeat (symbol 16) that would write past the end of the
    /// table is refused. Without the bound the `fill` would panic — or, under the
    /// `i + n` → `i - n` mutation, silently write the wrong window.
    #[test]
    fn a_code_length_repeat_that_overruns_the_table_is_refused() {
        let raw = dynamic_block_with_clen_stream(|w| {
            w.code(CLEN_SYM_8, 2); // lengths[0] = 8; i = 1
            w.code(CLEN_SYM_18, 2);
            w.bits(115, 7); // 11 + 115 = 126 zeros; i = 127
            w.code(CLEN_SYM_18, 2);
            w.bits(115, 7); // …and 126 more; i = 253
            w.code(CLEN_SYM_16, 2);
            w.bits(3, 2); // repeat the previous length 3 + 3 = 6 times → i + n = 259 > 258
        });
        assert_eq!(
            inflate(&raw).expect_err("the repeat overruns the table"),
            "code-length repeat overruns"
        );
    }

    /// The same bound on the zero-run symbols (17 / 18).
    #[test]
    fn a_zero_length_repeat_that_overruns_the_table_is_refused() {
        let raw = dynamic_block_with_clen_stream(|w| {
            w.code(CLEN_SYM_8, 2); // lengths[0] = 8; i = 1
            w.code(CLEN_SYM_18, 2);
            w.bits(127, 7); // 11 + 127 = 138 zeros; i = 139
            w.code(CLEN_SYM_18, 2);
            w.bits(127, 7); // …138 more → i + n = 277 > 258
        });
        assert_eq!(
            inflate(&raw).expect_err("the zero run overruns the table"),
            "zero-length repeat overruns"
        );
    }

    /// A code-length repeat with nothing to repeat is refused rather than
    /// indexing `lengths[-1]`.
    #[test]
    fn a_code_length_repeat_with_no_previous_length_is_refused() {
        let raw = dynamic_block_with_clen_stream(|w| {
            w.code(CLEN_SYM_16, 2); // the very first symbol
            w.bits(0, 2);
        });
        assert_eq!(
            inflate(&raw).expect_err("nothing to repeat"),
            "code-length repeat with no previous length"
        );
    }

    /// A table with no end-of-block code is refused: a stream that can never
    /// terminate must not begin decoding. Here every one of the 258 code lengths
    /// is zero, so symbol 256 has no code.
    #[test]
    fn a_dynamic_table_without_an_end_of_block_code_is_refused() {
        let raw = dynamic_block_with_clen_stream(|w| {
            w.code(CLEN_SYM_18, 2);
            w.bits(127, 7); // 11 + 127 = 138 zeros; i = 138
            w.code(CLEN_SYM_18, 2);
            w.bits(109, 7); // 11 + 109 = 120 more → i = 258, the table is full
        });
        assert_eq!(
            inflate(&raw).expect_err("no end-of-block code"),
            "dynamic block has no end-of-block code"
        );
    }

    /// The `0..=15` arm: a literal code length is stored verbatim. Reached here
    /// through `CLEN_SYM_0`, which the overrun tests also lean on.
    #[test]
    fn a_literal_code_length_symbol_is_stored_verbatim() {
        let raw = dynamic_block_with_clen_stream(|w| {
            w.code(CLEN_SYM_0, 2); // lengths[0] = 0
            w.code(CLEN_SYM_18, 2);
            w.bits(127, 7); // i = 139
            w.code(CLEN_SYM_18, 2);
            w.bits(108, 7); // i = 258
        });
        // Still no end-of-block code (every length is zero), so it is refused —
        // but by the *end-of-block* check, proving the `0..=15` arm advanced `i`.
        assert_eq!(
            inflate(&raw).expect_err("no end-of-block code"),
            "dynamic block has no end-of-block code"
        );
    }

    /// Over-subscribed code lengths are refused, never decoded into garbage.
    #[test]
    fn huffman_rejects_an_over_subscribed_code() {
        assert!(Huffman::build(&[1, 1, 1]).is_err());
        assert!(Huffman::build(&[1, 1]).is_ok(), "a complete code is fine");
        assert!(Huffman::build(&[1, 0]).is_ok(), "incomplete is allowed");
    }

    /// Truncated input errors rather than panicking (untrusted-input rule).
    #[test]
    fn truncated_streams_error_without_panicking() {
        assert!(inflate(&[]).is_err());
        assert!(inflate(&[0x01, 0x05]).is_err(), "truncated stored header");
        assert!(gunzip("t", &[]).is_err());
        assert!(
            gunzip(
                "t",
                &[
                    0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
                ]
            )
            .is_err()
        );
        // Not gzip at all.
        assert!(gunzip("t", &[0u8; 32]).is_err());
    }

    /// Assemble a gzip member around a **stored** deflate block, exercising the
    /// optional header fields the committed corpus never uses (`tar czf` emits
    /// none of them, so only a hand-built stream reaches this code).
    fn gzip(
        extra: Option<&[u8]>,
        name: Option<&[u8]>,
        comment: Option<&[u8]>,
        fhcrc: bool,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut flags = 0u8;
        if extra.is_some() {
            flags |= 0x04;
        }
        if name.is_some() {
            flags |= 0x08;
        }
        if comment.is_some() {
            flags |= 0x10;
        }
        if fhcrc {
            flags |= 0x02;
        }

        let mut out = vec![0x1f, 0x8b, 0x08, flags, 0, 0, 0, 0, 0, 0];
        if let Some(extra) = extra {
            out.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            out.extend_from_slice(extra);
        }
        for field in [name, comment].into_iter().flatten() {
            out.extend_from_slice(field);
            out.push(0);
        }
        if fhcrc {
            out.extend_from_slice(&[0xAB, 0xCD]); // unchecked; the sha256 pin is stronger
        }
        // One final stored block.
        out.push(0x01);
        out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(payload.len() as u16)).to_le_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(&crc32(payload).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out
    }

    /// The gzip framing round-trips with **every combination** of the optional
    /// header fields, so each field's skip arithmetic is pinned: a mis-skipped
    /// `FEXTRA` length, an `FNAME` scan that stops on the wrong byte, or an
    /// off-by-one on `FHCRC` all land the deflate reader on the wrong byte and
    /// produce garbage or an error.
    #[test]
    fn gzip_skips_every_optional_header_field() {
        let payload = b"supervisor: checkpoint committed";

        // No optional fields at all — the shape `tar czf` actually writes.
        assert_eq!(
            gunzip("t", &gzip(None, None, None, false, payload)).expect("plain"),
            payload
        );

        // `FEXTRA`'s length is a little-endian u16: 0x0102 = 258 bytes, chosen so
        // both halves are non-zero and `hi << 8 | lo` cannot be confused with
        // `hi >> 8`, `hi & lo`, or `hi ^ lo`.
        let extra = vec![0x5Au8; 0x0102];
        assert_eq!(
            gunzip("t", &gzip(Some(&extra), None, None, false, payload)).expect("extra"),
            payload
        );
        // …and a single-byte extra, where `2 + len` must not become `2 - len`.
        assert_eq!(
            gunzip("t", &gzip(Some(b"x"), None, None, false, payload)).expect("extra1"),
            payload
        );
        // …and an empty one, where `2 * len` would swallow the length field.
        assert_eq!(
            gunzip("t", &gzip(Some(b""), None, None, false, payload)).expect("extra0"),
            payload
        );

        // NUL-terminated strings: the scan must advance one byte at a time and
        // stop on the NUL, not on the first non-NUL.
        assert_eq!(
            gunzip("t", &gzip(None, Some(b"traces.tar"), None, false, payload)).expect("name"),
            payload
        );
        assert_eq!(
            gunzip("t", &gzip(None, None, Some(b"a comment"), false, payload)).expect("comment"),
            payload
        );
        assert_eq!(
            gunzip("t", &gzip(None, Some(b""), None, false, payload)).expect("empty name"),
            payload
        );

        // The 2-byte header CRC is skipped, not read.
        assert_eq!(
            gunzip("t", &gzip(None, None, None, true, payload)).expect("fhcrc"),
            payload
        );

        // All four at once, in the order RFC 1952 fixes.
        let all = gzip(Some(&extra), Some(b"n"), Some(b"c"), true, payload);
        assert_eq!(gunzip("t", &all).expect("all fields"), payload);
    }

    /// A truncated optional header errors rather than reading past the buffer.
    #[test]
    fn a_truncated_optional_header_is_refused() {
        let payload = b"x";
        for cut in [11usize, 12, 13] {
            let mut raw = gzip(Some(b"abcd"), None, None, false, payload);
            raw.truncate(cut);
            assert!(gunzip("t", &raw).is_err(), "FEXTRA truncated at {cut}");
        }
        // An FNAME with no terminating NUL runs off the end.
        let mut raw = gzip(None, Some(b"name"), None, false, payload);
        let end = raw.len();
        raw.truncate(end - 12); // drop the trailer, the block, and the NUL
        assert!(gunzip("t", &raw).is_err());
    }

    /// The magic is checked **byte by byte**: either half wrong is not a gzip
    /// stream. (A `||` → `&&` here would accept `\x1f\x00…`.)
    #[test]
    fn each_half_of_the_gzip_magic_is_checked() {
        let good = gzip(None, None, None, false, b"x");
        for (i, wrong) in [(0usize, 0x00u8), (1, 0x00)] {
            let mut raw = good.clone();
            raw[i] = wrong;
            assert_eq!(
                gunzip("t", &raw).expect_err("bad magic").to_string(),
                "cannot decode archive t: not a gzip stream (bad magic)"
            );
        }
        // A non-deflate compression method is refused separately.
        let mut raw = good.clone();
        raw[2] = 9;
        assert!(
            gunzip("t", &raw)
                .expect_err("bad method")
                .to_string()
                .contains("not deflate")
        );
    }

    /// The length floor is a *strict* `<`: 18 bytes is the smallest input that
    /// can carry a header plus a trailer, so it must get past the floor and fail
    /// somewhere later.
    #[test]
    fn the_minimum_length_floor_is_strict() {
        let short = |n: usize| {
            let mut raw = vec![0x1f, 0x8b, 0x08, 0x00];
            raw.resize(n, 0);
            gunzip("t", &raw).expect_err("cannot decode").to_string()
        };
        assert!(short(17).contains("shorter than a gzip header plus trailer"));
        assert!(
            !short(18).contains("shorter than a gzip header plus trailer"),
            "18 bytes clears the floor and fails on its contents instead"
        );
    }

    /// A corrupted payload fails the trailer's CRC-32, and a corrupted trailer
    /// length fails the length check — the two independent trailer guards.
    #[test]
    fn the_gzip_trailer_guards_both_the_crc_and_the_length() {
        let payload = b"supervisor";
        let good = gzip(None, None, None, false, payload);

        let mut bad_crc = good.clone();
        let n = bad_crc.len();
        bad_crc[n - 8] ^= 0xFF;
        assert!(
            gunzip("t", &bad_crc)
                .expect_err("crc")
                .to_string()
                .contains("CRC-32 mismatch")
        );

        let mut bad_len = good.clone();
        let n = bad_len.len();
        bad_len[n - 4] = bad_len[n - 4].wrapping_add(1);
        assert!(
            gunzip("t", &bad_len)
                .expect_err("length")
                .to_string()
                .contains("length mismatch")
        );
    }

    /// CRC-32 against the canonical `"123456789"` check value.
    /// A stream that would inflate past the cap is refused before the allocation,
    /// not after — the trailer's CRC comes too late to help.
    #[test]
    fn inflate_refuses_to_exceed_the_output_cap() {
        assert!(grow(0, MAX_INFLATED).is_ok());
        assert!(grow(1, MAX_INFLATED).is_err());
        assert!(grow(usize::MAX, 1).is_err(), "no overflow wrap");

        // A stored block claiming more bytes than the cap errors (and, here,
        // errors on truncation first — the point is that it never allocates).
        let mut raw = vec![0x01];
        raw.extend_from_slice(&u16::MAX.to_le_bytes());
        raw.extend_from_slice(&(!u16::MAX).to_le_bytes());
        assert!(inflate(&raw).is_err());
    }

    #[test]
    fn crc32_matches_the_canonical_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    /// A hand-built ustar archive yields its regular members and skips dirs.
    #[test]
    fn untar_reads_regular_members_and_skips_directories() {
        let mut a = Vec::new();
        a.extend_from_slice(&tar_header("./", 0, b'5'));
        a.extend_from_slice(&tar_header("./x.json", 5, b'0'));
        let mut body = b"[1,2]".to_vec();
        body.resize(512, 0);
        a.extend_from_slice(&body);
        a.extend_from_slice(&[0u8; 1024]); // end-of-archive
        let members = untar("t", &a).expect("untar");
        assert_eq!(members.len(), 1, "the directory member is skipped");
        assert_eq!(members[0].name, "./x.json");
        assert_eq!(members[0].data, b"[1,2]");
    }

    /// A member that claims more bytes than the archive holds errors.
    #[test]
    fn untar_rejects_a_truncated_member() {
        let a = tar_header("./x.json", 4096, b'0').to_vec();
        assert!(untar("t", &a).is_err());
        // A non-ustar block is refused rather than mis-parsed.
        assert!(untar("t", &[0xFFu8; 512]).is_err());
    }

    #[test]
    fn octal_fields_parse_and_reject() {
        assert_eq!(octal(b"0000005\0"), Some(5));
        assert_eq!(octal(b"       "), None, "no digits");
        assert_eq!(octal(b"00009\0"), None, "not octal");
    }

    /// Assemble the header of one ustar member.
    fn tar_header(name: &str, size: u64, typeflag: u8) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        let oct = format!("{size:011o}\0");
        h[124..124 + oct.len()].copy_from_slice(oct.as_bytes());
        h[156] = typeflag;
        h[257..262].copy_from_slice(b"ustar");
        h
    }

    /// An LSB-first bit writer, for hand-assembling deflate blocks in tests.
    #[derive(Default)]
    struct BitWriter {
        out: Vec<u8>,
        acc: u32,
        n: u32,
    }

    impl BitWriter {
        /// `n` bits of `v`, LSB first (the deflate convention for headers).
        fn bits(&mut self, v: u32, n: u32) {
            for i in 0..n {
                self.push((v >> i) & 1);
            }
        }
        /// `n` bits of a Huffman code, MSB first (the deflate convention for
        /// codes).
        fn code(&mut self, v: u32, n: u32) {
            for i in (0..n).rev() {
                self.push((v >> i) & 1);
            }
        }
        fn push(&mut self, bit: u32) {
            self.acc |= bit << self.n;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
        fn finish(mut self) -> Vec<u8> {
            if self.n > 0 {
                self.out.push(self.acc as u8);
            }
            self.out
        }
    }
}
