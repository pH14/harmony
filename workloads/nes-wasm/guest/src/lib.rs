// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

#[cfg(any(target_arch = "wasm32", test))]
mod guest {
    use alloc::{
        format,
        string::{String, ToString},
        vec,
        vec::Vec,
    };
    #[cfg(target_arch = "wasm32")]
    use consonance_wasm_guest::ImportedRequest;
    use consonance_wasm_guest::WasmTransport;
    use harmony_sdk::Sdk;
    use hypercall_proto::{
        Client, MAX_PAYLOAD,
        observation::{Descriptor, EVENT_ID},
    };
    use nes_agent::{CATALOG, Channel as NovaChannel, Core, NesAgent, REG_HANDLE, REG_LEN};
    #[cfg(test)]
    use shim::{
        ImportedRequest, quicknes_frame, quicknes_initialize, quicknes_save_ram, quicknes_work_ram,
    };

    #[cfg(not(test))]
    unsafe extern "C" {
        fn quicknes_initialize(rom: *const u8, length: u32) -> u32;
        fn quicknes_frame(joypad: u32);
        fn quicknes_work_ram(output: *mut u8, length: u32) -> u32;
        fn quicknes_save_ram(output: *mut u8, length: u32) -> u32;
    }

    struct QuickNes {
        _rom: Vec<u8>,
    }

    impl QuickNes {
        fn new(rom: Vec<u8>) -> Result<Self, String> {
            if !(1..=1024 * 1024).contains(&rom.len()) {
                return Err("ROM length outside profile".into());
            }
            // SAFETY: the bounded ROM slice remains live for this call and every
            // subsequent emulator call because the returned core owns the Vec.
            let status = unsafe { quicknes_initialize(rom.as_ptr(), rom.len() as u32) };
            if status != 0 {
                return Err(format!("QuickNES initialization failed: {status}"));
            }
            Ok(Self { _rom: rom })
        }
    }

    impl Core for QuickNes {
        fn serialize_size(&mut self) -> usize {
            0
        }
        fn serialize(&mut self, _: &mut [u8]) -> bool {
            false
        }
        fn run_frame(&mut self, joypad: u8) {
            // SAFETY: construction initialized the single-threaded core and all
            // callbacks; no other emulator operation races with this call.
            unsafe { quicknes_frame(u32::from(joypad)) }
        }
        fn read_work_ram(&mut self, out: &mut [u8]) -> bool {
            if out.len() != 2048 {
                return false;
            }
            // SAFETY: the C shim copies exactly the exclusive slice's 2048 bytes
            // from initialized core RAM and does not retain the destination.
            unsafe { quicknes_work_ram(out.as_mut_ptr(), out.len() as u32) != 0 }
        }
        fn read_save_ram(&mut self, out: &mut [u8]) -> Option<usize> {
            if out.len() != 8192 {
                return None;
            }
            // SAFETY: the C shim copies exactly the exclusive slice's 8192 bytes
            // from initialized core RAM and does not retain the destination.
            let len = unsafe { quicknes_save_ram(out.as_mut_ptr(), out.len() as u32) };
            (len != 0).then_some(len as usize)
        }
    }

    struct Channel(Sdk<WasmTransport<ImportedRequest>>);
    impl NovaChannel for Channel {
        type Error = String;
        fn payload_fetch(&mut self, out: &mut [u8; 2]) -> Result<(), String> {
            self.0
                .client_mut()
                .payload_fetch(out)
                .map_err(|e| e.to_string())
        }
        fn state_set(&mut self, reg: u32, value: u64) -> Result<(), String> {
            self.0.state_set(reg, value).map_err(|e| e.to_string())
        }
        fn state_max(&mut self, reg: u32, value: u64) -> Result<(), String> {
            self.0.state_max(reg, value).map_err(|e| e.to_string())
        }
        fn reachable(&mut self, point: u32) -> Result<(), String> {
            self.0.assert_reachable(point).map_err(|e| e.to_string())
        }
        fn frame_complete(&mut self, frame: u64) -> Result<(), String> {
            self.0.frame_complete(frame).map_err(|e| e.to_string())
        }
    }

    fn execute() -> Result<(), String> {
        let mut client = Client::new(WasmTransport(ImportedRequest));
        let mut length = [0; 4];
        client
            .payload_fetch(&mut length)
            .map_err(|e| e.to_string())?;
        let length = u32::from_le_bytes(length) as usize;
        if !(1..=1024 * 1024).contains(&length) {
            return Err("ROM length outside profile".into());
        }
        let mut rom = vec![0; length];
        for chunk in rom.chunks_mut(MAX_PAYLOAD) {
            client.payload_fetch(chunk).map_err(|e| e.to_string())?;
        }
        let mut agent = NesAgent::new(QuickNes::new(rom)?)?;
        let mut publication = vec![0; agent.layout().total_len()];
        agent.prime(&mut publication)?;
        let sdk = Sdk::init(WasmTransport(ImportedRequest), CATALOG).map_err(|e| e.to_string())?;
        let mut channel = Channel(sdk);
        let descriptor = Descriptor {
            handle: 1,
            address: publication.as_ptr() as usize as u64,
            len: publication.len() as u32,
        }
        .encode()
        .map_err(|e| e.to_string())?;
        channel
            .0
            .client_mut()
            .event_emit(EVENT_ID, &descriptor)
            .map_err(|e| e.to_string())?;
        channel.state_set(REG_HANDLE, 1)?;
        channel.state_set(REG_LEN, publication.len() as u64)?;
        channel.0.setup_complete().map_err(|e| e.to_string())?;
        loop {
            agent.run_chord(&mut channel, &mut publication)?;
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn play() -> u32 {
        match execute() {
            Ok(()) => 0,
            Err(error) => {
                let _ = Client::new(WasmTransport(ImportedRequest)).console_write(error.as_bytes());
                1
            }
        }
    }
    #[cfg(test)]
    mod shim {
        use core::sync::atomic::{AtomicU32, Ordering};
        static FRAMES: AtomicU32 = AtomicU32::new(0);
        pub struct ImportedRequest;
        impl consonance_wasm_guest::Request for ImportedRequest {
            fn request(&mut self, _: u32, _: &[u8], _: &mut [u8]) -> i32 {
                -1
            }
        }
        pub unsafe fn quicknes_initialize(rom: *const u8, length: u32) -> u32 {
            // SAFETY: the core wrapper supplies its owned, nonempty ROM extent.
            let rom = unsafe { core::slice::from_raw_parts(rom, length as usize) };
            assert!(rom.iter().all(|byte| *byte == 0x5a));
            FRAMES.store(0, Ordering::Relaxed);
            0
        }
        pub unsafe fn quicknes_frame(joypad: u32) {
            FRAMES.fetch_add(joypad, Ordering::Relaxed);
        }
        pub unsafe fn quicknes_work_ram(output: *mut u8, length: u32) -> u32 {
            assert_eq!(length, 2048);
            // SAFETY: the core wrapper supplies an exclusive 2048-byte slice.
            unsafe { core::slice::from_raw_parts_mut(output, length as usize) }
                .fill(FRAMES.load(Ordering::Relaxed) as u8);
            1
        }
        pub unsafe fn quicknes_save_ram(output: *mut u8, length: u32) -> u32 {
            assert_eq!(length, 8192);
            // SAFETY: the core wrapper supplies an exclusive 8192-byte slice.
            unsafe { core::slice::from_raw_parts_mut(output, length as usize) }.fill(0xff);
            length
        }
    }
    #[cfg(test)]
    #[test]
    fn core_buffers_and_owned_rom_follow_the_bounded_ffi_contract() {
        assert!(QuickNes::new(vec![]).is_err());
        let mut core = QuickNes::new(vec![0x5a; 16]).unwrap();
        core.run_frame(3);
        let mut work = [0; 2048];
        let mut save = [0; 8192];
        assert!(core.read_work_ram(&mut work));
        assert_eq!(work, [3; 2048]);
        assert_eq!(core.read_save_ram(&mut save), Some(8192));
        assert_eq!(save, [0xff; 8192]);
        assert!(!core.read_work_ram(&mut [0; 2047]));
        assert_eq!(core.read_save_ram(&mut [0; 8191]), None);
        assert_eq!(core._rom, [0x5a; 16]);
    }
}

#[cfg(any(target_arch = "wasm32", test))]
mod allocator {
    use core::alloc::{GlobalAlloc, Layout};
    pub struct Allocator;
    unsafe extern "C" {
        fn malloc(size: usize) -> *mut u8;
        fn free(pointer: *mut u8);
    }
    // SAFETY: libc supplies exclusive allocations aligned to at least 16 bytes;
    // excessive alignment returns null, and deallocation uses the matching free.
    unsafe impl GlobalAlloc for Allocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if layout.align() > 16 {
                return core::ptr::null_mut();
            }
            // SAFETY: GlobalAlloc requests have nonzero sizes; malloc either
            // returns an exclusive extent of that size or null.
            unsafe { malloc(layout.size()) }
        }
        unsafe fn dealloc(&self, pointer: *mut u8, _: Layout) {
            // SAFETY: GlobalAlloc's caller supplies a live pointer from malloc.
            unsafe { free(pointer) }
        }
    }
    #[cfg(test)]
    #[test]
    fn bounded_allocator_preserves_bytes_and_alignment() {
        for alignment in [1, 4, 8, 16] {
            let layout = Layout::from_size_align(127, alignment).unwrap();
            // SAFETY: the requested layout is valid, the returned extent is
            // checked before access, and freed exactly once with that layout.
            unsafe {
                let pointer = Allocator.alloc(layout);
                assert!(!pointer.is_null());
                assert_eq!((pointer as usize) % alignment, 0);
                let bytes = core::slice::from_raw_parts_mut(pointer, layout.size());
                bytes.fill(0x5a);
                assert!(bytes.iter().all(|byte| *byte == 0x5a));
                Allocator.dealloc(pointer, layout);
                assert!(
                    Allocator
                        .alloc(Layout::from_size_align(127, 32).unwrap())
                        .is_null()
                );
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[global_allocator]
static ALLOCATOR: allocator::Allocator = allocator::Allocator;

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    core::arch::wasm32::unreachable()
}
