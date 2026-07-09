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

    // FEXTRA: a 2-byte length then that many bytes.
    if flags & 0x04 != 0 {
        let lo = *input.get(at).ok_or_else(|| bad("truncated FEXTRA"))? as usize;
        let hi = *input.get(at + 1).ok_or_else(|| bad("truncated FEXTRA"))? as usize;
        at = at
            .checked_add(2 + (hi << 8 | lo))
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

/// Read a dynamic block's literal/length and distance codes.
fn dynamic_codes(br: &mut BitReader) -> std::result::Result<(Huffman, Huffman), String> {
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err("dynamic block declares too many codes".into());
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
                        0..=255 => out.push(sym as u8),
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

    /// CRC-32 against the canonical `"123456789"` check value.
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
