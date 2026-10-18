// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::cache::{CommittedExtent, segments::BackingWitness};
use crate::import_reuse::REFERENCE_LIMIT;
use std::io;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ImportRange {
    pub(crate) slot: usize,
    pub(crate) offset: usize,
    pub(crate) len: usize,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ImportLayout {
    pub(crate) backing_rows: Vec<usize>,
    pub(crate) ranges: Vec<ImportRange>,
}

fn malformed() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "import descriptor layout")
}

impl ImportLayout {
    pub(crate) fn from_extents(extents: &[CommittedExtent]) -> io::Result<Self> {
        if extents.is_empty() || extents.len() > REFERENCE_LIMIT {
            return Err(malformed());
        }
        let mut owners: Vec<BackingWitness> = Vec::new();
        let mut backing_rows = Vec::new();
        let mut ranges = Vec::with_capacity(extents.len());
        for (row, extent) in extents.iter().enumerate() {
            let owner = extent.backing_witness();
            let slot = match owners.iter().position(|prior| prior.same_backing(&owner)) {
                Some(slot) => slot,
                None => {
                    let slot = owners.len();
                    owners.push(owner);
                    backing_rows.push(row);
                    slot
                }
            };
            let (_, offset, len) = extent.span();
            ranges.push(ImportRange { slot, offset, len });
        }
        Ok(Self {
            backing_rows,
            ranges,
        })
    }

    pub(crate) fn encode_into(&self, near: u64, out: &mut Vec<u8>) {
        for value in [
            near,
            self.backing_rows.len() as u64,
            self.ranges.len() as u64,
        ] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        for row in &self.ranges {
            for value in [row.slot, row.offset, row.len] {
                out.extend_from_slice(&(value as u64).to_le_bytes());
            }
        }
    }

    pub(crate) fn decode(mut bytes: &[u8], descriptors: usize) -> io::Result<(u64, Self)> {
        fn number(bytes: &mut &[u8]) -> io::Result<u64> {
            let (head, tail) = bytes.split_at_checked(8).ok_or_else(malformed)?;
            *bytes = tail;
            Ok(u64::from_le_bytes(
                head.try_into().map_err(|_| malformed())?,
            ))
        }
        fn count(bytes: &mut &[u8]) -> io::Result<usize> {
            usize::try_from(number(bytes)?).map_err(|_| malformed())
        }
        let near = number(&mut bytes)?;
        let unique = count(&mut bytes)?;
        let rows = count(&mut bytes)?;
        if rows == 0
            || rows > REFERENCE_LIMIT
            || unique == 0
            || unique > rows
            || unique != descriptors
        {
            return Err(malformed());
        }
        let mut backing_rows = Vec::with_capacity(unique);
        let mut ranges = Vec::with_capacity(rows);
        for row in 0..rows {
            let slot = count(&mut bytes)?;
            let offset = count(&mut bytes)?;
            let len = count(&mut bytes)?;
            if slot >= unique
                || slot > backing_rows.len()
                || len == 0
                || offset.checked_add(len).is_none()
            {
                return Err(malformed());
            }
            if slot == backing_rows.len() {
                backing_rows.push(row);
            }
            ranges.push(ImportRange { slot, offset, len });
        }
        if !bytes.is_empty() || backing_rows.len() != unique {
            return Err(malformed());
        }
        Ok((
            near,
            Self {
                backing_rows,
                ranges,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::PageSegments;

    #[test]
    fn repeated_allocations_group_by_exact_live_owner_and_keep_row_order() {
        let mut first = PageSegments::new(4 * 4096, usize::MAX);
        let a = first.allocate(4096).unwrap().commit();
        let b = first.allocate(4096).unwrap().commit();
        let mut second = PageSegments::new(4096, usize::MAX);
        let c = second.allocate(4096).unwrap().commit();
        assert_eq!(a.span().0, c.span().0);
        assert_eq!(a.bytes(), c.bytes());
        let layout = ImportLayout::from_extents(&[a.clone(), b.clone(), c, a, b]).unwrap();
        assert_eq!(layout.backing_rows, [0, 2]);
        assert_eq!(
            layout.ranges.iter().map(|row| row.slot).collect::<Vec<_>>(),
            [0, 0, 1, 0, 0]
        );
        assert_eq!(
            layout
                .ranges
                .iter()
                .map(|row| row.offset)
                .collect::<Vec<_>>(),
            [0, 4096, 0, 0, 4096]
        );
        let mut bytes = Vec::new();
        layout.encode_into(9, &mut bytes);
        assert_eq!(ImportLayout::decode(&bytes, 2).unwrap(), (9, layout));
    }

    #[test]
    fn unique_backings_preserve_descriptors_and_expose_metadata_overhead() {
        let mut first = PageSegments::new(4096, usize::MAX);
        let a = first.allocate(4096).unwrap().commit();
        let b = first.allocate(4096).unwrap().commit();
        let layout = ImportLayout::from_extents(&[a, b]).unwrap();
        assert_eq!(layout.backing_rows, [0, 1]);
        let mut bytes = Vec::new();
        layout.encode_into(1, &mut bytes);
        assert_eq!(bytes.len(), 24 + 24 * 2);
        assert_eq!(bytes.len() - (16 + 16 * 2), 8 + 8 * 2);
        assert_eq!(ImportLayout::decode(&bytes, 2).unwrap(), (1, layout));
    }

    fn encoded(unique: u64, rows: &[[u64; 3]]) -> Vec<u8> {
        [1, unique, rows.len() as u64]
            .into_iter()
            .chain(rows.iter().flatten().copied())
            .flat_map(u64::to_le_bytes)
            .collect()
    }

    #[test]
    fn layout_rejects_bad_counts_slots_ranges_and_noncanonical_tables() {
        let valid = encoded(2, &[[0, 0, 4096], [1, 4096, 4096], [0, 8192, 4096]]);
        assert!(ImportLayout::decode(&valid, 2).is_ok());
        for descriptors in [0, 1, 3, REFERENCE_LIMIT + 1] {
            assert!(ImportLayout::decode(&valid, descriptors).is_err());
        }
        for (unique, rows) in [
            (0, vec![[0, 0, 1]]),
            (2, vec![[0, 0, 1]]),
            (1, vec![[1, 0, 1]]),
            (2, vec![[1, 0, 1], [0, 0, 1]]),
            (2, vec![[1, 0, 1], [0, 0, 1], [1, 1, 1]]),
            (2, vec![[0, 0, 1], [0, 1, 1]]),
            (1, vec![[0, 0, 0]]),
            (1, vec![[0, u64::MAX, 1]]),
            (1, vec![[0, 0, 1]; REFERENCE_LIMIT + 1]),
            (0, vec![]),
        ] {
            assert!(ImportLayout::decode(&encoded(unique, &rows), unique as usize).is_err());
        }
        for len in 0..valid.len() {
            assert!(ImportLayout::decode(&valid[..len], 2).is_err());
        }
        let mut trailing = valid;
        trailing.push(0);
        assert!(ImportLayout::decode(&trailing, 2).is_err());
    }

    #[test]
    fn grouping_preserves_reference_limit_even_with_one_backing() {
        let mut segments = PageSegments::new(4096, usize::MAX);
        let a = segments.allocate(4096).unwrap().commit();
        assert!(ImportLayout::from_extents(&[]).is_err());
        assert!(ImportLayout::from_extents(&vec![a.clone(); REFERENCE_LIMIT + 1]).is_err());
        let layout = ImportLayout::from_extents(&vec![a; REFERENCE_LIMIT]).unwrap();
        assert_eq!(layout.backing_rows.len(), 1);
        assert_eq!(layout.ranges.len(), REFERENCE_LIMIT);
    }
}
