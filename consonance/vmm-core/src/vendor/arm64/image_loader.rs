// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arm64 kernel `Image` loader (`tasks/112` M3) — the arm64 analogue of the
//! x86 `linux_loader`, but far smaller: an `Image` is a flat, self-decompressing
//! (or already-flat) blob with a fixed 64-byte header, so there is no bzImage
//! setup-header / `boot_params` / page-table apparatus. Multiboot is deleted for
//! ARM, not ported (`docs/ARCH-BOUNDARY.md` §B).
//!
//! **Trust boundary (Convention rule #4).** [`load`] is total over arbitrary
//! `&[u8]`: every field read is bounds-checked and every address is
//! `checked_*`; it never panics on a malformed image, only returns an
//! [`ImageLoadError`].

use super::board::{PAGE, RAM_BASE, align_up};

/// The arm64 `Image` header magic — `"ARM\x64"` read little-endian, at byte
/// offset [`MAGIC_OFFSET`] (`Documentation/arm64/booting.rst`). A **documented
/// hardware fact**, not a measured constant.
pub const IMAGE_MAGIC: u32 = 0x644d_5241;

/// Byte offset of the [`IMAGE_MAGIC`] field in the 64-byte header.
pub const MAGIC_OFFSET: usize = 56;

/// Byte offset of the little-endian `text_offset` field (image load offset).
const TEXT_OFFSET_OFF: usize = 8;
/// Byte offset of the little-endian `image_size` field (effective image size).
const IMAGE_SIZE_OFF: usize = 16;
/// Byte offset of the little-endian `flags` field.
const FLAGS_OFF: usize = 24;
/// The fixed header length.
const HEADER_LEN: usize = 64;

/// The AArch64 unconditional-branch (`B`) word a self-headed image's `code0`
/// must hold: `b #HEADER_LEN`, branching over the 64-byte header onto the
/// payload. Encoding (`Arm ARM` C6.2.26): opcode `0b000101` in bits `[31:26]`,
/// `imm26` in `[25:0]` as the offset **in instruction words** — `HEADER_LEN / 4
/// = 16` — so `(0b000101 << 26) | 16 = 0x1400_0010`.
///
/// This is load-bearing, not decoration: `load` reports the entry at the image's
/// first byte (`code0`), which the CPU architecturally executes
/// (`Documentation/arm64/booting.rst`), so `code0` must branch to the real entry
/// — real kernels emit exactly this `b` over the header. Without it a booted
/// image runs the zero/header word instead of the payload.
const CODE0_BRANCH_OVER_HEADER: u32 = 0x1400_0000 | (HEADER_LEN as u32 / 4);

/// The parsed, validated `Image` header (the fields [`load`] acts on).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ImageHeader {
    /// The load offset from the 2 MiB-aligned base, in bytes.
    pub text_offset: u64,
    /// The effective image size (code + BSS the loader must reserve), in bytes.
    /// May be `0` on very old images; treated as "at least the file length".
    pub image_size: u64,
    /// The kernel flags word (endianness, page size, placement — carried, not
    /// interpreted by the skeleton beyond the LE-endianness check).
    pub flags: u64,
}

/// Where the kernel landed and where it may run from — the loader's output the
/// entry-state builder and DTB placement consume.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LoadedImage {
    /// Guest-physical entry point (the first instruction: `RAM_BASE +
    /// text_offset`).
    pub entry_gpa: u64,
    /// Guest-physical address of the loaded image's first byte (== `entry_gpa`).
    pub load_gpa: u64,
    /// One past the reserved image extent (`text_offset + max(image_size,
    /// file_len)`), **relative to `RAM_BASE`** — the first free RAM offset for
    /// the DTB. Page-aligned up.
    pub end_off: u64,
}

/// Every way [`load`] / [`parse_header`] can reject an untrusted image.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ImageLoadError {
    /// The buffer is shorter than the 64-byte header.
    #[error("image shorter than the {HEADER_LEN}-byte arm64 Image header")]
    TooShort,
    /// The header magic at [`MAGIC_OFFSET`] is not [`IMAGE_MAGIC`].
    #[error("not an arm64 Image: magic {found:#010x} != {IMAGE_MAGIC:#010x}")]
    BadMagic {
        /// The magic value actually found.
        found: u32,
    },
    /// The kernel-endianness flag (bit 0) is set — a big-endian kernel, which
    /// this little-endian determinism guest never runs.
    #[error("big-endian kernel (flags bit 0 set); only little-endian is supported")]
    BigEndian,
    /// `text_offset` is not page-aligned (the boot protocol wants 2 MiB; the
    /// loader accepts any page alignment but not a sub-page offset).
    #[error("text_offset {text_offset:#x} is not {PAGE:#x}-aligned")]
    UnalignedTextOffset {
        /// The offending `text_offset`.
        text_offset: u64,
    },
    /// The image (at `text_offset`, spanning `max(image_size, file_len)`) does
    /// not fit in the guest RAM.
    #[error("image does not fit: end {end:#x} exceeds RAM {ram:#x}")]
    DoesNotFit {
        /// One past the reserved extent, relative to `RAM_BASE`.
        end: u64,
        /// The guest RAM length.
        ram: u64,
    },
}

/// Read a little-endian `u64` at `off`, or `None` if it runs past the buffer.
fn read_u64(image: &[u8], off: usize) -> Option<u64> {
    let end = off.checked_add(8)?;
    let bytes = image.get(off..end)?;
    Some(u64::from_le_bytes(bytes.try_into().expect("8-byte slice")))
}

/// Read a little-endian `u32` at `off`, or `None` if it runs past the buffer.
fn read_u32(image: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    let bytes = image.get(off..end)?;
    Some(u32::from_le_bytes(bytes.try_into().expect("4-byte slice")))
}

/// Parse and validate the 64-byte header, without touching guest RAM.
///
/// # Errors
/// [`ImageLoadError::TooShort`] / [`ImageLoadError::BadMagic`] /
/// [`ImageLoadError::BigEndian`] / [`ImageLoadError::UnalignedTextOffset`].
pub fn parse_header(image: &[u8]) -> Result<ImageHeader, ImageLoadError> {
    if image.len() < HEADER_LEN {
        return Err(ImageLoadError::TooShort);
    }
    let magic = read_u32(image, MAGIC_OFFSET).ok_or(ImageLoadError::TooShort)?;
    if magic != IMAGE_MAGIC {
        return Err(ImageLoadError::BadMagic { found: magic });
    }
    let text_offset = read_u64(image, TEXT_OFFSET_OFF).ok_or(ImageLoadError::TooShort)?;
    let image_size = read_u64(image, IMAGE_SIZE_OFF).ok_or(ImageLoadError::TooShort)?;
    let flags = read_u64(image, FLAGS_OFF).ok_or(ImageLoadError::TooShort)?;
    if flags & 1 != 0 {
        return Err(ImageLoadError::BigEndian);
    }
    if !text_offset.is_multiple_of(PAGE) {
        return Err(ImageLoadError::UnalignedTextOffset { text_offset });
    }
    Ok(ImageHeader {
        text_offset,
        image_size,
        flags,
    })
}

/// Parse the header and flat-load the image bytes into `ram` at `text_offset`.
/// `ram` is the whole guest RAM slice starting at [`RAM_BASE`]; it is presumed
/// zero (BSS past the file bytes is left as the caller's zeroed RAM, satisfying
/// the boot protocol's "region initialized to zero" requirement).
///
/// # Errors
/// Any [`parse_header`] error, or [`ImageLoadError::DoesNotFit`] if the image
/// (or its `image_size` extent) would run past `ram`.
pub fn load(image: &[u8], ram: &mut [u8]) -> Result<LoadedImage, ImageLoadError> {
    let hdr = parse_header(image)?;
    let ram_len = ram.len() as u64;
    let file_len = image.len() as u64;
    // The reserved extent is text_offset + the larger of the file length and
    // the header's effective image_size (which includes BSS the kernel needs
    // zeroed and reserved past the file bytes).
    let span = file_len.max(hdr.image_size);
    let end = hdr
        .text_offset
        .checked_add(span)
        .ok_or(ImageLoadError::DoesNotFit {
            end: u64::MAX,
            ram: ram_len,
        })?;
    if end > ram_len {
        return Err(ImageLoadError::DoesNotFit { end, ram: ram_len });
    }
    // Copy the file bytes; `text_offset + file_len <= end <= ram_len`, so the
    // destination slice is always fully in range (no panic).
    let start = hdr.text_offset as usize;
    let stop = start + image.len();
    ram[start..stop].copy_from_slice(image);
    Ok(LoadedImage {
        entry_gpa: RAM_BASE + hdr.text_offset,
        load_gpa: RAM_BASE + hdr.text_offset,
        end_off: align_up(end, PAGE),
    })
}

/// Build a minimal valid, **bootable** `Image` header in front of `code` (a
/// test/tooling helper — the M3 TCG smoke wraps a bare-metal payload with this).
/// `image_size` covers the whole blob. Not a guest-facing path.
///
/// `code0` is a [`CODE0_BRANCH_OVER_HEADER`] (`b #64`), **not** the payload: the
/// CPU enters at the image's first byte (which `load` reports as the entry), so
/// `code0` is executed and must branch over the 64-byte header to the payload
/// appended at [`HEADER_LEN`] — otherwise a booted image runs the header word.
pub fn wrap_image(code: &[u8], text_offset: u64, flags: u64) -> Vec<u8> {
    let mut out = vec![0u8; HEADER_LEN];
    out[0..4].copy_from_slice(&CODE0_BRANCH_OVER_HEADER.to_le_bytes());
    out[TEXT_OFFSET_OFF..TEXT_OFFSET_OFF + 8].copy_from_slice(&text_offset.to_le_bytes());
    let image_size = (HEADER_LEN as u64) + code.len() as u64;
    out[IMAGE_SIZE_OFF..IMAGE_SIZE_OFF + 8].copy_from_slice(&image_size.to_le_bytes());
    out[FLAGS_OFF..FLAGS_OFF + 8].copy_from_slice(&flags.to_le_bytes());
    out[MAGIC_OFFSET..MAGIC_OFFSET + 4].copy_from_slice(&IMAGE_MAGIC.to_le_bytes());
    out.extend_from_slice(code);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic Image: 64-byte header + `code_len` bytes, at `text_offset`.
    fn synth(text_offset: u64, image_size: u64, flags: u64, code_len: usize) -> Vec<u8> {
        let mut img = vec![0u8; HEADER_LEN + code_len];
        img[TEXT_OFFSET_OFF..TEXT_OFFSET_OFF + 8].copy_from_slice(&text_offset.to_le_bytes());
        img[IMAGE_SIZE_OFF..IMAGE_SIZE_OFF + 8].copy_from_slice(&image_size.to_le_bytes());
        img[FLAGS_OFF..FLAGS_OFF + 8].copy_from_slice(&flags.to_le_bytes());
        img[MAGIC_OFFSET..MAGIC_OFFSET + 4].copy_from_slice(&IMAGE_MAGIC.to_le_bytes());
        // Distinctive body so the load placement is checkable.
        for (i, b) in img[HEADER_LEN..].iter_mut().enumerate() {
            *b = (i as u8).wrapping_add(1);
        }
        img
    }

    #[test]
    fn parses_and_loads_a_valid_image() {
        let img = synth(0, 0, 0xA /* 4K page bits */, 128);
        let mut ram = vec![0u8; 0x10_0000];
        let loaded = load(&img, &mut ram).unwrap();
        assert_eq!(loaded.entry_gpa, RAM_BASE);
        assert_eq!(loaded.load_gpa, RAM_BASE);
        // The image bytes landed at offset 0; the body is right after the header.
        assert_eq!(&ram[..img.len()], &img[..]);
        // end_off is page-aligned past the image.
        assert_eq!(loaded.end_off, align_up(img.len() as u64, PAGE));
    }

    #[test]
    fn honors_a_nonzero_text_offset() {
        let img = synth(0x8_0000, 0, 0, 64);
        let mut ram = vec![0u8; 0x20_0000];
        let loaded = load(&img, &mut ram).unwrap();
        assert_eq!(loaded.entry_gpa, RAM_BASE + 0x8_0000);
        assert_eq!(&ram[0x8_0000..0x8_0000 + img.len()], &img[..]);
        // RAM before the load offset is untouched (zero).
        assert!(ram[..0x8_0000].iter().all(|&b| b == 0));
    }

    #[test]
    fn image_size_reserves_bss_past_the_file() {
        // file is 64+64=128 bytes but image_size claims 4096: the extent must
        // reflect the larger, so a too-small RAM is rejected.
        let img = synth(0, 4096, 0, 64);
        let mut small = vec![0u8; 2048];
        assert!(matches!(
            load(&img, &mut small),
            Err(ImageLoadError::DoesNotFit { .. })
        ));
        let mut ok = vec![0u8; 8192];
        let loaded = load(&img, &mut ok).unwrap();
        assert_eq!(loaded.end_off, 4096); // already page-aligned
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        // Too short.
        assert_eq!(parse_header(&[0u8; 10]), Err(ImageLoadError::TooShort));
        // Right size, bad magic.
        let mut bad = vec![0u8; HEADER_LEN];
        assert!(matches!(
            parse_header(&bad),
            Err(ImageLoadError::BadMagic { .. })
        ));
        // Good magic, big-endian flag.
        bad[MAGIC_OFFSET..MAGIC_OFFSET + 4].copy_from_slice(&IMAGE_MAGIC.to_le_bytes());
        bad[FLAGS_OFF] = 1;
        assert_eq!(parse_header(&bad), Err(ImageLoadError::BigEndian));
        // Unaligned text_offset.
        bad[FLAGS_OFF] = 0;
        bad[TEXT_OFFSET_OFF] = 1;
        assert!(matches!(
            parse_header(&bad),
            Err(ImageLoadError::UnalignedTextOffset { .. })
        ));
    }

    #[test]
    fn load_never_panics_on_arbitrary_prefixes() {
        let img = synth(0, 0, 0, 200);
        let mut ram = vec![0u8; 0x1_0000];
        for n in 0..img.len() {
            // Truncated images must error, never panic (rule #4).
            let _ = load(&img[..n], &mut ram);
        }
    }

    #[test]
    fn wrap_image_round_trips_through_the_parser() {
        let code = [0xAAu8; 40];
        let wrapped = wrap_image(&code, 0, 0xA);
        let hdr = parse_header(&wrapped).unwrap();
        assert_eq!(hdr.text_offset, 0);
        assert_eq!(hdr.image_size, wrapped.len() as u64);
        let mut ram = vec![0u8; 0x1000];
        let loaded = load(&wrapped, &mut ram).unwrap();
        assert_eq!(&ram[HEADER_LEN..HEADER_LEN + code.len()], &code[..]);
        assert_eq!(loaded.entry_gpa, RAM_BASE);
    }

    /// Decode an AArch64 `B` word into its **byte** branch distance, or `None`
    /// if the word is not a `B`. Sign-extends `imm26` by shifting it to the top
    /// and back (arithmetic), so no debug-overflow on a negative displacement.
    fn decode_b_offset(word: u32) -> Option<i64> {
        if word >> 26 != 0b000101 {
            return None;
        }
        Some(i64::from(((word << 6) as i32) >> 6) * 4)
    }

    /// The bug this guards (review r7): `wrap_image` put the payload at offset 64
    /// but `load` reports the entry at the image's first byte (`code0`). With a
    /// zero `code0` a booted image executes the header word, never the payload.
    /// `code0` must be a `B` that steps over the whole 64-byte header onto the
    /// payload — the portable proof of a bootable wrapped image (the TCG smoke
    /// boots the same artifact on real silicon/QEMU).
    #[test]
    fn wrap_image_entry_branches_over_the_header_onto_the_payload() {
        // A distinctive payload so we can tell code from header/zero bytes.
        let code: Vec<u8> = (0..48u8).map(|i| i.wrapping_add(0x40)).collect();
        let wrapped = wrap_image(&code, 0, 0xA);

        let mut ram = vec![0u8; 0x1000];
        let loaded = load(&wrapped, &mut ram).unwrap();
        // The entry is the image's first byte (code0) — what the CPU fetches.
        assert_eq!(loaded.entry_gpa, RAM_BASE);
        assert_eq!(loaded.entry_gpa, loaded.load_gpa);
        let entry_off = (loaded.entry_gpa - RAM_BASE) as usize; // 0

        // code0 must be an executable branch, not the zero/header word.
        let code0 = u32::from_le_bytes(ram[entry_off..entry_off + 4].try_into().unwrap());
        assert_ne!(code0, 0, "code0 must not be the zero word (the r7 bug)");
        let dist = decode_b_offset(code0).expect("code0 must be an AArch64 `B`");

        // Follow the fetch-then-branch: control must land exactly on the payload
        // (offset HEADER_LEN) — over the entire header — and the landing byte is
        // the payload's first byte, not a header byte.
        let target = entry_off as i64 + dist;
        assert_eq!(
            target, HEADER_LEN as i64,
            "code0 must branch over the 64-byte header to the payload"
        );
        assert_eq!(&ram[HEADER_LEN..HEADER_LEN + code.len()], &code[..]);
        assert_eq!(
            ram[target as usize], code[0],
            "the branch lands on the payload"
        );
    }
}
