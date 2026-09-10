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
//! | each page: guest frame number, byte length, bytes | 8 + 4 + n |
//! | sidecar length, bytes | 8 + n |

use std::error::Error;

/// The bytes every checkpoint blob starts with.
pub const MAGIC: &[u8; 8] = b"HARMCKPT";
/// The version this build writes and reads.
pub const VERSION: u32 = 1;

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
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes =
            Vec::with_capacity(self.pages.len().saturating_mul(4_112).saturating_add(68));
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&self.setup.to_le_bytes());
        bytes.extend_from_slice(&self.at.to_le_bytes());
        bytes.extend_from_slice(&self.image_identity);
        bytes.extend_from_slice(&(self.pages.len() as u64).to_le_bytes());
        for (gfn, page) in &self.pages {
            bytes.extend_from_slice(&gfn.to_le_bytes());
            bytes.extend_from_slice(&(page.len() as u32).to_le_bytes());
            bytes.extend_from_slice(page);
        }
        bytes.extend_from_slice(&(self.sidecar.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&self.sidecar);
        bytes
    }

    /// Decode a checkpoint blob.
    ///
    /// # Errors
    ///
    /// Returns an error when the bytes are not a checkpoint, name another
    /// version, or end early.
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
        let mut pages = Vec::with_capacity(count.min(1 << 20));
        for _ in 0..count {
            let gfn = reader.u64()?;
            let length = usize::try_from(reader.u32()?)?;
            pages.push((gfn, reader.take(length)?.to_vec()));
        }
        let sidecar_length = usize::try_from(reader.u64()?)?;
        let sidecar = reader.take(sidecar_length)?.to_vec();
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

    #[test]
    fn a_checkpoint_round_trips() {
        let checkpoint = sample();
        let decoded = Checkpoint::decode(&checkpoint.encode()).expect("decode");
        assert_eq!(decoded, checkpoint);
    }

    #[test]
    fn an_empty_checkpoint_round_trips() {
        let checkpoint = Checkpoint {
            pages: Vec::new(),
            sidecar: Vec::new(),
            ..sample()
        };
        assert_eq!(
            Checkpoint::decode(&checkpoint.encode()).expect("decode"),
            checkpoint
        );
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
        let mut bytes = sample().encode();
        bytes[8] = 99;
        let error = Checkpoint::decode(&bytes).expect_err("another version");
        assert!(error.to_string().contains("99"), "{error}");
        assert!(error.to_string().contains("reads 1"), "{error}");
    }

    #[test]
    fn a_truncated_checkpoint_is_an_error_rather_than_a_panic() {
        let bytes = sample().encode();
        for cut in [8, 20, 40, 70, bytes.len() - 1] {
            assert!(Checkpoint::decode(&bytes[..cut]).is_err(), "cut at {cut}");
        }
    }
}
