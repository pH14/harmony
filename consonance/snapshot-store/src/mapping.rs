// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs::File;
use std::io;

use memmap2::{MmapMut, MmapOptions};

pub struct Mapping {
    backing: Backing,
}

enum Backing {
    Mapped { map: Option<MmapMut>, _file: File },
    Anonymous { pages: Vec<AlignedPage>, len: usize },
}

#[repr(C, align(4096))]
#[derive(Clone)]
struct AlignedPage([u8; crate::PAGE_SIZE]);

impl Mapping {
    pub(crate) fn populate<'a>(
        file: &File,
        len: u64,
        pages: impl Iterator<Item = (u64, &'a [u8])>,
    ) -> io::Result<()> {
        let Some(len) = usize::try_from(len).ok().filter(|&l| l != 0) else {
            return Ok(());
        };
        // SAFETY: `file` is an anonymous unlinked tempfile created, sized, and written
        // exclusively by this process; no other handle to it exists, so it cannot be
        // truncated or modified behind the map's back (the UB/SIGBUS hazard `map_mut`
        // is unsafe about). The map is dropped before this function returns.
        let mut map = unsafe { MmapOptions::new().len(len).map_mut(file)? };
        for (gfn, data) in pages {
            let start = usize::try_from(gfn)
                .ok()
                .and_then(|g| g.checked_mul(crate::PAGE_SIZE))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "page offset overflows usize")
                })?;
            let end = start
                .checked_add(data.len())
                .filter(|&e| e <= len)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "page lies outside the image")
                })?;
            map[start..end].copy_from_slice(data);
        }
        map.flush()
    }

    pub(crate) fn new(file: File, len: u64) -> io::Result<Mapping> {
        let map = if len == 0 {
            None
        } else {
            let len: usize = len.try_into().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "logical image does not fit the address space",
                )
            })?;
            // SAFETY: `file` is an anonymous unlinked tempfile created, sized, and
            // written exclusively by this process; no other handle to it exists, so it
            // cannot be truncated or modified behind the map's back (the UB/SIGBUS
            // hazard `map_copy` is unsafe about). The map is MAP_PRIVATE, so writes
            // through it never reach the file.
            Some(unsafe { MmapOptions::new().len(len).map_copy(&file)? })
        };
        Ok(Mapping {
            backing: Backing::Mapped { map, _file: file },
        })
    }

    pub fn anonymous(len: usize) -> Mapping {
        let pages = vec![AlignedPage([0u8; crate::PAGE_SIZE]); len.div_ceil(crate::PAGE_SIZE)];
        Mapping {
            backing: Backing::Anonymous { pages, len },
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        match &self.backing {
            Backing::Mapped { map, .. } => map.as_deref().unwrap_or(&[]),
            // SAFETY: `pages` is one contiguous Vec allocation of
            // `pages.len() * PAGE_SIZE` initialized bytes (`AlignedPage` is a
            // `repr(C)` byte array with `size == align`, so no padding), and
            // `len <= pages.len() * PAGE_SIZE` by construction in `anonymous`.
            Backing::Anonymous { pages, len } => unsafe {
                std::slice::from_raw_parts(pages.as_ptr().cast::<u8>(), *len)
            },
        }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        match &mut self.backing {
            Backing::Mapped { map, .. } => map.as_deref_mut().unwrap_or(&mut []),
            // SAFETY: as in `as_slice`, plus uniqueness: the borrow derives from
            // `&mut self`, so no other reference into `pages` is live.
            Backing::Anonymous { pages, len } => unsafe {
                std::slice::from_raw_parts_mut(pages.as_mut_ptr().cast::<u8>(), *len)
            },
        }
    }

    pub fn len(&self) -> usize {
        match &self.backing {
            Backing::Mapped { map, .. } => map.as_ref().map_or(0, |m| m.len()),
            Backing::Anonymous { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod anon_tests {
    use super::*;
    use crate::PAGE_SIZE;

    #[test]
    fn anonymous_is_zero_filled_and_writes_are_visible() {
        let mut m = Mapping::anonymous(2 * PAGE_SIZE);
        assert_eq!(m.len(), 2 * PAGE_SIZE);
        assert!(!m.is_empty());
        assert!(m.as_slice().iter().all(|&b| b == 0), "starts zero-filled");
        m.as_mut_slice()[PAGE_SIZE] = 0xAB;
        assert_eq!(m.as_slice()[PAGE_SIZE], 0xAB, "a write is visible");
        assert_eq!(m.as_slice()[0], 0, "untouched bytes stay zero");
    }

    #[test]
    fn anonymous_zero_length_is_empty() {
        let m = Mapping::anonymous(0);
        assert!(m.is_empty());
        assert_eq!(m.as_slice(), &[] as &[u8]);
    }

    #[test]
    fn anonymous_base_is_4kib_aligned() {
        let m = Mapping::anonymous(3 * PAGE_SIZE);
        assert_eq!(
            m.as_slice().as_ptr() as usize % PAGE_SIZE,
            0,
            "anonymous backing must start at a page-aligned address"
        );
    }

    #[test]
    fn anonymous_len_is_exact_not_page_rounded() {
        let m = Mapping::anonymous(PAGE_SIZE + 10);
        assert_eq!(m.len(), PAGE_SIZE + 10);
        assert_eq!(m.as_slice().len(), PAGE_SIZE + 10);
    }
}

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use crate::PAGE_SIZE;

    fn sized(pages: u64) -> (File, u64) {
        let len = pages * PAGE_SIZE as u64;
        let file = tempfile::tempfile().unwrap();
        file.set_len(len).unwrap();
        (file, len)
    }

    #[test]
    fn populate_writes_each_page_at_its_gfn_offset() {
        let (file, len) = sized(4);
        let a = [0xAAu8; PAGE_SIZE];
        let b = [0xBBu8; PAGE_SIZE];
        Mapping::populate(&file, len, [(0u64, &a[..]), (3, &b[..])].into_iter()).unwrap();

        let m = Mapping::new(file, len).unwrap();
        let img = m.as_slice();
        assert_eq!(&img[0..PAGE_SIZE], &a[..]);
        assert_eq!(&img[PAGE_SIZE..2 * PAGE_SIZE], &[0u8; PAGE_SIZE][..]);
        assert_eq!(&img[2 * PAGE_SIZE..3 * PAGE_SIZE], &[0u8; PAGE_SIZE][..]);
        assert_eq!(&img[3 * PAGE_SIZE..], &b[..]);
    }

    #[cfg(unix)]
    #[test]
    fn populate_leaves_untouched_pages_as_holes() {
        const PAGES: u64 = 65_536;
        let (file, len) = sized(PAGES);
        let p = [1u8; PAGE_SIZE];
        Mapping::populate(&file, len, std::iter::once((PAGES - 1, &p[..]))).unwrap();
        let blocks_bytes = {
            use std::os::unix::fs::MetadataExt;
            file.metadata().unwrap().blocks() * 512
        };
        assert!(
            blocks_bytes < 16 * PAGE_SIZE as u64,
            "file is not sparse: {blocks_bytes} bytes allocated for one written page"
        );
    }

    #[test]
    fn populate_of_a_zero_length_image_is_a_no_op() {
        let (file, len) = sized(0);
        assert_eq!(len, 0);
        Mapping::populate(&file, len, std::iter::empty()).unwrap();
        let m = Mapping::new(file, len).unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn populate_rejects_a_page_outside_the_image() {
        let (file, len) = sized(2);
        let p = [1u8; PAGE_SIZE];
        let err = Mapping::populate(&file, len, std::iter::once((2u64, &p[..]))).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
