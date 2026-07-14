// SPDX-License-Identifier: AGPL-3.0-or-later
//! Multiboot v1 loader — flat-loads a task-04 payload image into guest RAM,
//! reproducing QEMU's `-kernel` handoff (BRINGUP "The entry contract").
//!
//! This is a **trust boundary** (conventions rule 4): the `image` is untrusted
//! input, so every malformed image yields a [`LoadError`] — never a panic, a
//! slice-index out-of-bounds, or an arithmetic overflow. The address-override
//! formula `file_off = header_file_offset − (header_addr − load_addr)` is honored
//! in full; for the current payloads `header_addr == load_addr == 0x100000`, so
//! `file_off = header_file_offset = 0x1000` (the loadable bytes follow the
//! ELF/program headers — they are **not** at file offset 0).

/// The Multiboot v1 **header** magic embedded in the payload image (`boot.s`
/// `MB_MAGIC`).
pub const MULTIBOOT_HEADER_MAGIC: u32 = 0x1BAD_B002;
/// The Multiboot v1 **bootloader** magic the loader passes to the guest in EAX
/// at entry (set by [`crate::vendor::x86::entry::protected_mode_entry`]).
pub const MULTIBOOT_BOOTLOADER_MAGIC: u32 = 0x2BAD_B002;
/// Guest-physical load address of the payload (1 MiB) — `linker.ld` `. = 1M`.
pub const PAYLOAD_LOAD_GPA: u32 = 0x0010_0000;
/// Max bytes scanned for the Multiboot header (Multiboot v1 requires it in the
/// first 8 KiB).
pub const MULTIBOOT_SEARCH_LEN: usize = 8192;

/// Multiboot v1 header flag bit 16: the address-override fields are valid.
const MB_FLAG_ADDRESS_OVERRIDE: u32 = 1 << 16;
/// Size of the address-override header (8 × `u32`: magic, flags, checksum, then
/// `header_addr`/`load_addr`/`load_end_addr`/`bss_end_addr`/`entry_addr`).
const MB_HEADER_LEN: usize = 32;

/// The address-override fields parsed out of the Multiboot header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MultibootHeader {
    /// Offset of the magic within the image file.
    pub header_file_offset: u32,
    /// `header_addr` field — the GPA the header itself is linked at.
    pub header_addr: u32,
    /// `load_addr` field — the GPA loading begins at.
    pub load_addr: u32,
    /// `load_end_addr` field — one past the last loaded byte's GPA.
    pub load_end_addr: u32,
    /// `bss_end_addr` field — one past the last BSS byte's GPA (zero-filled).
    pub bss_end_addr: u32,
    /// `entry_addr` field — the GPA to set `RIP`/`EIP` to.
    pub entry_addr: u32,
}

/// Result of flat-loading a payload into a guest-RAM slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoadedImage {
    /// Entry point GPA — fed to [`crate::vendor::x86::entry::protected_mode_entry`].
    pub entry_addr: u32,
    /// GPA the payload was loaded at (`PAYLOAD_LOAD_GPA` for these payloads).
    pub load_addr: u32,
    /// GPA one past the last loaded byte.
    pub load_end_addr: u32,
    /// GPA one past the last (zeroed) BSS byte.
    pub bss_end_addr: u32,
}

/// Errors the loader returns instead of panicking. The image is **untrusted
/// input** (conventions rule 4 / no-panic-on-untrusted-input): every malformed
/// image yields one of these, never a panic, slice-index OOB, or arithmetic
/// overflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LoadError {
    /// No Multiboot v1 header magic in the first `MULTIBOOT_SEARCH_LEN` bytes.
    #[error("no Multiboot v1 header (magic) found in the first {MULTIBOOT_SEARCH_LEN} bytes")]
    NoHeader,
    /// `magic + flags + checksum != 0 (mod 2^32)`.
    #[error("Multiboot header checksum invalid")]
    BadChecksum,
    /// The header lacks the address-override flag (bit 16).
    #[error("Multiboot header lacks the address-override flag (bit 16)")]
    NoAddressOverride,
    /// `load_end < load_addr`, `bss_end < load_end`, or the override formula
    /// underflows (`header_addr < load_addr`, or the file offset goes negative).
    #[error("address fields inconsistent (load_end < load_addr, or bss_end < load_end)")]
    BadAddressFields,
    /// The computed file offset or load span runs past the end of `image`.
    #[error("computed file offset or load span exceeds the image")]
    ImageTooSmall,
    /// The load/bss region does not fit in `guest_ram`.
    #[error("load/bss region [{0:#x}..{1:#x}) does not fit in guest RAM")]
    OutOfRange(u64, u64),
}

/// Read a little-endian `u32` at `off`, or `None` if `image` is too short.
fn read_u32(image: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    let bytes = image.get(off..end)?;
    // The slice is exactly 4 bytes, so the array conversion is infallible.
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Locate and parse the Multiboot v1 header: scan the first
/// `MULTIBOOT_SEARCH_LEN` bytes at 4-byte alignment for
/// `MULTIBOOT_HEADER_MAGIC`, verify `magic + flags + checksum == 0 (mod 2^32)`,
/// and require the address-override flag (bit 16). Returns the parsed fields or a
/// [`LoadError`]. Pure; never panics on arbitrary bytes.
pub fn parse_header(image: &[u8]) -> Result<MultibootHeader, LoadError> {
    let scan_limit = image.len().min(MULTIBOOT_SEARCH_LEN);
    // Scan at 4-byte alignment over every offset where a full `u32` fits
    // (`off + 4 <= scan_limit` ⇔ `off < scan_limit − 3`). A `for … step_by(4)` —
    // rather than a manual `off += 4` — so a mutation of the increment cannot turn
    // the scan into a non-terminating loop.
    for off in (0..scan_limit.saturating_sub(3)).step_by(4) {
        if read_u32(image, off) == Some(MULTIBOOT_HEADER_MAGIC) {
            return parse_header_at(image, off);
        }
    }
    Err(LoadError::NoHeader)
}

/// Validate and parse the 32-byte address-override header whose magic sits at
/// `off`. Split out so the scan loop stays small; any short read is
/// [`LoadError::ImageTooSmall`].
fn parse_header_at(image: &[u8], off: usize) -> Result<MultibootHeader, LoadError> {
    // The full address-override header must fit in the image.
    if off
        .checked_add(MB_HEADER_LEN)
        .is_none_or(|end| end > image.len())
    {
        return Err(LoadError::ImageTooSmall);
    }
    let field = |i: usize| read_u32(image, off + i * 4).ok_or(LoadError::ImageTooSmall);
    let magic = field(0)?;
    let flags = field(1)?;
    let checksum = field(2)?;

    // magic + flags + checksum == 0 (mod 2^32).
    if magic.wrapping_add(flags).wrapping_add(checksum) != 0 {
        return Err(LoadError::BadChecksum);
    }
    if flags & MB_FLAG_ADDRESS_OVERRIDE == 0 {
        return Err(LoadError::NoAddressOverride);
    }

    let header_addr = field(3)?;
    let load_addr = field(4)?;
    let load_end_addr = field(5)?;
    let bss_end_addr = field(6)?;
    let entry_addr = field(7)?;

    Ok(MultibootHeader {
        header_file_offset: off as u32,
        header_addr,
        load_addr,
        load_end_addr,
        bss_end_addr,
        entry_addr,
    })
}

/// Flat-load `image` into `guest_ram` (the host-side backing for GPA `0`):
/// 1. [`parse_header`];
/// 2. `file_off = header_file_offset − (header_addr − load_addr)` (checked;
///    underflow ⇒ [`LoadError::BadAddressFields`]);
/// 3. copy `image[file_off .. file_off + (load_end_addr − load_addr)]` into
///    `guest_ram[load_addr .. load_end_addr]`;
/// 4. zero `guest_ram[load_end_addr .. bss_end_addr]`.
///
/// All indexing is bounds-checked against both `image` and `guest_ram`; any
/// overflow is the corresponding [`LoadError`]. Returns the [`LoadedImage`].
pub fn load(image: &[u8], guest_ram: &mut [u8]) -> Result<LoadedImage, LoadError> {
    let h = parse_header(image)?;

    // Address fields must be monotone: load_addr <= load_end_addr <= bss_end_addr.
    if h.load_end_addr < h.load_addr || h.bss_end_addr < h.load_end_addr {
        return Err(LoadError::BadAddressFields);
    }

    // file_off = header_file_offset − (header_addr − load_addr), fully checked.
    // header_addr < load_addr or a negative file offset is an inconsistent image.
    let addr_delta = h
        .header_addr
        .checked_sub(h.load_addr)
        .ok_or(LoadError::BadAddressFields)?;
    let file_off = h
        .header_file_offset
        .checked_sub(addr_delta)
        .ok_or(LoadError::BadAddressFields)? as usize;

    // Copy span (load_end_addr − load_addr is non-negative by the check above).
    let copy_len = (h.load_end_addr - h.load_addr) as usize;
    let src_end = file_off
        .checked_add(copy_len)
        .ok_or(LoadError::ImageTooSmall)?;
    let src = image
        .get(file_off..src_end)
        .ok_or(LoadError::ImageTooSmall)?;

    // Destination: the whole [load_addr, bss_end_addr) region must fit in RAM.
    let load_addr = h.load_addr as usize;
    let load_end = h.load_end_addr as usize;
    let bss_end = h.bss_end_addr as usize;
    if bss_end > guest_ram.len() {
        return Err(LoadError::OutOfRange(
            h.load_addr as u64,
            h.bss_end_addr as u64,
        ));
    }

    // Steps 3 and 4: copy then zero-fill BSS. Indices are all validated above.
    guest_ram[load_addr..load_end].copy_from_slice(src);
    guest_ram[load_end..bss_end].fill(0);

    Ok(LoadedImage {
        entry_addr: h.entry_addr,
        load_addr: h.load_addr,
        load_end_addr: h.load_end_addr,
        bss_end_addr: h.bss_end_addr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic Multiboot image: fill the loadable region `[file_off,
    /// file_off + copy_len)` with `fill`, then overlay the 32-byte address-override
    /// header at `header_file_offset` (which, like the real payloads, sits *inside*
    /// the loaded region). Returns `(image, file_off)`.
    fn synth_image(
        header_file_offset: usize,
        header_addr: u32,
        load_addr: u32,
        load_end_addr: u32,
        bss_end_addr: u32,
        entry_addr: u32,
        fill: u8,
    ) -> (Vec<u8>, usize) {
        let flags = MB_FLAG_ADDRESS_OVERRIDE;
        let checksum = 0u32
            .wrapping_sub(MULTIBOOT_HEADER_MAGIC)
            .wrapping_sub(flags);
        let fields = [
            MULTIBOOT_HEADER_MAGIC,
            flags,
            checksum,
            header_addr,
            load_addr,
            load_end_addr,
            bss_end_addr,
            entry_addr,
        ];
        let addr_delta = header_addr - load_addr;
        let file_off = header_file_offset - addr_delta as usize;
        let copy_len = (load_end_addr - load_addr) as usize;

        let mut img = vec![0u8; (file_off + copy_len).max(header_file_offset + MB_HEADER_LEN)];
        // Fill the loadable region with the pattern, then overlay the header.
        img[file_off..file_off + copy_len].fill(fill);
        for (i, f) in fields.iter().enumerate() {
            img[header_file_offset + i * 4..header_file_offset + i * 4 + 4]
                .copy_from_slice(&f.to_le_bytes());
        }
        (img, file_off)
    }

    #[test]
    fn parses_and_loads_current_payload_shape() {
        // header_addr == load_addr == 0x100000, magic at file offset 0x1000 — the
        // header sits at the start of the loaded region, just like the payloads.
        let (img, _) = synth_image(
            0x1000, 0x10_0000, 0x10_0000, 0x10_0080, 0x10_00C0, 0x10_0000, 0xAA,
        );
        let h = parse_header(&img).unwrap();
        assert_eq!(h.header_file_offset, 0x1000);
        assert_eq!(h.load_addr, 0x10_0000);
        assert_eq!(h.entry_addr, 0x10_0000);

        let mut ram = vec![0u8; 0x20_0000];
        let loaded = load(&img, &mut ram).unwrap();
        assert_eq!(loaded.entry_addr, 0x10_0000);
        assert_eq!(loaded.load_addr, 0x10_0000);
        // The header round-trips at the region start (copied from file_off 0x1000,
        // proving the override formula's `header_file_offset` term is not dropped).
        assert_eq!(
            u32::from_le_bytes(ram[0x10_0000..0x10_0004].try_into().unwrap()),
            MULTIBOOT_HEADER_MAGIC
        );
        // Region bytes past the 32-byte header are the fill pattern.
        assert!(ram[0x10_0020..0x10_0080].iter().all(|&b| b == 0xAA));
        // BSS zeroed.
        assert!(ram[0x10_0080..0x10_00C0].iter().all(|&b| b == 0));
    }

    #[test]
    fn full_override_formula_header_addr_ne_load_addr() {
        // header_addr = load_addr + 0x100, magic at file offset 0x1100, so the
        // override formula must yield file_off = 0x1100 − 0x100 = 0x1000.
        let (img, file_off) = synth_image(
            0x1100, 0x10_0100, 0x10_0000, 0x10_0200, 0x10_0200, 0x10_0000, 0x5A,
        );
        assert_eq!(
            file_off, 0x1000,
            "override formula keeps the file-offset term"
        );
        let h = parse_header(&img).unwrap();
        assert_eq!(h.header_addr, 0x10_0100);
        assert_eq!(h.load_addr, 0x10_0000);

        let mut ram = vec![0u8; 0x20_0000];
        load(&img, &mut ram).unwrap();
        // The header lands at load_addr + (header_addr − load_addr) = +0x100,
        // proving the copy started at file_off 0x1000, not 0x1100 or 0x100.
        assert_eq!(
            u32::from_le_bytes(ram[0x10_0100..0x10_0104].try_into().unwrap()),
            MULTIBOOT_HEADER_MAGIC
        );
        // The fill pattern is present at the region start (copied from 0x1000).
        assert_eq!(ram[0x10_0000], 0x5A);
    }

    #[test]
    fn missing_magic_is_no_header() {
        let img = vec![0u8; 4096];
        assert_eq!(parse_header(&img), Err(LoadError::NoHeader));
    }

    #[test]
    fn bad_checksum_rejected() {
        let (mut img, _) = synth_image(0, 0x10_0000, 0x10_0000, 0x10_0040, 0x10_0040, 0x10_0000, 0);
        // Corrupt the checksum word.
        img[8..12].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        assert_eq!(parse_header(&img), Err(LoadError::BadChecksum));
    }

    #[test]
    fn missing_address_override_flag_rejected() {
        // Hand-build a header whose flags clear bit 16 but checksum still valid.
        let flags = 0u32;
        let checksum = 0u32
            .wrapping_sub(MULTIBOOT_HEADER_MAGIC)
            .wrapping_sub(flags);
        let mut img = vec![0u8; MB_HEADER_LEN];
        for (i, f) in [MULTIBOOT_HEADER_MAGIC, flags, checksum, 0, 0, 0, 0, 0]
            .iter()
            .enumerate()
        {
            img[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
        }
        assert_eq!(parse_header(&img), Err(LoadError::NoAddressOverride));
    }

    #[test]
    fn region_too_large_for_ram_is_out_of_range() {
        let (img, _) = synth_image(0, 0x10_0000, 0x10_0000, 0x10_0040, 0x10_0040, 0x10_0000, 1);
        let mut ram = vec![0u8; 0x1000]; // far too small for a 1 MiB load_addr
        assert!(matches!(
            load(&img, &mut ram),
            Err(LoadError::OutOfRange(_, _))
        ));
    }

    #[test]
    fn truncated_header_is_image_too_small() {
        // Magic present but the 32-byte header is cut short.
        let mut img = MULTIBOOT_HEADER_MAGIC.to_le_bytes().to_vec();
        img.extend_from_slice(&[0u8; 8]); // only 12 bytes total
        assert_eq!(parse_header(&img), Err(LoadError::ImageTooSmall));
    }

    /// A bare 32-byte address-override header at file offset 0 with arbitrary
    /// address fields (`header_addr == load_addr`), for the validation edge cases.
    fn addr_header(load_addr: u32, load_end: u32, bss_end: u32) -> Vec<u8> {
        let flags = MB_FLAG_ADDRESS_OVERRIDE;
        let checksum = 0u32
            .wrapping_sub(MULTIBOOT_HEADER_MAGIC)
            .wrapping_sub(flags);
        let fields = [
            MULTIBOOT_HEADER_MAGIC,
            flags,
            checksum,
            load_addr, // header_addr == load_addr ⇒ file_off = 0
            load_addr,
            load_end,
            bss_end,
            load_addr, // entry
        ];
        let mut img = vec![0u8; MB_HEADER_LEN];
        for (i, f) in fields.iter().enumerate() {
            img[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
        }
        img
    }

    #[test]
    fn load_address_field_validation() {
        // load_end < load_addr is inconsistent (kills the `<`→`==` and the
        // `||`→`&&` mutants on the monotonicity check; under `&&`/`==` the bad
        // fields slip through and the copy-span subtraction underflows).
        let mut ram = vec![0u8; 0x20_0000];
        assert_eq!(
            load(&addr_header(0x10_0000, 0x0F_0000, 0x10_0000), &mut ram),
            Err(LoadError::BadAddressFields)
        );
        // bss_end < load_end is likewise inconsistent (the second disjunct).
        assert_eq!(
            load(&addr_header(0x10_0000, 0x10_0040, 0x10_0000), &mut ram),
            Err(LoadError::BadAddressFields)
        );
        // load_end == load_addr is VALID (an empty load region) — a `<`→`<=`
        // mutant would wrongly reject it.
        let (empty, _) = synth_image(0, 0x10_0000, 0x10_0000, 0x10_0000, 0x10_0040, 0x10_0000, 0);
        assert!(load(&empty, &mut ram).is_ok());
    }

    #[test]
    fn load_region_exact_fit_is_accepted() {
        // bss_end == guest_ram.len() exactly fits (kills the `>`→`>=` mutant on the
        // out-of-range bound).
        let (img, _) = synth_image(0, 0x0, 0x0, 0x0, 0x40, 0x0, 0xCC);
        let mut ram = vec![0u8; 0x40];
        assert!(load(&img, &mut ram).is_ok());
        // One byte short ⇒ rejected.
        let mut tiny = vec![0u8; 0x3F];
        assert!(matches!(
            load(&img, &mut tiny),
            Err(LoadError::OutOfRange(_, _))
        ));
    }
}
