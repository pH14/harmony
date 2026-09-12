// SPDX-License-Identifier: AGPL-3.0-or-later
//! A minimal hand-rolled flattened-device-tree (FDT / DTB) writer (`tasks/112`
//! M3), plus a reader used only to prove the writer's output round-trips.
//!
//! Hand-rolled — no vetted FDT crate — to match the x86 hand-built-boot-struct
//! precedent (the ACPI/`boot_params` writers) and stay
//! inside the dependency whitelist (judgment call #2; a vetted `vm-fdt`-style
//! crate is an ask-by-comment if the foreman prefers). The DTB describes the
//! [`board`](super::board) memory map: the CPU (`psci` enable-method), memory,
//! the GICv3 (`arm,gic-v3`, distributor + redistributor), the PL011 console,
//! the generic timer (`arm,armv8-timer`, the PPIs), and a **reserved region for
//! the paravirt clock page** (the `hm-rk5` seam — reserved, not populated).
//!
//! **Everything in an FDT is big-endian**, regardless of the guest's byte order
//! — the format's own contract. The writer is deterministic (equal inputs ⇒
//! byte-identical output) and total; the reader never panics on arbitrary bytes.

use super::board::{CNTFRQ_HZ, GICD, GICR, PL011, PL011_SPI, RAM_BASE, VIRT_TIMER_INTID};

/// The fixed 64-byte AA-5/M1 bootloader seed, supplied unchanged to the M4
/// guest. The owned kernel credits all 512 bits before it can enter the
/// wall-time jitter harvester. Keeping the established M1 value is also part
/// of running the M1 guest-input oracle verbatim on the KVM substrate.
const BOOT_RNG_SEED: [u8; 64] = [
    0x48, 0x61, 0x72, 0x6d, 0x6f, 0x6e, 0x79, 0x2d, 0x41, 0x41, 0x35, 0x2d, 0x72, 0x6e, 0x67, 0x2d,
    0x73, 0x65, 0x65, 0x64, 0x2d, 0x76, 0x31, 0x2d, 0x64, 0x65, 0x74, 0x65, 0x72, 0x6d, 0x69, 0x6e,
    0x69, 0x73, 0x74, 0x69, 0x63, 0x2d, 0x66, 0x69, 0x78, 0x65, 0x64, 0x2d, 0x62, 0x79, 0x2d, 0x63,
    0x6f, 0x6e, 0x73, 0x74, 0x72, 0x75, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x2d, 0x30, 0x30, 0x30, 0x31,
];

/// FDT header magic (`0xd00dfeed`), stored big-endian at offset 0.
pub const FDT_MAGIC: u32 = 0xd00d_feed;
/// FDT format version this writer emits.
const FDT_VERSION: u32 = 17;
/// The last version this layout is backward-compatible with.
const FDT_LAST_COMP_VERSION: u32 = 16;

const FDT_BEGIN_NODE: u32 = 0x0000_0001;
const FDT_END_NODE: u32 = 0x0000_0002;
const FDT_PROP: u32 = 0x0000_0003;
const FDT_END: u32 = 0x0000_0009;

/// Interrupt-type cell value for an SPI (shared peripheral interrupt).
const GIC_SPI: u32 = 0;
/// Interrupt-type cell value for a PPI (private peripheral interrupt).
const GIC_PPI: u32 = 1;
/// Trigger flags cell: level-high (`IRQ_TYPE_LEVEL_HIGH`).
const IRQ_LEVEL_HIGH: u32 = 4;
/// The GIC's own phandle (referenced by every device's `interrupt-parent`).
const GIC_PHANDLE: u32 = 1;

/// A phandle-less GIC PPI number → its DT interrupt-cell `number` (PPIs are
/// numbered from 16 on the GIC but `GIC_PPI n` in the DT means INTID `16 + n`).
const fn ppi_dt_number(intid: u32) -> u32 {
    intid - 16
}

/// The FDT structure + strings builder. Nodes are opened/closed in order and
/// property names are interned into the strings block on first use.
struct Fdt {
    structure: Vec<u8>,
    strings: Vec<u8>,
    open_nodes: u32,
}

impl Fdt {
    fn new() -> Self {
        Self {
            structure: Vec::new(),
            strings: Vec::new(),
            open_nodes: 0,
        }
    }

    fn be32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_be_bytes());
    }

    /// Pad the structure block to a 4-byte boundary with zero bytes.
    fn pad4(&mut self) {
        while !self.structure.len().is_multiple_of(4) {
            self.structure.push(0);
        }
    }

    /// Intern a property name, returning its offset in the strings block.
    fn intern(&mut self, name: &str) -> u32 {
        let bytes = name.as_bytes();
        let mut i = 0;
        while i < self.strings.len() {
            let end = self.strings[i..]
                .iter()
                .position(|&b| b == 0)
                .map(|p| i + p)
                .unwrap_or(self.strings.len());
            if &self.strings[i..end] == bytes {
                return i as u32;
            }
            i = end + 1;
        }
        let off = self.strings.len() as u32;
        self.strings.extend_from_slice(bytes);
        self.strings.push(0);
        off
    }

    fn begin_node(&mut self, name: &str) {
        Self::be32(&mut self.structure, FDT_BEGIN_NODE);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        self.pad4();
        self.open_nodes += 1;
    }

    fn end_node(&mut self) {
        Self::be32(&mut self.structure, FDT_END_NODE);
        self.open_nodes -= 1;
    }

    fn prop_bytes(&mut self, name: &str, value: &[u8]) {
        let nameoff = self.intern(name);
        Self::be32(&mut self.structure, FDT_PROP);
        Self::be32(&mut self.structure, value.len() as u32);
        Self::be32(&mut self.structure, nameoff);
        self.structure.extend_from_slice(value);
        self.pad4();
    }

    fn prop_empty(&mut self, name: &str) {
        self.prop_bytes(name, &[]);
    }

    fn prop_str(&mut self, name: &str, value: &str) {
        let mut v = value.as_bytes().to_vec();
        v.push(0);
        self.prop_bytes(name, &v);
    }

    fn prop_u32(&mut self, name: &str, value: u32) {
        self.prop_bytes(name, &value.to_be_bytes());
    }

    fn prop_cells(&mut self, name: &str, cells: &[u32]) {
        let mut v = Vec::with_capacity(cells.len() * 4);
        for &c in cells {
            v.extend_from_slice(&c.to_be_bytes());
        }
        self.prop_bytes(name, &v);
    }
}

/// Emit a `reg`/address pair as two 32-bit cells (`#address-cells = 2`,
/// `#size-cells = 2`): a 64-bit value split hi:lo.
fn u64_cells(v: u64) -> [u32; 2] {
    [(v >> 32) as u32, v as u32]
}

/// Build the DTB for a guest with `ram_len` bytes of RAM at [`RAM_BASE`] and
/// the reserved paravirt-clock page at `pvclock_gpa`. Deterministic and total.
///
/// `bootargs` is the guest kernel command line (empty is fine for the
/// skeleton). The returned bytes are a complete, aligned FDT.
pub fn build(ram_len: u64, pvclock_gpa: u64, bootargs: &str) -> Vec<u8> {
    build_inner(ram_len, pvclock_gpa, bootargs, None)
}

/// Build the Linux DTB variant with an external initramfs range. The end is
/// exclusive, matching Linux's `linux,initrd-end` binding.
pub(crate) fn build_with_initrd(
    ram_len: u64,
    pvclock_gpa: u64,
    bootargs: &str,
    initrd_start: u64,
    initrd_end: u64,
) -> Vec<u8> {
    build_inner(
        ram_len,
        pvclock_gpa,
        bootargs,
        Some((initrd_start, initrd_end)),
    )
}

fn build_inner(
    ram_len: u64,
    pvclock_gpa: u64,
    bootargs: &str,
    initrd: Option<(u64, u64)>,
) -> Vec<u8> {
    let mut f = Fdt::new();

    f.begin_node("");
    f.prop_u32("#address-cells", 2);
    f.prop_u32("#size-cells", 2);
    f.prop_str("compatible", "linux,dummy-virt");
    f.prop_str("model", "harmony-arm64-virt");

    f.begin_node("chosen");
    f.prop_str("stdout-path", "/pl011@9000000");
    f.prop_str("bootargs", bootargs);
    f.prop_bytes("rng-seed", &BOOT_RNG_SEED);
    if let Some((start, end)) = initrd {
        f.prop_bytes("linux,initrd-start", &start.to_be_bytes());
        f.prop_bytes("linux,initrd-end", &end.to_be_bytes());
    }
    f.end_node();

    f.begin_node("psci");
    f.prop_str("compatible", "arm,psci-1.0");
    f.prop_str("method", "hvc");
    f.end_node();

    f.begin_node("cpus");
    f.prop_u32("#address-cells", 1);
    f.prop_u32("#size-cells", 0);
    f.begin_node("cpu@0");
    f.prop_str("device_type", "cpu");
    f.prop_str("compatible", "arm,armv8");
    f.prop_str("enable-method", "psci");
    f.prop_u32("reg", 0);
    f.end_node();
    f.end_node();

    f.begin_node("memory@40000000");
    f.prop_str("device_type", "memory");
    {
        let mut reg = Vec::new();
        reg.extend_from_slice(&u64_cells(RAM_BASE).map(u32::to_be_bytes).concat());
        reg.extend_from_slice(&u64_cells(ram_len).map(u32::to_be_bytes).concat());
        f.prop_bytes("reg", &reg);
    }
    f.end_node();

    f.begin_node("reserved-memory");
    f.prop_u32("#address-cells", 2);
    f.prop_u32("#size-cells", 2);
    f.prop_empty("ranges");
    f.begin_node(&format!("pvclock@{pvclock_gpa:x}"));
    f.prop_str("compatible", "harmony,pvclock-page");
    {
        let mut reg = Vec::new();
        reg.extend_from_slice(&u64_cells(pvclock_gpa).map(u32::to_be_bytes).concat());
        reg.extend_from_slice(&u64_cells(0x1000).map(u32::to_be_bytes).concat());
        f.prop_bytes("reg", &reg);
    }
    f.end_node();
    f.end_node();

    f.begin_node("intc@8000000");
    f.prop_str("compatible", "arm,gic-v3");
    f.prop_u32("#interrupt-cells", 3);
    f.prop_empty("interrupt-controller");
    f.prop_u32("#address-cells", 2);
    f.prop_u32("#size-cells", 2);
    f.prop_empty("ranges");
    f.prop_u32("phandle", GIC_PHANDLE);
    {
        let mut reg = Vec::new();
        for (base, len) in [GICD, GICR] {
            reg.extend_from_slice(&u64_cells(base).map(u32::to_be_bytes).concat());
            reg.extend_from_slice(&u64_cells(len).map(u32::to_be_bytes).concat());
        }
        f.prop_bytes("reg", &reg);
    }
    f.end_node();

    f.begin_node("timer");
    f.prop_str("compatible", "arm,armv8-timer");
    f.prop_u32("interrupt-parent", GIC_PHANDLE);
    f.prop_cells(
        "interrupts",
        &[
            GIC_PPI,
            13,
            IRQ_LEVEL_HIGH,
            GIC_PPI,
            14,
            IRQ_LEVEL_HIGH,
            GIC_PPI,
            ppi_dt_number(VIRT_TIMER_INTID),
            IRQ_LEVEL_HIGH,
            GIC_PPI,
            10,
            IRQ_LEVEL_HIGH,
        ],
    );
    f.prop_u32("clock-frequency", CNTFRQ_HZ as u32);
    f.end_node();

    f.begin_node("pl011@9000000");
    f.prop_str("compatible", "arm,pl011");
    f.prop_u32("interrupt-parent", GIC_PHANDLE);
    f.prop_cells("interrupts", &[GIC_SPI, PL011_SPI, IRQ_LEVEL_HIGH]);
    {
        let (base, len) = PL011;
        let mut reg = Vec::new();
        reg.extend_from_slice(&u64_cells(base).map(u32::to_be_bytes).concat());
        reg.extend_from_slice(&u64_cells(len).map(u32::to_be_bytes).concat());
        f.prop_bytes("reg", &reg);
    }
    f.end_node();

    f.end_node();
    debug_assert_eq!(f.open_nodes, 0, "every node closed");
    Fdt::be32(&mut f.structure, FDT_END);

    assemble(&f.structure, &f.strings)
}

/// Assemble the header + memory-reservation block + structure + strings into a
/// complete FDT. The layout (all big-endian): header (40 bytes), an empty
/// reservation block (one `{0,0}` terminator), the structure block, the
/// strings block.
fn assemble(structure: &[u8], strings: &[u8]) -> Vec<u8> {
    const HEADER_LEN: usize = 40;
    let off_mem_rsvmap = HEADER_LEN;
    let rsvmap = [0u8; 16];
    let off_dt_struct = off_mem_rsvmap + rsvmap.len();
    let off_dt_strings = off_dt_struct + structure.len();
    let totalsize = off_dt_strings + strings.len();

    let mut out = Vec::with_capacity(totalsize);
    let be = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_be_bytes());
    be(&mut out, FDT_MAGIC);
    be(&mut out, totalsize as u32);
    be(&mut out, off_dt_struct as u32);
    be(&mut out, off_dt_strings as u32);
    be(&mut out, off_mem_rsvmap as u32);
    be(&mut out, FDT_VERSION);
    be(&mut out, FDT_LAST_COMP_VERSION);
    be(&mut out, 0);
    be(&mut out, strings.len() as u32);
    be(&mut out, structure.len() as u32);
    debug_assert_eq!(out.len(), HEADER_LEN);
    out.extend_from_slice(&rsvmap);
    out.extend_from_slice(structure);
    out.extend_from_slice(strings);
    out
}

/// Errors from [`parse`] — a malformed FDT is a value, never a panic (rule #4).
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum FdtError {
    /// The buffer is shorter than the header or a claimed section runs past it.
    #[error("truncated FDT")]
    Truncated,
    /// The header magic is not [`FDT_MAGIC`].
    #[error("bad FDT magic")]
    BadMagic,
    /// A structure-block token was not one of the defined tokens, or nodes were
    /// unbalanced.
    #[error("malformed FDT structure")]
    Malformed,
}

/// A parsed device tree as a flat list of `(depth, name)` nodes and a lookup
/// of `(node_path_tail, prop_name) -> bytes`, enough for the round-trip test to
/// assert structure and read back specific properties.
#[derive(Debug, Default)]
pub struct ParsedFdt {
    /// Every node's name, in document order (root is `""`).
    pub nodes: Vec<String>,
    /// `(node_name, prop_name) -> value bytes`.
    pub props: Vec<(String, String, Vec<u8>)>,
}

impl ParsedFdt {
    /// The value of property `prop` on the first node named `node`.
    pub fn prop(&self, node: &str, prop: &str) -> Option<&[u8]> {
        self.props
            .iter()
            .find(|(n, p, _)| n == node && p == prop)
            .map(|(_, _, v)| v.as_slice())
    }
}

fn be32_at(buf: &[u8], off: usize) -> Result<u32, FdtError> {
    let end = off.checked_add(4).ok_or(FdtError::Truncated)?;
    let b = buf.get(off..end).ok_or(FdtError::Truncated)?;
    Ok(u32::from_be_bytes(b.try_into().expect("4-byte slice")))
}

/// Parse an FDT produced by [`build`]. Validates the magic and walks the
/// structure block, collecting node names and properties.
///
/// # Errors
/// [`FdtError`] for any malformed input.
pub fn parse(fdt: &[u8]) -> Result<ParsedFdt, FdtError> {
    if be32_at(fdt, 0)? != FDT_MAGIC {
        return Err(FdtError::BadMagic);
    }
    let off_dt_struct = be32_at(fdt, 8)? as usize;
    let off_dt_strings = be32_at(fdt, 12)? as usize;
    let size_dt_struct = be32_at(fdt, 36)? as usize;
    let struct_end = off_dt_struct
        .checked_add(size_dt_struct)
        .ok_or(FdtError::Truncated)?;
    if struct_end > fdt.len() || off_dt_strings > fdt.len() {
        return Err(FdtError::Truncated);
    }
    let strings = &fdt[off_dt_strings..];

    let node_block = &fdt[..struct_end];
    let be32_s = |off: usize| -> Result<u32, FdtError> {
        let end = off.checked_add(4).ok_or(FdtError::Truncated)?;
        if end > struct_end {
            return Err(FdtError::Truncated);
        }
        let b = fdt.get(off..end).ok_or(FdtError::Truncated)?;
        Ok(u32::from_be_bytes(b.try_into().expect("4-byte slice")))
    };

    let read_cstr = |buf: &[u8], start: usize| -> Result<(String, usize), FdtError> {
        let rel = buf
            .get(start..)
            .ok_or(FdtError::Truncated)?
            .iter()
            .position(|&b| b == 0)
            .ok_or(FdtError::Malformed)?;
        let s = String::from_utf8_lossy(&buf[start..start + rel]).into_owned();
        Ok((s, start + rel + 1))
    };

    let mut out = ParsedFdt::default();
    let mut pos = off_dt_struct;
    let mut stack: Vec<String> = Vec::new();
    loop {
        if pos >= struct_end {
            return Err(FdtError::Malformed);
        }
        let token = be32_s(pos)?;
        pos += 4;
        match token {
            FDT_BEGIN_NODE => {
                let (name, next) = read_cstr(node_block, pos)?;
                pos = (next + 3) & !3;
                out.nodes.push(name.clone());
                stack.push(name);
            }
            FDT_END_NODE => {
                stack.pop().ok_or(FdtError::Malformed)?;
            }
            FDT_PROP => {
                let len = be32_s(pos)? as usize;
                let nameoff = be32_s(pos + 4)? as usize;
                pos += 8;
                let vend = pos.checked_add(len).ok_or(FdtError::Truncated)?;
                if vend > struct_end {
                    return Err(FdtError::Truncated);
                }
                let value = fdt.get(pos..vend).ok_or(FdtError::Truncated)?.to_vec();
                pos = (vend + 3) & !3;
                let (pname, _) = read_cstr(strings, nameoff)?;
                let node = stack.last().cloned().unwrap_or_default();
                out.props.push((node, pname, value));
            }
            0x0000_0004 => {}
            FDT_END => {
                if !stack.is_empty() {
                    return Err(FdtError::Malformed);
                }
                return Ok(out);
            }
            _ => return Err(FdtError::Malformed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        build(0x2000_0000, RAM_BASE + 0x0101_0000, "console=ttyAMA0")
    }

    #[test]
    fn header_is_well_formed() {
        let dtb = sample();
        assert_eq!(be32_at(&dtb, 0).unwrap(), FDT_MAGIC);
        assert_eq!(be32_at(&dtb, 4).unwrap() as usize, dtb.len());
        assert_eq!(be32_at(&dtb, 20).unwrap(), FDT_VERSION);
        assert_eq!(be32_at(&dtb, 24).unwrap(), FDT_LAST_COMP_VERSION);
    }

    /// The pvclock GPA `sample()` builds with (RAM_BASE + 0x0101_0000).
    const SAMPLE_PVCLOCK_GPA: u64 = RAM_BASE + 0x0101_0000;

    #[test]
    fn round_trips_structure_and_properties() {
        let dtb = sample();
        let p = parse(&dtb).unwrap();
        let pvclock_node = format!("pvclock@{SAMPLE_PVCLOCK_GPA:x}");
        assert_eq!(p.nodes.first().map(String::as_str), Some(""));
        for n in [
            "chosen",
            "psci",
            "cpus",
            "cpu@0",
            "memory@40000000",
            "reserved-memory",
            pvclock_node.as_str(),
            "intc@8000000",
            "timer",
            "pl011@9000000",
        ] {
            assert!(p.nodes.iter().any(|x| x == n), "missing node {n}");
        }
        assert_eq!(
            p.prop("chosen", "stdout-path").unwrap(),
            b"/pl011@9000000\0"
        );
        assert_eq!(
            p.prop("chosen", "rng-seed").unwrap(),
            BOOT_RNG_SEED,
            "the guest CRNG must receive the fixed seed the owned kernel contract requires"
        );
        assert_eq!(p.prop("psci", "method").unwrap(), b"hvc\0");
        assert_eq!(
            p.prop("intc@8000000", "compatible").unwrap(),
            b"arm,gic-v3\0"
        );
        assert_eq!(
            p.prop("pl011@9000000", "compatible").unwrap(),
            b"arm,pl011\0"
        );
        assert_eq!(p.prop("intc@8000000", "reg").unwrap().len(), 2 * 4 * 4);
        assert!(p.prop(&pvclock_node, "no-map").is_none());
        assert_eq!(
            p.prop(&pvclock_node, "compatible").unwrap(),
            b"harmony,pvclock-page\0"
        );
        let reg = p.prop(&pvclock_node, "reg").unwrap();
        let reg_addr = u64::from_be_bytes(reg[0..8].try_into().unwrap());
        assert_eq!(reg_addr, SAMPLE_PVCLOCK_GPA);
        assert_eq!(pvclock_node, format!("pvclock@{reg_addr:x}"));
        assert_eq!(p.prop("reserved-memory", "ranges").unwrap(), b"");
        assert_eq!(
            p.prop("reserved-memory", "#address-cells").unwrap().len(),
            4
        );
        assert_eq!(p.prop("reserved-memory", "#size-cells").unwrap().len(), 4);
    }

    #[test]
    fn build_is_deterministic() {
        assert_eq!(sample(), sample());
    }

    #[test]
    fn linux_initramfs_range_round_trips_as_two_address_cells() {
        let start = RAM_BASE + 0x0200_0000;
        let end = start + 0x0012_3456;
        let dtb = build_with_initrd(
            0x2000_0000,
            SAMPLE_PVCLOCK_GPA,
            "console=ttyAMA0",
            start,
            end,
        );
        let parsed = parse(&dtb).unwrap();
        assert_eq!(
            parsed.prop("chosen", "linux,initrd-start").unwrap(),
            start.to_be_bytes()
        );
        assert_eq!(
            parsed.prop("chosen", "linux,initrd-end").unwrap(),
            end.to_be_bytes()
        );
    }

    #[test]
    fn parse_never_panics_on_arbitrary_prefixes() {
        let dtb = sample();
        for n in 0..dtb.len() {
            let _ = parse(&dtb[..n]);
        }
        let mut bad = dtb.clone();
        bad[0] ^= 0xFF;
        assert_eq!(parse(&bad).unwrap_err(), FdtError::BadMagic);
    }

    /// Review r6, case 1: a `size_dt_struct` that stops **short of** the real
    /// `FDT_END` (here, exactly the trailing terminator token is dropped from
    /// the declared block) must fail closed — the loop is bounded by
    /// `struct_end`, so it never walks past the declared block to find a
    /// terminator that the header says isn't there.
    #[test]
    fn parse_rejects_size_dt_struct_short_of_fdt_end() {
        let mut dtb = sample();
        let full = be32_at(&dtb, 36).unwrap();
        dtb[36..40].copy_from_slice(&(full - 4).to_be_bytes());
        let err = parse(&dtb).unwrap_err();
        assert!(
            matches!(err, FdtError::Malformed | FdtError::Truncated),
            "size_dt_struct short of FDT_END must fail closed, got {err:?}"
        );
    }

    /// Review r6, case 2: tokens that live **past** the declared struct block
    /// must not be decoded. The block below has no `FDT_END` inside it; the
    /// `FDT_END_NODE`/`FDT_END` that would "complete" the tree sit in the gap
    /// after `struct_end`. An unbounded loop would consume those out-of-block
    /// bytes and accept the FDT; the bounded loop rejects it. The identical
    /// bytes parse once the declaration is made honest — proving the rejection
    /// is the boundary check, not malformed tokens.
    #[test]
    fn parse_rejects_tokens_outside_the_declared_struct_block() {
        let tok = |v: u32| v.to_be_bytes();
        let mut block = Vec::new();
        block.extend_from_slice(&tok(FDT_BEGIN_NODE));
        block.extend_from_slice(&[0, 0, 0, 0]);
        block.extend_from_slice(&tok(FDT_PROP));
        block.extend_from_slice(&tok(4));
        block.extend_from_slice(&tok(0));
        block.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let mut trailing = Vec::new();
        trailing.extend_from_slice(&tok(FDT_END_NODE));
        trailing.extend_from_slice(&tok(FDT_END));
        let strings = b"p\0";

        let bounded = raw_fdt(&block, &trailing, strings, block.len() as u32);
        assert!(
            parse(&bounded).is_err(),
            "tokens past the declared struct block must not complete the tree"
        );

        let honest = raw_fdt(
            &block,
            &trailing,
            strings,
            (block.len() + trailing.len()) as u32,
        );
        let p = parse(&honest).expect("an honest declaration of the same bytes parses");
        assert_eq!(p.prop("", "p").unwrap(), &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    /// Assemble a raw FDT with the header written **verbatim**, so
    /// `size_dt_struct` may deliberately disagree with where the structure's
    /// terminator actually sits (which `build`/`assemble` never do). Layout:
    /// header(40) · rsvmap(16) · `struct_block` · `trailing` · `strings`.
    fn raw_fdt(
        struct_block: &[u8],
        trailing: &[u8],
        strings: &[u8],
        size_dt_struct: u32,
    ) -> Vec<u8> {
        const HEADER_LEN: u32 = 40;
        let rsvmap = [0u8; 16];
        let off_dt_struct = HEADER_LEN + rsvmap.len() as u32;
        let off_dt_strings = off_dt_struct + struct_block.len() as u32 + trailing.len() as u32;
        let totalsize = off_dt_strings + strings.len() as u32;
        let mut out = Vec::new();
        let be = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_be_bytes());
        be(&mut out, FDT_MAGIC);
        be(&mut out, totalsize);
        be(&mut out, off_dt_struct);
        be(&mut out, off_dt_strings);
        be(&mut out, HEADER_LEN);
        be(&mut out, FDT_VERSION);
        be(&mut out, FDT_LAST_COMP_VERSION);
        be(&mut out, 0);
        be(&mut out, strings.len() as u32);
        be(&mut out, size_dt_struct);
        out.extend_from_slice(&rsvmap);
        out.extend_from_slice(struct_block);
        out.extend_from_slice(trailing);
        out.extend_from_slice(strings);
        out
    }
}
