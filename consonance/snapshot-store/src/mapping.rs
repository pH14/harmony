// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mmap-backed [`Mapping`] returned by `Store::materialize` — the only module in
//! this crate permitted to use `unsafe` (for the `memmap2` mapping calls).

use std::fs::File;
use std::io;

use memmap2::{MmapMut, MmapOptions};

/// A private, mutable copy-on-write view of one snapshot's full logical memory image.
///
/// Backed by a sparse tempfile owned by this value: the file holds the snapshot's
/// resolved non-zero pages and holes everywhere else, and is mapped `MAP_PRIVATE`
/// (portable across macOS and Linux). Reads fault pages in lazily; writes copy the
/// touched page privately and never reach the file or the [`Store`](crate::Store) —
/// the snapshot stays immutable no matter what is written here. The unlinked tempfile
/// is reclaimed by the OS when the `Mapping` drops.
///
/// [`Store::materialize`](crate::Store::materialize) always produces the mmap backing.
/// The [`Mapping::anonymous`] seam produces the same *interface* over a plain heap
/// buffer, so code that takes a `Mapping` — including `unsafe` code that maps its
/// [`as_mut_slice`](Mapping::as_mut_slice) as memory — stays exercisable under the Miri
/// interpreter, which cannot execute `mmap`.
pub struct Mapping {
    backing: Backing,
}

/// How a [`Mapping`]'s bytes are held.
enum Backing {
    /// The production path: a copy-on-write `mmap` over a sparse tempfile.
    Mapped {
        /// `None` only for zero-length images (`mmap` rejects zero-length maps).
        map: Option<MmapMut>,
        /// Keeps the backing tempfile open for the mapping's lifetime. The kernel would
        /// keep the pages alive anyway, but holding the handle makes the ownership story
        /// explicit and the SAFETY argument below local.
        _file: File,
    },
    /// A test/Miri seam: a plain heap buffer — no `mmap`, no tempfile. Same observable
    /// bytes and interface as `Mapped`; built only by [`Mapping::anonymous`]. Its base
    /// is heap- (not page-) aligned, which is fine for its only use: driving a
    /// **mock** backend that records the region rather than a real
    /// `KVM_SET_USER_MEMORY_REGION` (that page-alignment requirement lives on the
    /// `mmap` path).
    Anonymous(Vec<u8>),
}

impl Mapping {
    /// Write `pages` (`(gfn, PAGE_SIZE bytes)`) into `file` through a single write
    /// mapping, one memcpy per page, at byte offset `gfn * PAGE_SIZE`.
    ///
    /// `file` must be a freshly created, unlinked tempfile already sized to exactly
    /// `len` bytes. Offsets not covered by `pages` are never touched, so the file stays
    /// sparse — zero and absent pages cost neither disk nor page cache. The mapping is
    /// flushed and unmapped before returning, so the caller may then map the same file
    /// copy-on-write via [`Mapping::new`].
    ///
    /// This replaces a `seek` + `write_all` pair of syscalls per page (task 95 M1.2b).
    pub(crate) fn populate<'a>(
        file: &File,
        len: u64,
        pages: impl Iterator<Item = (u64, &'a [u8])>,
    ) -> io::Result<()> {
        let Some(len) = usize::try_from(len).ok().filter(|&l| l != 0) else {
            // A zero-length image has no pages to write, and `mmap` rejects a
            // zero-length map. (`usize::try_from` can only fail on a 32-bit host with a
            // >4 GiB image, which `Mapping::new` rejects for the same reason.)
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

    /// Map `len` bytes of `file` copy-on-write. `file` must be a freshly created,
    /// unlinked tempfile already sized to at least `len` bytes.
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

    /// A `len`-byte, zero-filled mapping backed by a **plain heap buffer** instead of a
    /// tempfile `mmap` — the Miri-executable seam behind `Store::materialize`'s
    /// interface.
    ///
    /// Byte-observably identical to a freshly materialized all-zero image, but with no
    /// `mmap`/tempfile, so a consumer that takes a `Mapping` (and the `unsafe` pointer
    /// handling it performs on [`as_mut_slice`](Mapping::as_mut_slice) — e.g. mapping the
    /// buffer as a mock backend's guest RAM) can be driven under the Miri interpreter,
    /// which cannot execute `mmap`. Intended for tests / the UB safety net; production
    /// restores always go through `Store::materialize`. Fill via `as_mut_slice`.
    pub fn anonymous(len: usize) -> Mapping {
        Mapping {
            backing: Backing::Anonymous(vec![0u8; len]),
        }
    }

    /// The full logical image as bytes.
    pub fn as_slice(&self) -> &[u8] {
        match &self.backing {
            Backing::Mapped { map, .. } => map.as_deref().unwrap_or(&[]),
            Backing::Anonymous(buf) => buf,
        }
    }

    /// The full logical image as mutable bytes. Writes are private to this mapping.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        match &mut self.backing {
            Backing::Mapped { map, .. } => map.as_deref_mut().unwrap_or(&mut []),
            Backing::Anonymous(buf) => buf,
        }
    }

    /// Length in bytes (`mem_pages * PAGE_SIZE`).
    pub fn len(&self) -> usize {
        match &self.backing {
            Backing::Mapped { map, .. } => map.as_ref().map_or(0, |m| m.len()),
            Backing::Anonymous(buf) => buf.len(),
        }
    }

    /// True for a zero-length image.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// The `anonymous` seam is `mmap`-free, so unlike the rest of this module it runs under
// Miri — its whole point is to give the interpreter a `Mapping` to exercise.
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
}

// `mmap` is a real syscall: Miri cannot execute it, so this module's tests — every one
// of which must map something to observe anything — are excluded under the interpreter.
// The crate's unsafe lives entirely here and is exercised by these tests plus the
// `materialize` paths in `tests/{gates,oracle,stateful}.rs` on a real kernel.
#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use crate::PAGE_SIZE;

    /// A sized, empty tempfile of `pages` pages.
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
        assert_eq!(&img[PAGE_SIZE..2 * PAGE_SIZE], &[0u8; PAGE_SIZE][..]); // hole
        assert_eq!(&img[2 * PAGE_SIZE..3 * PAGE_SIZE], &[0u8; PAGE_SIZE][..]); // hole
        assert_eq!(&img[3 * PAGE_SIZE..], &b[..]);
    }

    // `st_blocks` is the only portable-across-macOS-and-Linux way to see a hole; both
    // gate targets are unix, and this is a test, not a logic fork in library code.
    #[cfg(unix)]
    #[test]
    fn populate_leaves_untouched_pages_as_holes() {
        // A large sparse image must not be materialized byte-by-byte: writing one page
        // of a 256 MiB image must leave the file's allocated size near one page.
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

    // `Mapping::new`'s copy-on-write semantics are covered end-to-end by
    // `tests/gates.rs::mapping_writes_are_private` through `Store::materialize`; cloning
    // the `File` here to re-map it would contradict the sole-handle precondition the
    // `SAFETY` comment above rests on.
}
