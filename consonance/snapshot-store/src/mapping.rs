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
pub struct Mapping {
    /// `None` only for zero-length images (`mmap` rejects zero-length maps).
    map: Option<MmapMut>,
    /// Keeps the backing tempfile open for the mapping's lifetime. The kernel would
    /// keep the pages alive anyway, but holding the handle makes the ownership story
    /// explicit and the SAFETY argument below local.
    _file: File,
}

impl Mapping {
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
        Ok(Mapping { map, _file: file })
    }

    /// The full logical image as bytes.
    pub fn as_slice(&self) -> &[u8] {
        self.map.as_deref().unwrap_or(&[])
    }

    /// The full logical image as mutable bytes. Writes are private to this mapping.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.map.as_deref_mut().unwrap_or(&mut [])
    }

    /// Length in bytes (`mem_pages * PAGE_SIZE`).
    pub fn len(&self) -> usize {
        self.map.as_ref().map_or(0, |m| m.len())
    }

    /// True for a zero-length image.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
