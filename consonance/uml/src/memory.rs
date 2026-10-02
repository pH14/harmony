// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cell::{RefCell, RefMut};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;
use std::rc::Rc;

use sha2::{Digest, Sha256};
use snapshot_store::{
    PAGE_SIZE, PageDelta, PageHash, SnapStats, SnapshotId, Store, StoreConfig, StoreStats,
};

pub(crate) const IMAGE_BYTES: usize = 256 << 20;
pub(crate) const IMAGE_PAGES: u64 = (IMAGE_BYTES / PAGE_SIZE) as u64;
const MAX_CHAIN_LEN: u32 = 32;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Store(#[from] snapshot_store::StoreError),
    #[error("guest memory is {got} bytes, the checkpoints hold {expected}")]
    Size { got: usize, expected: usize },
    #[error("the private image is {0} bytes, more than {IMAGE_BYTES}")]
    ImageTooLarge(u64),
    #[error("the checkpoints hold no guest memory yet")]
    Empty,
}

#[derive(Clone, Default)]
pub struct Checkpoints(Rc<RefCell<Option<Pages>>>);

pub(crate) struct Pages {
    pub(crate) store: Store,
    pub(crate) root: SnapshotId,
    physmem_bytes: usize,
}

pub(crate) struct Snapshot {
    checkpoints: Checkpoints,
    id: SnapshotId,
}

pub(crate) struct GuestMemory {
    pub(crate) physmem: OwnedFd,
    pub(crate) image: OwnedFd,
    mapping: Option<Mapping>,
}

#[derive(Clone, Copy)]
struct Mapping {
    base: NonNull<u8>,
    physmem_bytes: usize,
}

impl std::fmt::Debug for Checkpoints {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Checkpoints")
            .field("stats", &self.stats())
            .finish()
    }
}

impl Checkpoints {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> Option<StoreStats> {
        self.0
            .borrow()
            .as_ref()
            .map(|pages| pages.store.store_stats())
    }

    pub(crate) fn pages(&self, physmem_bytes: usize) -> Result<RefMut<'_, Pages>, MemoryError> {
        let mut pages = self.0.borrow_mut();
        match pages.as_ref() {
            Some(held) if held.physmem_bytes != physmem_bytes => {
                return Err(MemoryError::Size {
                    got: physmem_bytes,
                    expected: held.physmem_bytes,
                });
            }
            Some(_) => {}
            None => {
                let mut store = Store::new(StoreConfig {
                    mem_pages: IMAGE_PAGES + (physmem_bytes / PAGE_SIZE) as u64,
                });
                let root = store.begin_base().seal(image_state(0));
                *pages = Some(Pages {
                    store,
                    root,
                    physmem_bytes,
                });
            }
        }
        Ok(RefMut::map(pages, |pages| {
            pages.as_mut().expect("pages were just initialized")
        }))
    }

    pub(crate) fn held(&self) -> Result<RefMut<'_, Pages>, MemoryError> {
        RefMut::filter_map(self.0.borrow_mut(), Option::as_mut).map_err(|_| MemoryError::Empty)
    }

    pub(crate) fn adopt(&self, id: SnapshotId) -> Snapshot {
        Snapshot {
            checkpoints: self.clone(),
            id,
        }
    }

    pub(crate) fn delta_pages<R>(
        &self,
        base: &Snapshot,
        parent: Option<&Snapshot>,
        target: &Snapshot,
        use_delta: impl FnOnce(&PageDelta<'_>) -> R,
    ) -> Result<R, MemoryError> {
        let pages = self.held()?;
        let delta = pages
            .store
            .page_delta(base.id, parent.map(Snapshot::id), target.id)?;
        Ok(use_delta(&delta))
    }

    pub(crate) fn import_pages(
        &self,
        base: &Snapshot,
        near: &Snapshot,
        rows: &[(u64, &PageHash, &[u8; PAGE_SIZE])],
        image_bytes: u64,
    ) -> Result<Snapshot, MemoryError> {
        let mut pages = self.held()?;
        let near = if pages
            .store
            .stats(near.id)
            .is_ok_and(|stats| stats.chain_len < MAX_CHAIN_LEN)
        {
            near.id
        } else {
            base.id
        };
        let hashes: Vec<_> = rows.iter().map(|&(gfn, hash, _)| (gfn, hash)).collect();
        let rebase = pages.store.rebase(base.id, near, &hashes)?;
        let mut merged: Vec<(u64, &PageHash, &[u8; PAGE_SIZE])> = rebase
            .keep
            .iter()
            .map(|&index| rows[index])
            .chain(
                rebase
                    .origin
                    .iter()
                    .map(|(gfn, hash, data)| (*gfn, hash, &**data)),
            )
            .collect();
        merged.sort_unstable_by_key(|row| row.0);
        let mut builder = pages.store.derive(near)?;
        for (gfn, hash, data) in merged {
            builder.write_hashed_page(gfn, data, hash)?;
        }
        let id = builder.seal(image_state(image_bytes));
        drop(pages);
        Ok(self.adopt(id))
    }
}

impl Pages {
    pub(crate) fn image_bytes(&self, id: SnapshotId) -> Result<u64, MemoryError> {
        let state = self.store.vm_state(id)?;
        Ok(state
            .first_chunk::<8>()
            .map(|bytes| u64::from_le_bytes(*bytes))
            .unwrap_or(0))
    }

    pub(crate) fn stats(&self, id: SnapshotId) -> Result<SnapStats, MemoryError> {
        Ok(self.store.stats(id)?)
    }

    pub(crate) fn shorten(
        &mut self,
        id: SnapshotId,
        memory: &[u8],
    ) -> Result<SnapshotId, MemoryError> {
        if self.store.stats(id)?.chain_len < MAX_CHAIN_LEN {
            return Ok(id);
        }
        let state = self.store.vm_state(id)?.to_vec();
        let flat = self.store.flatten_base(id, memory, &[], state)?;
        self.store.release(id)?;
        self.store.gc();
        Ok(flat)
    }
}

pub(crate) fn image_state(bytes: u64) -> Vec<u8> {
    bytes.to_le_bytes().to_vec()
}

impl Snapshot {
    pub(crate) fn id(&self) -> SnapshotId {
        self.id
    }

    pub(crate) fn share(&self) -> Snapshot {
        if let Ok(mut pages) = self.checkpoints.held() {
            pages
                .store
                .retain(self.id)
                .expect("a live snapshot can be retained");
        }
        self.checkpoints.adopt(self.id)
    }

    pub(crate) fn image_bytes(&self) -> u64 {
        self.checkpoints
            .held()
            .and_then(|pages| pages.image_bytes(self.id))
            .unwrap_or(0)
    }

    pub(crate) fn digest(&self, digest: &mut Sha256) -> Result<(), MemoryError> {
        let pages = self.checkpoints.held()?;
        let delta = pages.store.page_delta(pages.root, None, self.id)?;
        for (gfn, hash, _) in &delta.changed {
            digest.update(gfn.to_le_bytes());
            digest.update(hash);
        }
        digest.update(pages.image_bytes(self.id)?.to_le_bytes());
        Ok(())
    }

    pub(crate) fn owned_pages(&self) -> u64 {
        self.checkpoints
            .held()
            .and_then(|pages| pages.stats(self.id))
            .map_or(0, |stats| stats.owned_pages)
    }
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("Snapshot").field(&self.id).finish()
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if let Ok(mut pages) = self.checkpoints.0.try_borrow_mut()
            && let Some(pages) = pages.as_mut()
            && pages.store.release(self.id).is_ok()
        {
            pages.store.gc();
        }
    }
}

impl GuestMemory {
    pub(crate) fn new() -> io::Result<Self> {
        let image = memfd(c"harmony-uml-image")?;
        resize(&image, IMAGE_BYTES as u64)?;
        Ok(Self {
            physmem: memfd(c"harmony-uml-physmem")?,
            image,
            mapping: None,
        })
    }

    pub(crate) fn physmem_bytes(&self) -> io::Result<usize> {
        usize::try_from(file_size(&self.physmem)?).map_err(io::Error::other)
    }

    pub(crate) fn map(&mut self) -> io::Result<usize> {
        let physmem_bytes = self.physmem_bytes()?;
        if let Some(mapping) = self.mapping {
            if mapping.physmem_bytes != physmem_bytes {
                return Err(io::Error::other("the guest resized its physical memory"));
            }
            return Ok(physmem_bytes);
        }
        if physmem_bytes == 0 || physmem_bytes % PAGE_SIZE != 0 {
            return Err(io::Error::other(format!(
                "guest physical memory is {physmem_bytes} bytes"
            )));
        }
        let total = IMAGE_BYTES + physmem_bytes;
        // SAFETY: a fresh anonymous reservation; mmap(2) chooses the address
        // and nothing else refers to it.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                total,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let mapping = Mapping {
            base: NonNull::new(base.cast()).ok_or_else(io::Error::last_os_error)?,
            physmem_bytes,
        };
        self.mapping = Some(mapping);
        for (fd, offset, length) in [
            (&self.image, 0, IMAGE_BYTES),
            (&self.physmem, IMAGE_BYTES, physmem_bytes),
        ] {
            // SAFETY: the target range lies inside the reservation made above,
            // which this mapping owns, so MAP_FIXED replaces only its own pages.
            let mapped = unsafe {
                libc::mmap(
                    base.cast::<u8>().add(offset).cast(),
                    length,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED | libc::MAP_FIXED,
                    fd.as_raw_fd(),
                    0,
                )
            };
            if mapped == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(physmem_bytes)
    }

    // SAFETY: callers touch the image range only while the image file is
    // `IMAGE_BYTES` long, and the physical memory range only while the guest
    // waits for the host in a memory exchange or after it has exited.
    pub(crate) unsafe fn bytes(&mut self) -> Option<&mut [u8]> {
        self.mapping.map(|mapping| {
            // SAFETY: the mapping covers `IMAGE_BYTES + physmem_bytes` bytes
            // and stays mapped until `self` is dropped.
            unsafe {
                std::slice::from_raw_parts_mut(
                    mapping.base.as_ptr(),
                    IMAGE_BYTES + mapping.physmem_bytes,
                )
            }
        })
    }

    pub(crate) fn clear_physmem(&self) -> io::Result<()> {
        punch(&self.physmem, self.physmem_bytes()? as u64)
    }

    pub(crate) fn clear_image(&self) -> io::Result<()> {
        resize(&self.image, 0)?;
        resize(&self.image, IMAGE_BYTES as u64)
    }

    pub(crate) fn restore_image_size(&self, written: u64) -> Result<(), MemoryError> {
        if written > IMAGE_BYTES as u64 {
            return Err(MemoryError::ImageTooLarge(written));
        }
        Ok(resize(&self.image, IMAGE_BYTES as u64)?)
    }
}

impl Drop for GuestMemory {
    fn drop(&mut self) {
        if let Some(mapping) = self.mapping.take() {
            // SAFETY: the range is the reservation this mapping owns; no slice
            // from `bytes` outlives `self`.
            unsafe {
                libc::munmap(
                    mapping.base.as_ptr().cast(),
                    IMAGE_BYTES + mapping.physmem_bytes,
                );
            }
        }
    }
}

pub(crate) fn data_extents(fd: &OwnedFd, logical: u64) -> io::Result<Vec<(u64, u64)>> {
    let mut extents = Vec::new();
    let mut at = 0;
    while at < logical {
        let offset = i64::try_from(at).map_err(io::Error::other)?;
        // SAFETY: lseek(2) reads only its integer arguments; `fd` is open.
        let start = unsafe { libc::lseek(fd.as_raw_fd(), offset, libc::SEEK_DATA) };
        if start < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENXIO) {
                break;
            }
            return Err(error);
        }
        // SAFETY: as above, with the data offset just returned.
        let end = unsafe { libc::lseek(fd.as_raw_fd(), start, libc::SEEK_HOLE) };
        if end < start {
            return Err(io::Error::last_os_error());
        }
        let (start, end) = (start.unsigned_abs(), end.unsigned_abs().min(logical));
        extents.push((start, end - start));
        at = end;
    }
    Ok(extents)
}

fn file_size(fd: &OwnedFd) -> io::Result<u64> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `stat` points to a live, writable stat buffer for the call and
    // `fd` is open.
    if unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat(2) succeeded, so it initialized the buffer.
    let stat = unsafe { stat.assume_init() };
    u64::try_from(stat.st_size).map_err(io::Error::other)
}

fn resize(fd: &OwnedFd, length: u64) -> io::Result<()> {
    let length = i64::try_from(length).map_err(io::Error::other)?;
    // SAFETY: ftruncate(2) reads only its integer arguments; `fd` is open.
    if unsafe { libc::ftruncate(fd.as_raw_fd(), length) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn punch(fd: &OwnedFd, length: u64) -> io::Result<()> {
    let length = i64::try_from(length).map_err(io::Error::other)?;
    // SAFETY: fallocate(2) reads only its integer arguments; `fd` is open.
    let result = unsafe {
        libc::fallocate(
            fd.as_raw_fd(),
            libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
            0,
            length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn memfd(name: &std::ffi::CStr) -> io::Result<OwnedFd> {
    let mut flags = libc::MFD_CLOEXEC | libc::MFD_EXEC;
    loop {
        // SAFETY: `name` is a NUL-terminated string that outlives the call.
        let fd = unsafe { libc::memfd_create(name.as_ptr(), flags) };
        if fd >= 0 {
            // SAFETY: memfd_create(2) succeeded, so `fd` is open and owned by
            // nothing else.
            return Ok(unsafe { OwnedFd::from_raw_fd(fd) });
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINVAL) && flags & libc::MFD_EXEC != 0 {
            flags &= !libc::MFD_EXEC;
            continue;
        }
        return Err(error);
    }
}
