// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;
use std::num::NonZeroUsize;

use sha2::{Digest, Sha256};

use super::CacheError;
use super::segments::PAGE;

const MAGIC: [u8; 8] = *b"HMCACHE2";
const HEADER: usize = 8 + 4 * 8 + 32;
const ROW: usize = 8 + 32 + 4;
const PARALLEL_PAGES: usize = 1024;

#[cfg(all(test, feature = "in-process", not(miri)))]
thread_local! {
    static WRITES: std::cell::RefCell<Option<Vec<(usize, usize, usize)>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, feature = "in-process", not(miri)))]
pub(crate) fn trace_writes<T>(run: impl FnOnce() -> T) -> (T, Vec<(usize, usize, usize)>) {
    struct Reset(Option<Vec<(usize, usize, usize)>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            WRITES.with(|writes| *writes.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(WRITES.with(|writes| writes.replace(Some(Vec::new()))));
    let result = run();
    let rows = WRITES.with(|writes| writes.borrow_mut().take().unwrap());
    (result, rows)
}

pub type HashedPage<'a> = (u64, &'a [u8; 32], &'a [u8; PAGE]);

pub type StoredPage<'a> = (u64, &'a [u8; 32], &'a [u8]);

#[derive(Debug)]
pub struct Delta<'a> {
    pub pages: Vec<StoredPage<'a>>,
    pub reverted: Vec<u64>,
    pub sidecar: &'a [u8],
}

impl Delta<'_> {
    #[must_use]
    pub fn with_sidecar<'b>(&'b self, sidecar: &'b [u8]) -> Encoded<'b> {
        let mut data = Vec::with_capacity(self.pages.iter().map(|page| page.2.len()).sum());
        let pages = self
            .pages
            .iter()
            .map(|&(gfn, hash, stored)| {
                data.extend_from_slice(stored);
                (gfn, hash, stored.len() as u32)
            })
            .collect();
        Encoded {
            pages,
            data,
            reverted: &self.reverted,
            sidecar,
        }
    }
}

fn checksum(counts: &[u8], body: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(MAGIC);
    digest.update(counts);
    digest.update(body);
    digest.finalize().into()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExtentWrites {
    pub encoded_bytes: u64,
    pub metadata_copy_bytes: u64,
    pub page_copy_bytes: u64,
}
impl ExtentWrites {
    pub fn merge(&mut self, other: Self) {
        self.encoded_bytes = self.encoded_bytes.saturating_add(other.encoded_bytes);
        self.metadata_copy_bytes = self
            .metadata_copy_bytes
            .saturating_add(other.metadata_copy_bytes);
        self.page_copy_bytes = self.page_copy_bytes.saturating_add(other.page_copy_bytes);
    }
    pub fn counters(self) -> Vec<(String, u64)> {
        [
            ("encoded_bytes", self.encoded_bytes),
            ("metadata_copy_bytes", self.metadata_copy_bytes),
            ("page_copy_bytes", self.page_copy_bytes),
        ]
        .into_iter()
        .map(|(key, value)| (format!("copy.session.extent.{key}"), value))
        .collect()
    }
}

fn helper_threads(pages: usize) -> usize {
    if pages < PARALLEL_PAGES {
        return 1;
    }
    std::thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(pages / PARALLEL_PAGES)
        .max(1)
}

fn on_threads<I: Sync, O: Send>(items: &[I], work: impl Fn(&[I]) -> O + Sync) -> Vec<O> {
    let threads = helper_threads(items.len());
    if threads < 2 {
        return vec![work(items)];
    }
    let chunk = items.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let mut parts = items.chunks(chunk);
        let first = parts.next().unwrap_or_default();
        let work = &work;
        let workers: Vec<_> = parts
            .map(|part| {
                (
                    part,
                    std::thread::Builder::new().spawn_scoped(scope, move || work(part)),
                )
            })
            .collect();
        let mut out = vec![work(first)];
        for (part, worker) in workers {
            out.push(match worker {
                Ok(handle) => match handle.join() {
                    Ok(value) => value,
                    Err(panic) => std::panic::resume_unwind(panic),
                },
                Err(_) => work(part),
            });
        }
        out
    })
}

fn pack(pages: &[HashedPage<'_>]) -> (Vec<u32>, Vec<u8>) {
    let mut lens = Vec::with_capacity(pages.len());
    let mut data = Vec::new();
    let mut scratch = vec![0u8; lz4_flex::block::get_maximum_output_size(PAGE)];
    for &(_, _, page) in pages {
        match lz4_flex::block::compress_into(page, &mut scratch) {
            Ok(len) if len < PAGE => {
                data.extend_from_slice(&scratch[..len]);
                lens.push(len as u32);
            }
            _ => {
                data.extend_from_slice(page);
                lens.push(PAGE as u32);
            }
        }
    }
    (lens, data)
}

#[derive(Debug)]
pub struct Encoded<'a> {
    pages: Vec<(u64, &'a [u8; 32], u32)>,
    data: Vec<u8>,
    reverted: &'a [u64],
    sidecar: &'a [u8],
}

pub fn encode<'a>(
    pages: &[HashedPage<'a>],
    reverted: &'a [u64],
    sidecar: &'a [u8],
) -> Result<Encoded<'a>, CacheError> {
    let mut rows = Vec::with_capacity(pages.len());
    let mut data = Vec::new();
    let mut at = 0;
    for (lens, packed) in on_threads(pages, pack) {
        for len in lens {
            let &(gfn, hash, _) = &pages[at];
            rows.push((gfn, hash, len));
            at += 1;
        }
        data.extend_from_slice(&packed);
    }
    let encoded = Encoded {
        pages: rows,
        data,
        reverted,
        sidecar,
    };
    encoded
        .body_len()
        .and_then(|body| body.checked_add(encoded.data.len()))
        .ok_or(CacheError::Malformed("extent size overflow"))?;
    Ok(encoded)
}

impl Encoded<'_> {
    fn body_len(&self) -> Option<usize> {
        HEADER
            .checked_add(self.pages.len().checked_mul(ROW)?)?
            .checked_add(self.reverted.len().checked_mul(8)?)?
            .checked_add(self.sidecar.len())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.body_len().expect("checked at encoding") + self.data.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn write(&self, out: &mut [u8]) -> Result<(), CacheError> {
        self.write_measured(out, &mut ExtentWrites::default())
    }

    pub fn write_measured(
        &self,
        out: &mut [u8],
        work: &mut ExtentWrites,
    ) -> Result<(), CacheError> {
        let len = self.len();
        if out.len() < len {
            return Err(CacheError::Malformed("extent is smaller than its delta"));
        }
        #[cfg(all(test, feature = "in-process", not(miri)))]
        WRITES.with(|writes| {
            if let Some(rows) = writes.borrow_mut().as_mut() {
                rows.push((out.as_ptr() as usize, len, self.data.len()));
            }
        });
        let mut counts = [0u8; 32];
        counts[..8].copy_from_slice(&(self.pages.len() as u64).to_le_bytes());
        counts[8..16].copy_from_slice(&(self.reverted.len() as u64).to_le_bytes());
        counts[16..24].copy_from_slice(&(self.sidecar.len() as u64).to_le_bytes());
        counts[24..].copy_from_slice(&(self.data.len() as u64).to_le_bytes());
        let mut at = HEADER;
        for &(gfn, hash, stored) in &self.pages {
            out[at..at + 8].copy_from_slice(&gfn.to_le_bytes());
            out[at + 8..at + 40].copy_from_slice(hash);
            out[at + 40..at + ROW].copy_from_slice(&stored.to_le_bytes());
            at += ROW;
        }
        for gfn in self.reverted {
            out[at..at + 8].copy_from_slice(&gfn.to_le_bytes());
            at += 8;
        }
        out[at..at + self.sidecar.len()].copy_from_slice(self.sidecar);
        at += self.sidecar.len();
        let sum = checksum(&counts, &out[HEADER..at]);
        out[..8].copy_from_slice(&MAGIC);
        out[8..40].copy_from_slice(&counts);
        out[40..HEADER].copy_from_slice(&sum);
        work.metadata_copy_bytes = work.metadata_copy_bytes.saturating_add(at as u64);
        out[at..len].copy_from_slice(&self.data);
        work.page_copy_bytes = work.page_copy_bytes.saturating_add(self.data.len() as u64);
        work.encoded_bytes = work.encoded_bytes.saturating_add(len as u64);
        Ok(())
    }
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
    let data = count(&bytes[32..40])?;
    let rows_end = pages
        .checked_mul(ROW)
        .and_then(|rows| rows.checked_add(HEADER))
        .ok_or(CacheError::Malformed("extent size overflow"))?;
    let reverted_end = reverted
        .checked_mul(8)
        .and_then(|rows| rows.checked_add(rows_end))
        .ok_or(CacheError::Malformed("extent size overflow"))?;
    let body_end = reverted_end
        .checked_add(sidecar)
        .ok_or(CacheError::Malformed("extent size overflow"))?;
    let len = body_end
        .checked_add(data)
        .ok_or(CacheError::Malformed("extent size overflow"))?;
    if bytes.len() < len {
        return Err(CacheError::Malformed("extent is truncated"));
    }
    if checksum(&bytes[8..40], &bytes[HEADER..body_end]) != bytes[40..HEADER] {
        return Err(CacheError::Malformed("extent checksum"));
    }
    let mut delta = Delta {
        pages: Vec::with_capacity(pages),
        reverted: (0..reverted)
            .map(|index| gfn_at(bytes, rows_end + index * 8))
            .collect(),
        sidecar: &bytes[reverted_end..body_end],
    };
    let mut start = body_end;
    for index in 0..pages {
        let row = HEADER + index * ROW;
        let stored = u32::from_le_bytes(bytes[row + 40..row + ROW].try_into().expect("length"));
        let stored = stored as usize;
        if stored == 0 || stored > PAGE || stored > len - start {
            return Err(CacheError::Malformed("extent page length"));
        }
        delta.pages.push((
            gfn_at(bytes, row),
            bytes[row + 8..row + 40].try_into().expect("hash row"),
            &bytes[start..start + stored],
        ));
        start += stored;
    }
    if start != len {
        return Err(CacheError::Malformed("extent page lengths"));
    }
    if !strictly_sorted(delta.pages.iter().map(|page| page.0))
        || !strictly_sorted(delta.reverted.iter().copied())
    {
        return Err(CacheError::Malformed("extent page numbers are not sorted"));
    }
    Ok(delta)
}

fn unpack(stored: &[u8], out: &mut [u8]) -> Result<(), CacheError> {
    if stored.len() == PAGE {
        out.copy_from_slice(stored);
        return Ok(());
    }
    match lz4_flex::block::decompress_into(stored, out) {
        Ok(PAGE) => Ok(()),
        _ => Err(CacheError::Malformed("extent page encoding")),
    }
}

fn unpack_all(part: &mut [(&[u8], &mut [u8])]) -> Result<(), CacheError> {
    part.iter_mut()
        .try_for_each(|(stored, out)| unpack(stored, out))
}

#[derive(Debug)]
pub struct Resolved<'a> {
    rows: Vec<(u64, &'a [u8; 32])>,
    data: Vec<u8>,
    pub sidecar: &'a [u8],
}

impl Resolved<'_> {
    #[must_use]
    pub fn pages(&self) -> Vec<HashedPage<'_>> {
        self.rows
            .iter()
            .zip(self.data.chunks_exact(PAGE))
            .map(|(&(gfn, hash), page)| (gfn, hash, page.try_into().expect("page")))
            .collect()
    }
}

pub fn resolve<'a>(chain: &[Delta<'a>]) -> Result<Resolved<'a>, CacheError> {
    let (anchor, deltas) = chain
        .split_first()
        .ok_or(CacheError::Malformed("empty extent chain"))?;
    if !anchor.reverted.is_empty() {
        return Err(CacheError::Malformed("an anchor reverts pages"));
    }
    let mut pages: BTreeMap<u64, StoredPage<'a>> =
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
    let pages: Vec<StoredPage<'a>> = pages.into_values().collect();
    let mut data = vec![0u8; pages.len() * PAGE];
    let mut targets: Vec<(&[u8], &mut [u8])> = pages
        .iter()
        .map(|page| page.2)
        .zip(data.chunks_exact_mut(PAGE))
        .collect();
    let threads = helper_threads(targets.len());
    let chunk = targets.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| -> Result<(), CacheError> {
        let mut parts = targets.chunks_mut(chunk);
        let first = parts.next().unwrap_or_default();
        let workers = parts
            .map(|part| std::thread::Builder::new().spawn_scoped(scope, move || unpack_all(part)))
            .collect::<Result<Vec<_>, _>>()?;
        let mut result = unpack_all(first);
        for worker in workers {
            let joined = match worker.join() {
                Ok(joined) => joined,
                Err(panic) => std::panic::resume_unwind(panic),
            };
            result = result.and(joined);
        }
        result
    })?;
    Ok(Resolved {
        rows: pages.iter().map(|&(gfn, hash, _)| (gfn, hash)).collect(),
        data,
        sidecar,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: u64) -> [u8; PAGE] {
        let mut state = seed | 1;
        let mut page = [0; PAGE];
        for byte in &mut page {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
        page
    }

    fn page(fill: u8) -> ([u8; 32], [u8; PAGE]) {
        ([fill; 32], [fill; PAGE])
    }

    fn written(encoded: &Encoded<'_>) -> Vec<u8> {
        let mut out = vec![0u8; encoded.len()];
        encoded.write(&mut out).unwrap();
        out
    }

    fn extent(pages: &[(u64, u8)], reverted: &[u64], sidecar: &[u8]) -> Vec<u8> {
        let owned: Vec<_> = pages.iter().map(|&(gfn, fill)| (gfn, page(fill))).collect();
        let rows: Vec<HashedPage<'_>> = owned
            .iter()
            .map(|(gfn, (hash, data))| (*gfn, hash, data))
            .collect();
        written(&encode(&rows, reverted, sidecar).unwrap())
    }

    fn summary(pages: &[HashedPage<'_>]) -> Vec<(u64, u8)> {
        pages
            .iter()
            .map(|&(gfn, hash, data)| {
                assert_eq!(hash[0], data[0]);
                assert!(data.iter().all(|&byte| byte == data[0]));
                (gfn, data[0])
            })
            .collect()
    }

    #[test]
    fn measured_extent_counts_stored_page_bytes_and_keeps_incompressible_pages_raw() {
        let flat = [7; PAGE];
        let random = noise(11);
        let hash = [3; 32];
        let pages = [(0, &hash, &flat), (1, &hash, &random)];
        let encoded = encode(&pages, &[2], b"sdk").unwrap();
        let mut output = vec![0; encoded.len()];
        let mut work = ExtentWrites::default();
        encoded.write_measured(&mut output, &mut work).unwrap();
        let delta = read_extent(&output).unwrap();
        assert!(delta.pages[0].2.len() < PAGE / 16);
        assert_eq!(delta.pages[1].2, &random[..]);
        let stored = delta.pages[0].2.len() + PAGE;
        let metadata = HEADER + 2 * ROW + 8 + 3;
        assert_eq!(
            work,
            ExtentWrites {
                encoded_bytes: (metadata + stored) as u64,
                metadata_copy_bytes: metadata as u64,
                page_copy_bytes: stored as u64
            }
        );
        let prior = work;
        assert!(encoded.write_measured(&mut [0; 1], &mut work).is_err());
        assert_eq!(work, prior);
        assert!(matches!(
            resolve(&[delta]),
            Err(CacheError::Malformed("an anchor reverts pages"))
        ));
        let anchor = written(&encode(&pages, &[], b"sdk").unwrap());
        let resolved = resolve(&[read_extent(&anchor).unwrap()]).unwrap();
        let pages = resolved.pages();
        assert_eq!((pages[0].2, pages[1].2), (&flat, &random));
    }

    #[test]
    fn an_extent_round_trips_and_keeps_its_stored_pages_under_a_new_sidecar() {
        let bytes = extent(&[(1, 7), (4, 9)], &[2, 3], b"sidecar");
        let delta = read_extent(&bytes).unwrap();
        assert_eq!(
            delta.pages.iter().map(|page| page.0).collect::<Vec<_>>(),
            vec![1, 4]
        );
        assert_eq!(delta.reverted, vec![2, 3]);
        assert_eq!(delta.sidecar, b"sidecar");
        let out = written(&delta.with_sidecar(b"other"));
        let copied = read_extent(&out).unwrap();
        assert_eq!(copied.pages, delta.pages);
        assert_eq!(copied.reverted, delta.reverted);
        assert_eq!(copied.sidecar, b"other");
    }

    #[test]
    fn a_changed_table_a_wrong_offset_or_a_bad_page_is_rejected() {
        let bytes = extent(&[(1, 7), (4, 9)], &[], b"s");
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
        let hash = [0; 32];
        let bad = [0xff; 10];
        let forged = Delta {
            pages: vec![(0, &hash, &bad[..])],
            reverted: Vec::new(),
            sidecar: b"",
        };
        let out = written(&forged.with_sidecar(b""));
        assert!(matches!(
            resolve(&[read_extent(&out).unwrap()]),
            Err(CacheError::Malformed("extent page encoding"))
        ));
    }

    #[test]
    fn resolution_applies_reverts_and_changes_in_order() {
        let anchor = extent(&[(0, 1), (1, 2), (2, 3)], &[], b"a");
        let first = extent(&[(1, 4), (5, 5)], &[2], b"b");
        let second = extent(&[(2, 6)], &[0, 5], b"c");
        let chain = [
            read_extent(&anchor).unwrap(),
            read_extent(&first).unwrap(),
            read_extent(&second).unwrap(),
        ];
        let resolved = resolve(&chain).unwrap();
        assert_eq!(summary(&resolved.pages()), vec![(1, 4), (2, 6)]);
        assert_eq!(resolved.sidecar, b"c");
        let resolved = resolve(&chain[..2]).unwrap();
        assert_eq!(summary(&resolved.pages()), vec![(0, 1), (1, 4), (5, 5)]);
        assert!(resolve(&chain[1..]).is_err(), "a chain starts at an anchor");
        assert!(resolve(&[]).is_err());
    }

    #[test]
    #[cfg_attr(miri, ignore = "compresses 12 MiB of pages")]
    fn large_extents_split_their_pages_across_threads_in_order() {
        let count = 3 * PARALLEL_PAGES + 5;
        let owned: Vec<_> = (0..count as u64)
            .map(|gfn| {
                let data = if gfn % 3 == 0 {
                    noise(gfn + 1)
                } else {
                    [gfn as u8; PAGE]
                };
                (gfn, [gfn as u8; 32], data)
            })
            .collect();
        let rows: Vec<HashedPage<'_>> = owned
            .iter()
            .map(|(gfn, hash, data)| (*gfn, hash, data))
            .collect();
        let out = written(&encode(&rows, &[], b"big").unwrap());
        assert!(out.len() < count * PAGE);
        let resolved = resolve(&[read_extent(&out).unwrap()]).unwrap();
        assert_eq!(resolved.pages(), rows);
    }
}
