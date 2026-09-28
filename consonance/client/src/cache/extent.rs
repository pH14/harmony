// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use super::CacheError;
use super::segments::PAGE;

const MAGIC: [u8; 8] = *b"HMCACHE1";
const HEADER: usize = 8 + 3 * 8 + 32;
const ROW: usize = 8 + 32;

pub type HashedPage<'a> = (u64, &'a [u8; 32], &'a [u8; PAGE]);

#[derive(Debug)]
pub struct Delta<'a> {
    pub pages: Vec<HashedPage<'a>>,
    pub reverted: Vec<u64>,
    pub sidecar: &'a [u8],
}

fn page_offset(pages: usize, reverted: usize, sidecar: usize) -> Option<usize> {
    HEADER
        .checked_add(pages.checked_mul(ROW)?)?
        .checked_add(reverted.checked_mul(8)?)?
        .checked_add(sidecar)?
        .checked_next_multiple_of(PAGE)
}

#[must_use]
pub fn extent_len(pages: usize, reverted: usize, sidecar: usize) -> Option<usize> {
    page_offset(pages, reverted, sidecar)?.checked_add(pages.checked_mul(PAGE)?)
}

fn checksum(counts: &[u8], body: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(MAGIC);
    digest.update(counts);
    digest.update(body);
    digest.finalize().into()
}

pub fn write_extent(
    out: &mut [u8],
    pages: &[HashedPage<'_>],
    reverted: &[u64],
    sidecar: &[u8],
) -> Result<(), CacheError> {
    let data = page_offset(pages.len(), reverted.len(), sidecar.len())
        .ok_or(CacheError::Malformed("extent size overflow"))?;
    let len = extent_len(pages.len(), reverted.len(), sidecar.len())
        .ok_or(CacheError::Malformed("extent size overflow"))?;
    if out.len() < len {
        return Err(CacheError::Malformed("extent is smaller than its delta"));
    }
    let mut counts = [0u8; 24];
    counts[..8].copy_from_slice(&(pages.len() as u64).to_le_bytes());
    counts[8..16].copy_from_slice(&(reverted.len() as u64).to_le_bytes());
    counts[16..].copy_from_slice(&(sidecar.len() as u64).to_le_bytes());
    let mut at = HEADER;
    for &(gfn, hash, _) in pages {
        out[at..at + 8].copy_from_slice(&gfn.to_le_bytes());
        out[at + 8..at + ROW].copy_from_slice(hash);
        at += ROW;
    }
    for gfn in reverted {
        out[at..at + 8].copy_from_slice(&gfn.to_le_bytes());
        at += 8;
    }
    out[at..at + sidecar.len()].copy_from_slice(sidecar);
    at += sidecar.len();
    let sum = checksum(&counts, &out[HEADER..at]);
    out[..8].copy_from_slice(&MAGIC);
    out[8..32].copy_from_slice(&counts);
    out[32..HEADER].copy_from_slice(&sum);
    for (index, &(_, _, page)) in pages.iter().enumerate() {
        let start = data + index * PAGE;
        out[start..start + PAGE].copy_from_slice(page);
    }
    Ok(())
}

fn count(bytes: &[u8]) -> Result<usize, CacheError> {
    let value = u64::from_le_bytes(bytes.try_into().expect("eight bytes"));
    usize::try_from(value).map_err(|_| CacheError::Malformed("extent count overflow"))
}

fn gfn_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

fn strictly_sorted(gfns: impl Iterator<Item = u64>) -> bool {
    let mut previous = None;
    for gfn in gfns {
        if previous.is_some_and(|previous| previous >= gfn) {
            return false;
        }
        previous = Some(gfn);
    }
    true
}

pub fn read_extent(bytes: &[u8]) -> Result<Delta<'_>, CacheError> {
    if bytes.len() < HEADER || bytes[..8] != MAGIC {
        return Err(CacheError::Malformed("extent header"));
    }
    let pages = count(&bytes[8..16])?;
    let reverted = count(&bytes[16..24])?;
    let sidecar = count(&bytes[24..32])?;
    let data = page_offset(pages, reverted, sidecar)
        .ok_or(CacheError::Malformed("extent size overflow"))?;
    let len = extent_len(pages, reverted, sidecar)
        .ok_or(CacheError::Malformed("extent size overflow"))?;
    if bytes.len() < len {
        return Err(CacheError::Malformed("extent is truncated"));
    }
    let rows_end = HEADER + pages * ROW;
    let reverted_end = rows_end + reverted * 8;
    let body_end = reverted_end + sidecar;
    if checksum(&bytes[8..32], &bytes[HEADER..body_end]) != bytes[32..HEADER] {
        return Err(CacheError::Malformed("extent checksum"));
    }
    let mut delta = Delta {
        pages: Vec::with_capacity(pages),
        reverted: (0..reverted)
            .map(|index| gfn_at(bytes, rows_end + index * 8))
            .collect(),
        sidecar: &bytes[reverted_end..body_end],
    };
    for index in 0..pages {
        let row = HEADER + index * ROW;
        let start = data + index * PAGE;
        delta.pages.push((
            gfn_at(bytes, row),
            bytes[row + 8..row + ROW].try_into().expect("hash row"),
            bytes[start..start + PAGE].try_into().expect("page"),
        ));
    }
    if !strictly_sorted(delta.pages.iter().map(|page| page.0))
        || !strictly_sorted(delta.reverted.iter().copied())
    {
        return Err(CacheError::Malformed("extent page numbers are not sorted"));
    }
    Ok(delta)
}

#[derive(Debug)]
pub struct Resolved<'a> {
    pub pages: Vec<HashedPage<'a>>,
    pub sidecar: &'a [u8],
}

pub fn resolve<'a>(chain: &[Delta<'a>]) -> Result<Resolved<'a>, CacheError> {
    let (anchor, deltas) = chain
        .split_first()
        .ok_or(CacheError::Malformed("empty extent chain"))?;
    if !anchor.reverted.is_empty() {
        return Err(CacheError::Malformed("an anchor reverts pages"));
    }
    let mut pages: BTreeMap<u64, HashedPage<'a>> =
        anchor.pages.iter().map(|&page| (page.0, page)).collect();
    for delta in deltas {
        for gfn in &delta.reverted {
            pages.remove(gfn);
        }
        for &page in &delta.pages {
            pages.insert(page.0, page);
        }
    }
    let sidecar = chain.last().map_or(anchor.sidecar, |delta| delta.sidecar);
    Ok(Resolved {
        pages: pages.into_values().collect(),
        sidecar,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(fill: u8) -> ([u8; 32], [u8; PAGE]) {
        ([fill; 32], [fill; PAGE])
    }

    fn encode(pages: &[(u64, u8)], reverted: &[u64], sidecar: &[u8]) -> Vec<u8> {
        let owned: Vec<_> = pages.iter().map(|&(gfn, fill)| (gfn, page(fill))).collect();
        let rows: Vec<HashedPage<'_>> = owned
            .iter()
            .map(|(gfn, (hash, data))| (*gfn, hash, data))
            .collect();
        let mut out = vec![0u8; extent_len(rows.len(), reverted.len(), sidecar.len()).unwrap()];
        write_extent(&mut out, &rows, reverted, sidecar).unwrap();
        out
    }

    fn summary(pages: &[HashedPage<'_>]) -> Vec<(u64, u8)> {
        pages
            .iter()
            .map(|&(gfn, hash, data)| {
                assert_eq!(hash[0], data[0]);
                (gfn, data[0])
            })
            .collect()
    }

    #[test]
    fn an_extent_round_trips() {
        let bytes = encode(&[(1, 7), (4, 9)], &[2, 3], b"sidecar");
        assert_eq!(bytes.len() % PAGE, 0);
        let delta = read_extent(&bytes).unwrap();
        assert_eq!(summary(&delta.pages), vec![(1, 7), (4, 9)]);
        assert_eq!(delta.reverted, vec![2, 3]);
        assert_eq!(delta.sidecar, b"sidecar");
    }

    #[test]
    fn a_changed_table_or_a_wrong_offset_is_rejected() {
        let bytes = encode(&[(1, 7), (4, 9)], &[2], b"s");
        let mut moved = bytes.clone();
        moved[HEADER] ^= 1;
        assert!(matches!(
            read_extent(&moved),
            Err(CacheError::Malformed("extent checksum"))
        ));
        let mut shifted = vec![0u8; 8];
        shifted.extend_from_slice(&bytes);
        assert!(read_extent(&shifted).is_err());
        assert!(read_extent(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn resolution_applies_reverts_and_changes_in_order() {
        let anchor = encode(&[(0, 1), (1, 2), (2, 3)], &[], b"a");
        let first = encode(&[(1, 4), (5, 5)], &[2], b"b");
        let second = encode(&[(2, 6)], &[0, 5], b"c");
        let chain = [
            read_extent(&anchor).unwrap(),
            read_extent(&first).unwrap(),
            read_extent(&second).unwrap(),
        ];
        let resolved = resolve(&chain).unwrap();
        assert_eq!(summary(&resolved.pages), vec![(1, 4), (2, 6)]);
        assert_eq!(resolved.sidecar, b"c");
        let resolved = resolve(&chain[..2]).unwrap();
        assert_eq!(summary(&resolved.pages), vec![(0, 1), (1, 4), (5, 5)]);
        assert!(resolve(&chain[1..]).is_err(), "a chain starts at an anchor");
        assert!(resolve(&[]).is_err());
    }
}
