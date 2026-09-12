// SPDX-License-Identifier: AGPL-3.0-or-later

use zerocopy::{FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout};

pub const PML4_GPA: u64 = 0x1000;
pub const PDPT_GPA: u64 = 0x2000;
pub const PD_GPA: u64 = 0x3000;
pub const GDT_GPA: u64 = 0x6000;
pub const BOOT_PARAMS_GPA: u64 = 0x7000;
pub const CMDLINE_GPA: u64 = 0x8000;

pub const CMDLINE_MAX: usize = 0x800;

pub const IDENTITY_MAP_BYTES: u64 = 1 << 30;
const LARGE_PAGE: u64 = 2 << 20;
const PD_ENTRIES: u64 = IDENTITY_MAP_BYTES / LARGE_PAGE;
const PTE_P_RW: u64 = 0b11;
const PDE_P_RW_PS: u64 = 0b1000_0011;

pub const SETUP_HEADER_OFFSET: usize = 0x1f1;
const BOOT_FLAG_MAGIC: u16 = 0xAA55;
const HDRS_MAGIC: u32 = 0x5372_6448;
const MIN_PROTOCOL_VERSION: u16 = 0x020c;
const XLF_KERNEL_64: u16 = 1;
pub const ENTRY_64_OFFSET: u64 = 0x200;
const SECTOR: usize = 512;
const DEFAULT_SETUP_SECTS: u8 = 4;
const TYPE_OF_LOADER_UNDEFINED: u8 = 0xFF;

const LOW_RAM_TOP: u64 = 0x000A_0000;
const DOORBELL_PAGES_START: u64 = 0x0000_E000;
const DOORBELL_PAGES_END: u64 = 0x0001_0000;
const HIGH_RAM_START: u64 = 0x0010_0000;
const E820_RAM: u32 = 1;
const E820_RESERVED: u32 = 2;
pub(crate) const LAPIC_MMIO_PAGE: u64 = 0xFEE0_0000;
pub(crate) const LAPIC_MMIO_PAGE_LEN: u64 = 0x1000;
pub const ACPI_RSDP_GPA: u64 = 0x000E_0000;
const ACPI_XSDT_GPA: u64 = ACPI_RSDP_GPA + 0x40;
const ACPI_MADT_GPA: u64 = ACPI_RSDP_GPA + 0x80;
const ACPI_LAPIC_BASE: u32 = LAPIC_MMIO_PAGE as u32;
const ACPI_BOOT_APIC_ID: u8 = 0;
const E820_MAX_ENTRIES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct SetupHeader {
    pub setup_sects: u8,
    pub root_flags: u16,
    pub syssize: u32,
    pub ram_size: u16,
    pub vid_mode: u16,
    pub root_dev: u16,
    pub boot_flag: u16,
    pub jump: u16,
    pub header: u32,
    pub version: u16,
    pub realmode_swtch: u32,
    pub start_sys_seg: u16,
    pub kernel_version: u16,
    pub type_of_loader: u8,
    pub loadflags: u8,
    pub setup_move_size: u16,
    pub code32_start: u32,
    pub ramdisk_image: u32,
    pub ramdisk_size: u32,
    pub bootsect_kludge: u32,
    pub heap_end_ptr: u16,
    pub ext_loader_ver: u8,
    pub ext_loader_type: u8,
    pub cmd_line_ptr: u32,
    pub initrd_addr_max: u32,
    pub kernel_alignment: u32,
    pub relocatable_kernel: u8,
    pub min_alignment: u8,
    pub xloadflags: u16,
    pub cmdline_size: u32,
    pub hardware_subarch: u32,
    pub hardware_subarch_data: u64,
    pub payload_offset: u32,
    pub payload_length: u32,
    pub setup_data: u64,
    pub pref_address: u64,
    pub init_size: u32,
    pub handover_offset: u32,
    pub kernel_info_offset: u32,
}

#[derive(Clone, Copy, Debug, Default, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct BootE820Entry {
    pub addr: u64,
    pub size: u64,
    pub type_: u32,
}

#[derive(Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct BootParams {
    _head: [u8; 0x070],
    pub acpi_rsdp_addr: [u8; 8],
    _head2: [u8; 0x1e8 - 0x078],
    pub e820_entries: u8,
    _pad_to_hdr: [u8; SETUP_HEADER_OFFSET - 0x1e9],
    pub hdr: SetupHeader,
    _pad_to_e820: [u8; 0x2d0 - (SETUP_HEADER_OFFSET + core::mem::size_of::<SetupHeader>())],
    pub e820_table: [BootE820Entry; E820_MAX_ENTRIES],
    _tail: [u8; 0x1000 - (0x2d0 + E820_MAX_ENTRIES * core::mem::size_of::<BootE820Entry>())],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GpaRange {
    pub start: u64,
    pub len: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinuxImage {
    pub entry_point: u64,
    pub boot_params_gpa: u64,
    pub page_table_root: u64,
    pub gdt_gpa: u64,
    pub cmdline_gpa: u64,
    pub kernel: GpaRange,
    pub initramfs: GpaRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LinuxLoadError {
    #[error("not a bzImage (too short, or boot_flag/HdrS magic absent)")]
    NotBzImage,
    #[error("boot-protocol version {found:#06x} < required {required:#06x} (no xloadflags)")]
    UnsupportedProtocol { found: u16, required: u16 },
    #[error("kernel has no 64-bit entry point (XLF_KERNEL_64 not set in xloadflags)")]
    No64BitEntry,
    #[error("setup sectors ({setup_bytes} bytes) exceed the {image_len}-byte image")]
    TruncatedImage {
        setup_bytes: usize,
        image_len: usize,
    },
    #[error("kernel load region [{load:#x}..{end:#x}) does not fit in {ram:#x} bytes of guest RAM")]
    KernelDoesNotFit { load: u64, end: u64, ram: u64 },
    #[error(
        "protected-mode kernel is {len} bytes — too small for the 64-bit entry at +{:#x}",
        ENTRY_64_OFFSET
    )]
    KernelTooSmall { len: u64 },
    #[error(
        "initramfs ({len} bytes) does not fit below the max load address without overlapping the kernel"
    )]
    InitramfsDoesNotFit { len: u64 },
    #[error("command line is {len} bytes; this kernel's effective limit is {limit} (excl. NUL)")]
    CmdlineTooLong { len: usize, limit: usize },
    #[error("guest RAM ({ram:#x} bytes) is too small for the boot structures")]
    RamTooSmall { ram: u64 },
}

fn read_u16(image: &[u8], off: usize) -> Option<u16> {
    let end = off.checked_add(2)?;
    let b = image.get(off..end)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32(image: &[u8], off: usize) -> Option<u32> {
    let end = off.checked_add(4)?;
    let b = image.get(off..end)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

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

#[cfg(any(all(target_os = "linux", target_arch = "x86_64"), test))]
pub(crate) fn write_rng_seed(mem: &mut [u8], seed: &[u8; 64]) -> Result<(), LinuxLoadError> {
    const RNG_GPA: u64 = 0x9000;
    let error = LinuxLoadError::RamTooSmall {
        ram: mem.len() as u64,
    };
    let mut record = [0u8; 80];
    record[8..12].copy_from_slice(&9u32.to_le_bytes());
    record[12..16].copy_from_slice(&64u32.to_le_bytes());
    record[16..].copy_from_slice(seed);
    write_at(mem, RNG_GPA, &record, error)?;
    write_at(mem, BOOT_PARAMS_GPA + 0x250, &RNG_GPA.to_le_bytes(), error)
}

pub fn parse_setup_header(image: &[u8]) -> Result<SetupHeader, LinuxLoadError> {
    if read_u16(image, 0x1fe) != Some(BOOT_FLAG_MAGIC) || read_u32(image, 0x202) != Some(HDRS_MAGIC)
    {
        return Err(LinuxLoadError::NotBzImage);
    }
    let tail = image
        .get(SETUP_HEADER_OFFSET..)
        .ok_or(LinuxLoadError::NotBzImage)?;
    let (hdr, _) = SetupHeader::read_from_prefix(tail).map_err(|_| LinuxLoadError::NotBzImage)?;

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

pub fn load(
    image: &[u8],
    initramfs: &[u8],
    ram_bytes: u64,
    cmdline: &str,
    mem: &mut [u8],
) -> Result<LinuxImage, LinuxLoadError> {
    let ram = ram_bytes.min(mem.len() as u64);
    if ram <= HIGH_RAM_START {
        return Err(LinuxLoadError::RamTooSmall { ram });
    }

    let hdr = parse_setup_header(image)?;

    let setup_sects = if hdr.setup_sects == 0 {
        DEFAULT_SETUP_SECTS
    } else {
        hdr.setup_sects
    };
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
    if (pm_kernel.len() as u64) <= ENTRY_64_OFFSET {
        return Err(LinuxLoadError::KernelTooSmall {
            len: pm_kernel.len() as u64,
        });
    }

    let load_addr = hdr.pref_address;
    if load_addr < HIGH_RAM_START {
        return Err(LinuxLoadError::KernelDoesNotFit {
            load: load_addr,
            end: load_addr,
            ram,
        });
    }
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
    write_at(
        mem,
        CMDLINE_GPA + cmdline.len() as u64,
        &[0u8],
        LinuxLoadError::RamTooSmall { ram },
    )?;

    let boot_params = build_boot_params(&hdr, &initramfs_range, cmdline.len(), ram);
    write_at(
        mem,
        BOOT_PARAMS_GPA,
        boot_params.as_bytes(),
        LinuxLoadError::RamTooSmall { ram },
    )?;

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
    let mut ceiling = ram.min(1u64 << 32);
    if initrd_addr_max != 0 {
        ceiling = ceiling.min(initrd_addr_max.saturating_add(1));
    }
    let place = |cap: u64| -> Option<u64> {
        let start = cap.checked_sub(len)? & !0xFFF;
        (start >= kernel_end).then_some(start)
    };
    let mut start = place(ceiling).ok_or(too_big)?;
    if overlaps_lapic_mmio_page(start, start.saturating_add(len)) {
        start = place(LAPIC_MMIO_PAGE).ok_or(too_big)?;
    }
    Ok(GpaRange { start, len })
}

fn cmdline_max(hdr: &SetupHeader) -> usize {
    (CMDLINE_MAX - 1).min(hdr.cmdline_size as usize)
}

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
    bp.hdr.cmdline_size = cmdline_max(hdr) as u32;
    let _ = cmdline_len;
    bp.hdr.ramdisk_image = initramfs.start as u32;
    bp.hdr.ramdisk_size = initramfs.len as u32;

    let mut n = 0usize;
    let push = |bp: &mut BootParams, n: &mut usize, addr: u64, size: u64, type_: u32| {
        bp.e820_table[*n] = BootE820Entry { addr, size, type_ };
        *n += 1;
    };

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

fn acpi_checksum(bytes: &[u8]) -> u8 {
    bytes
        .iter()
        .fold(0u8, |a, b| a.wrapping_add(*b))
        .wrapping_neg()
}

fn write_acpi_tables(mem: &mut [u8]) -> Result<(), LinuxLoadError> {
    let oob = LinuxLoadError::RamTooSmall { ram: ACPI_RSDP_GPA };
    const OEMID: &[u8; 6] = b"HARMNY";
    const OEM_TABLE_ID: &[u8; 8] = b"HARMONYT";
    const CREATOR_ID: &[u8; 4] = b"HARM";

    let mut madt = [0u8; 52];
    madt[0..4].copy_from_slice(b"APIC");
    madt[4..8].copy_from_slice(&52u32.to_le_bytes());
    madt[8] = 5;
    madt[10..16].copy_from_slice(OEMID);
    madt[16..24].copy_from_slice(OEM_TABLE_ID);
    madt[24..28].copy_from_slice(&1u32.to_le_bytes());
    madt[28..32].copy_from_slice(CREATOR_ID);
    madt[32..36].copy_from_slice(&1u32.to_le_bytes());
    madt[36..40].copy_from_slice(&ACPI_LAPIC_BASE.to_le_bytes());
    madt[40..44].copy_from_slice(&1u32.to_le_bytes());
    madt[44] = 0;
    madt[45] = 8;
    madt[46] = 0;
    madt[47] = ACPI_BOOT_APIC_ID;
    madt[48..52].copy_from_slice(&1u32.to_le_bytes());
    madt[9] = acpi_checksum(&madt);

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

    let mut rsdp = [0u8; 36];
    rsdp[0..8].copy_from_slice(b"RSD PTR ");
    rsdp[9..15].copy_from_slice(OEMID);
    rsdp[15] = 2;
    rsdp[20..24].copy_from_slice(&36u32.to_le_bytes());
    rsdp[24..32].copy_from_slice(&ACPI_XSDT_GPA.to_le_bytes());
    rsdp[8] = acpi_checksum(&rsdp[0..20]);
    rsdp[32] = acpi_checksum(&rsdp);

    write_at(mem, ACPI_MADT_GPA, &madt, oob)?;
    write_at(mem, ACPI_XSDT_GPA, &xsdt, oob)?;
    write_at(mem, ACPI_RSDP_GPA, &rsdp, oob)?;
    Ok(())
}

fn write_page_tables(mem: &mut [u8], ram: u64) -> Result<(), LinuxLoadError> {
    let oob = LinuxLoadError::RamTooSmall { ram };
    write_at(mem, PML4_GPA, &(PDPT_GPA | PTE_P_RW).to_le_bytes(), oob)?;
    write_at(mem, PDPT_GPA, &(PD_GPA | PTE_P_RW).to_le_bytes(), oob)?;
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

fn write_gdt(mem: &mut [u8]) -> Result<(), LinuxLoadError> {
    let oob = LinuxLoadError::RamTooSmall {
        ram: mem.len() as u64,
    };
    let gdt: [u64; 4] = [0, 0, GDT_CODE64, GDT_DATA];
    for (i, e) in gdt.iter().enumerate() {
        write_at(mem, GDT_GPA + (i as u64) * 8, &e.to_le_bytes(), oob)?;
    }
    Ok(())
}

pub const GDT_CODE64: u64 = 0x00AF_9B00_0000_FFFF;
pub const GDT_DATA: u64 = 0x00CF_9300_0000_FFFF;

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    #[test]
    fn acpi_tables_are_byte_exact() {
        let base = ACPI_RSDP_GPA as usize;
        let mut mem = vec![0u8; base + 0x1000];
        write_acpi_tables(&mut mem).expect("write_acpi_tables into a large-enough buffer");
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
        let mut hdr = SetupHeader::new_zeroed();
        hdr.setup_sects = setup_sects;
        hdr.boot_flag = BOOT_FLAG_MAGIC;
        hdr.header = HDRS_MAGIC;
        hdr.version = version;
        hdr.xloadflags = xloadflags;
        hdr.pref_address = pref_address;
        hdr.init_size = init_size;
        hdr.cmdline_size = 0x1000;
        hdr.initrd_addr_max = 0x7FFF_FFFF;
        let hb = hdr.as_bytes();
        img[SETUP_HEADER_OFFSET..SETUP_HEADER_OFFSET + hb.len()].copy_from_slice(hb);
        for (i, b) in img[pm_off..].iter_mut().enumerate() {
            *b = (0x40 + (i % 0x30)) as u8;
        }
        img
    }

    fn valid_bzimage() -> Vec<u8> {
        synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x10_0000, 0x40_0000, 0x2000)
    }

    #[test]
    fn setup_header_field_offsets() {
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
        assert_eq!(size_of::<SetupHeader>(), 0x26c - 0x1f1);
    }

    #[test]
    fn boot_params_field_offsets() {
        assert_eq!(offset_of!(BootParams, acpi_rsdp_addr), 0x070);
        assert_eq!(offset_of!(BootParams, e820_entries), 0x1e8);
        assert_eq!(offset_of!(BootParams, hdr), 0x1f1);
        assert_eq!(offset_of!(BootParams, e820_table), 0x2d0);
        assert_eq!(
            offset_of!(BootParams, hdr) + offset_of!(SetupHeader, ramdisk_image),
            0x218
        );
        assert_eq!(
            offset_of!(BootParams, hdr) + offset_of!(SetupHeader, cmd_line_ptr),
            0x228
        );
        assert_eq!(size_of::<BootParams>(), 0x1000);
        assert_eq!(size_of::<BootE820Entry>(), 20);
    }

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

    fn rd32(mem: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(mem[off..off + 4].try_into().unwrap())
    }
    fn rd64(mem: &[u8], off: usize) -> u64 {
        u64::from_le_bytes(mem[off..off + 8].try_into().unwrap())
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "allocates guest RAM; totality is covered by the miri-bounded loader proptest"
    )]
    fn load_pins_every_computed_value() {
        let img = synth_bzimage(2, 0x020f, XLF_KERNEL_64, 0x10_0000, 0x40_0000, 0x800);
        let pm_off = (2 + 1) * 512usize;
        let kernel_len = 0x800u64;
        let initramfs: Vec<u8> = (0..0x2345u32).map(|i| (i & 0xFF) as u8).collect();
        let ram = 8u64 << 20;
        let mut mem = vec![0xFFu8; ram as usize];
        let cmd = "console=ttyS0 panic=-1";
        let li = load(&img, &initramfs, ram, cmd, &mut mem).expect("load");

        assert_eq!(li.kernel.start, 0x10_0000, "load_addr = pref_address");
        assert_eq!(li.kernel.len, kernel_len, "kernel_len = pm_kernel.len()");
        assert_eq!(li.entry_point, 0x10_0200, "entry = load_addr + 0x200");
        assert_eq!(li.boot_params_gpa, 0x7000);
        assert_eq!(li.page_table_root, 0x1000);
        assert_eq!(li.gdt_gpa, 0x6000);
        assert_eq!(li.cmdline_gpa, 0x8000);
        assert_eq!(li.initramfs.start, 0x7F_D000, "page-aligned high placement");
        assert_eq!(li.initramfs.len, 0x2345);

        assert_eq!(
            &mem[0x10_0000..0x10_0000 + kernel_len as usize],
            &img[pm_off..pm_off + kernel_len as usize],
            "kernel copied from (setup_sects+1)*512"
        );
        let istart = li.initramfs.start as usize;
        assert_eq!(&mem[istart..istart + initramfs.len()], &initramfs[..]);

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
        assert_eq!(
            rd32(&mem, bp + 0x238),
            0x7FF,
            "cmdline_size = CMDLINE_MAX - 1"
        );

        let c = CMDLINE_GPA as usize;
        assert_eq!(&mem[c..c + cmd.len()], cmd.as_bytes());
        assert_eq!(mem[c + cmd.len()], 0);

        let e0 = bp + 0x2d0;
        assert_eq!(rd64(&mem, e0), 0);
        assert_eq!(rd64(&mem, e0 + 8), DOORBELL_PAGES_START);
        assert_eq!(rd32(&mem, e0 + 16), E820_RAM);
        let e1 = e0 + 20;
        assert_eq!(rd64(&mem, e1), DOORBELL_PAGES_START);
        assert_eq!(
            rd64(&mem, e1 + 8),
            DOORBELL_PAGES_END - DOORBELL_PAGES_START
        );
        assert_eq!(
            rd32(&mem, e1 + 16),
            E820_RESERVED,
            "doorbell pages reserved"
        );
        let e2 = e1 + 20;
        assert_eq!(rd64(&mem, e2), DOORBELL_PAGES_END);
        assert_eq!(rd64(&mem, e2 + 8), LOW_RAM_TOP - DOORBELL_PAGES_END);
        assert_eq!(rd32(&mem, e2 + 16), E820_RAM);
        let e3 = e2 + 20;
        assert_eq!(rd64(&mem, e3), HIGH_RAM_START);
        assert_eq!(rd64(&mem, e3 + 8), ram - HIGH_RAM_START);
        assert_eq!(rd32(&mem, e3 + 16), E820_RAM);
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

        let pml4_0 = u64::from_le_bytes(mem[0x1000..0x1008].try_into().unwrap());
        assert_eq!(pml4_0, PDPT_GPA | PTE_P_RW);
        let pdpt_0 = u64::from_le_bytes(mem[0x2000..0x2008].try_into().unwrap());
        assert_eq!(pdpt_0, PD_GPA | PTE_P_RW);
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
        let img = synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x10_0000, 0, 0x1000);
        let ram = 4u64 << 20;
        let mut mem = vec![0u8; ram as usize];
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

    #[test]
    #[cfg_attr(miri, ignore = "allocates guest RAM")]
    fn rejects_kernel_too_small_for_64bit_entry() {
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
        let mut img = synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x10_0000, 0x10_0000, 0x800);
        let addr_max: u32 = 0x0060_0000;
        img[0x22c..0x230].copy_from_slice(&addr_max.to_le_bytes());
        let ram = 8u64 << 20;
        let mut mem = vec![0u8; ram as usize];
        let initramfs = vec![0u8; 0x1000];
        let li = load(&img, &initramfs, ram, "x", &mut mem).expect("load");
        assert_eq!(li.initramfs.start, 0x5F_F000);
        assert!(li.initramfs.start + li.initramfs.len <= u64::from(addr_max) + 1);

        let mut low = synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x10_0000, 0x10_0000, 0x800);
        low[0x22c..0x230].copy_from_slice(&0x0010_0000u32.to_le_bytes());
        assert!(matches!(
            load(&low, &initramfs, ram, "x", &mut mem),
            Err(LinuxLoadError::InitramfsDoesNotFit { .. })
        ));
    }

    #[test]
    #[cfg_attr(miri, ignore = "allocates guest RAM")]
    fn caps_cmdline_against_header_cmdline_size() {
        let mut img = valid_bzimage();
        img[0x238..0x23c].copy_from_slice(&8u32.to_le_bytes());
        let ram = 8u64 << 20;
        let mut mem = vec![0u8; ram as usize];
        assert_eq!(
            load(&img, &[], ram, "012345678", &mut mem),
            Err(LinuxLoadError::CmdlineTooLong { len: 9, limit: 8 })
        );
        let li = load(&img, &[], ram, "01234567", &mut mem).expect("8 bytes fits");
        let _ = li;
        assert_eq!(rd32(&mem, BOOT_PARAMS_GPA as usize + 0x238), 8);
    }

    #[test]
    fn protocol_version_boundary_is_inclusive() {
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
        let img = synth_bzimage(1, 0x020f, XLF_KERNEL_64, 0x10_0000, 0x10_0000, 0x400);
        let ram = 0x20_0000u64;
        let mut mem = vec![0u8; ram as usize];
        let li = load(&img, &[], ram, "x", &mut mem).expect("kernel_end == ram fits");
        assert_eq!(li.initramfs.start, ram, "empty initramfs at start == ram");
        assert_eq!(li.initramfs.len, 0);
        let mut tight = vec![0u8; (ram - 0x1000) as usize];
        assert!(matches!(
            load(&img, &[], ram - 0x1000, "x", &mut tight),
            Err(LinuxLoadError::KernelDoesNotFit { .. })
        ));
    }

    mod e820_lapic_reservation {
        use super::super::*;
        use proptest::prelude::*;

        fn table_for(ram: u64) -> BootParams {
            build_boot_params(
                &SetupHeader::new_zeroed(),
                &GpaRange { start: 0, len: 0 },
                0,
                ram,
            )
        }

        fn entry(bp: &BootParams, i: usize) -> (u64, u64, u32) {
            let e = &bp.e820_table[i];
            (e.addr, e.size, e.type_)
        }

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

        fn cases(native: u32) -> ProptestConfig {
            let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { native });
            if cfg!(miri) {
                cfg.failure_persistence = None;
            }
            cfg
        }

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
            assert_eq!(entry(&bp, 6), (0, 0, 0));
        }

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
            assert_eq!(entry(&bp, 4), (0, 0, 0));
        }

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
                assert_low_split(&bp);
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

            #[test]
            fn reserved_page_is_never_typed_ram(ram in prop_oneof![
                (HIGH_RAM_START + 1)..=LAPIC_MMIO_PAGE,
                (LAPIC_MMIO_PAGE + 1)..=(LAPIC_MMIO_PAGE + 0x1000),
                (LAPIC_MMIO_PAGE + 0x1001)..=(64u64 << 30),
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
                if ram > LAPIC_MMIO_PAGE {
                    prop_assert_eq!(entry(&bp, 4), (LAPIC_MMIO_PAGE, 0x1000, E820_RESERVED));
                } else {
                    prop_assert_eq!(bp.e820_entries, 4);
                }
            }
        }
    }

    mod lapic_hole_placement {
        use super::super::*;

        #[test]
        fn overlaps_lapic_mmio_page_detects_straddle() {
            let p = LAPIC_MMIO_PAGE;
            assert!(!overlaps_lapic_mmio_page(0x10_0000, p));
            assert!(!overlaps_lapic_mmio_page(p + 0x1000, p + 0x2000));
            assert!(!overlaps_lapic_mmio_page(0, 0x1000));
            assert!(overlaps_lapic_mmio_page(p, p + 0x1000));
            assert!(overlaps_lapic_mmio_page(p - 0x1000, p + 1));
            assert!(overlaps_lapic_mmio_page(p + 0xFFF, p + 0x2000));
            assert!(overlaps_lapic_mmio_page(0, u64::MAX));
        }

        #[test]
        fn initramfs_straddling_the_hole_is_relocated_below() {
            let ram = 8u64 << 30;
            let initramfs = vec![0u8; 0x500];
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

        #[test]
        fn initramfs_above_the_hole_is_kept_high() {
            let ram = 8u64 << 30;
            let initramfs = vec![0u8; 0x1000];
            let r = place_initramfs(&initramfs, ram, 0x20_0000, 0).expect("fits high");
            assert!(
                r.start >= LAPIC_MMIO_PAGE + 0x1000,
                "kept above the hole: {:#x}",
                r.start
            );
        }

        #[test]
        fn initramfs_that_cannot_fit_below_the_hole_is_rejected() {
            let ram = 8u64 << 30;
            let initramfs = vec![0u8; 0x500];
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

#[cfg(test)]
mod rng_seed_tests {
    use super::*;

    #[test]
    fn seed_record_has_linux_layout_and_preserves_surroundings() {
        let mut mem = vec![0xa5; 0xa000];
        let seed = std::array::from_fn(|i| i as u8);
        write_rng_seed(&mut mem, &seed).unwrap();
        assert_eq!(&mem[0x7250..0x7258], &0x9000u64.to_le_bytes());
        assert_eq!(&mem[0x9000..0x9008], &[0; 8]);
        assert_eq!(&mem[0x9008..0x9010], &[9, 0, 0, 0, 64, 0, 0, 0]);
        assert_eq!(&mem[0x9010..0x9050], &seed);
        assert_eq!(mem[0x8fff], 0xa5);
        assert_eq!(mem[0x9050], 0xa5);
    }

    #[test]
    fn seed_record_rejects_short_ram() {
        for len in [0, 0x7257, 0x9000, 0x904f] {
            let mut mem = vec![0; len];
            assert_eq!(
                write_rng_seed(&mut mem, &[0; 64]),
                Err(LinuxLoadError::RamTooSmall { ram: len as u64 })
            );
        }
        write_rng_seed(&mut vec![0; 0x9050], &[0; 64]).unwrap();
    }
}
