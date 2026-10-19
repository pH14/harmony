// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::{
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
    }
}
