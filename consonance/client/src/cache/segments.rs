// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, io, ptr::NonNull, sync::Arc};

pub const PAGE: usize = 4096;
pub const SEGMENT_BYTES: usize = 64 << 20;

struct Segment {
    base: NonNull<u8>,
    len: usize,
    backing: Backing,
}

// SAFETY: a Segment owns its mapping; every byte range in it is handed to exactly one
// writable extent and, after commit, is only read, so sharing the base pointer across threads
// never creates overlapping mutable access.
unsafe impl Send for Segment {}
// SAFETY: see the Send impl; shared references never read a range that a writer still holds.
unsafe impl Sync for Segment {}

#[cfg(all(any(target_os = "linux", target_os = "macos"), not(miri)))]
type Backing = std::os::fd::OwnedFd;

#[cfg(any(miri, not(any(target_os = "linux", target_os = "macos"))))]
type Backing = std::alloc::Layout;

impl Segment {
    #[cfg(all(any(target_os = "linux", target_os = "macos"), not(miri)))]
    fn new(len: usize) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        let fd = shared_file(len)?;
        // SAFETY: fd is a live file of exactly len bytes; a fresh shared mapping aliases no Rust
        // object.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let base = NonNull::new(base.cast::<u8>()).ok_or_else(io::Error::last_os_error)?;
        Ok(Self {
            base,
            len,
            backing: fd,
        })
    }

    #[cfg(any(miri, not(any(target_os = "linux", target_os = "macos"))))]
    fn new(len: usize) -> io::Result<Self> {
        let layout = std::alloc::Layout::from_size_align(len.max(PAGE), PAGE)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        // SAFETY: layout has a nonzero size.
        let base = unsafe { std::alloc::alloc_zeroed(layout) };
        let base = NonNull::new(base).ok_or_else(|| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(Self {
            base,
            len,
            backing: layout,
        })
    }

    fn range(&self, offset: usize, len: usize) -> *mut u8 {
        assert!(
            offset.checked_add(len).is_some_and(|end| end <= self.len),
            "extent outside its segment"
        );
        // SAFETY: offset + len is within the mapping, so the result stays in bounds.
        unsafe { self.base.as_ptr().add(offset) }
    }
}

impl Drop for Segment {
    #[cfg(all(any(target_os = "linux", target_os = "macos"), not(miri)))]
    fn drop(&mut self) {
        // SAFETY: base and len describe the mapping created in new, and no extent outlives
        // the Arc that owns this Segment.
        unsafe { libc::munmap(self.base.as_ptr().cast(), self.len) };
    }

    #[cfg(any(miri, not(any(target_os = "linux", target_os = "macos"))))]
    fn drop(&mut self) {
        // SAFETY: base was allocated in new with this layout, and no extent outlives the Arc
        // that owns this Segment.
        unsafe { std::alloc::dealloc(self.base.as_ptr(), self.backing) };
    }
}

#[cfg(all(target_os = "linux", not(miri)))]
fn shared_file(len: usize) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    // SAFETY: the name is a NUL-terminated literal.
    let raw = unsafe { libc::memfd_create(c"harmony-snapshot-cache".as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: memfd_create returned a fresh descriptor that nothing else owns.
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    std::fs::File::from(fd.try_clone()?).set_len(len as u64)?;
    Ok(fd)
}

#[cfg(all(target_os = "macos", not(miri)))]
fn shared_file(len: usize) -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = std::ffi::CString::new(format!(
        "/hsc-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    // SAFETY: name is NUL-terminated; O_EXCL makes this call the only owner of the object.
    let raw = unsafe {
        libc::shm_open(
            name.as_ptr(),
            libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
            0o600,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: shm_open returned a fresh descriptor that nothing else owns.
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    // SAFETY: name is NUL-terminated; the open descriptor keeps the object alive.
    unsafe { libc::shm_unlink(name.as_ptr()) };
    std::fs::File::from(fd.try_clone()?).set_len(len as u64)?;
    Ok(fd)
}

pub type Abandon = Box<dyn FnOnce(u64) + Send>;

#[cfg(all(any(target_os = "linux", target_os = "macos"), not(miri)))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct SharedRange<'a> {
    pub fd: std::os::fd::BorrowedFd<'a>,
    pub offset: usize,
    pub len: usize,
}

#[cfg(all(any(target_os = "linux", target_os = "macos"), not(miri)))]
impl Segment {
    fn shared(&self, offset: usize, len: usize) -> SharedRange<'_> {
        use std::os::fd::AsFd;
        SharedRange {
            fd: self.backing.as_fd(),
            offset,
            len,
        }
    }
}

pub struct WritableExtent {
    segment: Arc<Segment>,
    segment_id: u64,
    offset: usize,
    len: usize,
    abandon: Option<Abandon>,
}

impl std::fmt::Debug for WritableExtent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WritableExtent")
            .field("segment_id", &self.segment_id)
            .field("offset", &self.offset)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl WritableExtent {
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        let start = self.segment.range(self.offset, self.len);
        // SAFETY: PageSegments hands this range to one WritableExtent only, the extent is
        // not Clone, and a CommittedExtent for the range exists only after commit consumes it.
        unsafe { std::slice::from_raw_parts_mut(start, self.len) }
    }

    pub(crate) fn commit(mut self) -> CommittedExtent {
        self.abandon = None;
        CommittedExtent {
            segment: Arc::clone(&self.segment),
            segment_id: self.segment_id,
            offset: self.offset,
            len: self.len,
        }
    }

    pub(crate) fn set_abandon(&mut self, abandon: Abandon) {
        self.abandon = Some(abandon);
    }

    #[cfg(all(any(target_os = "linux", target_os = "macos"), not(miri)))]
    pub(crate) fn shared_range(&self) -> SharedRange<'_> {
        self.segment.shared(self.offset, self.len)
    }
}

impl Drop for WritableExtent {
    fn drop(&mut self) {
        if let Some(abandon) = self.abandon.take() {
            abandon(self.segment_id);
        }
    }
}

#[derive(Clone)]
pub struct CommittedExtent {
    segment: Arc<Segment>,
    segment_id: u64,
    offset: usize,
    len: usize,
}

impl std::fmt::Debug for CommittedExtent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommittedExtent")
            .field("segment_id", &self.segment_id)
            .field("offset", &self.offset)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl CommittedExtent {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        let start = self.segment.range(self.offset, self.len);
        // SAFETY: a committed range was written by its one WritableExtent, which commit
        // consumed; nothing writes the range again while the Segment lives.
        unsafe { std::slice::from_raw_parts(start, self.len) }
    }

    pub(crate) fn segment_id(&self) -> u64 {
        self.segment_id
    }

    #[cfg(all(any(target_os = "linux", target_os = "macos"), not(miri)))]
    pub(crate) fn shared_range(&self) -> SharedRange<'_> {
        self.segment.shared(self.offset, self.len)
    }

    #[cfg(test)]
    pub(crate) fn offset(&self) -> usize {
        self.offset
    }
}

struct SegmentUse {
    segment: Arc<Segment>,
    used: usize,
    extents: usize,
}

pub struct PageSegments {
    segment_bytes: usize,
    open: Option<u64>,
    segments: BTreeMap<u64, SegmentUse>,
    next_id: u64,
    charged: usize,
}

impl std::fmt::Debug for PageSegments {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PageSegments")
            .field("segment_bytes", &self.segment_bytes)
            .field("open", &self.open)
            .field("segments", &self.segments.len())
            .field("charged", &self.charged)
            .finish_non_exhaustive()
    }
}

#[must_use]
pub fn rounded(len: usize) -> Option<usize> {
    len.checked_next_multiple_of(PAGE)
}

impl PageSegments {
    #[must_use]
    pub fn new(segment_bytes: usize) -> Self {
        Self {
            segment_bytes: segment_bytes.max(PAGE),
            open: None,
            segments: BTreeMap::new(),
            next_id: 0,
            charged: 0,
        }
    }

    #[must_use]
    pub fn charged(&self) -> usize {
        self.charged
    }

    #[must_use]
    pub fn segments(&self) -> usize {
        self.segments.len()
    }

    pub fn allocate(&mut self, len: usize) -> io::Result<WritableExtent> {
        let len = rounded(len.max(1))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "extent size overflow"))?;
        let fits = self.open.and_then(|id| {
            let used = self.segments.get(&id)?;
            (used.segment.len - used.used >= len).then_some(id)
        });
        let id = match fits {
            Some(id) => id,
            None => {
                if let Some(previous) = self.open.take() {
                    self.retire_if_empty(previous);
                }
                let segment = Arc::new(Segment::new(self.segment_bytes.max(len))?);
                let id = self.next_id;
                self.next_id += 1;
                self.segments.insert(
                    id,
                    SegmentUse {
                        segment,
                        used: 0,
                        extents: 0,
                    },
                );
                self.open = Some(id);
                id
            }
        };
        let used = self.segments.get_mut(&id).expect("open segment");
        let offset = used.used;
        used.used += len;
        used.extents += 1;
        self.charged += len;
        Ok(WritableExtent {
            segment: Arc::clone(&used.segment),
            segment_id: id,
            offset,
            len,
            abandon: None,
        })
    }

    pub fn free(&mut self, segment_id: u64) {
        let Some(used) = self.segments.get_mut(&segment_id) else {
            return;
        };
        used.extents = used.extents.saturating_sub(1);
        if self.open != Some(segment_id) {
            self.retire_if_empty(segment_id);
        }
    }

    fn retire_if_empty(&mut self, segment_id: u64) {
        if self
            .segments
            .get(&segment_id)
            .is_some_and(|used| used.extents == 0)
            && let Some(used) = self.segments.remove(&segment_id)
        {
            self.charged -= used.used;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extents_are_page_aligned_disjoint_and_charged() {
        let mut segments = PageSegments::new(4 * PAGE);
        let mut a = segments.allocate(10).unwrap();
        let mut b = segments.allocate(PAGE + 1).unwrap();
        assert_eq!((a.len(), b.len()), (PAGE, 2 * PAGE));
        assert_eq!(segments.charged(), 3 * PAGE);
        a.bytes_mut().fill(1);
        b.bytes_mut().fill(2);
        let a = a.commit();
        let b = b.commit();
        assert!(a.bytes().iter().all(|&byte| byte == 1));
        assert!(b.bytes().iter().all(|&byte| byte == 2));
        assert_eq!((a.offset(), b.offset()), (0, PAGE));
        assert_eq!(a.segment_id(), b.segment_id());
    }

    #[test]
    fn a_segment_is_released_when_its_last_extent_dies_after_it_closes() {
        let mut segments = PageSegments::new(2 * PAGE);
        let a = segments.allocate(PAGE).unwrap().commit();
        let b = segments.allocate(PAGE).unwrap().commit();
        segments.free(a.segment_id());
        assert_eq!(segments.charged(), 2 * PAGE, "the open segment stays");
        let c = segments.allocate(PAGE).unwrap().commit();
        assert_ne!(c.segment_id(), b.segment_id());
        assert_eq!(segments.segments(), 2);
        assert_eq!(
            segments.charged(),
            3 * PAGE,
            "a dead extent in a live segment is charged"
        );
        segments.free(b.segment_id());
        assert_eq!(segments.segments(), 1);
        assert_eq!(segments.charged(), PAGE);
        assert!(
            b.bytes().iter().all(|&byte| byte == 0),
            "readers keep the mapping"
        );
    }

    #[test]
    fn a_large_extent_gets_its_own_segment() {
        let mut segments = PageSegments::new(2 * PAGE);
        let _small = segments.allocate(PAGE).unwrap();
        let large = segments.allocate(5 * PAGE).unwrap();
        assert_eq!(large.len(), 5 * PAGE);
        assert_eq!(segments.segments(), 2);
    }

    #[test]
    fn an_abandoned_extent_runs_its_callback() {
        let mut segments = PageSegments::new(2 * PAGE);
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut extent = segments.allocate(PAGE).unwrap();
        extent.set_abandon(Box::new(move |id| sender.send(id).unwrap()));
        let id = extent.segment_id;
        drop(extent);
        assert_eq!(receiver.try_recv().unwrap(), id);
    }
}
