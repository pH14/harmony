// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fmt;

pub const WORK_RAM_LEN: usize = 2048;
pub const MAX_HOLD_FRAMES: u8 = 120;

pub const BILLBOARD_MAGIC: [u8; 4] = *b"HBBD";

pub const BILLBOARD_LAYOUT_VERSION: u16 = 1;

pub const HEADER_LEN: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BillboardLayout {
    savestate_len: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BillboardError {
    BufferTooSmall { got: usize, need: usize },
    SavestateTooLarge { got: usize },
}

impl fmt::Display for BillboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BillboardError::BufferTooSmall { got, need } => {
                write!(f, "billboard buffer is {got} bytes, layout needs {need}")
            }
            BillboardError::SavestateTooLarge { got } => {
                write!(f, "savestate of {got} bytes overflows the u32 region field")
            }
        }
    }
}

impl std::error::Error for BillboardError {}

impl BillboardLayout {
    pub fn new(savestate_len: usize) -> Result<Self, BillboardError> {
        let len32 = u32::try_from(savestate_len)
            .ok()
            .filter(|l| (*l as u64) + (HEADER_LEN + WORK_RAM_LEN) as u64 <= u64::from(u32::MAX))
            .ok_or(BillboardError::SavestateTooLarge { got: savestate_len })?;
        Ok(BillboardLayout {
            savestate_len: len32,
        })
    }

    pub fn savestate_len(&self) -> usize {
        self.savestate_len as usize
    }

    pub fn total_len(&self) -> usize {
        HEADER_LEN + self.savestate_len as usize + WORK_RAM_LEN
    }

    pub fn write_header(
        &self,
        buf: &mut [u8],
        frame: u32,
        joypad: u8,
    ) -> Result<(), BillboardError> {
        if buf.len() < self.total_len() {
            return Err(BillboardError::BufferTooSmall {
                got: buf.len(),
                need: self.total_len(),
            });
        }
        let savestate_off = HEADER_LEN as u32;
        let workram_off = savestate_off + self.savestate_len;
        buf[0..4].copy_from_slice(&BILLBOARD_MAGIC);
        buf[4..6].copy_from_slice(&BILLBOARD_LAYOUT_VERSION.to_le_bytes());
        buf[6..8].copy_from_slice(&0u16.to_le_bytes());
        buf[8..12].copy_from_slice(&frame.to_le_bytes());
        buf[12] = joypad;
        buf[13..16].copy_from_slice(&[0u8; 3]);
        buf[16..20].copy_from_slice(&savestate_off.to_le_bytes());
        buf[20..24].copy_from_slice(&self.savestate_len.to_le_bytes());
        buf[24..28].copy_from_slice(&workram_off.to_le_bytes());
        buf[28..32].copy_from_slice(&(WORK_RAM_LEN as u32).to_le_bytes());
        Ok(())
    }

    pub fn savestate_mut<'a>(&self, buf: &'a mut [u8]) -> &'a mut [u8] {
        let len = buf.len();
        let start = HEADER_LEN;
        let end = start + self.savestate_len as usize;
        &mut buf[start.min(len)..end.min(len)]
    }

    pub fn work_ram_mut<'a>(&self, buf: &'a mut [u8]) -> &'a mut [u8] {
        let len = buf.len();
        let start = HEADER_LEN + self.savestate_len as usize;
        let end = start + WORK_RAM_LEN;
        &mut buf[start.min(len)..end.min(len)]
    }
}

pub const NOVA_BILLBOARD_LAYOUT_VERSION: u16 = 2;
pub const NOVA_HEADER_LEN: usize = 32;
pub const NOVA_SAVE_RAM_LEN: usize = 8 * 1024;
pub const NOVA_WORK_RAM_RING_LEN: usize = MAX_HOLD_FRAMES as usize * WORK_RAM_LEN;

const NOVA_DEAD_FLAG: u16 = 1 << 0;
const NOVA_CLEARED_FLAG: u16 = 1 << 1;
const NOVA_WORK_RAM_OFFSET: usize = NOVA_HEADER_LEN;
const NOVA_SAVE_RAM_OFFSET: usize = NOVA_WORK_RAM_OFFSET + NOVA_WORK_RAM_RING_LEN;
const NOVA_SAVESTATE_OFFSET: usize = NOVA_SAVE_RAM_OFFSET + NOVA_SAVE_RAM_LEN;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NovaBillboardLayout {
    savestate_len: u32,
}

impl NovaBillboardLayout {
    pub fn new(savestate_len: usize) -> Result<Self, BillboardError> {
        let fixed_len = NOVA_SAVESTATE_OFFSET;
        let len32 = u32::try_from(savestate_len)
            .ok()
            .filter(|len| (*len as u64) + (fixed_len as u64) <= u64::from(u32::MAX))
            .ok_or(BillboardError::SavestateTooLarge { got: savestate_len })?;
        Ok(Self {
            savestate_len: len32,
        })
    }

    #[must_use]
    pub fn savestate_len(&self) -> usize {
        self.savestate_len as usize
    }

    #[must_use]
    pub const fn work_ram_offset(&self) -> usize {
        NOVA_WORK_RAM_OFFSET
    }

    #[must_use]
    pub const fn work_ram_ring_len(&self) -> usize {
        NOVA_WORK_RAM_RING_LEN
    }

    #[must_use]
    pub const fn save_ram_offset(&self) -> usize {
        NOVA_SAVE_RAM_OFFSET
    }

    #[must_use]
    pub const fn save_ram_len(&self) -> usize {
        NOVA_SAVE_RAM_LEN
    }

    #[must_use]
    pub const fn savestate_offset(&self) -> usize {
        NOVA_SAVE_RAM_OFFSET + NOVA_SAVE_RAM_LEN
    }

    #[must_use]
    pub fn total_len(&self) -> usize {
        self.savestate_offset() + self.savestate_len()
    }

    pub fn write_header(
        &self,
        buf: &mut [u8],
        frame: u32,
        joypad: u8,
        frames_run: u8,
        dead: bool,
        cleared: bool,
    ) -> Result<(), BillboardError> {
        if buf.len() < self.total_len() {
            return Err(BillboardError::BufferTooSmall {
                got: buf.len(),
                need: self.total_len(),
            });
        }
        let mut flags = 0_u16;
        if dead {
            flags |= NOVA_DEAD_FLAG;
        }
        if cleared {
            flags |= NOVA_CLEARED_FLAG;
        }
        buf[0..4].copy_from_slice(&BILLBOARD_MAGIC);
        buf[4..6].copy_from_slice(&NOVA_BILLBOARD_LAYOUT_VERSION.to_le_bytes());
        buf[6..8].copy_from_slice(&flags.to_le_bytes());
        buf[8..12].copy_from_slice(&frame.to_le_bytes());
        buf[12] = joypad;
        buf[13] = frames_run;
        buf[14..16].fill(0);
        buf[16..20].copy_from_slice(&(NOVA_WORK_RAM_OFFSET as u32).to_le_bytes());
        buf[20..24].copy_from_slice(&(NOVA_WORK_RAM_RING_LEN as u32).to_le_bytes());
        buf[24..28].copy_from_slice(&(NOVA_SAVE_RAM_OFFSET as u32).to_le_bytes());
        buf[28..32].copy_from_slice(&(NOVA_SAVE_RAM_LEN as u32).to_le_bytes());
        Ok(())
    }

    pub fn work_ram_ring_mut<'a>(&self, buf: &'a mut [u8]) -> &'a mut [u8] {
        let len = buf.len();
        let start = NOVA_WORK_RAM_OFFSET;
        let end = start + NOVA_WORK_RAM_RING_LEN;
        &mut buf[start.min(len)..end.min(len)]
    }

    pub fn work_ram_slot_mut<'a>(&self, buf: &'a mut [u8], slot: usize) -> &'a mut [u8] {
        if slot >= NOVA_WORK_RAM_RING_LEN / WORK_RAM_LEN {
            return &mut buf[0..0];
        }
        let Some(start) = NOVA_WORK_RAM_OFFSET.checked_add(slot * WORK_RAM_LEN) else {
            return &mut buf[0..0];
        };
        let end = start + WORK_RAM_LEN;
        let len = buf.len();
        &mut buf[start.min(len)..end.min(len)]
    }

    pub fn save_ram_mut<'a>(&self, buf: &'a mut [u8]) -> &'a mut [u8] {
        let len = buf.len();
        let start = NOVA_SAVE_RAM_OFFSET;
        let end = start + NOVA_SAVE_RAM_LEN;
        &mut buf[start.min(len)..end.min(len)]
    }

    pub fn savestate_mut<'a>(&self, buf: &'a mut [u8]) -> &'a mut [u8] {
        let len = buf.len();
        let start = self.savestate_offset();
        let end = start + self.savestate_len();
        &mut buf[start.min(len)..end.min(len)]
    }

    #[must_use]
    pub fn dead_flag(buf: &[u8]) -> bool {
        buf.get(6..8)
            .and_then(|bytes| bytes.try_into().ok())
            .map(|bytes| u16::from_le_bytes(bytes) & NOVA_DEAD_FLAG != 0)
            .unwrap_or(false)
    }

    #[must_use]
    pub fn cleared_flag(buf: &[u8]) -> bool {
        buf.get(6..8)
            .and_then(|bytes| bytes.try_into().ok())
            .map(|bytes| u16::from_le_bytes(bytes) & NOVA_CLEARED_FLAG != 0)
            .unwrap_or(false)
    }
}

pub type BillboardLayoutV2 = NovaBillboardLayout;

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_encode_billboard(
        frame: u32,
        joypad: u8,
        savestate: &[u8],
        work_ram: &[u8],
    ) -> Vec<u8> {
        let savestate_off = HEADER_LEN as u32;
        let savestate_len = savestate.len() as u32;
        let workram_off = savestate_off + savestate_len;
        let workram_len = work_ram.len() as u32;
        let mut buf = Vec::with_capacity(HEADER_LEN + savestate.len() + work_ram.len());
        buf.extend_from_slice(&BILLBOARD_MAGIC);
        buf.extend_from_slice(&BILLBOARD_LAYOUT_VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&frame.to_le_bytes());
        buf.push(joypad);
        buf.extend_from_slice(&[0u8; 3]);
        buf.extend_from_slice(&savestate_off.to_le_bytes());
        buf.extend_from_slice(&savestate_len.to_le_bytes());
        buf.extend_from_slice(&workram_off.to_le_bytes());
        buf.extend_from_slice(&workram_len.to_le_bytes());
        buf.extend_from_slice(savestate);
        buf.extend_from_slice(work_ram);
        buf
    }

    #[test]
    fn writer_matches_canonical_encoding() {
        let savestate: Vec<u8> = (0..40u8).map(|b| b.wrapping_mul(7)).collect();
        let work_ram: Vec<u8> = (0..WORK_RAM_LEN as u32).map(|b| (b % 251) as u8).collect();
        let expected = canonical_encode_billboard(7, 0b0000_0011, &savestate, &work_ram);

        let layout = BillboardLayout::new(savestate.len()).unwrap();
        let mut buf = vec![0u8; layout.total_len()];
        layout.write_header(&mut buf, 7, 0b0000_0011).unwrap();
        layout.savestate_mut(&mut buf).copy_from_slice(&savestate);
        layout.work_ram_mut(&mut buf).copy_from_slice(&work_ram);
        assert_eq!(buf, expected);
    }

    #[test]
    fn header_bytes_golden() {
        let layout = BillboardLayout::new(0x40).unwrap();
        let mut buf = vec![0u8; layout.total_len()];
        layout.write_header(&mut buf, 0x0102_0304, 0xA5).unwrap();
        #[rustfmt::skip]
        let expected: [u8; HEADER_LEN] = [
            b'H', b'B', b'B', b'D',
            0x01, 0x00,
            0x00, 0x00,
            0x04, 0x03, 0x02, 0x01,
            0xA5,
            0x00, 0x00, 0x00,
            0x20, 0x00, 0x00, 0x00,
            0x40, 0x00, 0x00, 0x00,
            0x60, 0x00, 0x00, 0x00,
            0x00, 0x08, 0x00, 0x00,
        ];
        assert_eq!(&buf[..HEADER_LEN], &expected);
    }

    #[test]
    fn total_len_is_header_plus_regions() {
        let layout = BillboardLayout::new(24_576).unwrap();
        assert_eq!(layout.total_len(), 32 + 24_576 + 2048);
        assert_eq!(layout.savestate_len(), 24_576);
    }

    #[test]
    fn rejects_short_buffers_and_oversized_savestates() {
        let layout = BillboardLayout::new(64).unwrap();
        let mut short = vec![0u8; layout.total_len() - 1];
        assert_eq!(
            layout.write_header(&mut short, 0, 0),
            Err(BillboardError::BufferTooSmall {
                got: layout.total_len() - 1,
                need: layout.total_len(),
            })
        );
        assert!(matches!(
            BillboardLayout::new(usize::MAX),
            Err(BillboardError::SavestateTooLarge { .. })
        ));
        assert!(matches!(
            BillboardLayout::new(u32::MAX as usize),
            Err(BillboardError::SavestateTooLarge { .. })
        ));
    }

    #[test]
    fn region_accessors_clamp_on_short_buffers() {
        let layout = BillboardLayout::new(64).unwrap();
        let mut tiny = vec![0u8; 8];
        assert!(layout.savestate_mut(&mut tiny).is_empty());
        assert!(layout.work_ram_mut(&mut tiny).is_empty());
    }

    #[test]
    fn nova_v2_header_and_regions_are_fixed_and_bounded() {
        let layout = NovaBillboardLayout::new(64).unwrap();
        assert_eq!(layout.work_ram_offset(), NOVA_HEADER_LEN);
        assert_eq!(layout.work_ram_ring_len(), 120 * WORK_RAM_LEN);
        assert_eq!(
            layout.save_ram_offset(),
            NOVA_HEADER_LEN + 120 * WORK_RAM_LEN
        );
        assert_eq!(layout.save_ram_len(), NOVA_SAVE_RAM_LEN);
        assert_eq!(
            layout.savestate_offset(),
            layout.save_ram_offset() + NOVA_SAVE_RAM_LEN
        );
        assert_eq!(
            layout.total_len(),
            NOVA_HEADER_LEN + 120 * WORK_RAM_LEN + NOVA_SAVE_RAM_LEN + 64
        );

        let mut buf = vec![0_u8; layout.total_len()];
        layout
            .write_header(&mut buf, 7, 0xA5, 3, true, true)
            .unwrap();
        assert_eq!(&buf[0..4], b"HBBD");
        assert_eq!(
            u16::from_le_bytes(buf[4..6].try_into().unwrap()),
            NOVA_BILLBOARD_LAYOUT_VERSION
        );
        assert_eq!(u16::from_le_bytes(buf[6..8].try_into().unwrap()), 3);
        assert_eq!(buf[12], 0xA5);
        assert_eq!(buf[13], 3);
        assert!(NovaBillboardLayout::dead_flag(&buf));
        assert!(NovaBillboardLayout::cleared_flag(&buf));
    }

    #[test]
    fn nova_v2_ring_slots_and_short_accessors_never_panic() {
        let layout = NovaBillboardLayout::new(64).unwrap();
        let mut buf = vec![0_u8; layout.total_len()];
        layout.work_ram_slot_mut(&mut buf, 0)[0] = 0x11;
        layout.work_ram_slot_mut(&mut buf, 119)[WORK_RAM_LEN - 1] = 0x22;
        assert_eq!(layout.work_ram_slot_mut(&mut buf, 0)[0], 0x11);
        assert_eq!(
            layout.work_ram_slot_mut(&mut buf, 119)[WORK_RAM_LEN - 1],
            0x22
        );
        assert!(layout.work_ram_slot_mut(&mut buf, 120).is_empty());

        let mut tiny = vec![0_u8; NOVA_HEADER_LEN + 3];
        assert_eq!(layout.work_ram_ring_mut(&mut tiny).len(), 3);
        assert!(layout.save_ram_mut(&mut tiny).is_empty());
        assert!(layout.savestate_mut(&mut tiny).is_empty());
    }
}

pub const BILLBOARD_VERSION: u16 = NOVA_BILLBOARD_LAYOUT_VERSION;
pub const BILLBOARD_WORK_RAM_OFFSET: usize = NOVA_WORK_RAM_OFFSET;
pub const BILLBOARD_WORK_RAM_LEN: usize = NOVA_WORK_RAM_RING_LEN;
pub const BILLBOARD_SAVE_RAM_OFFSET: usize = NOVA_SAVE_RAM_OFFSET;
pub const BILLBOARD_SAVE_RAM_LEN: usize = NOVA_SAVE_RAM_LEN;
pub const BILLBOARD_OBSERVATION_LEN: usize = NOVA_SAVE_RAM_OFFSET + NOVA_SAVE_RAM_LEN;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);
impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for DecodeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillboardObservation {
    pub work_frames: Vec<[u8; WORK_RAM_LEN]>,
    pub endpoint_work_ram: [u8; WORK_RAM_LEN],
    pub save_ram: [u8; NOVA_SAVE_RAM_LEN],
}

pub fn parse_billboard(
    bytes: &[u8],
    require_frames: bool,
) -> Result<BillboardObservation, DecodeError> {
    if bytes.len() < BILLBOARD_OBSERVATION_LEN {
        return Err(DecodeError("billboard bytes are truncated".to_owned()));
    }
    if bytes.get(0..4) != Some(BILLBOARD_MAGIC.as_slice()) {
        return Err(DecodeError("billboard magic is absent".to_owned()));
    }
    if read_u16(bytes, 4)? != BILLBOARD_VERSION {
        return Err(DecodeError("unsupported billboard version".to_owned()));
    }
    if read_u16(bytes, 6)? & !0b11 != 0 {
        return Err(DecodeError(
            "billboard has unknown endpoint flags".to_owned(),
        ));
    }
    if bytes.get(14..16) != Some(&[0, 0]) {
        return Err(DecodeError(
            "billboard reserved bytes are nonzero".to_owned(),
        ));
    }
    let frame_count = u64::from(read_u32(bytes, 8)?);
    let frames_run = bytes
        .get(13)
        .copied()
        .ok_or_else(|| DecodeError("billboard frames-run field is truncated".to_owned()))?;
    if frames_run > MAX_HOLD_FRAMES || (require_frames && frames_run == 0) {
        return Err(DecodeError("billboard frame count is malformed".to_owned()));
    }
    let work_offset = usize::try_from(read_u32(bytes, 16)?)
        .map_err(|_| DecodeError("billboard work offset overflow".to_owned()))?;
    let work_len = usize::try_from(read_u32(bytes, 20)?)
        .map_err(|_| DecodeError("billboard work length overflow".to_owned()))?;
    let save_offset = usize::try_from(read_u32(bytes, 24)?)
        .map_err(|_| DecodeError("billboard save offset overflow".to_owned()))?;
    let save_len = usize::try_from(read_u32(bytes, 28)?)
        .map_err(|_| DecodeError("billboard save length overflow".to_owned()))?;
    if (work_offset, work_len, save_offset, save_len)
        != (
            BILLBOARD_WORK_RAM_OFFSET,
            BILLBOARD_WORK_RAM_LEN,
            BILLBOARD_SAVE_RAM_OFFSET,
            BILLBOARD_SAVE_RAM_LEN,
        )
    {
        return Err(DecodeError(
            "billboard observation regions are malformed".to_owned(),
        ));
    }
    let slot = |index: usize| -> Result<[u8; WORK_RAM_LEN], DecodeError> {
        let start = work_offset
            .checked_add(
                index
                    .checked_mul(WORK_RAM_LEN)
                    .ok_or_else(|| DecodeError("billboard work slot offset overflow".to_owned()))?,
            )
            .ok_or_else(|| DecodeError("billboard work slot offset overflow".to_owned()))?;
        let end = start
            .checked_add(WORK_RAM_LEN)
            .ok_or_else(|| DecodeError("billboard work slot end overflow".to_owned()))?;
        bytes
            .get(start..end)
            .ok_or_else(|| DecodeError("billboard work slot is truncated".to_owned()))?
            .try_into()
            .map_err(|_| DecodeError("billboard work slot is malformed".to_owned()))
    };
    let work_frames = (0..usize::from(frames_run))
        .map(slot)
        .collect::<Result<Vec<_>, _>>()?;
    let endpoint_work_ram = slot(usize::from(frames_run.saturating_sub(1)))?;
    let save_end = save_offset
        .checked_add(save_len)
        .ok_or_else(|| DecodeError("billboard save end overflow".to_owned()))?;
    let save_ram = bytes
        .get(save_offset..save_end)
        .ok_or_else(|| DecodeError("billboard save window is truncated".to_owned()))?
        .try_into()
        .map_err(|_| DecodeError("billboard save window is malformed".to_owned()))?;
    if frame_count < u64::from(frames_run) {
        return Err(DecodeError("billboard frame count is malformed".to_owned()));
    }
    Ok(BillboardObservation {
        work_frames,
        endpoint_work_ram,
        save_ram,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, DecodeError> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| DecodeError("billboard u16 offset overflow".to_owned()))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| DecodeError("billboard u16 field is truncated".to_owned()))?
        .try_into()
        .map(u16::from_le_bytes)
        .map_err(|_| DecodeError("billboard u16 field is malformed".to_owned()))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, DecodeError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| DecodeError("billboard u32 offset overflow".to_owned()))?;
    bytes
        .get(offset..end)
        .ok_or_else(|| DecodeError("billboard u32 field is truncated".to_owned()))?
        .try_into()
        .map(u32::from_le_bytes)
        .map_err(|_| DecodeError("billboard u32 field is malformed".to_owned()))
}

#[cfg(test)]
mod decoder_tests {
    use super::*;
    #[test]
    fn decoder_checks_regions_flags_and_truncation() {
        let layout = NovaBillboardLayout::new(0).unwrap();
        let mut bytes = vec![0; layout.total_len()];
        layout
            .write_header(&mut bytes, 3, 0, 3, false, false)
            .unwrap();
        assert_eq!(parse_billboard(&bytes, true).unwrap().work_frames.len(), 3);
        for end in 0..bytes.len() {
            assert!(parse_billboard(&bytes[..end], false).is_err());
        }
        for offset in [0, 4, 6, 14, 16, 20, 24, 28] {
            let mut bad = bytes.clone();
            bad[offset] ^= 0x80;
            assert!(parse_billboard(&bad, true).is_err(), "offset {offset}");
        }
        bytes[13] = 121;
        assert!(parse_billboard(&bytes, true).is_err());
    }
}
