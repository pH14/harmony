// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    error::Error,
    fmt,
    io::{self, Write},
    mem::size_of,
};

#[derive(Debug)]
pub struct ImageLimit;
impl fmt::Display for ImageLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("checkpoint image exceeds its byte limit")
    }
}
impl Error for ImageLimit {}

pub struct ImageBudget {
    limit: usize,
    bytes: usize,
    shared: BTreeMap<(&'static str, usize), usize>,
}

impl ImageBudget {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            bytes: 0,
            shared: BTreeMap::new(),
        }
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn remaining(&self) -> usize {
        self.limit - self.bytes
    }
    pub fn reserve(&mut self, bytes: usize) -> Result<(), Box<dyn Error>> {
        let total = self.bytes.checked_add(bytes).ok_or(ImageLimit)?;
        if total > self.limit {
            return Err(ImageLimit.into());
        }
        self.bytes = total;
        Ok(())
    }
    pub fn array<T>(&mut self, capacity: usize) -> Result<(), Box<dyn Error>> {
        self.reserve(capacity.checked_mul(size_of::<T>()).ok_or(ImageLimit)?)
    }
    pub fn shared(
        &mut self,
        domain: &'static str,
        identity: usize,
        bytes: usize,
    ) -> Result<bool, Box<dyn Error>> {
        if let Some(previous) = self.shared.get(&(domain, identity)) {
            if *previous != bytes {
                return Err("checkpoint shared allocation changed size".into());
            }
            return Ok(false);
        }
        self.reserve(
            bytes
                .checked_add(map_charge::<(&'static str, usize), usize>()?)
                .ok_or(ImageLimit)?,
        )?;
        self.shared.insert((domain, identity), bytes);
        Ok(true)
    }
    pub fn map_entry<K, V>(&mut self) -> Result<(), Box<dyn Error>> {
        self.reserve(map_charge::<K, V>()?)
    }
}

fn map_charge<K, V>() -> Result<usize, ImageLimit> {
    size_of::<(K, V)>()
        .checked_mul(12)
        .and_then(|bytes| bytes.checked_add(256))
        .ok_or(ImageLimit)
}

pub(crate) struct LimitedBytes {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}
impl LimitedBytes {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }
    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
    pub(crate) fn exceeded(&self) -> bool {
        self.exceeded
    }
    pub(crate) fn capture(&mut self) -> impl Write + '_ {
        CaptureBytes(self)
    }
}

struct CaptureBytes<'a>(&'a mut LimitedBytes);

impl Write for CaptureBytes<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.exceeded() {
            return Ok(bytes.len());
        }
        match self.0.write(bytes) {
            Err(error)
                if self.0.exceeded()
                    && error
                        .get_ref()
                        .is_some_and(|inner| inner.is::<ImageLimit>()) =>
            {
                Ok(bytes.len())
            }
            result => result,
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
impl Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let len = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other(ImageLimit))?;
        if len > self.limit {
            self.exceeded = true;
            return Err(io::Error::other(ImageLimit));
        }
        if len > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .max(1024)
                .saturating_mul(2)
                .min(self.limit)
                .max(len);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(io::Error::other)?;
            if self.bytes.capacity() > self.limit {
                self.exceeded = true;
                return Err(io::Error::other(ImageLimit));
            }
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct ByteCount(pub(crate) usize);
impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other(ImageLimit))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct StreamedBytes<F>(pub F);

pub(crate) fn write_streamed_bytes(
    out: &mut dyn Write,
    write: impl Fn(&mut dyn Write) -> Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let failure = RefCell::new(None);
    let result = postcard::to_io(
        &StreamedBytes(|out: &mut dyn Write| match write(out) {
            Ok(()) => Ok(()),
            Err(error) => {
                *failure.borrow_mut() = Some(error);
                Err("checkpoint stream callback failed".into())
            }
        }),
        out,
    )
    .map(|_| ());
    match failure.into_inner() {
        Some(error) => Err(error),
        None => Ok(result?),
    }
}

impl<F> Serialize for StreamedBytes<F>
where
    F: Fn(&mut dyn Write) -> Result<(), Box<dyn Error>>,
{
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut count = ByteCount::default();
        (self.0)(&mut count).map_err(serde::ser::Error::custom)?;
        let mut sequence = serializer.serialize_seq(Some(count.0))?;
        struct Bytes<'a, S> {
            sequence: &'a mut S,
            remaining: usize,
        }
        impl<S: SerializeSeq> Write for Bytes<'_, S> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if bytes.len() > self.remaining {
                    return Err(io::Error::other("checkpoint byte stream grew"));
                }
                for byte in bytes {
                    self.sequence
                        .serialize_element(byte)
                        .map_err(|e| io::Error::other(e.to_string()))?;
                }
                self.remaining -= bytes.len();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut bytes = Bytes {
            sequence: &mut sequence,
            remaining: count.0,
        };
        (self.0)(&mut bytes).map_err(serde::ser::Error::custom)?;
        if bytes.remaining != 0 {
            return Err(serde::ser::Error::custom("checkpoint byte stream shrank"));
        }
        sequence.end()
    }
}

pub struct Sequence<I>(pub I);
impl<I> Serialize for Sequence<I>
where
    I: Clone + Iterator,
    I::Item: Serialize,
{
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.clone().count()))?;
        for value in self.0.clone() {
            sequence.serialize_element(&value)?;
        }
        sequence.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_budget_checks_bounds_before_insertion_and_separates_allocation_domains() {
        let mut budget = ImageBudget::new(4096);
        assert!(budget.shared("page", 7, 128).unwrap());
        let first = budget.bytes();
        assert!(!budget.shared("page", 7, 128).unwrap());
        assert_eq!(budget.bytes(), first);
        assert!(budget.shared("page", 7, 129).is_err());
        assert_eq!(budget.bytes(), first);
        assert!(budget.shared("history", 7, 128).unwrap());
        let before = budget.bytes();
        assert!(budget.reserve(usize::MAX).is_err());
        assert!(budget.array::<u64>(usize::MAX).is_err());
        assert_eq!(budget.bytes(), before);
        budget.reserve(budget.remaining()).unwrap();
        assert!(budget.reserve(1).is_err());
        assert_eq!(budget.bytes(), 4096);
    }
    #[test]
    fn streaming_bytes_preserve_the_postcard_vector_contract_without_materializing_it() {
        let value = vec![0_u8, 1, 127, 128, 255];
        let expected = postcard::to_allocvec(&value).unwrap();
        let stream = StreamedBytes(|out: &mut dyn Write| {
            out.write_all(&value)?;
            Ok(())
        });
        assert_eq!(postcard::to_allocvec(&stream).unwrap(), expected);
        let mut out = LimitedBytes::new(expected.len());
        postcard::to_io(&stream, &mut out).unwrap();
        assert_eq!(out.finish(), expected);
        let mut out = LimitedBytes::new(expected.len() - 1);
        assert!(postcard::to_io(&stream, &mut out).is_err());
        assert!(out.bytes.len() <= out.limit);
    }

    #[test]
    fn streamed_writer_preserves_vector_bytes_and_uncaptured_postcard_sink_errors() {
        let value = [0_u8, 1, 127, 128, 255];
        let expected = postcard::to_allocvec(&value.to_vec()).unwrap();
        let mut out = Vec::new();
        write_streamed_bytes(&mut out, |out| {
            out.write_all(&value)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(out, expected);

        let baseline = postcard::to_io(
            &StreamedBytes(|out: &mut dyn Write| {
                out.write_all(&value)?;
                Ok(())
            }),
            LimitedBytes::new(0),
        )
        .err()
        .unwrap();
        let calls = std::cell::Cell::new(0);
        let error = write_streamed_bytes(&mut LimitedBytes::new(0), |out| {
            calls.set(calls.get() + 1);
            out.write_all(&value)?;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(calls.get(), 1);
        assert_eq!(error.downcast_ref::<postcard::Error>(), Some(&baseline));
    }

    #[test]
    fn capture_discards_overflow_without_growing_or_changing_strict_sink_behavior() {
        let mut bytes = LimitedBytes::new(8);
        bytes.capture().write_all(&[1; 3]).unwrap();
        bytes.capture().write_all(&[2; 5]).unwrap();
        assert!(!bytes.exceeded());
        let capacity = bytes.bytes.capacity();
        assert!(capacity <= 8);
        bytes.capture().write_all(&[3]).unwrap();
        assert!(bytes.exceeded());
        bytes.capture().write_all(&[4; 1024]).unwrap();
        bytes.capture().flush().unwrap();
        assert_eq!(bytes.bytes.capacity(), capacity);
        assert_eq!(bytes.finish(), [1, 1, 1, 2, 2, 2, 2, 2]);

        let mut strict = LimitedBytes::new(8);
        let error = strict.write_all(&[5; 9]).unwrap_err();
        assert!(error.get_ref().unwrap().is::<ImageLimit>());
        assert!(strict.exceeded());
        assert!(strict.bytes.is_empty());
    }
}
