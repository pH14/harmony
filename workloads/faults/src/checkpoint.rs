// SPDX-License-Identifier: AGPL-3.0-or-later

//! The retained bytes of one whole-VM checkpoint.
//!
//! A workspace stores checkpoints as opaque blobs, so the encoding is this
//! crate's business. It is a small versioned binary form rather than JSON:
//! a checkpoint of a gigabyte-class guest carries megabytes of page bytes, and
//! a text encoding of those would cost several times their size on every fork,
//! run, and exec.
//!
//! The format is deliberately literal, so an operator can identify a blob:
//!
//! | field | bytes |
//! |---|---|
//! | `HARMCKPT` | 8 |
//! | version | 4, little-endian |
//! | setup handle | 8 |
//! | virtual time | 8 |
//! | image identity | 32 |
//! | page count | 8 |
//! | each page: guest frame number, byte length, 4096 bytes | 8 + 4 + 4096 |
//! | sidecar length, bytes | 8 + n |

use std::error::Error;

/// The bytes every checkpoint blob starts with.
pub const MAGIC: &[u8; 8] = b"HARMCKPT";
/// The version this build writes and reads.
pub const VERSION: u32 = 1;

// V1's old parser accepted arbitrary page lengths, but only this restorable
// guest-page shape is supported by the VMM. Malformed legacy shapes are
// rejected instead of being retained as pages that cannot be restored.
const PAGE_SIZE: usize = 4096;
const PAGE_RECORD_SIZE: usize = 8 + 4 + PAGE_SIZE;
const HEADER_SIZE: usize = 8 + 4 + 8 + 8 + 32 + 8;
const SIDECAR_LENGTH_SIZE: usize = 8;

/// One whole-VM checkpoint in the shape the workspace stores.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Checkpoint {
    /// The session's sealed setup handle. A checkpoint only restores into a
    /// session that reached the same setup point.
    pub setup: u64,
    /// Virtual time at the captured point.
    pub at: u64,
    /// The image identity the session booted.
    pub image_identity: [u8; 32],
    /// The pages the point owns, by guest frame number.
    pub pages: Vec<(u64, Vec<u8>)>,
    /// Opaque device and CPU state.
    pub sidecar: Vec<u8>,
}

impl Checkpoint {
    /// Encode the checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when a page does not have the fixed 4096-byte shape,
    /// page frame numbers are not strictly increasing, a length cannot be
    /// represented in the wire format, or the output allocation fails.
    pub fn encode(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        let page_count = u64::try_from(self.pages.len())?;
        let pages_bytes = self
            .pages
            .len()
            .checked_mul(PAGE_RECORD_SIZE)
            .ok_or("checkpoint page data length overflows")?;
        let capacity = HEADER_SIZE
            .checked_add(pages_bytes)
            .and_then(|length| length.checked_add(SIDECAR_LENGTH_SIZE))
            .and_then(|length| length.checked_add(self.sidecar.len()))
            .ok_or("checkpoint encoded length overflows")?;
        let sidecar_length = u64::try_from(self.sidecar.len())?;
        let page_length = u32::try_from(PAGE_SIZE)?;

        let mut previous_gfn = None;
        for (index, (gfn, page)) in self.pages.iter().enumerate() {
            if page.len() != PAGE_SIZE {
                return Err(format!(
                    "checkpoint page {index} has length {}, expected {PAGE_SIZE}",
                    page.len()
                )
                .into());
            }
            if previous_gfn.is_some_and(|previous| *gfn <= previous) {
                return Err(format!(
                    "checkpoint page {index} GFN {gfn} is not strictly greater than the previous GFN"
                )
                .into());
            }
            previous_gfn = Some(*gfn);
        }

        let mut bytes = Vec::new();
        bytes.try_reserve_exact(capacity)?;
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&self.setup.to_le_bytes());
        bytes.extend_from_slice(&self.at.to_le_bytes());
        bytes.extend_from_slice(&self.image_identity);
        bytes.extend_from_slice(&page_count.to_le_bytes());
        for (gfn, page) in &self.pages {
            bytes.extend_from_slice(&gfn.to_le_bytes());
            bytes.extend_from_slice(&page_length.to_le_bytes());
            bytes.extend_from_slice(page);
        }
        bytes.extend_from_slice(&sidecar_length.to_le_bytes());
        bytes.extend_from_slice(&self.sidecar);
        Ok(bytes)
    }

    /// Decode a checkpoint blob.
    ///
    /// # Errors
    ///
    /// Returns an error when the bytes are not a checkpoint, name another
    /// version, end early, contain trailing bytes, or violate the page
    /// invariants.
    pub fn decode(bytes: &[u8]) -> Result<Self, Box<dyn Error>> {
        let mut reader = Reader { bytes, at: 0 };
        if reader.take(MAGIC.len())? != MAGIC {
            return Err("this blob is not a Harmony checkpoint".into());
        }
        let version = reader.u32()?;
        if version != VERSION {
            return Err(format!(
                "checkpoint version {version} was written by another build; this one reads {VERSION}"
            )
            .into());
        }
        let setup = reader.u64()?;
        let at = reader.u64()?;
        let mut image_identity = [0_u8; 32];
        image_identity.copy_from_slice(reader.take(32)?);
        let count = usize::try_from(reader.u64()?)?;
        let remaining = reader.remaining()?;
        let maximum_count = remaining
            .checked_sub(SIDECAR_LENGTH_SIZE)
            .map_or(0, |bytes| bytes / PAGE_RECORD_SIZE);
        if count > maximum_count {
            return Err(format!(
                "checkpoint declares {count} pages, but only {remaining} bytes remain"
            )
            .into());
        }

        let mut pages = Vec::with_capacity(count);
        let mut previous_gfn = None;
        for index in 0..count {
            let gfn = reader.u64()?;
            let length = usize::try_from(reader.u32()?)?;
            if length != PAGE_SIZE {
                return Err(format!(
                    "checkpoint page {index} has length {length}, expected {PAGE_SIZE}"
                )
                .into());
            }
            if previous_gfn.is_some_and(|previous| gfn <= previous) {
                return Err(format!(
                    "checkpoint page {index} GFN {gfn} is not strictly greater than the previous GFN"
                )
                .into());
            }
            previous_gfn = Some(gfn);
            pages.push((gfn, reader.take(length)?.to_vec()));
        }
        let sidecar_length = usize::try_from(reader.u64()?)?;
        let sidecar = reader.take(sidecar_length)?.to_vec();
        reader.finish()?;
        Ok(Self {
            setup,
            at,
            image_identity,
            pages,
            sidecar,
        })
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> Result<usize, Box<dyn Error>> {
        self.bytes
            .len()
            .checked_sub(self.at)
            .ok_or("checkpoint reader advanced past input".into())
    }

    fn finish(&self) -> Result<(), Box<dyn Error>> {
        let remaining = self.remaining()?;
        if remaining != 0 {
            return Err(format!("checkpoint has {remaining} trailing bytes").into());
        }
        Ok(())
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], Box<dyn Error>> {
        let end = self
            .at
            .checked_add(length)
            .ok_or("checkpoint length overflows")?;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or("checkpoint ends before its declared contents")?;
        self.at = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, Box<dyn Error>> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }

    fn u64(&mut self) -> Result<u64, Box<dyn Error>> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Checkpoint {
        Checkpoint {
            setup: 7,
            at: 4_000_000_000,
            image_identity: [9_u8; 32],
            pages: vec![(0, vec![1_u8; 4096]), (4096, vec![2_u8; 4096])],
            sidecar: b"device state".to_vec(),
        }
    }

    fn legacy_empty_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"HARMCKPT");
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&7_u64.to_le_bytes());
        bytes.extend_from_slice(&4_000_000_000_u64.to_le_bytes());
        bytes.extend_from_slice(&[9_u8; 32]);
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes
    }

    fn bytes_with_pages(gfns: &[u64]) -> Vec<u8> {
        let mut bytes = legacy_empty_bytes();
        bytes[60..68].copy_from_slice(
            &u64::try_from(gfns.len())
                .expect("test page count fits in a u64")
                .to_le_bytes(),
        );
        bytes.truncate(HEADER_SIZE);
        for gfn in gfns {
            bytes.extend_from_slice(&gfn.to_le_bytes());
            bytes.extend_from_slice(
                &u32::try_from(PAGE_SIZE)
                    .expect("test page length fits in a u32")
                    .to_le_bytes(),
            );
            bytes.extend(std::iter::repeat_n(0_u8, PAGE_SIZE));
        }
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes
    }

    fn populated_page(seed: u8, step: u8) -> Vec<u8> {
        (0..PAGE_SIZE)
            .map(|index| seed.wrapping_add((index as u8).wrapping_mul(step)))
            .collect()
    }

    fn legacy_populated_bytes() -> Vec<u8> {
        let first_page = populated_page(0x37, 0x13);
        let second_page = populated_page(0xa9, 0x27);
        let sidecar = b"opaque\0device\xffstate";

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"HARMCKPT");
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&0x0102_0304_0506_0708_u64.to_le_bytes());
        bytes.extend_from_slice(&0x1112_1314_1516_1718_u64.to_le_bytes());
        bytes.extend_from_slice(&[
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff, 0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0x01, 0x23, 0x45, 0x67,
            0x89, 0xab, 0xcd, 0xef,
        ]);
        bytes.extend_from_slice(&2_u64.to_le_bytes());
        for (gfn, page) in [(0x1234_u64, first_page), (0x1_0000_u64, second_page)] {
            bytes.extend_from_slice(&gfn.to_le_bytes());
            bytes.extend_from_slice(&4096_u32.to_le_bytes());
            bytes.extend_from_slice(&page);
        }
        bytes.extend_from_slice(
            &u64::try_from(sidecar.len())
                .expect("test sidecar length fits in a u64")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(sidecar);
        bytes
    }

    fn populated_checkpoint() -> Checkpoint {
        Checkpoint {
            setup: 0x0102_0304_0506_0708,
            at: 0x1112_1314_1516_1718,
            image_identity: [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff, 0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0xfe, 0x01, 0x23, 0x45, 0x67,
                0x89, 0xab, 0xcd, 0xef,
            ],
            pages: vec![
                (0x1234, populated_page(0x37, 0x13)),
                (0x1_0000, populated_page(0xa9, 0x27)),
            ],
            sidecar: b"opaque\0device\xffstate".to_vec(),
        }
    }

    #[test]
    fn a_checkpoint_round_trips() {
        let checkpoint = sample();
        let bytes = checkpoint.encode().expect("encode");
        let decoded = Checkpoint::decode(&bytes).expect("decode");
        assert_eq!(decoded, checkpoint);
    }

    #[test]
    fn an_empty_checkpoint_round_trips() {
        let checkpoint = Checkpoint {
            pages: Vec::new(),
            sidecar: vec![0_u8, 0xff, 1],
            ..sample()
        };
        let bytes = checkpoint.encode().expect("encode");
        assert_eq!(Checkpoint::decode(&bytes).expect("decode"), checkpoint);
    }

    #[test]
    fn the_known_v1_empty_blob_remains_byte_compatible() {
        let bytes = legacy_empty_bytes();
        assert_eq!(bytes.len(), HEADER_SIZE + SIDECAR_LENGTH_SIZE);
        let expected = Checkpoint {
            setup: 7,
            at: 4_000_000_000,
            image_identity: [9_u8; 32],
            pages: Vec::new(),
            sidecar: Vec::new(),
        };
        assert_eq!(Checkpoint::decode(&bytes).expect("legacy decode"), expected);
        assert_eq!(expected.encode().expect("legacy encode"), bytes);
    }

    #[test]
    fn the_known_v1_populated_blob_remains_byte_compatible() {
        let bytes = legacy_populated_bytes();
        let expected = populated_checkpoint();
        assert_eq!(Checkpoint::decode(&bytes).expect("legacy decode"), expected);
        assert_eq!(expected.encode().expect("legacy encode"), bytes);
    }

    #[test]
    fn a_blob_that_is_not_a_checkpoint_is_named_rather_than_misread() {
        let error = Checkpoint::decode(b"{\"pages\":[]}").expect_err("not a checkpoint");
        assert!(
            error.to_string().contains("not a Harmony checkpoint"),
            "{error}"
        );
    }

    #[test]
    fn another_version_is_refused_with_both_numbers() {
        let mut bytes = sample().encode().expect("encode");
        bytes[8] = 99;
        let error = Checkpoint::decode(&bytes).expect_err("another version");
        assert!(error.to_string().contains("99"), "{error}");
        assert!(error.to_string().contains("reads 1"), "{error}");
    }

    #[test]
    fn a_truncated_checkpoint_is_an_error_rather_than_a_panic() {
        let bytes = sample().encode().expect("encode");
        for cut in [0, 1, 8, 20, 40, 60, 68, 70, bytes.len() - 1] {
            assert!(Checkpoint::decode(&bytes[..cut]).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = sample().encode().expect("encode");
        bytes.push(0xa5);
        let error = Checkpoint::decode(&bytes).expect_err("trailing bytes");
        assert!(error.to_string().contains("trailing"), "{error}");
    }

    #[test]
    fn an_impossible_page_count_is_rejected_before_allocation() {
        let mut bytes = legacy_empty_bytes();
        bytes[60..68].copy_from_slice(&u64::MAX.to_le_bytes());
        let error = Checkpoint::decode(&bytes).expect_err("impossible page count");
        assert!(error.to_string().contains("declares"), "{error}");
    }

    #[test]
    fn corrupt_lengths_are_rejected() {
        let mut page_bytes = bytes_with_pages(&[0]);
        let length_at = HEADER_SIZE + 8;
        page_bytes[length_at..length_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let error = Checkpoint::decode(&page_bytes).expect_err("page length");
        assert!(error.to_string().contains("length"), "{error}");

        let mut sidecar_bytes = legacy_empty_bytes();
        sidecar_bytes[HEADER_SIZE..].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Checkpoint::decode(&sidecar_bytes).is_err());
    }

    #[test]
    fn duplicate_and_out_of_order_gfns_are_rejected() {
        for gfns in [[7_u64, 7_u64], [8_u64, 4_u64]] {
            let bytes = bytes_with_pages(&gfns);
            let error = Checkpoint::decode(&bytes).expect_err("invalid GFN order");
            assert!(error.to_string().contains("strictly greater"), "{error}");
        }
    }

    #[test]
    fn encoding_enforces_page_shape_and_order() {
        let mut wrong_length = sample();
        wrong_length.pages[0].1.pop();
        assert!(wrong_length.encode().is_err());

        let mut duplicate = sample();
        duplicate.pages[1].0 = duplicate.pages[0].0;
        assert!(duplicate.encode().is_err());

        let mut out_of_order = sample();
        out_of_order.pages[0].0 = 8;
        out_of_order.pages[1].0 = 4;
        assert!(out_of_order.encode().is_err());
    }
}
