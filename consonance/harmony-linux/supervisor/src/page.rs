// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::reconcile::EventPark;
use process_proto::events::{
    EVENT_PAGE_EDGES, EVENT_PAGE_GENERATION, EVENT_PAGE_HOLD_NANOS, EVENT_PAGE_TARGET_END,
    EVENT_PAGE_TARGET_START, EVENT_PAGE_WORDS,
};
use std::sync::atomic::{AtomicU64, Ordering, fence};

pub type PageWords = [AtomicU64; EVENT_PAGE_WORDS];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PageRecord {
    pub edges: u64,
    pub hold_nanos: u64,
    pub target_start: u64,
    pub target_end: u64,
}

impl PageRecord {
    #[must_use]
    pub fn of(park: EventPark) -> Self {
        let (target_start, target_end) = park
            .target
            .map_or((0, 0), |target| (target.start, target.end));
        Self {
            edges: u64::from(park.edges),
            hold_nanos: park.hold_nanos,
            target_start,
            target_end,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ParkPage<'a> {
    words: &'a PageWords,
}

impl<'a> ParkPage<'a> {
    #[must_use]
    pub fn new(words: &'a PageWords) -> Self {
        Self { words }
    }

    #[must_use]
    pub fn generation(self) -> u64 {
        self.words[EVENT_PAGE_GENERATION].load(Ordering::Acquire)
    }

    pub fn publish(self, record: PageRecord) -> u64 {
        let generation = &self.words[EVENT_PAGE_GENERATION];
        let current = generation.load(Ordering::Relaxed);
        generation.store(current.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.words[EVENT_PAGE_EDGES].store(record.edges, Ordering::Relaxed);
        self.words[EVENT_PAGE_HOLD_NANOS].store(record.hold_nanos, Ordering::Relaxed);
        self.words[EVENT_PAGE_TARGET_START].store(record.target_start, Ordering::Relaxed);
        self.words[EVENT_PAGE_TARGET_END].store(record.target_end, Ordering::Relaxed);
        let published = current.wrapping_add(2);
        generation.store(published, Ordering::Release);
        published
    }

    #[must_use]
    pub fn read(self, seen: u64) -> Option<(u64, PageRecord)> {
        let generation = self.words[EVENT_PAGE_GENERATION].load(Ordering::Acquire);
        if generation == seen || generation & 1 != 0 {
            return None;
        }
        let record = PageRecord {
            edges: self.words[EVENT_PAGE_EDGES].load(Ordering::Relaxed),
            hold_nanos: self.words[EVENT_PAGE_HOLD_NANOS].load(Ordering::Relaxed),
            target_start: self.words[EVENT_PAGE_TARGET_START].load(Ordering::Relaxed),
            target_end: self.words[EVENT_PAGE_TARGET_END].load(Ordering::Relaxed),
        };
        fence(Ordering::Acquire);
        (self.words[EVENT_PAGE_GENERATION].load(Ordering::Relaxed) == generation)
            .then_some((generation, record))
    }
}

#[cfg(target_os = "linux")]
pub use mapping::PageMapping;

#[cfg(target_os = "linux")]
mod mapping {
    use super::{PageWords, ParkPage};
    use process_proto::events::EVENT_PAGE_BYTES;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::ptr::NonNull;

    #[derive(Debug)]
    pub struct PageMapping {
        file: OwnedFd,
        words: NonNull<PageWords>,
    }

    impl PageMapping {
        pub fn create() -> io::Result<Self> {
            // SAFETY: the name is a static NUL-terminated string and the flag is
            // a constant; memfd_create reads only these arguments.
            let fd =
                unsafe { libc::memfd_create(c"harmony-event-page".as_ptr(), libc::MFD_CLOEXEC) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `fd` is a new descriptor returned by memfd_create, so this
            // `OwnedFd` takes sole responsibility for closing it.
            let file = unsafe { OwnedFd::from_raw_fd(fd) };
            let length = libc::off_t::try_from(EVENT_PAGE_BYTES)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            // SAFETY: `file` owns the open memfd for the duration of the call, and
            // ftruncate reads only the descriptor and the length.
            if unsafe { libc::ftruncate(file.as_raw_fd(), length) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: a null hint lets the kernel choose a fresh address, so the
            // mapping overlaps no Rust object; the length and descriptor describe
            // the page-sized memfd that `file` owns, and MAP_FAILED is checked.
            let mapped = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    EVENT_PAGE_BYTES,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    file.as_raw_fd(),
                    0,
                )
            };
            if mapped == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            let words = NonNull::new(mapped.cast::<PageWords>())
                .ok_or_else(|| io::Error::from(io::ErrorKind::AddrNotAvailable))?;
            Ok(Self { file, words })
        }

        #[must_use]
        pub fn page(&self) -> ParkPage<'_> {
            // SAFETY: `words` is the page-aligned start of a live shared mapping
            // of `EVENT_PAGE_BYTES` zero-filled bytes, enough for the page words,
            // and `AtomicU64` has the size and alignment of `u64`; the mapping is
            // removed only when `self` drops, after the borrow ends; and every
            // access to the page, here and in the node's runtime, is atomic.
            ParkPage::new(unsafe { self.words.as_ref() })
        }

        #[must_use]
        pub fn fd(&self) -> RawFd {
            self.file.as_raw_fd()
        }
    }

    impl Drop for PageMapping {
        fn drop(&mut self) {
            // SAFETY: `words` is the address mmap returned for EVENT_PAGE_BYTES
            // bytes, and `page` ties every reference into it to a borrow of `self`.
            unsafe {
                libc::munmap(self.words.as_ptr().cast(), EVENT_PAGE_BYTES);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use process_proto::events::ParkTarget;
    use std::sync::Arc;
    use std::thread;

    fn words() -> PageWords {
        std::array::from_fn(|_| AtomicU64::new(0))
    }

    fn record(value: u64) -> PageRecord {
        PageRecord {
            edges: value,
            hold_nanos: value,
            target_start: value,
            target_end: value.wrapping_add(1),
        }
    }

    #[test]
    fn a_published_record_is_read_once_under_an_even_generation() {
        let words = words();
        let page = ParkPage::new(&words);
        assert_eq!(page.generation(), 0);
        assert_eq!(page.read(0), None);
        assert_eq!(page.publish(record(7)), 2);
        assert_eq!(page.read(0), Some((2, record(7))));
        assert_eq!(page.read(2), None);
        assert_eq!(page.publish(record(9)), 4);
        assert_eq!(page.read(2), Some((4, record(9))));
    }

    #[test]
    fn a_record_being_written_is_not_read() {
        let words = words();
        let page = ParkPage::new(&words);
        page.publish(record(3));
        words[EVENT_PAGE_GENERATION].store(3, Ordering::Relaxed);
        assert_eq!(page.read(0), None);
        assert_eq!(page.read(2), None);
    }

    #[test]
    fn a_park_record_carries_its_target_or_zeroes() {
        let park = EventPark {
            edges: 4,
            hold_nanos: 5,
            target: ParkTarget::new(0x40, 0x41),
            at_resume: true,
            start: 0,
        };
        assert_eq!(
            PageRecord::of(park),
            PageRecord {
                edges: 4,
                hold_nanos: 5,
                target_start: 0x40,
                target_end: 0x41,
            }
        );
        assert_eq!(
            PageRecord::of(EventPark {
                target: None,
                ..park
            }),
            PageRecord {
                edges: 4,
                hold_nanos: 5,
                target_start: 0,
                target_end: 0,
            }
        );
    }

    #[test]
    fn a_concurrent_reader_sees_only_whole_records() {
        let rounds: u64 = if cfg!(miri) { 40 } else { 20_000 };
        let words = Arc::new(words());
        let writer = {
            let words = Arc::clone(&words);
            thread::spawn(move || {
                for value in 1..=rounds {
                    ParkPage::new(&words).publish(record(value));
                }
            })
        };
        let mut seen = 0;
        let mut last = 0;
        while last < rounds {
            if let Some((generation, read)) = ParkPage::new(&words).read(seen) {
                assert_eq!(generation % 2, 0);
                assert!(generation > seen);
                assert_eq!(read, record(read.edges));
                assert!(read.edges > last);
                assert_eq!(generation, read.edges * 2);
                seen = generation;
                last = read.edges;
            }
            thread::yield_now();
        }
        writer.join().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore = "memfd_create and mmap are foreign calls")]
    fn a_mapped_page_starts_at_generation_zero_and_is_shared_through_its_descriptor() {
        let mapping = PageMapping::create().unwrap();
        assert_eq!(mapping.page().generation(), 0);
        assert_eq!(mapping.page().publish(record(5)), 2);
        let path = format!("/proc/self/fd/{}", mapping.fd());
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(bytes.len(), process_proto::events::EVENT_PAGE_BYTES);
        let word =
            |index: usize| u64::from_ne_bytes(bytes[index * 8..index * 8 + 8].try_into().unwrap());
        assert_eq!(word(EVENT_PAGE_GENERATION), 2);
        assert_eq!(word(EVENT_PAGE_EDGES), 5);
        assert_eq!(word(EVENT_PAGE_TARGET_END), 6);
    }
}
