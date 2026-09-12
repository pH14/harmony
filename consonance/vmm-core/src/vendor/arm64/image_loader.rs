// SPDX-License-Identifier: AGPL-3.0-or-later

use super::board::{PAGE, RAM_BASE, align_up};

pub const IMAGE_MAGIC: u32 = 0x644d_5241;

pub const MAGIC_OFFSET: usize = 56;

const TEXT_OFFSET_OFF: usize = 8;
const IMAGE_SIZE_OFF: usize = 16;
const FLAGS_OFF: usize = 24;
const HEADER_LEN: usize = 64;

const CODE0_BRANCH_OVER_HEADER: u32 = 0x1400_0000 | (HEADER_LEN as u32 / 4);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ImageHeader {
    pub text_offset: u64,
    pub image_size: u64,
    pub flags: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LoadedImage {
    pub entry_gpa: u64,
    pub load_gpa: u64,
    pub end_off: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ImageLoadError {
    #[error("image shorter than the {HEADER_LEN}-byte arm64 Image header")]
    TooShort,
    #[error("not an arm64 Image: magic {found:#010x} != {IMAGE_MAGIC:#010x}")]
    BadMagic { found: u32 },
    #[error("big-endian kernel (flags bit 0 set); only little-endian is supported")]
    BigEndian,
    #[error("text_offset {text_offset:#x} is not {PAGE:#x}-aligned")]
    UnalignedTextOffset { text_offset: u64 },
    #[error("image does not fit: end {end:#x} exceeds RAM {ram:#x}")]
    DoesNotFit { end: u64, ram: u64 },
}

fn read_u64(image: &[u8], off: usize) -> Option<u64> {
    let end = off.checked_add(8)?;
    let bytes = image.get(off..end)?;
    Some(u64::from_le_bytes(bytes.try_into().expect("8-byte slice")))
}

fn read_u32(image: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    let bytes = image.get(off..end)?;
    Some(u32::from_le_bytes(bytes.try_into().expect("4-byte slice")))
}

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

pub fn load(image: &[u8], ram: &mut [u8]) -> Result<LoadedImage, ImageLoadError> {
    let hdr = parse_header(image)?;
    let ram_len = ram.len() as u64;
    let file_len = image.len() as u64;
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
    let start = hdr.text_offset as usize;
    let stop = start + image.len();
    ram[start..stop].copy_from_slice(image);
    Ok(LoadedImage {
        entry_gpa: RAM_BASE + hdr.text_offset,
        load_gpa: RAM_BASE + hdr.text_offset,
        end_off: align_up(end, PAGE),
    })
}

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

    fn synth(text_offset: u64, image_size: u64, flags: u64, code_len: usize) -> Vec<u8> {
        let mut img = vec![0u8; HEADER_LEN + code_len];
        img[TEXT_OFFSET_OFF..TEXT_OFFSET_OFF + 8].copy_from_slice(&text_offset.to_le_bytes());
        img[IMAGE_SIZE_OFF..IMAGE_SIZE_OFF + 8].copy_from_slice(&image_size.to_le_bytes());
        img[FLAGS_OFF..FLAGS_OFF + 8].copy_from_slice(&flags.to_le_bytes());
        img[MAGIC_OFFSET..MAGIC_OFFSET + 4].copy_from_slice(&IMAGE_MAGIC.to_le_bytes());
        for (i, b) in img[HEADER_LEN..].iter_mut().enumerate() {
            *b = (i as u8).wrapping_add(1);
        }
        img
    }

    #[test]
    fn parses_and_loads_a_valid_image() {
        let img = synth(0, 0, 0xA, 128);
        let mut ram = vec![0u8; 0x10_0000];
        let loaded = load(&img, &mut ram).unwrap();
        assert_eq!(loaded.entry_gpa, RAM_BASE);
        assert_eq!(loaded.load_gpa, RAM_BASE);
        assert_eq!(&ram[..img.len()], &img[..]);
        assert_eq!(loaded.end_off, align_up(img.len() as u64, PAGE));
    }

    #[test]
    fn honors_a_nonzero_text_offset() {
        let img = synth(0x8_0000, 0, 0, 64);
        let mut ram = vec![0u8; 0x20_0000];
        let loaded = load(&img, &mut ram).unwrap();
        assert_eq!(loaded.entry_gpa, RAM_BASE + 0x8_0000);
        assert_eq!(&ram[0x8_0000..0x8_0000 + img.len()], &img[..]);
        assert!(ram[..0x8_0000].iter().all(|&b| b == 0));
    }

    #[test]
    fn image_size_reserves_bss_past_the_file() {
        let img = synth(0, 4096, 0, 64);
        let mut small = vec![0u8; 2048];
        assert!(matches!(
            load(&img, &mut small),
            Err(ImageLoadError::DoesNotFit { .. })
        ));
        let mut ok = vec![0u8; 8192];
        let loaded = load(&img, &mut ok).unwrap();
        assert_eq!(loaded.end_off, 4096);
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        assert_eq!(parse_header(&[0u8; 10]), Err(ImageLoadError::TooShort));
        let mut bad = vec![0u8; HEADER_LEN];
        assert!(matches!(
            parse_header(&bad),
            Err(ImageLoadError::BadMagic { .. })
        ));
        bad[MAGIC_OFFSET..MAGIC_OFFSET + 4].copy_from_slice(&IMAGE_MAGIC.to_le_bytes());
        bad[FLAGS_OFF] = 1;
        assert_eq!(parse_header(&bad), Err(ImageLoadError::BigEndian));
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

    fn decode_b_offset(word: u32) -> Option<i64> {
        if word >> 26 != 0b000101 {
            return None;
        }
        Some(i64::from(((word << 6) as i32) >> 6) * 4)
    }

    #[test]
    fn wrap_image_entry_branches_over_the_header_onto_the_payload() {
        let code: Vec<u8> = (0..48u8).map(|i| i.wrapping_add(0x40)).collect();
        let wrapped = wrap_image(&code, 0, 0xA);

        let mut ram = vec![0u8; 0x1000];
        let loaded = load(&wrapped, &mut ram).unwrap();
        assert_eq!(loaded.entry_gpa, RAM_BASE);
        assert_eq!(loaded.entry_gpa, loaded.load_gpa);
        let entry_off = (loaded.entry_gpa - RAM_BASE) as usize;

        let code0 = u32::from_le_bytes(ram[entry_off..entry_off + 4].try_into().unwrap());
        assert_ne!(code0, 0, "code0 must not be the zero word (the r7 bug)");
        let dist = decode_b_offset(code0).expect("code0 must be an AArch64 `B`");

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
