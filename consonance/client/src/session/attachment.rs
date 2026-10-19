// SPDX-License-Identifier: AGPL-3.0-or-later

use control_proto::SnapId;
use sha2::{Digest, Sha256};
use std::error::Error;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SnapshotAttachment {
    pub domain: Vec<u8>,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachedSnapshotReceipt {
    pub snapshot: SnapId,
    pub moment: u64,
    pub machine_commitment: [u8; 32],
    pub attachment: SnapshotAttachment,
    pub object_commitment: [u8; 32],
}

const MAGIC: &[u8; 8] = b"HMSESS02";

pub(super) struct Envelope<'a> {
    pub machine_commitment: [u8; 32],
    pub raw: &'a [u8],
    pub attachment: SnapshotAttachment,
}

pub(super) fn encode(
    raw: &[u8],
    machine: [u8; 32],
    attachment: &SnapshotAttachment,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let len = 64usize
        .checked_add(raw.len())
        .and_then(|n| n.checked_add(attachment.domain.len()))
        .and_then(|n| n.checked_add(attachment.bytes.len()))
        .ok_or("snapshot attachment size overflows")?;
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&machine);
    for n in [raw.len(), attachment.domain.len(), attachment.bytes.len()] {
        out.extend_from_slice(&u64::try_from(n)?.to_le_bytes());
    }
    out.extend_from_slice(raw);
    out.extend_from_slice(&attachment.domain);
    out.extend_from_slice(&attachment.bytes);
    Ok(out)
}

pub(super) fn decode(bytes: &[u8]) -> Result<Envelope<'_>, Box<dyn Error>> {
    if bytes.len() < 64 || &bytes[..8] != MAGIC {
        return Err("cached attachment envelope differs".into());
    }
    let machine_commitment = bytes[8..40].try_into()?;
    let mut at = 64usize;
    let mut fields = Vec::with_capacity(3);
    for offset in [40, 48, 56] {
        let len = usize::try_from(u64::from_le_bytes(bytes[offset..offset + 8].try_into()?))?;
        let end = at
            .checked_add(len)
            .ok_or("snapshot attachment size overflows")?;
        fields.push(bytes.get(at..end).ok_or("truncated snapshot attachment")?);
        at = end;
    }
    if at != bytes.len() {
        return Err("trailing snapshot attachment bytes".into());
    }
    Ok(Envelope {
        machine_commitment,
        raw: fields[0],
        attachment: SnapshotAttachment {
            domain: fields[1].to_vec(),
            bytes: fields[2].to_vec(),
        },
    })
}

pub(super) fn commitment(machine: [u8; 32], attachment: &SnapshotAttachment) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"consonance-cache-object-v2\0");
    h.update(machine);
    for field in [&attachment.domain, &attachment.bytes] {
        h.update((field.len() as u64).to_le_bytes());
        h.update(field);
    }
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attachment_encoding_binds_domain_and_bytes_and_rejects_malformed_lengths() {
        let a = SnapshotAttachment {
            domain: b"test-v1".to_vec(),
            bytes: vec![1, 2, 3],
        };
        let bytes = encode(b"machine", [7; 32], &a).unwrap();
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.raw, b"machine");
        assert_eq!(decoded.attachment, a);
        let mut other = a.clone();
        other.bytes[0] ^= 1;
        assert_ne!(commitment([7; 32], &a), commitment([7; 32], &other));
        other = a.clone();
        other.domain.push(0);
        assert_ne!(commitment([7; 32], &a), commitment([7; 32], &other));
        for n in 0..bytes.len() {
            assert!(decode(&bytes[..n]).is_err());
        }
        let mut bad = bytes.clone();
        bad[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode(&bad).is_err());
        let mut bad = bytes.clone();
        bad.push(0);
        assert!(decode(&bad).is_err());
        let mut bad = bytes;
        bad[..8].copy_from_slice(b"HMSESS01");
        assert!(decode(&bad).is_err());
    }
}
