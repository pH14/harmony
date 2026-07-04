// SPDX-License-Identifier: AGPL-3.0-or-later
//! Direct 64-bit Linux boot loader (the Firecracker / cloud-hypervisor model) —
//! hands control to the kernel's **64-bit entry point** directly, with **no**
//! 16-bit real-mode / bzImage setup-code emulation (integrator ruling,
//! 2026-06-25).
//!
//! Given a `bzImage`, an `initramfs.cpio.gz`, the guest RAM size, and a kernel
//! command line, [`load`] writes everything the kernel needs into guest RAM and
//! returns a [`LinuxImage`] describing the 64-bit entry, the `boot_params`
//! ("zero page") GPA, the identity page-table root (`CR3`), the boot GDT, and the
//! loaded ranges. [`crate::entry::long_mode_entry`] turns those into the
//! architectural long-mode entry state; [`crate::bringup::boot_linux`] composes
//! the two over a backend.
//!
//! This is a **trust boundary** (conventions rule 4): the `image` and `initramfs`
//! are untrusted bytes, so every malformed input yields a [`LinuxLoadError`] —
//! never a panic, a slice-index out-of-bounds, or an arithmetic overflow. Every
//! read of the image is bounds-checked and every address computation is
//! `checked_*`; [`load`] is total over arbitrary `&[u8]`.
//!
//! ## What the x86/Linux 64-bit boot protocol requires
//!
//! (Documentation/arch/x86/boot.rst, "64-bit BOOT PROTOCOL".) The loader:
//! 1. parses the bzImage [`SetupHeader`] (at file offset [`SETUP_HEADER_OFFSET`] =
//!    `0x1f1`), checking `boot_flag == 0xAA55`, `header == "HdrS"`,
//!    `version >= 0x020c`, and the `XLF_KERNEL_64` bit of `xloadflags`;
//! 2. loads the **protected-mode kernel** (the bytes after `(setup_sects+1)*512`)
//!    at `pref_address`, with `init_size` bytes of run room;
//! 3. loads the **initramfs** high in RAM (page-aligned, below 4 GiB), recording
//!    its GPA/len in `hdr.ramdisk_image`/`ramdisk_size`;
//! 4. builds the **`boot_params`** zero page: a minimal E820 map over guest RAM
//!    ([`e820_entries`](BootParams) / [`e820_table`](BootParams)), the command
//!    line (`cmd_line_ptr`), the copied/filled `setup_header`, and
//!    `type_of_loader`;
//! 5. builds an **identity page table** (2 MiB pages over the first
//!    [`IDENTITY_MAP_BYTES`]) and a flat 64-bit **GDT** (`__BOOT_CS`=`0x10`,
//!    `__BOOT_DS`=`0x18`).
//!
//! The 64-bit entry is `load_addr + 0x200` ([`ENTRY_64_OFFSET`]); `RSI` must hold
//! the `boot_params` GPA.

use zerocopy::{FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout};

// ---------------------------------------------------------------------------
// Fixed low-RAM layout (all below 1 MiB, so it never collides with the
// protected-mode kernel, which loads at `pref_address >= 0x10_0000`).
// ---------------------------------------------------------------------------

/// GPA of the top-level identity page table (PML4) — also the `CR3` value.
pub const PML4_GPA: u64 = 0x1000;
/// GPA of the single page-directory-pointer table (PDPT).
pub const PDPT_GPA: u64 = 0x2000;
/// GPA of the single page directory (PD) backing the identity map.
pub const PD_GPA: u64 = 0x3000;
/// GPA of the boot GDT (`__BOOT_CS` / `__BOOT_DS`).
pub const GDT_GPA: u64 = 0x6000;
/// GPA of the `boot_params` "zero page" (a full 4 KiB struct).
pub const BOOT_PARAMS_GPA: u64 = 0x7000;
/// GPA of the kernel command-line string.
pub const CMDLINE_GPA: u64 = 0x8000;

/// Max command-line length (including the NUL terminator). 2 KiB — comfortably
/// within the page at [`CMDLINE_GPA`] and the kernel's default `cmdline_size`.
pub const CMDLINE_MAX: usize = 0x800;

/// Bytes of guest physical address space the boot page table identity-maps with
/// 2 MiB pages (the first 1 GiB). The kernel rebuilds its own page tables very
/// early, so this only has to cover the load region + zero page + cmdline + the
/// initramfs; 1 GiB covers any layout this loader produces for a multi-hundred-MiB
/// guest.
pub const IDENTITY_MAP_BYTES: u64 = 1 << 30;
/// 2 MiB large-page size.
const LARGE_PAGE: u64 = 2 << 20;
/// Number of 2 MiB entries in the single page directory that backs the identity
/// map (`IDENTITY_MAP_BYTES / 2 MiB` = 512 for the 1 GiB map — exactly one full
/// PD, so the map needs only PML4 → PDPT → one PD).
const PD_ENTRIES: u64 = IDENTITY_MAP_BYTES / LARGE_PAGE;
/// Page-table entry flags: Present | Writable.
const PTE_P_RW: u64 = 0b11;
/// Large-page PDE flags: Present | Writable | Page-Size (2 MiB).
const PDE_P_RW_PS: u64 = 0b1000_0011;

// ---------------------------------------------------------------------------
// x86/Linux boot-protocol constants.
// ---------------------------------------------------------------------------

/// File offset of [`SetupHeader`] within a bzImage (and its offset within
/// [`BootParams`]).
pub const SETUP_HEADER_OFFSET: usize = 0x1f1;
/// `boot_flag` magic at offset `0x1fe`.
const BOOT_FLAG_MAGIC: u16 = 0xAA55;
/// `header` magic "HdrS" at offset `0x202`.
const HDRS_MAGIC: u32 = 0x5372_6448;
/// Minimum boot-protocol version that defines `xloadflags`/`XLF_KERNEL_64`
/// (protocol 2.12).
const MIN_PROTOCOL_VERSION: u16 = 0x020c;
/// `xloadflags` bit 0: a 64-bit entry point exists at `load_addr + 0x200`. Written
/// `1` (not `1 << 0`, whose shift carries an equivalent `>>` mutant).
const XLF_KERNEL_64: u16 = 1;
/// The 64-bit entry point's offset past the protected-mode load address.
pub const ENTRY_64_OFFSET: u64 = 0x200;
/// A sector is 512 bytes; the protected-mode kernel begins at
/// `(setup_sects + 1) * 512`.
const SECTOR: usize = 512;
/// `setup_sects == 0` is historically read as 4.
const DEFAULT_SETUP_SECTS: u8 = 4;
/// `type_of_loader` value for an "undefined"/custom loader (high nibble 0xF).
const TYPE_OF_LOADER_UNDEFINED: u8 = 0xFF;

/// Top of the low-memory usable E820 region (640 KiB); the `0xA0000..0x100000`
/// hole (legacy VGA/BIOS) is left unmapped so the kernel never uses it.
const LOW_RAM_TOP: u64 = 0x000A_0000;
/// The hypercall-doorbell REQ/RESP pages (task 73): `vmcall-transport`'s
/// `REQ_GPA` = `0xE000` and `RESP_GPA` = `0xF000` — two 4 KiB pages the guest SDK
/// stages its request/response frames in. They fall inside the usable low-RAM
/// span, so the E820 map **reserves** `[0xE000, 0x10000)` (splitting entry 0):
/// `GUEST_HAS_SDK` is advertised unconditionally, so a Linux guest must never
/// allocate over the pages the doorbell transport reads/writes.
const DOORBELL_PAGES_START: u64 = 0x0000_E000;
/// One past the doorbell pages: `0xE000 + 2 * 4 KiB`.
const DOORBELL_PAGES_END: u64 = 0x0001_0000;
/// Start of high memory (1 MiB).
const HIGH_RAM_START: u64 = 0x0010_0000;
/// E820 entry type: usable RAM.
const E820_RAM: u32 = 1;
/// E820 entry type: reserved (not usable RAM). Used to mark the xAPIC MMIO page so
/// the kernel does not treat it as RAM (which would zero the page on init).
const E820_RESERVED: u32 = 2;
/// The xAPIC (LAPIC) MMIO page: 4 KiB at `0xFEE00000`. Reserved in the E820 map and
/// left as a memslot hole by the backend, so the guest's LAPIC accesses fault to the
/// userspace deterministic xAPIC model (`KVM_EXIT_MMIO`) instead of being serviced
/// from RAM — the seam that lets the V-time LAPIC timer actually tick (see
/// `docs/CPU-MSR-CONTRACT.md` §6 / the LAPIC-timer rows). The IOAPIC page
/// (`0xFEC00000`) is deliberately NOT reserved: Linux runs in virtual-wire mode (no
/// MADT) and never uses it.
const LAPIC_MMIO_PAGE: u64 = 0xFEE0_0000;
/// GPA of the minimal ACPI tables (RSDP -> XSDT -> MADT). Placed in the legacy
/// BIOS region `[0xA0000, 0x100000)`, which the memslot backs but the usable-RAM
/// E820 map deliberately omits — so the kernel reads the tables via the RSDP
/// pointer yet never allocates over them, and no E820 split is needed. Static
/// bytes (no timestamps) => byte-identical every boot.
pub const ACPI_RSDP_GPA: u64 = 0x000E_0000;
/// GPA of the XSDT (36-byte header + one 8-byte entry pointing at the MADT).
const ACPI_XSDT_GPA: u64 = ACPI_RSDP_GPA + 0x40;
/// GPA of the MADT (APIC) table.
const ACPI_MADT_GPA: u64 = ACPI_RSDP_GPA + 0x80;
/// Local-APIC MMIO base advertised in the MADT — must equal the contract's xAPIC
/// base and the backend memslot hole ([`LAPIC_MMIO_PAGE`]).
const ACPI_LAPIC_BASE: u32 = LAPIC_MMIO_PAGE as u32;
/// Boot-CPU local-APIC ID. The VMM models a single vCPU with APIC ID 0.
const ACPI_BOOT_APIC_ID: u8 = 0;
/// `boot_params.e820_table` capacity (`E820_MAX_ENTRIES_ZEROPAGE`).
const E820_MAX_ENTRIES: usize = 128;

// ---------------------------------------------------------------------------
// `#[repr(C)]` boot-protocol structures (offsets pinned by the layout test).
// ---------------------------------------------------------------------------

/// The bzImage `setup_header` (Documentation/arch/x86/boot.rst, "THE REAL-MODE
/// KERNEL HEADER"). `#[repr(C, packed)]`: the kernel declares it `packed`, so
/// several `u32`/`u64` fields sit at offsets that natural alignment would pad —
/// the packed layout reproduces the on-disk bytes exactly. Read out of the
/// untrusted image with [`zerocopy::FromBytes`] (bounds-checked, no panic) and
/// copied verbatim into [`BootParams::hdr`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct SetupHeader {
    /// Number of 512-byte setup sectors (0 ⇒ 4). `0x1f1`.
    pub setup_sects: u8,
    /// Root filesystem flags. `0x1f2`.
    pub root_flags: u16,
    /// Size of the protected-mode kernel in 16-byte paragraphs. `0x1f4`.
    pub syssize: u32,
    /// Obsolete RAM size field. `0x1f8`.
    pub ram_size: u16,
    /// Video mode. `0x1fa`.
    pub vid_mode: u16,
    /// Default root device. `0x1fc`.
    pub root_dev: u16,
    /// Boot-sector signature; must be `0xAA55`. `0x1fe`.
    pub boot_flag: u16,
    /// Real-mode jump instruction. `0x200`.
    pub jump: u16,
    /// Magic; must be "HdrS" (`0x5372_6448`). `0x202`.
    pub header: u32,
    /// Boot-protocol version. `0x206`.
    pub version: u16,
    /// Real-mode switch hook. `0x208`.
    pub realmode_swtch: u32,
    /// Load-low segment (obsolete). `0x20c`.
    pub start_sys_seg: u16,
    /// Pointer to kernel version string. `0x20e`.
    pub kernel_version: u16,
    /// Bootloader identifier; the loader sets this. `0x210`.
    pub type_of_loader: u8,
    /// Boot-protocol flags (LOADED_HIGH, CAN_USE_HEAP, …). `0x211`.
    pub loadflags: u8,
    /// Real-mode setup move size. `0x212`.
    pub setup_move_size: u16,
    /// 32-bit protected-mode entry/load address. `0x214`.
    pub code32_start: u32,
    /// Initramfs load GPA (set by the loader). `0x218`.
    pub ramdisk_image: u32,
    /// Initramfs size in bytes (set by the loader). `0x21c`.
    pub ramdisk_size: u32,
    /// Obsolete. `0x220`.
    pub bootsect_kludge: u32,
    /// End of real-mode heap. `0x224`.
    pub heap_end_ptr: u16,
    /// Extended loader version. `0x226`.
    pub ext_loader_ver: u8,
    /// Extended loader type. `0x227`.
    pub ext_loader_type: u8,
    /// Command-line GPA (set by the loader). `0x228`.
    pub cmd_line_ptr: u32,
    /// Highest legal initramfs address. `0x22c`.
    pub initrd_addr_max: u32,
    /// Required physical alignment of the kernel. `0x230`.
    pub kernel_alignment: u32,
    /// Whether the kernel is relocatable. `0x234`.
    pub relocatable_kernel: u8,
    /// Minimum alignment (log2). `0x235`.
    pub min_alignment: u8,
    /// 64-bit/load-above-4G capability flags. `0x236`.
    pub xloadflags: u16,
    /// Maximum command-line length. `0x238`.
    pub cmdline_size: u32,
    /// Hardware subarchitecture. `0x23c`.
    pub hardware_subarch: u32,
    /// Subarchitecture-specific data. `0x240`.
    pub hardware_subarch_data: u64,
    /// Offset of the embedded payload. `0x248`.
    pub payload_offset: u32,
    /// Length of the embedded payload. `0x24c`.
    pub payload_length: u32,
    /// Linked list of `setup_data`. `0x250`.
    pub setup_data: u64,
    /// Preferred load address (relocatable kernels). `0x258`.
    pub pref_address: u64,
    /// Linear memory the kernel needs from `load_addr` to run. `0x260`.
    pub init_size: u32,
    /// EFI handover entry offset. `0x264`.
    pub handover_offset: u32,
    /// Offset of the `kernel_info` structure. `0x268`.
    pub kernel_info_offset: u32,
}

/// One `boot_params.e820_table` entry (`struct boot_e820_entry`): a flat,
/// `packed` 20-byte record.
#[derive(Clone, Copy, Debug, Default, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct BootE820Entry {
    /// Region base GPA.
    pub addr: u64,
    /// Region size in bytes.
    pub size: u64,
    /// Region type (`1` = usable RAM).
    pub type_: u32,
}

/// The Linux `boot_params` "zero page" (`struct boot_params`), trimmed to the
/// fields this loader writes with byte-exact padding between them. Every member is
/// 1-byte-aligned ([`SetupHeader`]/[`BootE820Entry`] are `packed`, the rest are
/// `u8` arrays), so `#[repr(C)]` introduces **no** padding and the offsets match
/// the kernel's layout — pinned by [`tests::boot_params_field_offsets`].
#[derive(Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct BootParams {
    /// Everything before `e820_entries` (screen_info, apm, EDD, setup_data
    /// pointers, …) — zeroed; the kernel tolerates a zero screen_info.
    _head: [u8; 0x070],
    /// Physical address of the ACPI RSDP (`boot_params.acpi_rsdp_addr`, offset
    /// `0x070`), little-endian. Pointing the SMP kernel at our MADT (whose
    /// Local-APIC entry sets `acpi_lapic`) flips `apic_intr_mode` from
    /// `APIC_VIRTUAL_WIRE_NO_CONFIG` to `APIC_VIRTUAL_WIRE`, which is what makes
    /// `native_smp_prepare_cpus` register the LAPIC-timer clockevent (task 56).
    pub acpi_rsdp_addr: [u8; 8],
    /// Bytes `0x078..0x1e8` (rest of the pre-`e820_entries` header) — zeroed.
    _head2: [u8; 0x1e8 - 0x078],
    /// Number of valid [`Self::e820_table`] entries. Offset `0x1e8`.
    pub e820_entries: u8,
    /// Padding from `0x1e9` up to the setup header at `0x1f1`.
    _pad_to_hdr: [u8; SETUP_HEADER_OFFSET - 0x1e9],
    /// The setup header, copied from the bzImage and patched. Offset `0x1f1`.
    pub hdr: SetupHeader,
    /// Padding from the end of `hdr` up to `e820_table` at `0x2d0`.
    _pad_to_e820: [u8; 0x2d0 - (SETUP_HEADER_OFFSET + core::mem::size_of::<SetupHeader>())],
    /// The E820 memory map. Offset `0x2d0`.
    pub e820_table: [BootE820Entry; E820_MAX_ENTRIES],
    /// Trailing padding to a full 4 KiB page.
    _tail: [u8; 0x1000 - (0x2d0 + E820_MAX_ENTRIES * core::mem::size_of::<BootE820Entry>())],
}

// ---------------------------------------------------------------------------
// Result + error types.
// ---------------------------------------------------------------------------

/// A loaded guest-physical byte range `[start, start + len)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GpaRange {
    /// Range start GPA.
    pub start: u64,
    /// Range length in bytes.
    pub len: u64,
}

/// Everything [`crate::entry::long_mode_entry`] and [`crate::bringup`] need to run
/// the loaded kernel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinuxImage {
    /// The 64-bit entry point GPA (`load_addr + 0x200`); set `RIP` here.
    pub entry_point: u64,
    /// The `boot_params` zero-page GPA; set `RSI` here.
    pub boot_params_gpa: u64,
    /// The identity page-table root; set `CR3` here.
    pub page_table_root: u64,
    /// The boot GDT GPA; set `GDTR.base` here.
    pub gdt_gpa: u64,
    /// The command-line GPA.
    pub cmdline_gpa: u64,
    /// Where the protected-mode kernel image was loaded.
    pub kernel: GpaRange,
    /// Where the initramfs was loaded.
    pub initramfs: GpaRange,
}

/// Errors [`load`] returns instead of panicking. The image/initramfs are
/// **untrusted input** (conventions rule 4 / no-panic-on-untrusted-input): every
/// malformed input is one of these, never a panic, slice-index OOB, or arithmetic
/// overflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LinuxLoadError {
    /// The image is too short to contain a setup header, or its `boot_flag`
    /// signature / `HdrS` magic is absent — not a bzImage.
    #[error("not a bzImage (too short, or boot_flag/HdrS magic absent)")]
    NotBzImage,
    /// The boot-protocol `version` is older than 2.12 (no `xloadflags`).
    #[error("boot-protocol version {found:#06x} < required {required:#06x} (no xloadflags)")]
    UnsupportedProtocol {
        /// The version the header declares.
        found: u16,
        /// The minimum version this loader requires.
        required: u16,
    },
    /// `xloadflags` lacks `XLF_KERNEL_64`: no 64-bit entry point to jump to.
    #[error("kernel has no 64-bit entry point (XLF_KERNEL_64 not set in xloadflags)")]
    No64BitEntry,
    /// The setup-sector count runs past the end of the image (no protected-mode
    /// kernel).
    #[error("setup sectors ({setup_bytes} bytes) exceed the {image_len}-byte image")]
    TruncatedImage {
        /// Bytes the setup region claims.
        setup_bytes: usize,
        /// The actual image length.
        image_len: usize,
    },
    /// The protected-mode kernel + its `init_size` run room does not fit in guest
    /// RAM at the chosen load address.
    #[error("kernel load region [{load:#x}..{end:#x}) does not fit in {ram:#x} bytes of guest RAM")]
    KernelDoesNotFit {
        /// The chosen load address.
        load: u64,
        /// One past the highest byte the kernel needs.
        end: u64,
        /// Guest RAM size.
        ram: u64,
    },
    /// The protected-mode kernel is shorter than the 64-bit entry offset
    /// ([`ENTRY_64_OFFSET`] `+ 1`), so the entry `load_addr + 0x200` would point at
    /// RAM no kernel byte was copied to (a jump into stale/zero memory).
    #[error(
        "protected-mode kernel is {len} bytes — too small for the 64-bit entry at +{:#x}",
        ENTRY_64_OFFSET
    )]
    KernelTooSmall {
        /// The protected-mode kernel length in bytes.
        len: u64,
    },
    /// The initramfs does not fit in RAM, would land below 1 MiB / above the
    /// header's `initrd_addr_max` (or 4 GiB), or would overlap the kernel's run
    /// region.
    #[error(
        "initramfs ({len} bytes) does not fit below the max load address without overlapping the kernel"
    )]
    InitramfsDoesNotFit {
        /// The initramfs length.
        len: u64,
    },
    /// The command line (plus NUL) exceeds the effective limit — the smaller of
    /// [`CMDLINE_MAX`] `- 1` and the kernel's declared `cmdline_size`.
    #[error("command line is {len} bytes; this kernel's effective limit is {limit} (excl. NUL)")]
    CmdlineTooLong {
        /// The command-line length (excluding NUL).
        len: usize,
        /// The effective max accepted (excluding NUL).
        limit: usize,
    },
    /// Guest RAM is too small to hold the fixed low-memory boot structures (page
    /// tables, GDT, zero page, cmdline) or the minimal high-memory region.
    #[error("guest RAM ({ram:#x} bytes) is too small for the boot structures")]
    RamTooSmall {
        /// Guest RAM size.
        ram: u64,
    },
}

// ---------------------------------------------------------------------------
// Bounds-checked little-endian readers (the totality primitives).
// ---------------------------------------------------------------------------

/// Read a little-endian `u16` at `off`, or `None` if `image` is too short.
fn read_u16(image: &[u8], off: usize) -> Option<u16> {
    let end = off.checked_add(2)?;
    let b = image.get(off..end)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

/// Read a little-endian `u32` at `off`, or `None` if `image` is too short.
fn read_u32(image: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    let b = image.get(off..end)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Copy `src` into `mem[gpa .. gpa + src.len())`, bounds-checked. `on_oob` names
/// the error to return if it does not fit. Never panics, never writes OOB.
fn write_at(
    mem: &mut [u8],
    gpa: u64,
    src: &[u8],
    on_oob: LinuxLoadError,
) -> Result<(), LinuxLoadError> {
    let start = usize::try_from(gpa).map_err(|_| on_oob)?;
    let end = start.checked_add(src.len()).ok_or(on_oob)?;
    let dst = mem.get_mut(start..end).ok_or(on_oob)?;
    dst.copy_from_slice(src);
    Ok(())
}

// ---------------------------------------------------------------------------
// Header parsing.
// ---------------------------------------------------------------------------

/// Parse and validate the bzImage [`SetupHeader`] out of `image`. Pure; never
/// panics on arbitrary bytes.
///
/// # Errors
/// [`LinuxLoadError::NotBzImage`] if the image is too short or the
/// `boot_flag`/`HdrS` magics are absent; [`LinuxLoadError::UnsupportedProtocol`]
/// for a pre-2.12 kernel; [`LinuxLoadError::No64BitEntry`] if `XLF_KERNEL_64` is
/// clear.
pub fn parse_setup_header(image: &[u8]) -> Result<SetupHeader, LinuxLoadError> {
    // The two magics gate everything else; check them with bounds-safe readers
    // before the (also bounds-safe) struct read, so a non-bzImage is the clean
    // `NotBzImage` rather than a too-short error.
    if read_u16(image, 0x1fe) != Some(BOOT_FLAG_MAGIC) || read_u32(image, 0x202) != Some(HDRS_MAGIC)
    {
        return Err(LinuxLoadError::NotBzImage);
    }
    let tail = image
        .get(SETUP_HEADER_OFFSET..)
        .ok_or(LinuxLoadError::NotBzImage)?;
    let (hdr, _) = SetupHeader::read_from_prefix(tail).map_err(|_| LinuxLoadError::NotBzImage)?;

    // `version`/`xloadflags` are packed fields; copy to locals before comparing
    // (no references into a packed struct).
    let version = hdr.version;
    if version < MIN_PROTOCOL_VERSION {
        return Err(LinuxLoadError::UnsupportedProtocol {
            found: version,
            required: MIN_PROTOCOL_VERSION,
        });
    }
    let xloadflags = hdr.xloadflags;
    if xloadflags & XLF_KERNEL_64 == 0 {
        return Err(LinuxLoadError::No64BitEntry);
    }
    Ok(hdr)
}

// ---------------------------------------------------------------------------
// The loader.
// ---------------------------------------------------------------------------

/// Flat-load a bzImage `image` + `initramfs` into `mem` (the host backing for GPA
/// `0`) for a direct 64-bit boot, build the `boot_params`/page-table/GDT, and
/// return the [`LinuxImage`].
///
/// `ram_bytes` is the guest RAM size (must equal `mem.len()`); `cmdline` is the
/// kernel command line (a NUL is appended). All indexing is bounds-checked
/// against both inputs and `mem`; any inconsistency is the corresponding
/// [`LinuxLoadError`] (totality, conventions rule 4).
pub fn load(
    image: &[u8],
    initramfs: &[u8],
    ram_bytes: u64,
    cmdline: &str,
    mem: &mut [u8],
) -> Result<LinuxImage, LinuxLoadError> {
    // The guest RAM must have a non-empty high-memory region. Clamp `ram` to the
    // actual backing so a caller passing a mismatched `ram_bytes` can never make us
    // write past `mem`. `ram > HIGH_RAM_START` (1 MiB) is the binding minimum: every
    // fixed low structure (page tables, GDT, zero page, cmdline) ends below `0x9000`
    // — far under 1 MiB — so this single check guarantees they all fit, and the
    // kernel (`load_addr >= 1 MiB`) gets a non-empty `[1 MiB, ram)` E820 region.
    let ram = ram_bytes.min(mem.len() as u64);
    if ram <= HIGH_RAM_START {
        return Err(LinuxLoadError::RamTooSmall { ram });
    }

    let hdr = parse_setup_header(image)?;

    // 1. Locate the protected-mode kernel: it follows the setup sectors.
    let setup_sects = if hdr.setup_sects == 0 {
        DEFAULT_SETUP_SECTS
    } else {
        hdr.setup_sects
    };
    // (setup_sects + 1) * 512, checked.
    let pm_offset = (usize::from(setup_sects))
        .checked_add(1)
        .and_then(|s| s.checked_mul(SECTOR))
        .ok_or(LinuxLoadError::NotBzImage)?;
    let pm_kernel = image
        .get(pm_offset..)
        .ok_or(LinuxLoadError::TruncatedImage {
            setup_bytes: pm_offset,
            image_len: image.len(),
        })?;
    // The 64-bit entry is at `load_addr + ENTRY_64_OFFSET`, so the copied kernel
    // must have at least `ENTRY_64_OFFSET + 1` bytes — else the entry points at RAM
    // no kernel byte was written to (a jump into stale/zero memory). This also
    // subsumes the empty-tail case.
    if (pm_kernel.len() as u64) <= ENTRY_64_OFFSET {
        return Err(LinuxLoadError::KernelTooSmall {
            len: pm_kernel.len() as u64,
        });
    }

    // 2. Choose the load address: the kernel's preferred address. The protocol
    //    guarantees `pref_address >= 0x10_0000`, so the low structures never
    //    collide; a kernel that asks for less is rejected as not-fitting.
    let load_addr = hdr.pref_address;
    if load_addr < HIGH_RAM_START {
        return Err(LinuxLoadError::KernelDoesNotFit {
            load: load_addr,
            end: load_addr,
            ram,
        });
    }
    // The kernel needs `max(image bytes, init_size)` of room from `load_addr`.
    let kernel_len = pm_kernel.len() as u64;
    let run_room = kernel_len.max(u64::from(hdr.init_size));
    let kernel_end = load_addr
        .checked_add(run_room)
        .ok_or(LinuxLoadError::KernelDoesNotFit {
            load: load_addr,
            end: u64::MAX,
            ram,
        })?;
    if kernel_end > ram {
        return Err(LinuxLoadError::KernelDoesNotFit {
            load: load_addr,
            end: kernel_end,
            ram,
        });
    }
    // The reserved xAPIC MMIO page (`build_boot_params` marks it `E820_RESERVED`;
    // the backend leaves a matching memslot hole) is UNMAPPED in the guest. The
    // kernel image must not land in it — those bytes would be written to host
    // backing the guest cannot read back (the page faults to the userspace LAPIC).
    // The kernel loads at the header's `pref_address` and cannot be relocated, so a
    // load region that would straddle the page is rejected. (Real kernels load near
    // 1 MiB, far below the ~4 GiB page; this guards a hostile/oversized header.)
    if overlaps_lapic_mmio_page(load_addr, kernel_end) {
        return Err(LinuxLoadError::KernelDoesNotFit {
            load: load_addr,
            end: kernel_end,
            ram,
        });
    }
    write_at(
        mem,
        load_addr,
        pm_kernel,
        LinuxLoadError::KernelDoesNotFit {
            load: load_addr,
            end: kernel_end,
            ram,
        },
    )?;

    // 3. Place the initramfs as high as possible (page-aligned), above the
    //    kernel's run region and at or below the header's `initrd_addr_max`.
    let initramfs_range =
        place_initramfs(initramfs, ram, kernel_end, u64::from(hdr.initrd_addr_max))?;
    write_at(
        mem,
        initramfs_range.start,
        initramfs,
        LinuxLoadError::InitramfsDoesNotFit {
            len: initramfs.len() as u64,
        },
    )?;

    // 4. Command line (string + NUL). The effective limit is the smaller of our
    //    buffer (CMDLINE_MAX-1) and the kernel's declared `cmdline_size` — never
    //    advertise or write past what this kernel says it accepts.
    let cmdline_limit = cmdline_max(&hdr);
    if cmdline.len() > cmdline_limit {
        return Err(LinuxLoadError::CmdlineTooLong {
            len: cmdline.len(),
            limit: cmdline_limit,
        });
    }
    write_at(
        mem,
        CMDLINE_GPA,
        cmdline.as_bytes(),
        LinuxLoadError::RamTooSmall { ram },
    )?;
    // The NUL terminator (the page is otherwise pre-zeroed, but be explicit).
    write_at(
        mem,
        CMDLINE_GPA + cmdline.len() as u64,
        &[0u8],
        LinuxLoadError::RamTooSmall { ram },
    )?;

    // 5. boot_params (the zero page).
    let boot_params = build_boot_params(&hdr, &initramfs_range, cmdline.len(), ram);
    write_at(
        mem,
        BOOT_PARAMS_GPA,
        boot_params.as_bytes(),
        LinuxLoadError::RamTooSmall { ram },
    )?;

    // 6. Identity page table + boot GDT.
    write_page_tables(mem, ram)?;
    write_acpi_tables(mem)?;
    write_gdt(mem)?;

    Ok(LinuxImage {
        entry_point: load_addr + ENTRY_64_OFFSET,
        boot_params_gpa: BOOT_PARAMS_GPA,
        page_table_root: PML4_GPA,
        gdt_gpa: GDT_GPA,
        cmdline_gpa: CMDLINE_GPA,
        kernel: GpaRange {
            start: load_addr,
            len: kernel_len,
        },
        initramfs: initramfs_range,
    })
}

/// Choose a page-aligned initramfs GPA: as high as possible, above `kernel_end`
/// and at or below the lowest of RAM top, 4 GiB, and the header's
/// `initrd_addr_max` (`0` = unset ⇒ no extra cap). The end `start + len` must not
/// exceed that ceiling. Returns the range (`start` may equal `kernel_end` for a
/// tight fit); an empty initramfs gets length 0.
/// Whether `[start, end)` overlaps the reserved xAPIC MMIO page
/// `[LAPIC_MMIO_PAGE, LAPIC_MMIO_PAGE + 0x1000)` — an UNMAPPED hole in the guest's
/// address space (`build_boot_params` marks it `E820_RESERVED`; the backend leaves a
/// matching memslot hole). A guest-visible image (kernel or initramfs) placed here
/// would be written to host backing the guest cannot read back. Pure; `end` is
/// exclusive.
fn overlaps_lapic_mmio_page(start: u64, end: u64) -> bool {
    start < LAPIC_MMIO_PAGE + 0x1000 && LAPIC_MMIO_PAGE < end
}

fn place_initramfs(
    initramfs: &[u8],
    ram: u64,
    kernel_end: u64,
    initrd_addr_max: u64,
) -> Result<GpaRange, LinuxLoadError> {
    let len = initramfs.len() as u64;
    let too_big = LinuxLoadError::InitramfsDoesNotFit { len };
    // Highest address the initramfs end may reach: min(RAM top, 4 GiB), further
    // capped to `initrd_addr_max + 1` when the kernel declares one (the ramdisk
    // must occupy `[start, start+len) ⊆ [0, initrd_addr_max]`).
    let mut ceiling = ram.min(1u64 << 32);
    if initrd_addr_max != 0 {
        ceiling = ceiling.min(initrd_addr_max.saturating_add(1));
    }
    // The highest page-aligned `start` so `[start, start+len)` fits below `cap` and
    // above `kernel_end`; `None` if it does not fit.
    let place = |cap: u64| -> Option<u64> {
        let start = cap.checked_sub(len)? & !0xFFF;
        (start >= kernel_end).then_some(start)
    };
    let mut start = place(ceiling).ok_or(too_big)?;
    // Keep the initramfs out of the reserved xAPIC MMIO hole. The highest placement
    // is preferred (and the region just ABOVE the page — `[page+0x1000, ceiling)` —
    // is valid RAM, so a ramdisk that fits there is left there); only one that would
    // straddle the page is relocated to sit ENTIRELY BELOW it (cap the ceiling at the
    // page). If it then does not fit above `kernel_end`, it does not fit at all.
    if overlaps_lapic_mmio_page(start, start.saturating_add(len)) {
        start = place(LAPIC_MMIO_PAGE).ok_or(too_big)?;
    }
    Ok(GpaRange { start, len })
}

/// The effective max command-line length (excluding NUL) this kernel accepts: the
/// smaller of our buffer ([`CMDLINE_MAX`] `- 1`) and the header's declared
/// `cmdline_size`. Honoring `cmdline_size` keeps the loader from advertising or
/// writing past what the kernel allocated for its command line.
fn cmdline_max(hdr: &SetupHeader) -> usize {
    (CMDLINE_MAX - 1).min(hdr.cmdline_size as usize)
}

/// Build the `boot_params` zero page: copy the parsed `hdr`, patch the loader-owned
/// fields (`type_of_loader`, `cmd_line_ptr`/`cmdline_size`, `ramdisk_*`,
/// `code32_start`), and write the E820 map. Pure.
fn build_boot_params(
    hdr: &SetupHeader,
    initramfs: &GpaRange,
    cmdline_len: usize,
    ram: u64,
) -> BootParams {
    let mut bp = BootParams::new_zeroed();
    bp.hdr = *hdr;
    bp.hdr.type_of_loader = TYPE_OF_LOADER_UNDEFINED;
    bp.hdr.code32_start = hdr.pref_address as u32;
    bp.hdr.cmd_line_ptr = CMDLINE_GPA as u32;
    // Advertise the effective buffer — never more than the kernel's declared
    // `cmdline_size` (left as the copied header value when smaller).
    bp.hdr.cmdline_size = cmdline_max(hdr) as u32;
    let _ = cmdline_len; // cmdline_size advertises the buffer, not the string len
    bp.hdr.ramdisk_image = initramfs.start as u32;
    bp.hdr.ramdisk_size = initramfs.len as u32;

    // A running index into `e820_table`, so the doorbell-reservation split (below)
    // and the xAPIC split compose without hand-tracking entry numbers.
    let mut n = 0usize;
    let push = |bp: &mut BootParams, n: &mut usize, addr: u64, size: u64, type_: u32| {
        bp.e820_table[*n] = BootE820Entry { addr, size, type_ };
        *n += 1;
    };

    // E820 low RAM `[0, 640 KiB)`, SPLIT to **reserve** the two hypercall-doorbell
    // pages `[0xE000, 0x10000)` (task 73) so a Linux SDK guest never allocates over
    // REQ_GPA/RESP_GPA. The `0xA0000..0x100000` legacy hole stays omitted. The
    // doorbell span sits strictly inside `(0, LOW_RAM_TOP)`, so all three parts are
    // non-empty. (Mirrors the xAPIC carve-out pattern below.)
    push(&mut bp, &mut n, 0, DOORBELL_PAGES_START, E820_RAM);
    push(
        &mut bp,
        &mut n,
        DOORBELL_PAGES_START,
        DOORBELL_PAGES_END - DOORBELL_PAGES_START,
        E820_RESERVED,
    );
    push(
        &mut bp,
        &mut n,
        DOORBELL_PAGES_END,
        LOW_RAM_TOP - DOORBELL_PAGES_END,
        E820_RAM,
    );

    // High RAM `[1 MiB, ram)` with the 4 KiB xAPIC MMIO page (`LAPIC_MMIO_PAGE`)
    // carved out as **reserved**: that page must NOT be usable RAM, or the kernel
    // zeroes it on init (its content is then dead RAM and the backend's matching
    // memslot hole — which routes LAPIC accesses to the userspace xAPIC model — would
    // have nothing behind it). `load` guarantees `ram > HIGH_RAM_START`, so the first
    // high-RAM chunk is non-empty. Three shapes, by how far RAM reaches:
    //
    //   * `ram <= page`        → one high-RAM entry `[1 MiB, ram)` (page never RAM).
    //   * `page < ram <= page+0x1000` → `[1 MiB, page) RAM` + `[page, +0x1000) RESERVED`
    //     (the tail past the page is empty, so it is omitted).
    //   * `ram > page+0x1000`  → the full split with a `[page+0x1000, ram) RAM`
    //     tail (the 8 GiB Postgres-guest shape).
    //
    // For any page-aligned `ram` only the first and last shapes occur (no page
    // multiple lies strictly inside the page); the middle shape keeps the
    // never-typed-RAM invariant exact for the pathological boundary too.
    if ram > LAPIC_MMIO_PAGE {
        push(
            &mut bp,
            &mut n,
            HIGH_RAM_START,
            LAPIC_MMIO_PAGE - HIGH_RAM_START,
            E820_RAM,
        );
        push(&mut bp, &mut n, LAPIC_MMIO_PAGE, 0x1000, E820_RESERVED);
        if ram > LAPIC_MMIO_PAGE + 0x1000 {
            push(
                &mut bp,
                &mut n,
                LAPIC_MMIO_PAGE + 0x1000,
                ram - (LAPIC_MMIO_PAGE + 0x1000),
                E820_RAM,
            );
        }
    } else {
        push(
            &mut bp,
            &mut n,
            HIGH_RAM_START,
            ram - HIGH_RAM_START,
            E820_RAM,
        );
    }
    bp.e820_entries = n as u8;
    bp.acpi_rsdp_addr = ACPI_RSDP_GPA.to_le_bytes();
    bp
}

/// ACPI 1-byte checksum: the value that makes the sum of `bytes` zero (mod 256).
fn acpi_checksum(bytes: &[u8]) -> u8 {
    bytes
        .iter()
        .fold(0u8, |a, b| a.wrapping_add(*b))
        .wrapping_neg()
}

/// Write a minimal ACPI table set — RSDP -> XSDT -> MADT — into the guest's legacy
/// BIOS region and return the RSDP GPA (for `boot_params.acpi_rsdp_addr`). The MADT
/// carries one Processor-Local-APIC entry and **no** IO-APIC entry, so Linux sets
/// `acpi_lapic` (=> `apic_intr_mode == APIC_VIRTUAL_WIRE`) without enabling the
/// IO-APIC routing the VMM does not model. With `CONFIG_SMP=y` that is exactly what
/// makes `native_smp_prepare_cpus` set up the LAPIC-timer clockevent so the periodic
/// tick fires and the tree-RCU idle `HLT` resumes (task 56). All bytes are static
/// (no timestamps) => byte-identical every boot, so the tables are part of the
/// deterministic guest input.
fn write_acpi_tables(mem: &mut [u8]) -> Result<(), LinuxLoadError> {
    let oob = LinuxLoadError::RamTooSmall { ram: ACPI_RSDP_GPA };
    const OEMID: &[u8; 6] = b"HARMNY";
    const OEM_TABLE_ID: &[u8; 8] = b"HARMONYT";
    const CREATOR_ID: &[u8; 4] = b"HARM";

    // --- MADT (signature "APIC"): 36-byte SDT header + 8-byte flags/addr + one
    //     8-byte Processor-Local-APIC structure = 52 bytes. ---
    let mut madt = [0u8; 52];
    madt[0..4].copy_from_slice(b"APIC");
    madt[4..8].copy_from_slice(&52u32.to_le_bytes());
    madt[8] = 5; // revision (ACPI 4.0+)
    madt[10..16].copy_from_slice(OEMID);
    madt[16..24].copy_from_slice(OEM_TABLE_ID);
    madt[24..28].copy_from_slice(&1u32.to_le_bytes());
    madt[28..32].copy_from_slice(CREATOR_ID);
    madt[32..36].copy_from_slice(&1u32.to_le_bytes());
    madt[36..40].copy_from_slice(&ACPI_LAPIC_BASE.to_le_bytes()); // Local APIC address
    madt[40..44].copy_from_slice(&1u32.to_le_bytes()); // flags: PCAT_COMPAT (8259 present)
    madt[44] = 0; // structure type: Processor Local APIC
    madt[45] = 8; // structure length
    madt[46] = 0; // ACPI processor UID
    madt[47] = ACPI_BOOT_APIC_ID; // APIC ID
    madt[48..52].copy_from_slice(&1u32.to_le_bytes()); // local-APIC flags: Enabled
    madt[9] = acpi_checksum(&madt);

    // --- XSDT (signature "XSDT"): 36-byte header + one 8-byte entry -> MADT. ---
    let mut xsdt = [0u8; 44];
    xsdt[0..4].copy_from_slice(b"XSDT");
    xsdt[4..8].copy_from_slice(&44u32.to_le_bytes());
    xsdt[8] = 1;
    xsdt[10..16].copy_from_slice(OEMID);
    xsdt[16..24].copy_from_slice(OEM_TABLE_ID);
    xsdt[24..28].copy_from_slice(&1u32.to_le_bytes());
    xsdt[28..32].copy_from_slice(CREATOR_ID);
    xsdt[32..36].copy_from_slice(&1u32.to_le_bytes());
    xsdt[36..44].copy_from_slice(&ACPI_MADT_GPA.to_le_bytes());
    xsdt[9] = acpi_checksum(&xsdt);

    // --- RSDP (ACPI 2.0, 36 bytes): two checksums (first 20 bytes, then all 36). ---
    let mut rsdp = [0u8; 36];
    rsdp[0..8].copy_from_slice(b"RSD PTR ");
    rsdp[9..15].copy_from_slice(OEMID);
    rsdp[15] = 2; // revision (ACPI 2.0+ => XSDT present)
    // rsdp[16..20] RsdtAddress = 0 (the 64-bit XSDT is authoritative)
    rsdp[20..24].copy_from_slice(&36u32.to_le_bytes()); // length
    rsdp[24..32].copy_from_slice(&ACPI_XSDT_GPA.to_le_bytes());
    rsdp[8] = acpi_checksum(&rsdp[0..20]); // legacy checksum (first 20 bytes)
    rsdp[32] = acpi_checksum(&rsdp); // extended checksum (all 36 bytes)

    write_at(mem, ACPI_MADT_GPA, &madt, oob)?;
    write_at(mem, ACPI_XSDT_GPA, &xsdt, oob)?;
    write_at(mem, ACPI_RSDP_GPA, &rsdp, oob)?;
    Ok(())
}

/// Write the identity page table — PML4[0] → PDPT, PDPT[0] → one PD, and that PD's
/// [`PD_ENTRIES`] 2 MiB large-page entries covering [`IDENTITY_MAP_BYTES`]
/// (`PD[j]` maps GPA `j · 2 MiB`). Every entry is written through bounds-checked
/// [`write_at`]; if the tables do not fit, [`LinuxLoadError::RamTooSmall`].
fn write_page_tables(mem: &mut [u8], ram: u64) -> Result<(), LinuxLoadError> {
    let oob = LinuxLoadError::RamTooSmall { ram };
    // PML4[0] -> PDPT, PDPT[0] -> the single PD (one PD covers the whole map).
    write_at(mem, PML4_GPA, &(PDPT_GPA | PTE_P_RW).to_le_bytes(), oob)?;
    write_at(mem, PDPT_GPA, &(PD_GPA | PTE_P_RW).to_le_bytes(), oob)?;
    // PD[j] -> the j-th 2 MiB large page.
    for j in 0..PD_ENTRIES {
        let phys = j * LARGE_PAGE;
        write_at(
            mem,
            PD_GPA + j * 8,
            &(phys | PDE_P_RW_PS).to_le_bytes(),
            oob,
        )?;
    }
    Ok(())
}

/// Write the flat 64-bit boot GDT: null, an unused slot, `__BOOT_CS` (selector
/// `0x10`, 64-bit code), and `__BOOT_DS` (selector `0x18`, data) — the descriptors
/// the 64-bit boot protocol requires.
fn write_gdt(mem: &mut [u8]) -> Result<(), LinuxLoadError> {
    let oob = LinuxLoadError::RamTooSmall {
        ram: mem.len() as u64,
    };
    // [null, unused, code64 @ 0x10, data @ 0x18].
    let gdt: [u64; 4] = [0, 0, GDT_CODE64, GDT_DATA];
    for (i, e) in gdt.iter().enumerate() {
        write_at(mem, GDT_GPA + (i as u64) * 8, &e.to_le_bytes(), oob)?;
    }
    Ok(())
}

/// `__BOOT_CS` (selector `0x10`): base 0, limit `0xFFFFF`, present, DPL 0, code
/// exec/read/accessed, `L=1` (64-bit), `G=1`. Access `0x9B`, flags `0xA`.
pub const GDT_CODE64: u64 = 0x00AF_9B00_0000_FFFF;
/// `__BOOT_DS` (selector `0x18`): base 0, limit `0xFFFFF`, present, DPL 0, data
/// read/write/accessed, `D/B=1`, `G=1`. Access `0x93`, flags `0xC`.
pub const GDT_DATA: u64 = 0x00CF_9300_0000_FFFF;

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    /// The minimal ACPI tables (RSDP → XSDT → MADT, task 56 MADT+ARAT keystone) are
    /// fully static, so their exact bytes are pinned here. This is both a determinism
    /// guard on the ACPI guest input and a mutation guard: it fixes the computed 1-byte
    /// checksums (`madt[9]`, `xsdt[9]`, `rsdp[8]`, `rsdp[32]`), the RSDP→XSDT→MADT GPA
    /// pointers (the `ACPI_RSDP_GPA + 0x40 / + 0x80` offset arithmetic), and that
    /// `write_acpi_tables` actually emits the bytes. Read from the fixed `ACPI_RSDP_GPA`
    /// base (not the derived offsets) so a wrong offset lands the table outside the
    /// asserted window.
    #[test]
    fn acpi_tables_are_byte_exact() {
        let base = ACPI_RSDP_GPA as usize;
        let mut mem = vec![0u8; base + 0x1000];
        write_acpi_tables(&mut mem).expect("write_acpi_tables into a large-enough buffer");
        // RSDP @ +0x00 (36 B), XSDT @ +0x40 (44 B), MADT @ +0x80 (52 B); zeros in the gaps.
        const GOLDEN: [u8; 0xC0] = [
            0x52, 0x53, 0x44, 0x20, 0x50, 0x54, 0x52, 0x20, 0x10, 0x48, 0x41, 0x52, 0x4d, 0x4e,
            0x59, 0x02, 0x00, 0x00, 0x00, 0x00, 0x24, 0x00, 0x00, 0x00, 0x40, 0x00, 0x0e, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x8e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x58, 0x53, 0x44, 0x54, 0x2c, 0x00,
            0x00, 0x00, 0x01, 0x97, 0x48, 0x41, 0x52, 0x4d, 0x4e, 0x59, 0x48, 0x41, 0x52, 0x4d,
            0x4f, 0x4e, 0x59, 0x54, 0x01, 0x00, 0x00, 0x00, 0x48, 0x41, 0x52, 0x4d, 0x01, 0x00,
            0x00, 0x00, 0x80, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x41, 0x50, 0x49, 0x43, 0x34, 0x00, 0x00, 0x00, 0x05, 0x57, 0x48, 0x41,
            0x52, 0x4d, 0x4e, 0x59, 0x48, 0x41, 0x52, 0x4d, 0x4f, 0x4e, 0x59, 0x54, 0x01, 0x00,
            0x00, 0x00, 0x48, 0x41, 0x52, 0x4d, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0xe0, 0xfe,
            0x01, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(
            &mem[base..base + GOLDEN.len()],
            &GOLDEN[..],
            "ACPI RSDP/XSDT/MADT bytes drifted (checksum, offset arithmetic, or table content)"
        );
    }

    /// A minimal but valid bzImage `SetupHeader` bytes prefix for tests: a buffer
    /// with the magics, version, xloadflags, setup_sects, pref_address, init_size
    /// set, padded so the protected-mode kernel begins at `(setup_sects+1)*512`
    /// and carries `pm_len` marker bytes.
    fn synth_bzimage(
        setup_sects: u8,
        version: u16,
        xloadflags: u16,
        pref_address: u64,
        init_size: u32,
        pm_len: usize,
    ) -> Vec<u8> {
        let real_setup_sects = if setup_sects == 0 { 4 } else { setup_sects };
        let pm_off = (usize::from(real_setup_sects) + 1) * 512;
        let mut img = vec![0u8; pm_off + pm_len];
        // Build the setup header via the struct, then splice at 0x1f1.
        let mut hdr = SetupHeader::new_zeroed();
        hdr.setup_sects = setup_sects;
        hdr.boot_flag = BOOT_FLAG_MAGIC;
        hdr.header = HDRS_MAGIC;
        hdr.version = version;
        hdr.xloadflags = xloadflags;
        hdr.pref_address = pref_address;
        hdr.init_size = init_size;
        // Header maxima a modern bzImage advertises, so the cmdline / initramfs
        // caps don't artificially reject a valid synthetic load. `cmdline_size`
        // (0x1000) is set ABOVE our buffer `CMDLINE_MAX - 1` (0x7FF) so the
        // loader's `cmdline_max = (CMDLINE_MAX-1).min(hdr.cmdline_size)` is bound by
        // the `CMDLINE_MAX - 1` term — pinned exactly by `load_pins_every_computed_value`.
        hdr.cmdline_size = 0x1000;
        hdr.initrd_addr_max = 0x7FFF_FFFF;
        let hb = hdr.as_bytes();
        img[SETUP_HEADER_OFFSET..SETUP_HEADER_OFFSET + hb.len()].copy_from_slice(hb);
        // Mark the protected-mode kernel region so we can assert the copy.
        for (i, b) in img[pm_off..].iter_mut().enumerate() {
            *b = (0x40 + (i % 0x30)) as u8;
        }
        img
    }

    fn valid_bzimage() -> Vec<u8> {
        synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x10_0000, 0x40_0000, 0x2000)
    }

    // --- Gate 1: layout pinned --------------------------------------------

    #[test]
    fn setup_header_field_offsets() {
        // Pin the setup_header field offsets against the x86 boot protocol.
        assert_eq!(offset_of!(SetupHeader, setup_sects), 0x1f1 - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, boot_flag), 0x1fe - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, header), 0x202 - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, version), 0x206 - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, type_of_loader), 0x210 - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, code32_start), 0x214 - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, ramdisk_image), 0x218 - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, ramdisk_size), 0x21c - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, cmd_line_ptr), 0x228 - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, kernel_alignment), 0x230 - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, xloadflags), 0x236 - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, cmdline_size), 0x238 - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, pref_address), 0x258 - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, init_size), 0x260 - 0x1f1);
        assert_eq!(offset_of!(SetupHeader, kernel_info_offset), 0x268 - 0x1f1);
        // The whole header spans 0x1f1..0x26c.
        assert_eq!(size_of::<SetupHeader>(), 0x26c - 0x1f1);
    }

    #[test]
    fn boot_params_field_offsets() {
        // The gate-1 absolute offsets within boot_params (the x86 boot protocol).
        assert_eq!(offset_of!(BootParams, acpi_rsdp_addr), 0x070);
        assert_eq!(offset_of!(BootParams, e820_entries), 0x1e8);
        assert_eq!(offset_of!(BootParams, hdr), 0x1f1);
        assert_eq!(offset_of!(BootParams, e820_table), 0x2d0);
        // The composed setup-header fields land at their absolute boot_params
        // offsets too (hdr base + intra-header offset).
        assert_eq!(
            offset_of!(BootParams, hdr) + offset_of!(SetupHeader, ramdisk_image),
            0x218
        );
        assert_eq!(
            offset_of!(BootParams, hdr) + offset_of!(SetupHeader, cmd_line_ptr),
            0x228
        );
        // boot_params is exactly one page.
        assert_eq!(size_of::<BootParams>(), 0x1000);
        // Each E820 entry is 20 bytes.
        assert_eq!(size_of::<BootE820Entry>(), 20);
    }

    // --- Header parsing ----------------------------------------------------

    #[test]
    fn parses_a_valid_bzimage() {
        let img = valid_bzimage();
        let hdr = parse_setup_header(&img).expect("valid");
        assert_eq!({ hdr.boot_flag }, BOOT_FLAG_MAGIC);
        assert_eq!({ hdr.header }, HDRS_MAGIC);
        assert_eq!({ hdr.pref_address }, 0x10_0000);
    }

    #[test]
    fn rejects_non_bzimage() {
        assert_eq!(parse_setup_header(&[]), Err(LinuxLoadError::NotBzImage));
        assert_eq!(
            parse_setup_header(&[0u8; 4096]),
            Err(LinuxLoadError::NotBzImage)
        );
        // boot_flag present but HdrS absent.
        let mut img = vec![0u8; 4096];
        img[0x1fe..0x200].copy_from_slice(&BOOT_FLAG_MAGIC.to_le_bytes());
        assert_eq!(parse_setup_header(&img), Err(LinuxLoadError::NotBzImage));
    }

    #[test]
    fn rejects_old_protocol() {
        let img = synth_bzimage(1, 0x0205, XLF_KERNEL_64, 0x10_0000, 0, 0x1000);
        assert_eq!(
            parse_setup_header(&img),
            Err(LinuxLoadError::UnsupportedProtocol {
                found: 0x0205,
                required: MIN_PROTOCOL_VERSION,
            })
        );
    }

    #[test]
    fn rejects_no_64bit_entry() {
        let img = synth_bzimage(1, 0x020f, 0, 0x10_0000, 0, 0x1000);
        assert_eq!(parse_setup_header(&img), Err(LinuxLoadError::No64BitEntry));
    }

    // --- Loading -----------------------------------------------------------

    /// Read a little-endian `u32`/`u64` out of guest memory at an absolute offset.
    fn rd32(mem: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(mem[off..off + 4].try_into().unwrap())
    }
    fn rd64(mem: &[u8], off: usize) -> u64 {
        u64::from_le_bytes(mem[off..off + 8].try_into().unwrap())
    }

    /// **Exact-value gate** (mutation hardening): for a fully-known synthetic
    /// bzImage, pin *every* address and `boot_params` field the loader computes, so
    /// `+`↔`-`, `<`↔`<=`, `*`↔`/`, and dropped-term mutants in `load` /
    /// `place_initramfs` / `build_boot_params` all change an asserted value and die.
    #[test]
    #[cfg_attr(
        miri,
        ignore = "allocates guest RAM; totality is covered by the miri-bounded loader proptest"
    )]
    fn load_pins_every_computed_value() {
        // setup_sects=2 ⇒ pm_offset = (2+1)*512 = 0x600 (distinct from 0x200/0x400
        // so a +/-/* mutant on it shifts the copied bytes). init_size 0x40_0000.
        let img = synth_bzimage(2, 0x020f, XLF_KERNEL_64, 0x10_0000, 0x40_0000, 0x800);
        let pm_off = (2 + 1) * 512usize;
        let kernel_len = 0x800u64;
        // initramfs length 0x2345 is NOT a page multiple, so the `& !0xFFF` align is
        // observable, and `ceiling - len` lands mid-page.
        let initramfs: Vec<u8> = (0..0x2345u32).map(|i| (i & 0xFF) as u8).collect();
        let ram = 8u64 << 20; // 0x80_0000
        // Pre-fill 0xFF (not 0): the loader only writes the bytes it computes, so an
        // off-by-N NUL terminator or a mis-placed field lands on `0xFF` and is
        // distinguishable from the deliberate zeros (kills the cmdline-NUL `+`→`-`/`*`
        // mutant, which would otherwise be masked by a zeroed buffer).
        let mut mem = vec![0xFFu8; ram as usize];
        let cmd = "console=ttyS0 panic=-1"; // 22 bytes
        let li = load(&img, &initramfs, ram, cmd, &mut mem).expect("load");

        // --- returned LinuxImage: every field exact -----------------------
        assert_eq!(li.kernel.start, 0x10_0000, "load_addr = pref_address");
        assert_eq!(li.kernel.len, kernel_len, "kernel_len = pm_kernel.len()");
        assert_eq!(li.entry_point, 0x10_0200, "entry = load_addr + 0x200");
        assert_eq!(li.boot_params_gpa, 0x7000);
        assert_eq!(li.page_table_root, 0x1000);
        assert_eq!(li.gdt_gpa, 0x6000);
        assert_eq!(li.cmdline_gpa, 0x8000);
        // place_initramfs: top = min(ram,4G,initrd_max+1) - len = 0x80_0000-0x2345
        // = 0x7FDCBB; start = 0x7FDCBB & !0xFFF = 0x7FD000.
        assert_eq!(li.initramfs.start, 0x7F_D000, "page-aligned high placement");
        assert_eq!(li.initramfs.len, 0x2345);

        // --- the protected-mode kernel was copied from EXACTLY pm_off -----
        assert_eq!(
            &mem[0x10_0000..0x10_0000 + kernel_len as usize],
            &img[pm_off..pm_off + kernel_len as usize],
            "kernel copied from (setup_sects+1)*512"
        );
        // --- the initramfs landed at its exact GPA, byte-for-byte ----------
        let istart = li.initramfs.start as usize;
        assert_eq!(&mem[istart..istart + initramfs.len()], &initramfs[..]);

        // --- boot_params fields at their absolute offsets ------------------
        let bp = BOOT_PARAMS_GPA as usize;
        assert_eq!(
            mem[bp + 0x1e8],
            4,
            "e820_entries = 4 (3-entry doorbell low split + 1 high)"
        );
        assert_eq!(mem[bp + 0x210], TYPE_OF_LOADER_UNDEFINED, "type_of_loader");
        assert_eq!(rd32(&mem, bp + 0x214), 0x10_0000, "code32_start = pref");
        assert_eq!(rd32(&mem, bp + 0x218), 0x7F_D000, "ramdisk_image");
        assert_eq!(rd32(&mem, bp + 0x21c), 0x2345, "ramdisk_size");
        assert_eq!(rd32(&mem, bp + 0x228), 0x8000, "cmd_line_ptr");
        // hdr.cmdline_size is 0x1000 (> our buffer) so the CMDLINE_MAX-1 = 0x7FF term
        // binds — pins `(CMDLINE_MAX - 1).min(hdr.cmdline_size)`.
        assert_eq!(
            rd32(&mem, bp + 0x238),
            0x7FF,
            "cmdline_size = CMDLINE_MAX - 1"
        );

        // --- command line, NUL-terminated, at CMDLINE_GPA -----------------
        let c = CMDLINE_GPA as usize;
        assert_eq!(&mem[c..c + cmd.len()], cmd.as_bytes());
        assert_eq!(mem[c + cmd.len()], 0);

        // --- E820: the 3-entry low split (doorbell pages reserved) then
        //     [1M, ram) usable (task 73) ----------------------------------
        let e0 = bp + 0x2d0;
        assert_eq!(rd64(&mem, e0), 0);
        assert_eq!(rd64(&mem, e0 + 8), DOORBELL_PAGES_START); // 0xE000
        assert_eq!(rd32(&mem, e0 + 16), E820_RAM);
        let e1 = e0 + 20;
        assert_eq!(rd64(&mem, e1), DOORBELL_PAGES_START); // 0xE000
        assert_eq!(
            rd64(&mem, e1 + 8),
            DOORBELL_PAGES_END - DOORBELL_PAGES_START
        ); // 0x2000
        assert_eq!(
            rd32(&mem, e1 + 16),
            E820_RESERVED,
            "doorbell pages reserved"
        );
        let e2 = e1 + 20;
        assert_eq!(rd64(&mem, e2), DOORBELL_PAGES_END); // 0x10000
        assert_eq!(rd64(&mem, e2 + 8), LOW_RAM_TOP - DOORBELL_PAGES_END); // 0x90000
        assert_eq!(rd32(&mem, e2 + 16), E820_RAM);
        let e3 = e2 + 20;
        assert_eq!(rd64(&mem, e3), HIGH_RAM_START); // 0x100000
        assert_eq!(rd64(&mem, e3 + 8), ram - HIGH_RAM_START); // 0x70_0000
        assert_eq!(rd32(&mem, e3 + 16), E820_RAM);
        // The fifth entry slot is untouched (exactly four entries written).
        assert_eq!(rd64(&mem, e3 + 20), 0);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "allocates guest RAM; totality is covered by the miri-bounded loader proptest"
    )]
    fn page_tables_identity_map_with_2mib_pages() {
        let img = valid_bzimage();
        let ram = 8u64 << 20;
        let mut mem = vec![0u8; ram as usize];
        load(&img, &[], ram, "x", &mut mem).expect("load");

        // PML4[0] -> PDPT (present+rw).
        let pml4_0 = u64::from_le_bytes(mem[0x1000..0x1008].try_into().unwrap());
        assert_eq!(pml4_0, PDPT_GPA | PTE_P_RW);
        // PDPT[0] -> PD (present+rw).
        let pdpt_0 = u64::from_le_bytes(mem[0x2000..0x2008].try_into().unwrap());
        assert_eq!(pdpt_0, PD_GPA | PTE_P_RW);
        // PD[0] = 0 | present+rw+PS; PD[1] = 2 MiB | ...; PD[511] = 1022 MiB | ...
        let pd_0 = u64::from_le_bytes(mem[0x3000..0x3008].try_into().unwrap());
        assert_eq!(pd_0, PDE_P_RW_PS);
        let pd_1 = u64::from_le_bytes(mem[0x3008..0x3010].try_into().unwrap());
        assert_eq!(pd_1, LARGE_PAGE | PDE_P_RW_PS);
        let pd_511 = u64::from_le_bytes(
            mem[0x3000 + 511 * 8..0x3000 + 511 * 8 + 8]
                .try_into()
                .unwrap(),
        );
        assert_eq!(pd_511, (511 * LARGE_PAGE) | PDE_P_RW_PS);
        // Exactly PD_ENTRIES (512) entries: the last is written, the one past it is
        // not — pinning the `0..PD_ENTRIES` bound (kills the ±1 / `IDENTITY_MAP_BYTES
        // / LARGE_PAGE` mutants, which would write one fewer/extra PDE).
        assert_eq!(PD_ENTRIES, 512);
        let pd_512 = u64::from_le_bytes(
            mem[0x3000 + 512 * 8..0x3000 + 512 * 8 + 8]
                .try_into()
                .unwrap(),
        );
        assert_eq!(pd_512, 0, "no PDE written past PD_ENTRIES");
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "allocates guest RAM; totality is covered by the miri-bounded loader proptest"
    )]
    fn writes_boot_gdt() {
        let img = valid_bzimage();
        let ram = 8u64 << 20;
        let mut mem = vec![0u8; ram as usize];
        load(&img, &[], ram, "x", &mut mem).expect("load");
        let g = GDT_GPA as usize;
        assert_eq!(u64::from_le_bytes(mem[g..g + 8].try_into().unwrap()), 0);
        // __BOOT_CS at selector 0x10 (index 2), __BOOT_DS at 0x18 (index 3).
        assert_eq!(
            u64::from_le_bytes(mem[g + 0x10..g + 0x18].try_into().unwrap()),
            GDT_CODE64
        );
        assert_eq!(
            u64::from_le_bytes(mem[g + 0x18..g + 0x20].try_into().unwrap()),
            GDT_DATA
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "allocates guest RAM; totality is covered by the miri-bounded loader proptest"
    )]
    fn rejects_kernel_that_does_not_fit() {
        // init_size larger than RAM.
        let img = synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x10_0000, 0, 0x1000);
        let ram = 4u64 << 20; // 4 MiB
        let mut mem = vec![0u8; ram as usize];
        // pref_address 1 MiB + a 1 MiB image fits, but make init_size huge:
        let img2 = synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x10_0000, 0x8000_0000, 0x1000);
        assert!(matches!(
            load(&img2, &[], ram, "x", &mut mem),
            Err(LinuxLoadError::KernelDoesNotFit { .. })
        ));
        let _ = img;
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "allocates guest RAM; totality is covered by the miri-bounded loader proptest"
    )]
    fn rejects_initramfs_overlap() {
        // A kernel that nearly fills RAM leaves no room for the initramfs.
        let img = synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x10_0000, 0, 0x1000);
        let ram = 8u64 << 20;
        let mut mem = vec![0u8; ram as usize];
        let big_initramfs = vec![0u8; (ram - 0x10_0000) as usize];
        assert!(matches!(
            load(&img, &big_initramfs, ram, "x", &mut mem),
            Err(LinuxLoadError::InitramfsDoesNotFit { .. })
        ));
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "allocates guest RAM; totality is covered by the miri-bounded loader proptest"
    )]
    fn rejects_overlong_cmdline() {
        let img = valid_bzimage();
        let ram = 8u64 << 20;
        let mut mem = vec![0u8; ram as usize];
        let long = "a".repeat(CMDLINE_MAX);
        assert!(matches!(
            load(&img, &[], ram, &long, &mut mem),
            Err(LinuxLoadError::CmdlineTooLong { .. })
        ));
    }

    #[test]
    fn rejects_tiny_ram() {
        let img = valid_bzimage();
        let mut mem = vec![0u8; 0x9000];
        assert!(matches!(
            load(&img, &[], 0x9000, "x", &mut mem),
            Err(LinuxLoadError::RamTooSmall { .. })
        ));
    }

    // --- robustness (codex P2) --------------------------------------------

    #[test]
    #[cfg_attr(miri, ignore = "allocates guest RAM")]
    fn rejects_kernel_too_small_for_64bit_entry() {
        // A protected-mode kernel of exactly ENTRY_64_OFFSET bytes has no byte at
        // the entry `load_addr + 0x200`; one more byte is the minimum.
        let ram = 8u64 << 20;
        let mut mem = vec![0u8; ram as usize];
        let too_small = synth_bzimage(
            1,
            0x020f,
            XLF_KERNEL_64,
            0x10_0000,
            0,
            ENTRY_64_OFFSET as usize,
        );
        assert_eq!(
            load(&too_small, &[], ram, "x", &mut mem),
            Err(LinuxLoadError::KernelTooSmall {
                len: ENTRY_64_OFFSET
            })
        );
        // Exactly one more byte is accepted (boundary: `<=` not `<`).
        let just_enough = synth_bzimage(
            1,
            0x020f,
            XLF_KERNEL_64,
            0x10_0000,
            0,
            ENTRY_64_OFFSET as usize + 1,
        );
        assert!(load(&just_enough, &[], ram, "x", &mut mem).is_ok());
    }

    #[test]
    #[cfg_attr(miri, ignore = "allocates guest RAM")]
    fn honors_initrd_addr_max() {
        // Patch the header's initrd_addr_max (offset 0x22c) below RAM top so the
        // initramfs must be placed under it, not near the RAM ceiling.
        let mut img = synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x10_0000, 0x10_0000, 0x800);
        let addr_max: u32 = 0x0060_0000; // 6 MiB
        img[0x22c..0x230].copy_from_slice(&addr_max.to_le_bytes());
        let ram = 8u64 << 20; // 8 MiB
        let mut mem = vec![0u8; ram as usize];
        let initramfs = vec![0u8; 0x1000];
        let li = load(&img, &initramfs, ram, "x", &mut mem).expect("load");
        // end = start + len must be <= initrd_addr_max + 1; placed just under it
        // (page-aligned): (0x60_0000 + 1 - 0x1000) & !0xFFF = 0x5FF000.
        assert_eq!(li.initramfs.start, 0x5F_F000);
        assert!(li.initramfs.start + li.initramfs.len <= u64::from(addr_max) + 1);

        // An initrd_addr_max below the kernel's run region rejects (no legal spot).
        let mut low = synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x10_0000, 0x10_0000, 0x800);
        low[0x22c..0x230].copy_from_slice(&0x0010_0000u32.to_le_bytes()); // 1 MiB
        assert!(matches!(
            load(&low, &initramfs, ram, "x", &mut mem),
            Err(LinuxLoadError::InitramfsDoesNotFit { .. })
        ));
    }

    #[test]
    #[cfg_attr(miri, ignore = "allocates guest RAM")]
    fn caps_cmdline_against_header_cmdline_size() {
        // A kernel advertising a small cmdline_size (offset 0x238) rejects a longer
        // command line, even though it is under CMDLINE_MAX.
        let mut img = valid_bzimage();
        img[0x238..0x23c].copy_from_slice(&8u32.to_le_bytes()); // cmdline_size = 8
        let ram = 8u64 << 20;
        let mut mem = vec![0u8; ram as usize];
        assert_eq!(
            load(&img, &[], ram, "012345678", &mut mem), // 9 bytes > 8
            Err(LinuxLoadError::CmdlineTooLong { len: 9, limit: 8 })
        );
        // Exactly cmdline_size bytes is accepted, and boot_params advertises 8.
        let li = load(&img, &[], ram, "01234567", &mut mem).expect("8 bytes fits");
        let _ = li;
        assert_eq!(rd32(&mem, BOOT_PARAMS_GPA as usize + 0x238), 8);
    }

    // --- exact boundary tests (mutation hardening of the `<`/`>`/`<=`/`>=`) ---

    #[test]
    fn protocol_version_boundary_is_inclusive() {
        // Exactly MIN_PROTOCOL_VERSION (2.12) is ACCEPTED (kills `<`→`<=`); one
        // below is rejected (kills `<`→`>`/`==`).
        let ok = synth_bzimage(1, MIN_PROTOCOL_VERSION, XLF_KERNEL_64, 0x10_0000, 0, 0x400);
        assert!(
            parse_setup_header(&ok).is_ok(),
            "version == 2.12 is accepted"
        );
        let bad = synth_bzimage(
            1,
            MIN_PROTOCOL_VERSION - 1,
            XLF_KERNEL_64,
            0x10_0000,
            0,
            0x400,
        );
        assert_eq!(
            parse_setup_header(&bad),
            Err(LinuxLoadError::UnsupportedProtocol {
                found: MIN_PROTOCOL_VERSION - 1,
                required: MIN_PROTOCOL_VERSION,
            })
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "allocates guest RAM")]
    fn rejects_pref_address_below_high_ram() {
        // A pref_address just below 1 MiB is rejected (the entry/load region must be
        // in high RAM) — kills `load_addr < HIGH_RAM_START` → `>`/`==`.
        let img = synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x000F_0000, 0, 0x400);
        let ram = 8u64 << 20;
        let mut mem = vec![0u8; ram as usize];
        assert!(matches!(
            load(&img, &[], ram, "x", &mut mem),
            Err(LinuxLoadError::KernelDoesNotFit {
                load: 0x000F_0000,
                ..
            })
        ));
    }

    #[test]
    #[cfg_attr(miri, ignore = "allocates guest RAM")]
    fn ram_exactly_high_ram_start_is_too_small() {
        // ram == HIGH_RAM_START has an empty high region → RamTooSmall (kills the
        // `ram <= HIGH_RAM_START` → `<` boundary mutant; one byte more is not
        // RamTooSmall).
        let img = valid_bzimage();
        let n = HIGH_RAM_START as usize;
        let mut mem = vec![0u8; n];
        assert_eq!(
            load(&img, &[], HIGH_RAM_START, "x", &mut mem),
            Err(LinuxLoadError::RamTooSmall {
                ram: HIGH_RAM_START
            })
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "allocates guest RAM")]
    fn kernel_end_equal_to_ram_fits() {
        // pref=1 MiB, init_size=1 MiB ⇒ kernel_end = 2 MiB; ram = 2 MiB (page
        // aligned) ⇒ kernel_end == ram (kills `kernel_end > ram` → `>=`), and an
        // empty initramfs is placed at start == kernel_end == ram (kills
        // `start < kernel_end` in place_initramfs → `<=`).
        let img = synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x10_0000, 0x10_0000, 0x400);
        let ram = 0x20_0000u64;
        let mut mem = vec![0u8; ram as usize];
        let li = load(&img, &[], ram, "x", &mut mem).expect("kernel_end == ram fits");
        assert_eq!(li.initramfs.start, ram, "empty initramfs at start == ram");
        assert_eq!(li.initramfs.len, 0);
        // One byte less of RAM (still page-multiple boundary moot) no longer fits.
        let mut tight = vec![0u8; (ram - 0x1000) as usize];
        assert!(matches!(
            load(&img, &[], ram - 0x1000, "x", &mut tight),
            Err(LinuxLoadError::KernelDoesNotFit { .. })
        ));
    }

    /// E820 reservation splits: the 4 KiB xAPIC LAPIC MMIO page (task 54, gate 1)
    /// and the two hypercall-doorbell pages `[0xE000, 0x10000)` (task 73). Each is
    /// carved out of usable RAM and marked `E820_RESERVED`, so the kernel never
    /// zeroes it — the LAPIC page routes to the userspace xAPIC model, and the
    /// doorbell pages stay intact for the guest SDK transport. The doorbell split
    /// makes low RAM **three** entries (indices 0–2); high RAM starts at index 3.
    mod e820_lapic_reservation {
        use super::super::*; // crate items, incl. the private `build_boot_params`
        use proptest::prelude::*;

        /// E820 table for a guest of `ram` bytes. Only `ram` drives the map, so a
        /// zeroed header + empty ramdisk suffice.
        fn table_for(ram: u64) -> BootParams {
            build_boot_params(
                &SetupHeader::new_zeroed(),
                &GpaRange { start: 0, len: 0 },
                0,
                ram,
            )
        }

        /// `(addr, size, type_)` of entry `i`, copied out by value (the entries are
        /// `#[repr(packed)]`, so taking a field reference would be unaligned).
        fn entry(bp: &BootParams, i: usize) -> (u64, u64, u32) {
            let e = &bp.e820_table[i];
            (e.addr, e.size, e.type_)
        }

        /// The three low-RAM entries every guest has (task 73): `[0, 0xE000) RAM`,
        /// the reserved doorbell pages `[0xE000, 0x10000)`, and `[0x10000, 640K)
        /// RAM`. High RAM begins at index 3.
        fn assert_low_split(bp: &BootParams) {
            assert_eq!(entry(bp, 0), (0, DOORBELL_PAGES_START, E820_RAM));
            assert_eq!(
                entry(bp, 1),
                (
                    DOORBELL_PAGES_START,
                    DOORBELL_PAGES_END - DOORBELL_PAGES_START,
                    E820_RESERVED
                ),
                "the doorbell pages are reserved"
            );
            assert_eq!(
                entry(bp, 2),
                (
                    DOORBELL_PAGES_END,
                    LOW_RAM_TOP - DOORBELL_PAGES_END,
                    E820_RAM
                )
            );
        }

        /// Far fewer cases under Miri (10–100× slower interpreted), and no failure
        /// persistence there (its regression-file path uses `getcwd`, which Miri's
        /// fs isolation rejects) — mirrors the loader's other proptest helpers.
        fn cases(native: u32) -> ProptestConfig {
            let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { native });
            if cfg!(miri) {
                cfg.failure_persistence = None;
            }
            cfg
        }

        /// 8 GiB guest: EXACTLY six entries — the 3-entry low split (doorbell pages
        /// reserved) + the 3-entry high split with the LAPIC page `E820_RESERVED`.
        #[test]
        fn eight_gib_guest_reserves_the_lapic_page() {
            let ram = 8u64 << 30;
            let bp = table_for(ram);
            assert_eq!(bp.e820_entries, 6);
            assert_low_split(&bp);
            assert_eq!(
                entry(&bp, 3),
                (HIGH_RAM_START, LAPIC_MMIO_PAGE - HIGH_RAM_START, E820_RAM)
            );
            assert_eq!(entry(&bp, 4), (LAPIC_MMIO_PAGE, 0x1000, E820_RESERVED));
            assert_eq!(
                entry(&bp, 5),
                (
                    LAPIC_MMIO_PAGE + 0x1000,
                    ram - (LAPIC_MMIO_PAGE + 0x1000),
                    E820_RAM
                )
            );
            // The 7th slot is untouched (exactly six entries written).
            assert_eq!(entry(&bp, 6), (0, 0, 0));
        }

        /// Sub-`0xFEE01000` guest (2 GiB): the 3-entry low split + one high-RAM
        /// entry, no reserved LAPIC page.
        #[test]
        fn sub_page_guest_is_four_entries() {
            let ram = 2u64 << 30;
            let bp = table_for(ram);
            assert_eq!(bp.e820_entries, 4);
            assert_low_split(&bp);
            assert_eq!(
                entry(&bp, 3),
                (HIGH_RAM_START, ram - HIGH_RAM_START, E820_RAM)
            );
            // No fifth entry written.
            assert_eq!(entry(&bp, 4), (0, 0, 0));
        }

        /// Page-aligned boundaries: RAM ending exactly at the page start stays a
        /// single high-RAM entry (the page is excluded); ending one page past it
        /// reserves the page with the empty tail dropped. Both atop the 3-entry
        /// low split (⇒ 4 and 5 entries).
        #[test]
        fn page_aligned_boundaries() {
            let bp = table_for(LAPIC_MMIO_PAGE);
            assert_eq!(bp.e820_entries, 4);
            assert_low_split(&bp);
            assert_eq!(
                entry(&bp, 3),
                (HIGH_RAM_START, LAPIC_MMIO_PAGE - HIGH_RAM_START, E820_RAM)
            );

            let bp = table_for(LAPIC_MMIO_PAGE + 0x1000);
            assert_eq!(bp.e820_entries, 5);
            assert_low_split(&bp);
            assert_eq!(
                entry(&bp, 3),
                (HIGH_RAM_START, LAPIC_MMIO_PAGE - HIGH_RAM_START, E820_RAM)
            );
            assert_eq!(entry(&bp, 4), (LAPIC_MMIO_PAGE, 0x1000, E820_RESERVED));
            assert_eq!(entry(&bp, 5), (0, 0, 0));
        }

        /// THE task-73 property: for ANY guest RAM size, the doorbell pages
        /// `[0xE000, 0x10000)` are reserved and NEVER inside a usable-RAM E820
        /// entry — so a Linux SDK guest cannot allocate over REQ_GPA/RESP_GPA.
        #[test]
        fn doorbell_pages_are_reserved_for_every_ram_size() {
            for ram in [
                HIGH_RAM_START + 0x1000,
                2u64 << 30,
                LAPIC_MMIO_PAGE,
                LAPIC_MMIO_PAGE + 0x1000,
                8u64 << 30,
            ] {
                let bp = table_for(ram);
                assert_low_split(&bp); // the reserved doorbell entry, exactly
                for i in 0..bp.e820_entries as usize {
                    let (addr, size, type_) = entry(&bp, i);
                    if type_ == E820_RAM {
                        let overlaps =
                            addr < DOORBELL_PAGES_END && DOORBELL_PAGES_START < addr + size;
                        assert!(
                            !overlaps,
                            "ram={ram:#x}: RAM entry {i} [{addr:#x}, +{size:#x}) covers a doorbell page"
                        );
                    }
                }
            }
        }

        proptest! {
            #![proptest_config(cases(512))]

            /// THE gate-1 property: for ANY guest RAM size the LAPIC MMIO page is NEVER
            /// inside a usable-RAM E820 entry — it is reserved (RAM reaches it) or
            /// unmapped (RAM stops short). Also pins the per-case shape.
            #[test]
            fn reserved_page_is_never_typed_ram(ram in prop_oneof![
                (HIGH_RAM_START + 1)..=LAPIC_MMIO_PAGE,             // 2-entry
                (LAPIC_MMIO_PAGE + 1)..=(LAPIC_MMIO_PAGE + 0x1000), // 3-entry band
                (LAPIC_MMIO_PAGE + 0x1001)..=(64u64 << 30),        // 4-entry
            ]) {
                let bp = table_for(ram);
                let n = bp.e820_entries as usize;
                for i in 0..n {
                    let (addr, size, type_) = entry(&bp, i);
                    if type_ == E820_RAM {
                        let overlaps =
                            addr < LAPIC_MMIO_PAGE + 0x1000 && LAPIC_MMIO_PAGE < addr + size;
                        prop_assert!(
                            !overlaps,
                            "RAM entry {i} [{addr:#x}, +{size:#x}) covers the LAPIC page"
                        );
                    }
                }
                // The LAPIC page is reserved exactly when RAM reaches it — now at
                // index 4 (the 3-entry doorbell low split precedes it). When RAM
                // stops short, only the low split + one high-RAM entry (4 total).
                if ram > LAPIC_MMIO_PAGE {
                    prop_assert_eq!(entry(&bp, 4), (LAPIC_MMIO_PAGE, 0x1000, E820_RESERVED));
                } else {
                    prop_assert_eq!(bp.e820_entries, 4);
                }
            }
        }
    }

    /// Kernel + initramfs placement must avoid the reserved xAPIC MMIO hole (task 54
    /// review): a guest-visible image placed in the unmapped page would be written to
    /// host backing the guest cannot read back.
    mod lapic_hole_placement {
        use super::super::*; // crate items, incl. private place_initramfs / the guard

        /// The overlap predicate the kernel guard and the initramfs relocation share
        /// (testing it covers the kernel-guard decision without a multi-GiB `load`).
        #[test]
        fn overlaps_lapic_mmio_page_detects_straddle() {
            let p = LAPIC_MMIO_PAGE;
            // Disjoint (touching the exclusive edges) → no overlap.
            assert!(!overlaps_lapic_mmio_page(0x10_0000, p));
            assert!(!overlaps_lapic_mmio_page(p + 0x1000, p + 0x2000));
            assert!(!overlaps_lapic_mmio_page(0, 0x1000));
            // Overlapping the page in every way → overlap.
            assert!(overlaps_lapic_mmio_page(p, p + 0x1000)); // exactly the page
            assert!(overlaps_lapic_mmio_page(p - 0x1000, p + 1)); // straddles low edge
            assert!(overlaps_lapic_mmio_page(p + 0xFFF, p + 0x2000)); // last byte of page
            assert!(overlaps_lapic_mmio_page(0, u64::MAX)); // contains it
        }

        /// An initramfs whose highest placement would land in the hole is relocated to
        /// sit ENTIRELY BELOW the page (the ceiling is pushed inside the page via
        /// `initrd_addr_max`, so a tiny ramdisk reproduces the straddle without a
        /// multi-GiB allocation).
        #[test]
        fn initramfs_straddling_the_hole_is_relocated_below() {
            let ram = 8u64 << 30;
            let initramfs = vec![0u8; 0x500];
            // ceiling = initrd_addr_max + 1 = 0xFEE00800 — INSIDE the page, so the
            // high placement (start 0xFEE00000) overlaps the hole.
            let r = place_initramfs(&initramfs, ram, 0x20_0000, LAPIC_MMIO_PAGE + 0x7FF)
                .expect("relocates below the hole");
            assert!(
                !overlaps_lapic_mmio_page(r.start, r.start + r.len),
                "relocated out of the hole: [{:#x}, +{:#x})",
                r.start,
                r.len
            );
            assert!(
                r.start + r.len <= LAPIC_MMIO_PAGE,
                "sits entirely below the page"
            );
            assert_eq!(r.start % 0x1000, 0, "page-aligned");
        }

        /// A ramdisk that fits ABOVE the page (between `page+0x1000` and the ceiling)
        /// is left at its high placement — the hole is only avoided, not a hard cap.
        #[test]
        fn initramfs_above_the_hole_is_kept_high() {
            let ram = 8u64 << 30; // ceiling clamps to 4 GiB, well above the page
            let initramfs = vec![0u8; 0x1000];
            let r = place_initramfs(&initramfs, ram, 0x20_0000, 0).expect("fits high");
            assert!(
                r.start >= LAPIC_MMIO_PAGE + 0x1000,
                "kept above the hole: {:#x}",
                r.start
            );
        }

        /// If the ramdisk cannot fit below the hole either (kernel pushed up against
        /// the relocation target), it is rejected — mirroring the existing
        /// does-not-fit error.
        #[test]
        fn initramfs_that_cannot_fit_below_the_hole_is_rejected() {
            let ram = 8u64 << 30;
            let initramfs = vec![0u8; 0x500];
            // Same straddling ceiling, but kernel_end sits just above where the
            // below-the-hole placement (0xFEDFF000) would start → no room.
            let err = place_initramfs(
                &initramfs,
                ram,
                LAPIC_MMIO_PAGE - 0x800,
                LAPIC_MMIO_PAGE + 0x7FF,
            )
            .expect_err("must not fit");
            assert!(matches!(err, LinuxLoadError::InitramfsDoesNotFit { .. }));
        }
    }
}
