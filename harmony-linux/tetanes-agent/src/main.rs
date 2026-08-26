// SPDX-License-Identifier: AGPL-3.0-or-later
//! Linux/aarch64 entry point for the M2 TetaNES guest payload.

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn main() {
    if let Err(error) = live::run() {
        eprintln!("TETANES_AGENT_FAIL: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "aarch64", not(miri))))]
fn main() {
    eprintln!("harmony-tetanes-agent live mode requires Linux/aarch64");
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
mod live {
    use std::{
        fs::{File, OpenOptions},
        io::{Read, Seek, SeekFrom},
        os::fd::AsRawFd,
    };

    use harmony_sdk::{Point, Sdk};
    use harmony_tetanes_agent::{
        Agent, Channel, GUEST_PAGE_SIZE, WRAM_SIZE, decode_pagemap_entry, pagemap_offset,
    };
    use hypercall_doorbell::{MmioDoorbell, PAGE_SIZE, REQ_GPA, RESP_GPA, VmcallTransport};

    const DOORBELL_GPA: u64 = 0x0A00_0000;
    const REG_WRAM_GPA: u32 = 1;
    const REG_WRAM_LEN: u32 = 2;
    const CATALOG: &[Point] = &[
        Point::state(REG_WRAM_GPA, "tetanes_wram_gpa"),
        Point::state(REG_WRAM_LEN, "tetanes_wram_len"),
    ];

    type Transport = VmcallTransport<MmioDoorbell>;

    struct LiveChannel {
        sdk: Sdk<Transport>,
    }

    impl Channel for LiveChannel {
        type Error = String;

        fn payload_fetch(&mut self, out: &mut [u8]) -> Result<(), Self::Error> {
            self.sdk
                .client_mut()
                .payload_fetch(out)
                .map_err(|error| format!("payload_fetch: {error:?}"))
        }

        fn frame_complete(&mut self, frame_count: u64) -> Result<(), Self::Error> {
            self.sdk
                .frame_complete(frame_count)
                .map_err(|error| format!("frame_complete: {error:?}"))
        }
    }

    pub fn run() -> Result<(), String> {
        let rom_path = std::env::args()
            .nth(1)
            .unwrap_or_else(|| "/opt/harmony/smb.nes".to_owned());
        let rom = std::fs::read(&rom_path).map_err(|error| format!("read {rom_path}: {error}"))?;
        let mut agent = Agent::from_rom_bytes(&rom).map_err(|error| format!("ROM: {error:?}"))?;

        let (wram_gpa, mirror) = pinned_wram()?;
        agent
            .mirror_wram::<String>(mirror)
            .map_err(|error| format!("prime WRAM: {error:?}"))?;

        let transport = open_transport()?;
        let mut sdk =
            Sdk::init(transport, CATALOG).map_err(|error| format!("sdk init: {error:?}"))?;
        sdk.state_set(REG_WRAM_GPA, wram_gpa)
            .map_err(|error| format!("publish WRAM GPA: {error:?}"))?;
        sdk.state_set(REG_WRAM_LEN, WRAM_SIZE as u64)
            .map_err(|error| format!("publish WRAM length: {error:?}"))?;
        println!("TETANES_WRAM_READY gpa={wram_gpa:#x} len={WRAM_SIZE}");
        sdk.setup_complete()
            .map_err(|error| format!("setup_complete: {error:?}"))?;

        let mut channel = LiveChannel { sdk };
        loop {
            agent
                .run_chord(&mut channel, mirror)
                .map_err(|error| format!("run chord: {error:?}"))?;
        }
    }

    fn pinned_wram() -> Result<(u64, &'static mut [u8]), String> {
        // SAFETY: one private anonymous page; MAP_FAILED is checked before use.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                GUEST_PAGE_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(format!(
                "mmap WRAM mirror: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: `ptr` is a writable mapping of exactly one page.
        unsafe { std::ptr::write_bytes(ptr.cast::<u8>(), 0, GUEST_PAGE_SIZE) };
        // SAFETY: the same live page mapping is pinned; the return is checked.
        if unsafe { libc::mlock(ptr, GUEST_PAGE_SIZE) } != 0 {
            return Err(format!(
                "mlock WRAM mirror: {}",
                std::io::Error::last_os_error()
            ));
        }
        let vaddr = ptr as u64;
        let mut pagemap = File::open("/proc/self/pagemap")
            .map_err(|error| format!("/proc/self/pagemap: {error}"))?;
        pagemap
            .seek(SeekFrom::Start(pagemap_offset(vaddr)))
            .map_err(|error| format!("pagemap seek: {error}"))?;
        let mut entry = [0_u8; 8];
        pagemap
            .read_exact(&mut entry)
            .map_err(|error| format!("pagemap read: {error}"))?;
        let gpa = decode_pagemap_entry(u64::from_le_bytes(entry), vaddr)?;
        // SAFETY: WRAM_SIZE is less than the live page mapping, which is leaked
        // for the process lifetime and exclusively owned by this agent.
        let mirror = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<u8>(), WRAM_SIZE) };
        Ok((gpa, mirror))
    }

    fn open_transport() -> Result<Transport, String> {
        let req = map_phys(REQ_GPA, PAGE_SIZE)?;
        let resp = map_phys(RESP_GPA, PAGE_SIZE)?;
        let doorbell = map_phys(DOORBELL_GPA, PAGE_SIZE)?.cast::<u32>();
        // SAFETY: `doorbell` is a page mapping of the board's aligned, 32-bit,
        // store-only MMIO register and is leaked for the transport lifetime.
        let doorbell = unsafe { MmioDoorbell::new(doorbell) };
        // SAFETY: req/resp are distinct page-sized mappings of the ABI control
        // pages, exclusively owned here and leaked for the process lifetime.
        Ok(unsafe { VmcallTransport::with_doorbell(req as u64, resp as u64, doorbell) })
    }

    fn map_phys(gpa: u64, len: usize) -> Result<*mut u8, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/mem")
            .map_err(|error| format!("/dev/mem: {error}"))?;
        // SAFETY: standard shared mapping at a page-aligned physical address;
        // the result is checked and intentionally leaked for the agent lifetime.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                gpa as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(format!(
                "mmap /dev/mem @ {gpa:#x}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(ptr.cast::<u8>())
    }
}
