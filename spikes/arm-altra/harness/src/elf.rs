// SPDX-License-Identifier: AGPL-3.0-or-later
//! A minimal ELF64 reader: just enough to find a symbol, read the bytes between two
//! of them, load an image into guest RAM, and enumerate an image's executable code.
//!
//! Hand-rolled rather than pulled from a crate, for one reason: this is the tool
//! that decides whether a payload's counting window contains the branches the oracle
//! claims, and whether a guest kernel image contains a raw counter read or an LL/SC
//! exclusive. It is small, it is on the trusted path of two acceptance gates, and
//! it runs over **untrusted input** — a vendor kernel image is not this program's to
//! trust.
//!
//! Two properties therefore hold, and both are tested:
//!
//! 1. **It never panics.** Every offset that comes from the file is combined with
//!    `checked_add`, and every read is bounds-checked. A header claiming
//!    `e_shoff = u64::MAX` returns [`ElfError::Truncated`]; it does not overflow in
//!    debug or wrap into a plausible garbage parse in release. There is no `unsafe`.
//! 2. **It fails closed.** An image that yields no executable scan surface is an
//!    *error*, not a clean scan — because for AA-5 "a clean scan of the shipped
//!    guest kernel *is* the enforcement" (there is no ECV trap behind it), and a
//!    scanner that reports "found 0 raw counter reads" on an image it could not read
//!    would be an instrument that goes green without measuring the thing.

use std::collections::BTreeMap;
use thiserror::Error;

/// Why an ELF could not be read.
#[derive(Debug, Error)]
pub enum ElfError {
    /// Not an ELF64 little-endian aarch64 object.
    #[error("not an aarch64 ELF64 little-endian object")]
    NotAarch64Elf,
    /// A header or table ran past the end of the file (or its offsets overflowed).
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
    /// The image contains no executable bytes this reader can find.
    ///
    /// **Fail closed.** A stripped image (no section table, `PT_LOAD` segments only)
    /// used to yield an empty scan surface, and an empty surface scanned clean — so
    /// the AA-4 exclusives scan and the AA-5 counter-read scan would both pass an
    /// image they had not read a byte of. That is a failure to scan, and it is
    /// reported as one.
    #[error(
        "no executable scan surface: the image has no executable PT_LOAD segment and no \
         executable section. A scan of nothing is not a clean scan"
    )]
    NoExecutableSurface,
}

/// A section's placement.
#[derive(Clone, Debug)]
struct Section {
    addr: u64,
    offset: u64,
    size: u64,
    /// `SHF_EXECINSTR`.
    executable: bool,
    /// `SHT_NOBITS` (.bss) occupies addresses but no file bytes.
    nobits: bool,
}

/// A `PT_LOAD` program header — how the image is actually loaded, and the scan
/// surface that survives stripping.
#[derive(Clone, Debug)]
struct Segment {
    vaddr: u64,
    offset: u64,
    file_size: u64,
    mem_size: u64,
    /// `PF_X`.
    executable: bool,
}

/// One loadable segment, resolved to the bytes behind it.
#[derive(Clone, Copy, Debug)]
pub struct LoadSegment<'a> {
    /// The address the segment is linked at.
    pub vaddr: u64,
    /// How many bytes it occupies in memory (`p_memsz`) — larger than `bytes` for
    /// `.bss`, whose tail the loader zeroes.
    pub mem_size: usize,
    /// The file bytes backing it (`p_filesz`).
    pub bytes: &'a [u8],
}

/// A parsed ELF64 object.
#[derive(Debug)]
pub struct Elf {
    data: Vec<u8>,
    sections: Vec<Section>,
    segments: Vec<Segment>,
    /// Symbol name -> value. `BTreeMap`, not `HashMap`: nothing here may make an
    /// output depend on iteration order.
    symbols: BTreeMap<String, u64>,
    entry: u64,
}

/// `base + off`, or a truncation error.
///
/// Every header-field read goes through this. It is the fix for a real panic: an
/// aarch64 ELF header with `e_shoff = u64::MAX` reached `base + 4` and overflowed
/// (debug) or wrapped and mis-parsed garbage as section headers (release).
fn at(base: usize, off: usize) -> Result<usize, ElfError> {
    base.checked_add(off)
        .ok_or(ElfError::Truncated("header field offset overflowed"))
}

/// A file offset, as `usize`, or a truncation error naming the field.
fn offset(value: u64, what: &'static str) -> Result<usize, ElfError> {
    usize::try_from(value).map_err(|_| ElfError::Truncated(what))
}

/// Read a little-endian integer at `off`, or `None` if it would run past the end
/// (or the offset arithmetic overflows).
fn u16_at(d: &[u8], off: usize) -> Option<u16> {
    let end = off.checked_add(2)?;
    d.get(off..end)?.try_into().ok().map(u16::from_le_bytes)
}

fn u32_at(d: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    d.get(off..end)?.try_into().ok().map(u32::from_le_bytes)
}

fn u64_at(d: &[u8], off: usize) -> Option<u64> {
    let end = off.checked_add(8)?;
    d.get(off..end)?.try_into().ok().map(u64::from_le_bytes)
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
    /// wrong; [`ElfError::Truncated`] if a table runs past the end of the file or
    /// its offsets overflow. Never panics, whatever the bytes say.
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

        let entry = u64_at(&data, 0x18).ok_or(ElfError::Truncated("e_entry"))?;
        let segments = parse_segments(&data)?;
        let (sections, symbols) = parse_sections(&data)?;

        Ok(Elf {
            data,
            sections,
            segments,
            symbols,
            entry,
        })
    }

    /// The image's entry point (`e_entry`) — where the harness sets `PC`.
    #[must_use]
    pub fn entry(&self) -> u64 {
        self.entry
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
                // Checked throughout: `s.offset` is a file-supplied u64, and
                // `offset + (start - addr)` is exactly where an adversarial section
                // header would overflow.
                let from = s
                    .offset
                    .checked_add(start - s.addr)
                    .and_then(|o| usize::try_from(o).ok())
                    .ok_or(ElfError::RangeNotMapped { start, end })?;
                let to = s
                    .offset
                    .checked_add(end - s.addr)
                    .and_then(|o| usize::try_from(o).ok())
                    .ok_or(ElfError::RangeNotMapped { start, end })?;
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
        self.bracketed("__win", name)
    }

    /// The bytes of a `[__vec_<name>_start, __vec_<name>_end)` handler region.
    ///
    /// # Errors
    /// As [`Elf::window`].
    pub fn handler(&self, name: &str) -> Result<(u64, &[u8]), ElfError> {
        self.bracketed("__vec", name)
    }

    /// The bytes between a `<prefix>_<name>_start` / `_end` symbol pair.
    fn bracketed(&self, prefix: &str, name: &str) -> Result<(u64, &[u8]), ElfError> {
        let sym = name.replace('-', "_");
        let start = self.symbol(&format!("{prefix}_{sym}_start"))?;
        let end = self.symbol(&format!("{prefix}_{sym}_end"))?;
        if end <= start {
            return Err(ElfError::BackwardsWindow {
                name: name.to_string(),
                start,
                end,
            });
        }
        Ok((start, self.bytes(start, end)?))
    }

    /// The image's loadable segments, resolved to their file bytes — what the KVM
    /// harness copies into guest RAM.
    #[must_use]
    pub fn load_segments(&self) -> Vec<LoadSegment<'_>> {
        self.segments
            .iter()
            .filter_map(|s| {
                let from = usize::try_from(s.offset).ok()?;
                let len = usize::try_from(s.file_size).ok()?;
                let to = from.checked_add(len)?;
                Some(LoadSegment {
                    vaddr: s.vaddr,
                    mem_size: usize::try_from(s.mem_size).ok()?,
                    bytes: self.data.get(from..to)?,
                })
            })
            .collect()
    }

    /// Copy the image's loadable segments into a guest-RAM buffer whose physical base
    /// is `ram_base`, zeroing each segment's `.bss`-style tail.
    ///
    /// This is the **memory-safety-critical** half of loading a payload, and it lives
    /// here — in safe, portable, Miri-reachable code operating on a `&mut [u8]` — on
    /// purpose. The KVM harness (`sys::machine`) is Linux-only and its mmap/ioctl
    /// paths cannot run under the interpreter, so the bounds arithmetic that decides
    /// whether a copy stays in range is factored out to where Miri *can* check it: it
    /// drives this against an in-process `Vec` instead of an mmap. A malformed ELF
    /// with `p_filesz > p_memsz` is the reason the span below takes the **max** of the
    /// file and memory sizes — bounding only by `mem_size` would let the
    /// `copy_from_slice` write `bytes.len()` past the end.
    ///
    /// Every write is a checked slice operation, so there is no `unsafe` and no way to
    /// write out of bounds: an over-range segment returns [`ElfError::RangeNotMapped`]
    /// rather than corrupting memory.
    ///
    /// # Errors
    /// [`ElfError::RangeNotMapped`] if any segment falls below `ram_base` or its span
    /// does not fit in `dst`.
    pub fn load_into(&self, dst: &mut [u8], ram_base: u64) -> Result<(), ElfError> {
        for seg in self.load_segments() {
            let offset = seg
                .vaddr
                .checked_sub(ram_base)
                .and_then(|o| usize::try_from(o).ok())
                .ok_or(ElfError::RangeNotMapped {
                    start: seg.vaddr,
                    end: seg.vaddr,
                })?;
            // Bound by the bytes actually WRITTEN: the file-bytes copy is `bytes.len()`,
            // the zeroed tail extends to `mem_size`. `p_filesz > p_memsz` is legal in a
            // malformed ELF, so take the larger of the two.
            let span = seg.mem_size.max(seg.bytes.len());
            let end = offset
                .checked_add(span)
                .filter(|end| *end <= dst.len())
                .ok_or(ElfError::RangeNotMapped {
                    start: seg.vaddr,
                    end: seg.vaddr.saturating_add(span as u64),
                })?;
            let file_end = offset + seg.bytes.len();
            dst[offset..file_end].copy_from_slice(seg.bytes);
            dst[file_end..end].fill(0);
        }
        Ok(())
    }

    /// Every **executable** byte range of the image, in address order — the whole-image
    /// scan surface for AA-4's exclusives scan and AA-5's counter-read scan.
    ///
    /// Program headers first: a stripped image (no section table, executable
    /// `PT_LOAD` segments only — which real distro and vendor kernel images routinely
    /// are) still has a scan surface, and it is exactly this one. Sections are the
    /// refinement used when the image has them and no loadable segments (a relocatable
    /// object, which is what an unlinked payload is).
    ///
    /// # Errors
    /// [`ElfError::NoExecutableSurface`] when the image yields no executable bytes at
    /// all. That is the fail-closed half: a scan of nothing may never be reported as
    /// a clean scan.
    pub fn executable_ranges(&self) -> Result<Vec<(u64, &[u8])>, ElfError> {
        let mut ranges: Vec<(u64, &[u8])> = self
            .segments
            .iter()
            .filter(|s| s.executable && s.file_size > 0)
            .filter_map(|s| {
                let from = usize::try_from(s.offset).ok()?;
                let len = usize::try_from(s.file_size).ok()?;
                let to = from.checked_add(len)?;
                Some((s.vaddr, self.data.get(from..to)?))
            })
            .collect();

        if ranges.is_empty() {
            ranges = self
                .sections
                .iter()
                .filter(|s| s.executable && !s.nobits && s.size > 0)
                .filter_map(|s| {
                    let from = usize::try_from(s.offset).ok()?;
                    let len = usize::try_from(s.size).ok()?;
                    let to = from.checked_add(len)?;
                    Some((s.addr, self.data.get(from..to)?))
                })
                .collect();
        }

        if ranges.is_empty() {
            return Err(ElfError::NoExecutableSurface);
        }
        ranges.sort_by_key(|&(addr, _)| addr);
        Ok(ranges)
    }
}

/// Parse the program headers: the `PT_LOAD` segments.
fn parse_segments(data: &[u8]) -> Result<Vec<Segment>, ElfError> {
    const PT_LOAD: u32 = 1;
    const PF_X: u32 = 1;

    let phoff = u64_at(data, 0x20).ok_or(ElfError::Truncated("e_phoff"))?;
    let phentsize = u16_at(data, 0x36).ok_or(ElfError::Truncated("e_phentsize"))? as usize;
    let phnum = u16_at(data, 0x38).ok_or(ElfError::Truncated("e_phnum"))? as usize;
    if phoff == 0 || phnum == 0 {
        return Ok(Vec::new());
    }
    let phoff = offset(phoff, "e_phoff")?;

    let mut segments = Vec::with_capacity(phnum);
    for i in 0..phnum {
        // Elf64_Phdr: p_type(0), p_flags(4), p_offset(8), p_vaddr(0x10),
        // p_paddr(0x18), p_filesz(0x20), p_memsz(0x28).
        let base = at(
            phoff,
            i.checked_mul(phentsize)
                .ok_or(ElfError::Truncated("phdr index"))?,
        )?;
        let p_type = u32_at(data, base).ok_or(ElfError::Truncated("p_type"))?;
        let p_flags = u32_at(data, at(base, 4)?).ok_or(ElfError::Truncated("p_flags"))?;
        let p_offset = u64_at(data, at(base, 8)?).ok_or(ElfError::Truncated("p_offset"))?;
        let p_vaddr = u64_at(data, at(base, 0x10)?).ok_or(ElfError::Truncated("p_vaddr"))?;
        let p_filesz = u64_at(data, at(base, 0x20)?).ok_or(ElfError::Truncated("p_filesz"))?;
        let p_memsz = u64_at(data, at(base, 0x28)?).ok_or(ElfError::Truncated("p_memsz"))?;

        if p_type != PT_LOAD {
            continue;
        }
        segments.push(Segment {
            vaddr: p_vaddr,
            offset: p_offset,
            file_size: p_filesz,
            mem_size: p_memsz,
            executable: p_flags & PF_X != 0,
        });
    }
    Ok(segments)
}

/// Parse the section headers and the symbol table.
type Sections = (Vec<Section>, BTreeMap<String, u64>);

fn parse_sections(data: &[u8]) -> Result<Sections, ElfError> {
    const SHT_SYMTAB: u32 = 2;
    const SHT_NOBITS: u32 = 8;
    const SHF_EXECINSTR: u64 = 0x4;

    let shoff = u64_at(data, 0x28).ok_or(ElfError::Truncated("e_shoff"))?;
    let shentsize = u16_at(data, 0x3A).ok_or(ElfError::Truncated("e_shentsize"))? as usize;
    let shnum = u16_at(data, 0x3C).ok_or(ElfError::Truncated("e_shnum"))? as usize;
    if shoff == 0 || shnum == 0 {
        // A stripped image. Not an error here — `executable_ranges` is what decides
        // whether the image is scannable, and it reads the program headers.
        return Ok((Vec::new(), BTreeMap::new()));
    }
    let shoff = offset(shoff, "e_shoff")?;

    let mut sections = Vec::with_capacity(shnum);
    // (sh_type, sh_offset, sh_size, sh_link) of every section, for the symtab pass.
    let mut raw = Vec::with_capacity(shnum);
    for i in 0..shnum {
        // Elf64_Shdr: sh_name(0), sh_type(4), sh_flags(8), sh_addr(0x10),
        // sh_offset(0x18), sh_size(0x20), sh_link(0x28).
        let base = at(
            shoff,
            i.checked_mul(shentsize)
                .ok_or(ElfError::Truncated("shdr index"))?,
        )?;
        let sh_type = u32_at(data, at(base, 4)?).ok_or(ElfError::Truncated("sh_type"))?;
        let sh_flags = u64_at(data, at(base, 8)?).ok_or(ElfError::Truncated("sh_flags"))?;
        let addr = u64_at(data, at(base, 0x10)?).ok_or(ElfError::Truncated("sh_addr"))?;
        let offset = u64_at(data, at(base, 0x18)?).ok_or(ElfError::Truncated("sh_offset"))?;
        let size = u64_at(data, at(base, 0x20)?).ok_or(ElfError::Truncated("sh_size"))?;
        let link = u32_at(data, at(base, 0x28)?).ok_or(ElfError::Truncated("sh_link"))?;

        sections.push(Section {
            addr,
            offset,
            size,
            executable: sh_flags & SHF_EXECINSTR != 0,
            nobits: sh_type == SHT_NOBITS,
        });
        raw.push((sh_type, offset, size, link));
    }

    // Symbols: the .symtab section; its sh_link names the string table.
    let mut symbols = BTreeMap::new();
    for &(sh_type, sym_off, size, link) in &raw {
        if sh_type != SHT_SYMTAB {
            continue;
        }
        let strtab = raw
            .get(link as usize)
            .ok_or(ElfError::Truncated("symtab sh_link"))?;
        let stroff = offset(strtab.1, "strtab sh_offset")?;
        let sym_off = offset(sym_off, "symtab sh_offset")?;

        // Elf64_Sym is 24 bytes: st_name(u32), st_info(u8), st_other(u8),
        // st_shndx(u16), st_value(u64), st_size(u64).
        let count = usize::try_from(size / 24).map_err(|_| ElfError::Truncated("symtab size"))?;
        for i in 0..count {
            let base = at(
                sym_off,
                i.checked_mul(24)
                    .ok_or(ElfError::Truncated("symbol index"))?,
            )?;
            let name_off = u32_at(data, base).ok_or(ElfError::Truncated("st_name"))? as usize;
            let value = u64_at(data, at(base, 8)?).ok_or(ElfError::Truncated("st_value"))?;
            if name_off == 0 {
                continue;
            }
            if let Some(name) = str_at(data, at(stroff, name_off)?) {
                symbols.insert(name, value);
            }
        }
    }

    Ok((sections, symbols))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal, valid aarch64 ELF64 header with the given `e_shoff`/`e_phoff`
    /// fields — the shape an adversarial (or merely corrupt) image has.
    fn header(shoff: u64, shnum: u16, phoff: u64, phnum: u16) -> Vec<u8> {
        let mut d = vec![0u8; 64];
        d[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        d[4] = 2; // ELFCLASS64
        d[5] = 1; // ELFDATA2LSB
        d[16..18].copy_from_slice(&2u16.to_le_bytes()); // e_type = ET_EXEC
        d[18..20].copy_from_slice(&183u16.to_le_bytes()); // EM_AARCH64
        d[0x18..0x20].copy_from_slice(&0x4008_0000u64.to_le_bytes()); // e_entry
        d[0x20..0x28].copy_from_slice(&phoff.to_le_bytes()); // e_phoff
        d[0x28..0x30].copy_from_slice(&shoff.to_le_bytes()); // e_shoff
        d[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        d[0x38..0x3A].copy_from_slice(&phnum.to_le_bytes()); // e_phnum
        d[0x3A..0x3C].copy_from_slice(&64u16.to_le_bytes()); // e_shentsize
        d[0x3C..0x3E].copy_from_slice(&shnum.to_le_bytes()); // e_shnum
        d
    }

    /// A stripped image: program headers only, no section table — one executable
    /// `PT_LOAD` segment carrying `code`.
    fn stripped_image(code: &[u8], executable: bool) -> Vec<u8> {
        one_segment_image(
            0x4008_0000,
            code,
            code.len() as u64,
            code.len() as u64,
            executable,
        )
    }

    /// A stripped image with a single `PT_LOAD` segment, with fully controllable
    /// `p_vaddr`, file bytes, `p_filesz` and `p_memsz` — so a malformed
    /// `p_filesz > p_memsz` (or an out-of-range vaddr) can be constructed on purpose.
    fn one_segment_image(
        vaddr: u64,
        code: &[u8],
        p_filesz: u64,
        p_memsz: u64,
        executable: bool,
    ) -> Vec<u8> {
        const PHOFF: u64 = 64;
        const CODE_OFF: u64 = 64 + 56;
        let mut d = header(0, 0, PHOFF, 1);
        let mut ph = vec![0u8; 56];
        ph[0..4].copy_from_slice(&1u32.to_le_bytes()); // p_type = PT_LOAD
        let flags: u32 = if executable { 4 | 1 } else { 4 };
        ph[4..8].copy_from_slice(&flags.to_le_bytes());
        ph[8..16].copy_from_slice(&CODE_OFF.to_le_bytes()); // p_offset
        ph[16..24].copy_from_slice(&vaddr.to_le_bytes()); // p_vaddr
        ph[32..40].copy_from_slice(&p_filesz.to_le_bytes()); // p_filesz
        ph[40..48].copy_from_slice(&p_memsz.to_le_bytes()); // p_memsz
        d.extend_from_slice(&ph);
        d.extend_from_slice(code);
        d
    }

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

    #[test]
    fn an_absurd_section_offset_is_an_error_not_an_overflow_panic() {
        // The repro from the review: `e_shoff = u64::MAX` with a plausible header
        // reached `base + 4` and panicked with "attempt to add with overflow" in
        // debug — and in release it wrapped and parsed garbage as section headers.
        let d = header(u64::MAX, 1, 0, 0);
        assert!(matches!(Elf::parse(d), Err(ElfError::Truncated(_))));

        // The same shape one field over: a huge-but-not-maximal offset must land as
        // a bounds failure, not a wrap.
        let d = header(u64::MAX - 8, 4, 0, 0);
        assert!(matches!(Elf::parse(d), Err(ElfError::Truncated(_))));
    }

    #[test]
    fn an_absurd_program_header_offset_is_an_error_too() {
        let d = header(0, 0, u64::MAX, 3);
        assert!(matches!(Elf::parse(d), Err(ElfError::Truncated(_))));
    }

    #[test]
    fn a_section_count_that_overruns_the_file_is_an_error() {
        // shoff is inside the file, but 4096 sections of 64 bytes are not.
        let d = header(64, 4096, 0, 0);
        assert!(matches!(Elf::parse(d), Err(ElfError::Truncated(_))));
    }

    #[test]
    fn a_stripped_image_still_has_a_scan_surface() {
        // The AA-5 hole: a stripped image (no section table) used to yield an empty
        // `loadable_ranges()`, and `arm-scan counter-reads` then printed "found 0"
        // and exited 0 — a clean bill of health for an image it had not read.
        // `ret; nop`
        let code = [0xC0, 0x03, 0x5F, 0xD6, 0x1F, 0x20, 0x03, 0xD5];
        let elf = Elf::parse(stripped_image(&code, true)).expect("valid stripped ELF");
        assert!(elf.sections.is_empty(), "this fixture has no sections");

        let ranges = elf
            .executable_ranges()
            .expect("a stripped image is scannable");
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].0, 0x4008_0000);
        assert_eq!(ranges[0].1, &code);
    }

    #[test]
    fn an_image_with_no_executable_surface_fails_closed() {
        // A PT_LOAD that is not executable, and no sections: there is nothing to
        // scan, and "nothing" must never be reported as "clean".
        let elf = Elf::parse(stripped_image(&[0; 8], false)).expect("valid ELF");
        assert!(matches!(
            elf.executable_ranges(),
            Err(ElfError::NoExecutableSurface)
        ));
    }

    #[test]
    fn load_segments_resolve_to_their_bytes() {
        let code = [0xC0, 0x03, 0x5F, 0xD6];
        let elf = Elf::parse(stripped_image(&code, true)).expect("valid ELF");
        let segs = elf.load_segments();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].vaddr, 0x4008_0000);
        assert_eq!(segs[0].bytes, &code);
        assert_eq!(segs[0].mem_size, code.len());
        assert_eq!(elf.entry(), 0x4008_0000);
    }

    // `load_into` is the memory-safety-critical copy, factored out of the KVM harness
    // (whose mmap/ioctl paths Miri can't run) into safe code the interpreter CAN run.
    // These tests are the Miri-reachable seam the unsafe⇒Miri contract wants.

    #[test]
    fn load_into_places_a_segment_at_its_offset_and_zeroes_the_bss_tail() {
        let base = 0x4000_0000u64;
        // vaddr 0x40080000 → offset 0x80000; 4 code bytes, mem_size 8 → 4-byte tail.
        let code = [0xAA, 0xBB, 0xCC, 0xDD];
        let img = one_segment_image(0x4008_0000, &code, 4, 8, true);
        let elf = Elf::parse(img).expect("valid ELF");

        let mut ram = vec![0xFFu8; 0x0009_0000];
        elf.load_into(&mut ram, base).expect("fits");
        assert_eq!(&ram[0x80000..0x80004], &code, "file bytes copied");
        assert_eq!(&ram[0x80004..0x80008], &[0, 0, 0, 0], "bss tail zeroed");
        assert_eq!(ram[0x80008], 0xFF, "nothing written past the segment");
    }

    #[test]
    fn load_into_refuses_a_segment_below_ram_base() {
        // vaddr below the base underflows: it must error, not wrap to a huge offset.
        let img = one_segment_image(0x1000, &[0; 4], 4, 4, true);
        let elf = Elf::parse(img).expect("valid ELF");
        let mut ram = vec![0u8; 0x1000];
        assert!(matches!(
            elf.load_into(&mut ram, 0x4000_0000),
            Err(ElfError::RangeNotMapped { .. })
        ));
    }

    #[test]
    fn load_into_refuses_a_segment_that_overruns_the_buffer() {
        let img = one_segment_image(0x4000_0000, &[0; 16], 16, 16, true);
        let elf = Elf::parse(img).expect("valid ELF");
        let mut ram = vec![0u8; 8]; // too small for a 16-byte segment
        assert!(matches!(
            elf.load_into(&mut ram, 0x4000_0000),
            Err(ElfError::RangeNotMapped { .. })
        ));
    }

    #[test]
    fn a_malformed_p_filesz_greater_than_p_memsz_cannot_write_out_of_bounds() {
        // The P1 the review found: `p_filesz > p_memsz`. Bounding the copy by
        // `p_memsz` alone would let `copy_from_slice(bytes)` — which writes
        // `p_filesz` bytes — run past a destination the check thought was in range.
        // Placed at the very end of the buffer so any over-write is a real OOB that
        // Miri (and the checked slice op) would catch.
        let code = vec![0x42u8; 16]; // p_filesz = 16
        // mem_size claims only 4 bytes; the segment sits so its 4-byte mem span ends
        // exactly at the buffer end, but the 16 file bytes would overrun it.
        let ram_len = 0x8010usize;
        let vaddr = 0x4000_0000 + (ram_len as u64 - 4);
        let img = one_segment_image(vaddr, &code, 16, 4, true);
        let elf = Elf::parse(img).expect("valid ELF");
        let mut ram = vec![0u8; ram_len];
        // It must be refused (the span is bounded by max(file, mem) = 16, which does
        // not fit), NOT panic and NOT write past the buffer.
        assert!(matches!(
            elf.load_into(&mut ram, 0x4000_0000),
            Err(ElfError::RangeNotMapped { .. })
        ));
    }
}
