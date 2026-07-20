// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic arm64 Linux boot layout for the AA-5(c) spike.
//!
//! Linux boots from a flat `Image`, not an ELF. This module validates the
//! 64-byte arm64 Image header, places the kernel/initramfs/DTB without overlap,
//! and emits the small fixed-board DTB the spike's KVM machine implements.
//! Everything here is safe and total over untrusted artifact bytes; the Linux
//! syscall seam only supplies the resulting RAM slice to KVM.

/// Guest RAM begins at the managed params-page ABI address.
pub const RAM_BASE: u64 = oracle_model::PARAMS_GPA;
/// AA-5's Linux guest gets a separate 256 MiB slot; bare payloads remain 4 MiB.
pub const RAM_SIZE: usize = 256 << 20;
/// The fixed managed pvclock page used by the spike ABI.
pub const PVCLOCK_GPA: u64 = oracle_model::PVCLOCK_GPA;
/// ARM pvclock registration MMIO page (INTEGRATION §1.3).
pub const PVCLOCK_REGISTER_BASE: u64 = 0x0b00_0000;
/// Size of the reserved ARM pvclock registration MMIO surface.
pub const PVCLOCK_REGISTER_SIZE: u64 = 0x1000;
/// Dedicated, userspace-owned clockevent PPI. KVM owns the architected virtual timer's
/// default INTID 27, so the Harmony event must use a different private interrupt.
pub const HARMONY_CLOCKEVENT_PPI: u32 = 20;
/// Leave the lower 128 MiB for the kernel and its effective/BSS extent.
pub const INITRAMFS_GPA: u64 = RAM_BASE + 0x0800_0000;
/// Place the DTB 16 MiB below the end of the 256 MiB RAM bank.
pub const DTB_GPA: u64 = RAM_BASE + 0x0f00_0000;
/// Largest Image file the fixed layout will read before validation. The
/// effective Image extent must end before the initramfs placement (and the
/// pvclock overlap check is tighter for ordinary nonzero text offsets).
pub const MAX_IMAGE_BYTES: u64 = INITRAMFS_GPA - RAM_BASE - KERNEL_LOAD_OFFSET;
/// Largest initramfs file that can end before the fixed DTB placement.
pub const MAX_INITRAMFS_BYTES: u64 = DTB_GPA - INITRAMFS_GPA;

const PAGE: u64 = 0x1000;
const IMAGE_HEADER_LEN: usize = 64;
const IMAGE_MAGIC_OFFSET: usize = 56;
const IMAGE_TEXT_OFFSET: usize = 8;
const IMAGE_SIZE_OFFSET: usize = 16;
const IMAGE_FLAGS_OFFSET: usize = 24;
const IMAGE_MAGIC: u32 = 0x644d_5241; // little-endian "ARM\x64"
const MAX_BOOTARGS: usize = 4096;

const GICD_BASE: u64 = 0x0800_0000;
const GICD_SIZE: u64 = 0x0001_0000;
const GICR_BASE: u64 = 0x080a_0000;
const GICR_SIZE: u64 = 0x0002_0000;
const UART_BASE: u64 = 0x0900_0000;
const UART_SIZE: u64 = 0x1000;
const UART_SPI: u32 = 1;

/// Fixed board addresses, factored so small unit-test RAM can exercise layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BoardLayout {
    /// GPA of the RAM slice's first byte.
    pub ram_base: u64,
    /// Offset from `ram_base` at which the kernel image is placed (before the
    /// header's own `text_offset`). A real 6.18 `Image` carries `text_offset` 0,
    /// which would sit the kernel's first pages exactly over the reserved low
    /// pages (params, pvclock) — and the host publishes into the pvclock page
    /// live, so the kernel must load wholly above them.
    pub kernel_offset: u64,
    /// Reserved host-maintained clock page.
    pub pvclock_gpa: u64,
    /// One-shot guest-to-host pvclock registration MMIO page.
    pub pvclock_register_base: u64,
    /// Initramfs load address.
    pub initramfs_gpa: u64,
    /// DTB load address.
    pub dtb_gpa: u64,
}

/// Kernel placement offset on the real board: the arm64 boot protocol accepts
/// any 2 MiB-aligned base, and 2 MiB clears the reserved low pages.
pub const KERNEL_LOAD_OFFSET: u64 = 0x0020_0000;

/// The AA-5(c) board layout used on the Altra.
pub const BOARD: BoardLayout = BoardLayout {
    ram_base: RAM_BASE,
    kernel_offset: KERNEL_LOAD_OFFSET,
    pvclock_gpa: PVCLOCK_GPA,
    pvclock_register_base: PVCLOCK_REGISTER_BASE,
    initramfs_gpa: INITRAMFS_GPA,
    dtb_gpa: DTB_GPA,
};

/// Validated boot output consumed by the KVM entry-state writer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LoadedLinux {
    /// The kernel's first instruction.
    pub entry_gpa: u64,
    /// DTB address passed in `x0`.
    pub dtb_gpa: u64,
    /// Inclusive initramfs start advertised in `/chosen`.
    pub initramfs_start: u64,
    /// Exclusive initramfs end advertised in `/chosen`.
    pub initramfs_end: u64,
    /// Complete generated DTB bytes (also copied into RAM).
    pub dtb: Vec<u8>,
}

/// Refusals while loading untrusted Linux boot artifacts.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum LinuxBootError {
    /// The flat Image does not contain its fixed header.
    #[error("Image is shorter than the 64-byte arm64 header")]
    ImageTooShort,
    /// The header is not an arm64 Linux Image.
    #[error("Image magic {found:#010x} is not ARM64 magic {IMAGE_MAGIC:#010x}")]
    BadImageMagic {
        /// The word found at offset 56.
        found: u32,
    },
    /// The kernel advertises big-endian execution; the spike is LE-only.
    #[error("big-endian arm64 Image is unsupported")]
    BigEndianImage,
    /// The Image load offset must at least be page-aligned.
    #[error("Image text_offset {0:#x} is not page-aligned")]
    UnalignedTextOffset(u64),
    /// AA-5(c) needs an init process, so an empty initramfs is never a boot.
    #[error("initramfs is empty")]
    EmptyInitramfs,
    /// The command line is bounded before it reaches a DTB length field.
    #[error("bootargs length {len} exceeds the {MAX_BOOTARGS}-byte bound")]
    BootargsTooLong {
        /// Supplied byte length.
        len: usize,
    },
    /// Work time cannot advance while the sole vCPU sleeps in WFI. The owned kernel must
    /// use the generic idle poll loop so every pending exact-work target remains reachable.
    #[error("Linux bootargs must contain the exact `nohlt` token for work-clock idle polling")]
    MissingNohlt,
    /// One artifact address/extent overflowed or fell outside RAM.
    #[error(
        "{artifact} range [{start:#x}, {end:#x}) is outside RAM [{ram_start:#x}, {ram_end:#x})"
    )]
    OutsideRam {
        /// Artifact name.
        artifact: &'static str,
        /// Inclusive GPA.
        start: u64,
        /// Exclusive GPA (or `u64::MAX` after overflow).
        end: u64,
        /// RAM start GPA.
        ram_start: u64,
        /// RAM end GPA.
        ram_end: u64,
    },
    /// Two fixed regions overlap and would silently corrupt a boot artifact.
    #[error("{first} overlaps {second}")]
    Overlap {
        /// First region.
        first: &'static str,
        /// Second region.
        second: &'static str,
    },
}

#[derive(Clone, Copy)]
struct Region {
    name: &'static str,
    start: u64,
    end: u64,
}

impl Region {
    fn new(
        name: &'static str,
        start: u64,
        len: u64,
        ram_start: u64,
        ram_end: u64,
    ) -> Result<Self, LinuxBootError> {
        let end = start.checked_add(len).ok_or(LinuxBootError::OutsideRam {
            artifact: name,
            start,
            end: u64::MAX,
            ram_start,
            ram_end,
        })?;
        if start < ram_start || end > ram_end {
            return Err(LinuxBootError::OutsideRam {
                artifact: name,
                start,
                end,
                ram_start,
                ram_end,
            });
        }
        Ok(Self { name, start, end })
    }

    fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

fn read_u64(bytes: &[u8], off: usize) -> Result<u64, LinuxBootError> {
    let end = off.checked_add(8).ok_or(LinuxBootError::ImageTooShort)?;
    let value = bytes.get(off..end).ok_or(LinuxBootError::ImageTooShort)?;
    Ok(u64::from_le_bytes(
        value
            .try_into()
            .map_err(|_| LinuxBootError::ImageTooShort)?,
    ))
}

fn read_u32(bytes: &[u8], off: usize) -> Result<u32, LinuxBootError> {
    let end = off.checked_add(4).ok_or(LinuxBootError::ImageTooShort)?;
    let value = bytes.get(off..end).ok_or(LinuxBootError::ImageTooShort)?;
    Ok(u32::from_le_bytes(
        value
            .try_into()
            .map_err(|_| LinuxBootError::ImageTooShort)?,
    ))
}

/// Load the AA-5(c) board's Image + initramfs and generate/copy its DTB.
///
/// # Errors
/// [`LinuxBootError`] for malformed artifacts, overflow, out-of-RAM placement,
/// or any overlap. No untrusted field is used as an unchecked slice index.
pub fn load(
    image: &[u8],
    initramfs: &[u8],
    bootargs: &str,
    ram: &mut [u8],
) -> Result<LoadedLinux, LinuxBootError> {
    load_at(BOARD, image, initramfs, bootargs, ram)
}

/// The layout-parametric form used by bounded unit tests.
fn load_at(
    board: BoardLayout,
    image: &[u8],
    initramfs: &[u8],
    bootargs: &str,
    ram: &mut [u8],
) -> Result<LoadedLinux, LinuxBootError> {
    if image.len() < IMAGE_HEADER_LEN {
        return Err(LinuxBootError::ImageTooShort);
    }
    let magic = read_u32(image, IMAGE_MAGIC_OFFSET)?;
    if magic != IMAGE_MAGIC {
        return Err(LinuxBootError::BadImageMagic { found: magic });
    }
    let text_offset = read_u64(image, IMAGE_TEXT_OFFSET)?;
    let image_size = read_u64(image, IMAGE_SIZE_OFFSET)?;
    let flags = read_u64(image, IMAGE_FLAGS_OFFSET)?;
    if flags & 1 != 0 {
        return Err(LinuxBootError::BigEndianImage);
    }
    if !text_offset.is_multiple_of(PAGE) {
        return Err(LinuxBootError::UnalignedTextOffset(text_offset));
    }
    if initramfs.is_empty() {
        return Err(LinuxBootError::EmptyInitramfs);
    }
    if bootargs.len() > MAX_BOOTARGS {
        return Err(LinuxBootError::BootargsTooLong {
            len: bootargs.len(),
        });
    }
    if !bootargs.split_ascii_whitespace().any(|arg| arg == "nohlt") {
        return Err(LinuxBootError::MissingNohlt);
    }

    let ram_len = ram.len() as u64;
    let ram_end = board
        .ram_base
        .checked_add(ram_len)
        .ok_or(LinuxBootError::OutsideRam {
            artifact: "RAM",
            start: board.ram_base,
            end: u64::MAX,
            ram_start: board.ram_base,
            ram_end: u64::MAX,
        })?;
    let kernel_start = board
        .ram_base
        .checked_add(board.kernel_offset)
        .and_then(|base| {
            // The protocol places the image text_offset bytes past a 2 MiB-aligned base.
            base.is_multiple_of(0x0020_0000).then_some(base)
        })
        .and_then(|base| base.checked_add(text_offset))
        .ok_or(LinuxBootError::OutsideRam {
            artifact: "Image",
            start: board.ram_base,
            end: u64::MAX,
            ram_start: board.ram_base,
            ram_end,
        })?;
    let kernel_span = (image.len() as u64).max(image_size);
    let kernel = Region::new("Image", kernel_start, kernel_span, board.ram_base, ram_end)?;
    let pvclock = Region::new("pvclock", board.pvclock_gpa, PAGE, board.ram_base, ram_end)?;
    let initrd_region = Region::new(
        "initramfs",
        board.initramfs_gpa,
        initramfs.len() as u64,
        board.ram_base,
        ram_end,
    )?;

    let dtb = build_dtb(
        board,
        ram_len,
        initrd_region.start,
        initrd_region.end,
        bootargs,
    );
    let dtb_region = Region::new(
        "DTB",
        board.dtb_gpa,
        dtb.len() as u64,
        board.ram_base,
        ram_end,
    )?;
    for (a, b) in [
        (kernel, pvclock),
        (kernel, initrd_region),
        (kernel, dtb_region),
        (pvclock, initrd_region),
        (pvclock, dtb_region),
        (initrd_region, dtb_region),
    ] {
        if a.overlaps(b) {
            return Err(LinuxBootError::Overlap {
                first: a.name,
                second: b.name,
            });
        }
    }

    copy_to_ram(ram, board.ram_base, kernel.start, image, "Image", ram_end)?;
    copy_to_ram(
        ram,
        board.ram_base,
        initrd_region.start,
        initramfs,
        "initramfs",
        ram_end,
    )?;
    copy_to_ram(ram, board.ram_base, dtb_region.start, &dtb, "DTB", ram_end)?;

    Ok(LoadedLinux {
        entry_gpa: kernel.start,
        dtb_gpa: dtb_region.start,
        initramfs_start: initrd_region.start,
        initramfs_end: initrd_region.end,
        dtb,
    })
}

fn copy_to_ram(
    ram: &mut [u8],
    ram_base: u64,
    gpa: u64,
    bytes: &[u8],
    artifact: &'static str,
    ram_end: u64,
) -> Result<(), LinuxBootError> {
    let start_u64 = gpa
        .checked_sub(ram_base)
        .ok_or(LinuxBootError::OutsideRam {
            artifact,
            start: gpa,
            end: gpa.saturating_add(bytes.len() as u64),
            ram_start: ram_base,
            ram_end,
        })?;
    let start = usize::try_from(start_u64).map_err(|_| LinuxBootError::OutsideRam {
        artifact,
        start: gpa,
        end: gpa.saturating_add(bytes.len() as u64),
        ram_start: ram_base,
        ram_end,
    })?;
    let stop = start
        .checked_add(bytes.len())
        .ok_or(LinuxBootError::OutsideRam {
            artifact,
            start: gpa,
            end: u64::MAX,
            ram_start: ram_base,
            ram_end,
        })?;
    let dst = ram.get_mut(start..stop).ok_or(LinuxBootError::OutsideRam {
        artifact,
        start: gpa,
        end: gpa.saturating_add(bytes.len() as u64),
        ram_start: ram_base,
        ram_end,
    })?;
    dst.copy_from_slice(bytes);
    Ok(())
}

// ---- Deterministic FDT writer ---------------------------------------------

const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_END: u32 = 9;
const GIC_PHANDLE: u32 = 1;
const CLOCK_PHANDLE: u32 = 2;

struct Fdt {
    structure: Vec<u8>,
    strings: Vec<u8>,
}

impl Fdt {
    fn token(&mut self, value: u32) {
        self.structure.extend_from_slice(&value.to_be_bytes());
    }

    fn pad(&mut self) {
        while !self.structure.len().is_multiple_of(4) {
            self.structure.push(0);
        }
    }

    fn intern(&mut self, name: &str) -> u32 {
        let mut pos = 0usize;
        while pos < self.strings.len() {
            let Some(rel_end) = self.strings[pos..].iter().position(|b| *b == 0) else {
                break;
            };
            let end = pos + rel_end;
            if self.strings.get(pos..end) == Some(name.as_bytes()) {
                return pos as u32;
            }
            pos = end + 1;
        }
        let offset = self.strings.len() as u32;
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        offset
    }

    fn begin(&mut self, name: &str) {
        self.token(FDT_BEGIN_NODE);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        self.pad();
    }

    fn end(&mut self) {
        self.token(FDT_END_NODE);
    }

    fn prop(&mut self, name: &str, value: &[u8]) {
        let nameoff = self.intern(name);
        self.token(FDT_PROP);
        self.token(value.len() as u32);
        self.token(nameoff);
        self.structure.extend_from_slice(value);
        self.pad();
    }

    fn empty(&mut self, name: &str) {
        self.prop(name, &[]);
    }

    fn string(&mut self, name: &str, value: &str) {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        self.prop(name, &bytes);
    }

    fn strings(&mut self, name: &str, values: &[&str]) {
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        self.prop(name, &bytes);
    }

    fn u32(&mut self, name: &str, value: u32) {
        self.prop(name, &value.to_be_bytes());
    }

    fn u64(&mut self, name: &str, value: u64) {
        self.prop(name, &value.to_be_bytes());
    }

    fn cells(&mut self, name: &str, values: &[u32]) {
        let mut bytes = Vec::with_capacity(values.len() * 4);
        for value in values {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        self.prop(name, &bytes);
    }

    fn reg(&mut self, name: &str, ranges: &[(u64, u64)]) {
        let mut bytes = Vec::with_capacity(ranges.len() * 16);
        for (base, len) in ranges {
            bytes.extend_from_slice(&base.to_be_bytes());
            bytes.extend_from_slice(&len.to_be_bytes());
        }
        self.prop(name, &bytes);
    }
}

fn build_dtb(
    board: BoardLayout,
    ram_len: u64,
    initramfs_start: u64,
    initramfs_end: u64,
    bootargs: &str,
) -> Vec<u8> {
    let mut f = Fdt {
        structure: Vec::new(),
        strings: Vec::new(),
    };
    f.begin("");
    f.u32("#address-cells", 2);
    f.u32("#size-cells", 2);
    f.string("compatible", "linux,dummy-virt");
    f.string("model", "harmony-aa5-arm64");
    f.u32("interrupt-parent", GIC_PHANDLE);

    f.begin("chosen");
    f.string("stdout-path", "/pl011@9000000");
    f.string("bootargs", bootargs);
    f.u64("linux,initrd-start", initramfs_start);
    f.u64("linux,initrd-end", initramfs_end);
    f.end();

    f.begin("psci");
    f.string("compatible", "arm,psci-0.2");
    f.string("method", "hvc");
    f.end();

    f.begin("cpus");
    f.u32("#address-cells", 1);
    f.u32("#size-cells", 0);
    f.begin("cpu@0");
    f.string("device_type", "cpu");
    f.string("compatible", "arm,armv8");
    f.string("enable-method", "psci");
    f.u32("reg", 0);
    f.end();
    f.end();

    f.begin(&format!("memory@{:x}", board.ram_base));
    f.string("device_type", "memory");
    f.reg("reg", &[(board.ram_base, ram_len)]);
    f.end();

    f.begin("reserved-memory");
    f.u32("#address-cells", 2);
    f.u32("#size-cells", 2);
    f.empty("ranges");
    f.begin(&format!("pvclock@{:x}", board.pvclock_gpa));
    f.string("compatible", "harmony,pvclock-page");
    f.reg("reg", &[(board.pvclock_gpa, PAGE)]);
    // Reserve the host-owned page from the guest allocator while retaining its
    // normal linear-map alias. The arm64 clock reader is needed before the
    // general memremap allocator is available; `no-map` would make that early
    // reader impossible without a second architecture-specific fixmap.
    f.end();
    f.end();

    f.begin(&format!(
        "pvclock-register@{:x}",
        board.pvclock_register_base
    ));
    f.string("compatible", "harmony,pvclock-register-v1");
    f.reg(
        "reg",
        &[(board.pvclock_register_base, PVCLOCK_REGISTER_SIZE)],
    );
    f.end();

    f.begin("clock@0");
    f.string("compatible", "fixed-clock");
    f.u32("#clock-cells", 0);
    f.u32("clock-frequency", 24_000_000);
    f.u32("phandle", CLOCK_PHANDLE);
    f.end();

    f.begin("intc@8000000");
    f.string("compatible", "arm,gic-v3");
    f.u32("#interrupt-cells", 3);
    f.empty("interrupt-controller");
    f.u32("#address-cells", 2);
    f.u32("#size-cells", 2);
    f.empty("ranges");
    f.u32("phandle", GIC_PHANDLE);
    f.reg("reg", &[(GICD_BASE, GICD_SIZE), (GICR_BASE, GICR_SIZE)]);
    f.end();

    f.begin("timer");
    f.string("compatible", "arm,armv8-timer");
    f.cells(
        "interrupts",
        &[
            1,
            13,
            4, // secure physical PPI
            1,
            14,
            4, // non-secure physical PPI
            1,
            HARMONY_CLOCKEVENT_PPI - 16,
            4, // Harmony clockevent PPI (INTID 20)
            1,
            10,
            4, // hypervisor PPI
        ],
    );
    // Work time advances whenever the owned nohlt guest runs; the host's exact clockevent
    // publication/injection path therefore has no CPU-idle stop state.
    f.empty("always-on");
    // Do not advertise `clock-frequency`: on KVM the architected CNTFRQ_EL0
    // value is the timer frequency. A copied QEMU-board constant could disagree
    // with the live Altra counter and make Linux's clockevent conversion wrong.
    f.end();

    f.begin("pl011@9000000");
    // Linux's OF platform population recognizes this as an AMBA device only
    // with the PrimeCell fallback. Earlycon can print with the first string
    // alone, which would otherwise let a boot marker pass while the real
    // ttyAMA console driver never bound.
    f.strings("compatible", &["arm,pl011", "arm,primecell"]);
    f.reg("reg", &[(UART_BASE, UART_SIZE)]);
    f.cells("interrupts", &[0, UART_SPI, 4]);
    f.cells("clocks", &[CLOCK_PHANDLE, CLOCK_PHANDLE]);
    f.strings("clock-names", &["uartclk", "apb_pclk"]);
    f.end();

    f.end();
    f.token(FDT_END);

    const HEADER_LEN: usize = 40;
    let reserve = [0u8; 16];
    let off_mem_rsvmap = HEADER_LEN;
    let off_dt_struct = off_mem_rsvmap + reserve.len();
    let off_dt_strings = off_dt_struct + f.structure.len();
    let totalsize = off_dt_strings + f.strings.len();
    let mut out = Vec::with_capacity(totalsize);
    for value in [
        0xd00d_feed,
        totalsize as u32,
        off_dt_struct as u32,
        off_dt_strings as u32,
        off_mem_rsvmap as u32,
        17,
        16,
        0,
        f.strings.len() as u32,
        f.structure.len() as u32,
    ] {
        out.extend_from_slice(&value.to_be_bytes());
    }
    out.extend_from_slice(&reserve);
    out.extend_from_slice(&f.structure);
    out.extend_from_slice(&f.strings);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_BOARD: BoardLayout = BoardLayout {
        ram_base: 0x4000_0000,
        kernel_offset: 0,
        pvclock_gpa: 0x4000_1000,
        pvclock_register_base: PVCLOCK_REGISTER_BASE,
        initramfs_gpa: 0x4000_8000,
        dtb_gpa: 0x4000_c000,
    };

    fn image(text_offset: u64, image_size: u64, flags: u64, body: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; IMAGE_HEADER_LEN + body];
        bytes[IMAGE_TEXT_OFFSET..IMAGE_TEXT_OFFSET + 8].copy_from_slice(&text_offset.to_le_bytes());
        bytes[IMAGE_SIZE_OFFSET..IMAGE_SIZE_OFFSET + 8].copy_from_slice(&image_size.to_le_bytes());
        bytes[IMAGE_FLAGS_OFFSET..IMAGE_FLAGS_OFFSET + 8].copy_from_slice(&flags.to_le_bytes());
        bytes[IMAGE_MAGIC_OFFSET..IMAGE_MAGIC_OFFSET + 4]
            .copy_from_slice(&IMAGE_MAGIC.to_le_bytes());
        bytes[IMAGE_HEADER_LEN..].fill(0xa5);
        bytes
    }

    fn be32(bytes: &[u8], off: usize) -> u32 {
        u32::from_be_bytes(bytes[off..off + 4].try_into().unwrap())
    }

    fn cstr(bytes: &[u8], off: usize) -> (&str, usize) {
        let end = off + bytes[off..].iter().position(|b| *b == 0).unwrap();
        (std::str::from_utf8(&bytes[off..end]).unwrap(), end + 1)
    }

    type ParsedDtb = (
        Vec<String>,
        std::collections::BTreeMap<(String, String), Vec<u8>>,
    );

    fn parse_generated_dtb(dtb: &[u8]) -> ParsedDtb {
        let structure_start = be32(dtb, 8) as usize;
        let strings_start = be32(dtb, 12) as usize;
        let strings_len = be32(dtb, 32) as usize;
        let structure_len = be32(dtb, 36) as usize;
        let structure_end = structure_start + structure_len;
        let strings = &dtb[strings_start..strings_start + strings_len];
        let mut nodes = Vec::new();
        let mut props = std::collections::BTreeMap::new();
        let mut stack = Vec::new();
        let mut pos = structure_start;
        loop {
            let token = be32(dtb, pos);
            pos += 4;
            match token {
                FDT_BEGIN_NODE => {
                    let (name, next) = cstr(&dtb[..structure_end], pos);
                    nodes.push(name.to_string());
                    stack.push(name.to_string());
                    pos = (next + 3) & !3;
                }
                FDT_END_NODE => {
                    stack.pop().unwrap();
                }
                FDT_PROP => {
                    let len = be32(dtb, pos) as usize;
                    let nameoff = be32(dtb, pos + 4) as usize;
                    pos += 8;
                    let value = dtb[pos..pos + len].to_vec();
                    pos = (pos + len + 3) & !3;
                    let (name, _) = cstr(strings, nameoff);
                    props.insert((stack.last().cloned().unwrap(), name.to_string()), value);
                }
                FDT_END => {
                    assert!(stack.is_empty());
                    assert_eq!(pos, structure_end);
                    break;
                }
                other => panic!("unexpected generated FDT token {other}"),
            }
        }
        (nodes, props)
    }

    #[test]
    fn loads_all_artifacts_and_builds_a_deterministic_dtb() {
        let image = image(0x2000, 0, 0, 256);
        let initramfs = vec![0x5a; 1024];
        let mut first = vec![0u8; 0x1_0000];
        let loaded = load_at(
            TEST_BOARD,
            &image,
            &initramfs,
            "console=ttyAMA0 rdinit=/init nohlt",
            &mut first,
        )
        .unwrap();
        assert_eq!(loaded.entry_gpa, TEST_BOARD.ram_base + 0x2000);
        assert_eq!(loaded.dtb_gpa, TEST_BOARD.dtb_gpa);
        assert_eq!(loaded.initramfs_end, TEST_BOARD.initramfs_gpa + 1024);
        assert_eq!(&first[0x2000..0x2000 + image.len()], image.as_slice());
        assert_eq!(&first[0x8000..0x8400], initramfs.as_slice());
        assert_eq!(be32(&loaded.dtb, 0), 0xd00d_feed);
        assert_eq!(be32(&loaded.dtb, 4) as usize, loaded.dtb.len());
        assert_eq!(
            &first[0xc000..0xc000 + loaded.dtb.len()],
            loaded.dtb.as_slice()
        );

        let mut second = vec![0u8; 0x1_0000];
        let again = load_at(
            TEST_BOARD,
            &image,
            &initramfs,
            "console=ttyAMA0 rdinit=/init nohlt",
            &mut second,
        )
        .unwrap();
        assert_eq!(loaded, again);
        assert_eq!(first, second);
    }

    #[test]
    fn a_real_shape_image_loads_above_the_reserved_low_pages() {
        // A pinned 6.18 Image carries text_offset 0: without the board's kernel_offset
        // its footprint would cover the pvclock page the host publishes into. This is
        // the on-silicon 2026-07-20 finding, locked in both directions.
        let board = BoardLayout {
            ram_base: 0x4000_0000,
            kernel_offset: 0x0020_0000,
            pvclock_gpa: 0x4000_1000,
            pvclock_register_base: PVCLOCK_REGISTER_BASE,
            initramfs_gpa: 0x4000_0000 + 0x0040_0000,
            dtb_gpa: 0x4000_0000 + 0x0060_0000,
        };
        let image = image(0, 0x0018_0000, 0, 256);
        let initramfs = vec![0x5a; 512];
        let mut ram = vec![0u8; 0x0080_0000];
        let loaded = load_at(board, &image, &initramfs, "nohlt", &mut ram).unwrap();
        assert_eq!(loaded.entry_gpa, board.ram_base + 0x0020_0000);
        assert_eq!(
            &ram[0x0020_0000..0x0020_0000 + image.len()],
            image.as_slice()
        );

        // The same image against a kernel_offset-0 board must be REFUSED, not loaded
        // over the live-published page.
        let overlapping = BoardLayout {
            kernel_offset: 0,
            ..board
        };
        let err = load_at(overlapping, &image, &initramfs, "nohlt", &mut ram).unwrap_err();
        assert!(matches!(err, LinuxBootError::Overlap { .. }), "{err:?}");
    }

    #[test]
    fn generated_dtb_round_trips_every_load_bearing_linux_binding() {
        let image = image(0x2000, 0, 0, 16);
        let mut ram = vec![0u8; 0x1_0000];
        let loaded = load_at(
            TEST_BOARD,
            &image,
            &[0x5a; 128],
            "console=ttyAMA0 rdinit=/init nohlt",
            &mut ram,
        )
        .unwrap();
        let (nodes, props) = parse_generated_dtb(&loaded.dtb);
        for node in [
            "chosen",
            "psci",
            "cpu@0",
            "memory@40000000",
            "reserved-memory",
            "pvclock@40001000",
            "pvclock-register@b000000",
            "intc@8000000",
            "timer",
            "pl011@9000000",
        ] {
            assert!(nodes.iter().any(|found| found == node), "missing {node}");
        }
        let prop = |node: &str, name: &str| {
            props
                .get(&(node.to_string(), name.to_string()))
                .map(Vec::as_slice)
                .unwrap()
        };
        assert_eq!(prop("chosen", "stdout-path"), b"/pl011@9000000\0");
        assert_eq!(
            prop("chosen", "bootargs"),
            b"console=ttyAMA0 rdinit=/init nohlt\0"
        );
        assert_eq!(prop("psci", "compatible"), b"arm,psci-0.2\0");
        assert_eq!(prop("psci", "method"), b"hvc\0");
        assert_eq!(prop("intc@8000000", "compatible"), b"arm,gic-v3\0");
        assert_eq!(prop("intc@8000000", "reg").len(), 32);
        let timer_interrupts = prop("timer", "interrupts");
        assert_eq!(timer_interrupts.len(), 48);
        let timer_cells: Vec<u32> = timer_interrupts
            .chunks_exact(4)
            .map(|cell| u32::from_be_bytes(cell.try_into().unwrap()))
            .collect();
        assert_eq!(
            timer_cells,
            [
                1,
                13,
                4,
                1,
                14,
                4,
                1,
                HARMONY_CLOCKEVENT_PPI - 16,
                4,
                1,
                10,
                4
            ]
        );
        assert_eq!(prop("timer", "always-on"), b"");
        assert_eq!(
            prop("pl011@9000000", "compatible"),
            b"arm,pl011\0arm,primecell\0"
        );
        assert_eq!(prop("pl011@9000000", "clocks").len(), 8);
        assert_eq!(prop("reserved-memory", "ranges"), b"");
        assert!(
            !props.contains_key(&("pvclock@40001000".to_string(), "no-map".to_string())),
            "pvclock must remain reserved but linearly mapped for the early clock reader"
        );
        assert_eq!(prop("pvclock@40001000", "reg").len(), 16);
        assert_eq!(
            prop("pvclock-register@b000000", "compatible"),
            b"harmony,pvclock-register-v1\0"
        );
        assert_eq!(prop("pvclock-register@b000000", "reg").len(), 16);
    }

    #[test]
    fn rejects_truncation_bad_magic_endianness_and_unaligned_offset() {
        let mut ram = vec![0u8; 0x1_0000];
        let initramfs = [1u8];
        for len in 0..IMAGE_HEADER_LEN {
            assert_eq!(
                load_at(TEST_BOARD, &vec![0; len], &initramfs, "", &mut ram),
                Err(LinuxBootError::ImageTooShort)
            );
        }
        let mut bad = image(0x2000, 0, 0, 0);
        bad[IMAGE_MAGIC_OFFSET] ^= 1;
        assert!(matches!(
            load_at(TEST_BOARD, &bad, &initramfs, "", &mut ram),
            Err(LinuxBootError::BadImageMagic { .. })
        ));
        assert_eq!(
            load_at(
                TEST_BOARD,
                &image(0x2000, 0, 1, 0),
                &initramfs,
                "",
                &mut ram
            ),
            Err(LinuxBootError::BigEndianImage)
        );
        assert_eq!(
            load_at(
                TEST_BOARD,
                &image(0x2001, 0, 0, 0),
                &initramfs,
                "",
                &mut ram
            ),
            Err(LinuxBootError::UnalignedTextOffset(0x2001))
        );
    }

    #[test]
    fn rejects_every_overlap_and_out_of_bounds_extent() {
        let mut ram = vec![0u8; 0x1_0000];
        let initramfs = [1u8; 16];
        assert!(matches!(
            load_at(
                TEST_BOARD,
                &image(0, 0x3000, 0, 0),
                &initramfs,
                "nohlt",
                &mut ram
            ),
            Err(LinuxBootError::Overlap { .. })
        ));
        assert!(matches!(
            load_at(
                TEST_BOARD,
                &image(0x2000, 0x7000, 0, 0),
                &initramfs,
                "nohlt",
                &mut ram
            ),
            Err(LinuxBootError::Overlap { .. })
        ));
        assert!(matches!(
            load_at(
                TEST_BOARD,
                &image(0x2000, u64::MAX, 0, 0),
                &initramfs,
                "nohlt",
                &mut ram
            ),
            Err(LinuxBootError::OutsideRam { .. })
        ));
    }

    #[test]
    fn rejects_empty_initramfs_and_unbounded_bootargs() {
        let image = image(0x2000, 0, 0, 0);
        let mut ram = vec![0u8; 0x1_0000];
        assert_eq!(
            load_at(TEST_BOARD, &image, &[], "", &mut ram),
            Err(LinuxBootError::EmptyInitramfs)
        );
        assert!(matches!(
            load_at(
                TEST_BOARD,
                &image,
                &[1],
                &"x".repeat(MAX_BOOTARGS + 1),
                &mut ram
            ),
            Err(LinuxBootError::BootargsTooLong { .. })
        ));
        assert_eq!(
            load_at(TEST_BOARD, &image, &[1], "nohlty", &mut ram),
            Err(LinuxBootError::MissingNohlt)
        );
    }
}
