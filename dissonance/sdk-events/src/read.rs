// SPDX-License-Identifier: AGPL-3.0-or-later
//! A forward-only, panic-free byte reader shared by the event decoder and the
//! catalog parser. Every read past end returns `None`; length-prefixed blobs are
//! bounds-checked against the real buffer before slicing, so an untrusted length
//! can never force an out-of-bounds read or an unbounded allocation.

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    /// A `u16`-length-prefixed byte blob.
    pub(crate) fn bytes_lp16(&mut self) -> Option<&'a [u8]> {
        let len = self.u16()? as usize;
        self.take(len)
    }

    pub(crate) fn at_end(&self) -> bool {
        self.pos == self.buf.len()
    }
}
