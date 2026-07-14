//! A minimal ELF64 reader: just enough to find a symbol and read the bytes
//! between two of them.
//!
//! Hand-rolled rather than pulled from a crate, for one reason: this is the tool
//! that decides whether a payload's counting window contains the branches the
//! oracle claims, and whether a guest kernel image contains a raw counter read.
//! It is small, it is on the trusted path of two acceptance gates, and it must not
//! panic on a malformed file. Every read is bounds-checked; there is no `unsafe`.

use std::collections::BTreeMap;
use thiserror::Error;

/// Why an ELF could not be read.
#[derive(Debug, Error)]
pub enum ElfError {
    /// Not an ELF64 little-endian aarch64 object.
    #[error("not an aarch64 ELF64 little-endian object")]
    NotAarch64Elf,
    /// A header or table ran past the end of the file.
    #[error("truncated ELF: {0}")]
    Truncated(&'static str),
    /// A symbol the caller needs is absent.
    #[error("symbol not found: {0}")]
    MissingSymbol(String),
    /// The requested address range is not backed by file contents.
    #[error("address range {start:#x}..{end:#x} is not in any loadable section")]
    RangeNotMapped {
        /// First address requested.
        start: u64,
        /// One past the last address requested.
        end: u64,
    },
    /// The window's end symbol is not after its start symbol.
    #[error("window {name}: end ({end:#x}) is not after start ({start:#x})")]
    BackwardsWindow {
        /// The window's name.
        name: String,
        /// Start address.
        start: u64,
        /// End address.
        end: u64,
    },
}

/// A section's placement.
#[derive(Clone, Debug)]
struct Section {
    addr: u64,
    offset: u64,
    size: u64,
    /// `SHT_NOBITS` (.bss) occupies addresses but no file bytes.
    nobits: bool,
}

/// A parsed ELF64 object.
#[derive(Debug)]
pub struct Elf {
    data: Vec<u8>,
    sections: Vec<Section>,
    /// Symbol name -> value. `BTreeMap`, not `HashMap`: nothing here may make an
    /// output depend on iteration order.
    symbols: BTreeMap<String, u64>,
}

/// Read a little-endian integer at `off`, or `None` if it would run past the end.
fn u16_at(d: &[u8], off: usize) -> Option<u16> {
    d.get(off..off + 2)?.try_into().ok().map(u16::from_le_bytes)
}

fn u32_at(d: &[u8], off: usize) -> Option<u32> {
    d.get(off..off + 4)?.try_into().ok().map(u32::from_le_bytes)
}

fn u64_at(d: &[u8], off: usize) -> Option<u64> {
    d.get(off..off + 8)?.try_into().ok().map(u64::from_le_bytes)
}

/// A NUL-terminated string starting at `off` in `d`.
fn str_at(d: &[u8], off: usize) -> Option<String> {
    let rest = d.get(off..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    String::from_utf8(rest[..end].to_vec()).ok()
}

impl Elf {
    /// Parse an ELF64 aarch64 little-endian object.
    ///
    /// # Errors
    /// [`ElfError::NotAarch64Elf`] if the magic, class, endianness or machine is
    /// wrong; [`ElfError::Truncated`] if a table runs past the end of the file.
    pub fn parse(data: Vec<u8>) -> Result<Elf, ElfError> {
        // e_ident: magic, class=ELFCLASS64(2), data=ELFDATA2LSB(1).
        if data.len() < 64
            || data.get(..4) != Some(&[0x7f, b'E', b'L', b'F'])
            || data.get(4) != Some(&2)
            || data.get(5) != Some(&1)
        {
            return Err(ElfError::NotAarch64Elf);
        }
        // e_machine == EM_AARCH64 (183).
        if u16_at(&data, 18) != Some(183) {
            return Err(ElfError::NotAarch64Elf);
        }

        let shoff = u64_at(&data, 0x28).ok_or(ElfError::Truncated("e_shoff"))? as usize;
        let shentsize = u16_at(&data, 0x3A).ok_or(ElfError::Truncated("e_shentsize"))? as usize;
        let shnum = u16_at(&data, 0x3C).ok_or(ElfError::Truncated("e_shnum"))? as usize;

        let mut sections = Vec::with_capacity(shnum);
        // Section-header fields we need: sh_type(4), sh_addr(0x10), sh_offset(0x18),
        // sh_size(0x20), sh_link(0x28).
        let mut raw = Vec::with_capacity(shnum);
        for i in 0..shnum {
            let base = shoff
                .checked_add(
                    i.checked_mul(shentsize)
                        .ok_or(ElfError::Truncated("shdr"))?,
                )
                .ok_or(ElfError::Truncated("shdr"))?;
            let sh_type = u32_at(&data, base + 4).ok_or(ElfError::Truncated("sh_type"))?;
            let addr = u64_at(&data, base + 0x10).ok_or(ElfError::Truncated("sh_addr"))?;
            let offset = u64_at(&data, base + 0x18).ok_or(ElfError::Truncated("sh_offset"))?;
            let size = u64_at(&data, base + 0x20).ok_or(ElfError::Truncated("sh_size"))?;
            let link = u32_at(&data, base + 0x28).ok_or(ElfError::Truncated("sh_link"))?;
            const SHT_NOBITS: u32 = 8;
            const SHT_SYMTAB: u32 = 2;
            sections.push(Section {
                addr,
                offset,
                size,
                nobits: sh_type == SHT_NOBITS,
            });
            raw.push((sh_type, offset, size, link));
            let _ = SHT_SYMTAB;
        }

        // Symbols: the .symtab section (SHT_SYMTAB=2); its sh_link names the
        // string table.
        let mut symbols = BTreeMap::new();
        for &(sh_type, offset, size, link) in &raw {
            if sh_type != 2 {
                continue;
            }
            let strtab = raw
                .get(link as usize)
                .ok_or(ElfError::Truncated("symtab sh_link"))?;
            let stroff = strtab.1 as usize;

            // Elf64_Sym is 24 bytes: st_name(u32), st_info(u8), st_other(u8),
            // st_shndx(u16), st_value(u64), st_size(u64).
            let count = (size / 24) as usize;
            for i in 0..count {
                let base = offset as usize + i * 24;
                let name_off = u32_at(&data, base).ok_or(ElfError::Truncated("st_name"))? as usize;
                let value = u64_at(&data, base + 8).ok_or(ElfError::Truncated("st_value"))?;
                if name_off == 0 {
                    continue;
                }
                if let Some(name) = str_at(&data, stroff + name_off) {
                    symbols.insert(name, value);
                }
            }
        }

        Ok(Elf {
            data,
            sections,
            symbols,
        })
    }

    /// The address of a symbol.
    ///
    /// # Errors
    /// [`ElfError::MissingSymbol`] if it is absent.
    pub fn symbol(&self, name: &str) -> Result<u64, ElfError> {
        self.symbols
            .get(name)
            .copied()
            .ok_or_else(|| ElfError::MissingSymbol(name.to_string()))
    }

    /// Whether a symbol exists.
    #[must_use]
    pub fn has_symbol(&self, name: &str) -> bool {
        self.symbols.contains_key(name)
    }

    /// The file bytes backing `[start, end)`.
    ///
    /// # Errors
    /// [`ElfError::RangeNotMapped`] if no single loadable, file-backed section
    /// covers the range.
    pub fn bytes(&self, start: u64, end: u64) -> Result<&[u8], ElfError> {
        if end < start {
            return Err(ElfError::RangeNotMapped { start, end });
        }
        for s in &self.sections {
            if s.nobits || s.size == 0 {
                continue;
            }
            let s_end = s.addr.saturating_add(s.size);
            if start >= s.addr && end <= s_end {
                let from = (s.offset + (start - s.addr)) as usize;
                let to = (s.offset + (end - s.addr)) as usize;
                return self
                    .data
                    .get(from..to)
                    .ok_or(ElfError::RangeNotMapped { start, end });
            }
        }
        Err(ElfError::RangeNotMapped { start, end })
    }

    /// The bytes of a `[__win_<name>_start, __win_<name>_end)` window.
    ///
    /// # Errors
    /// [`ElfError::MissingSymbol`] if either bracket is absent — which is itself a
    /// finding: a payload whose window symbols are gone cannot have its count
    /// verified, and must not be quietly skipped.
    pub fn window(&self, name: &str) -> Result<(u64, &[u8]), ElfError> {
        let sym = name.replace('-', "_");
        let start = self.symbol(&format!("__win_{sym}_start"))?;
        let end = self.symbol(&format!("__win_{sym}_end"))?;
        if end <= start {
            return Err(ElfError::BackwardsWindow {
                name: name.to_string(),
                start,
                end,
            });
        }
        Ok((start, self.bytes(start, end)?))
    }

    /// The bytes of a `[__vec_<name>_start, __vec_<name>_end)` handler region.
    ///
    /// # Errors
    /// As [`Elf::window`].
    pub fn handler(&self, name: &str) -> Result<(u64, &[u8]), ElfError> {
        let sym = name.replace('-', "_");
        let start = self.symbol(&format!("__vec_{sym}_start"))?;
        let end = self.symbol(&format!("__vec_{sym}_end"))?;
        if end <= start {
            return Err(ElfError::BackwardsWindow {
                name: name.to_string(),
                start,
                end,
            });
        }
        Ok((start, self.bytes(start, end)?))
    }

    /// Every file-backed, non-empty section's `(addr, bytes)` — the whole-image
    /// scan surface for AA-4's exclusives scan and AA-5's counter-read scan.
    #[must_use]
    pub fn loadable_ranges(&self) -> Vec<(u64, &[u8])> {
        self.sections
            .iter()
            .filter(|s| !s.nobits && s.size > 0 && s.addr != 0)
            .filter_map(|s| {
                let from = s.offset as usize;
                let to = from.checked_add(s.size as usize)?;
                Some((s.addr, self.data.get(from..to)?))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_elf_input_without_panicking() {
        assert!(matches!(Elf::parse(vec![]), Err(ElfError::NotAarch64Elf)));
        assert!(matches!(
            Elf::parse(vec![0; 64]),
            Err(ElfError::NotAarch64Elf)
        ));
        assert!(matches!(
            Elf::parse(
                b"\x7fELF not really an elf at all, padded out to sixty-four bytes......".to_vec()
            ),
            Err(ElfError::NotAarch64Elf)
        ));
    }

    #[test]
    fn rejects_a_truncated_header_without_panicking() {
        // Valid magic/class/endianness but nothing else. Must be an error, never a
        // panic: this parser runs over untrusted kernel images.
        let mut d = vec![0u8; 63];
        d[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        d[4] = 2;
        d[5] = 1;
        assert!(Elf::parse(d).is_err());
    }
}
