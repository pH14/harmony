// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end gate for the hypercall-doorbell transport with **no hypervisor**.
//!
//! A `LoopbackHost: IoDoorbell` plays the host: it reaches the two **fixed** shared pages through
//! the GPAs it was constructed with (exactly as the production host knows them from the ABI, not
//! from the doorbell — an `OUT` carries no pointer), runs a real `hypercall_proto::Dispatcher` over
//! the request bytes the guest staged, and writes the response **frame** into the response page.
//! There is no return value: the response length is folded into the frame header, which is the
//! single-`OUT`, atomic doorbell the crate ships. The round-trip tests drive the unmodified task-01
//! `Client` through all five service calls; the hostile-length and decode-boundary tests prove the
//! load-bearing bound check holds for *any* host-written response page.

use core::ptr;
use std::cell::RefCell;
use std::rc::Rc;

use hypercall_proto::{
    Client, ClientError, Dispatcher, FrameHeader, HEADER_LEN, MAX_PAYLOAD, MemBlockDevice,
    ProtoError, SeededEntropy, Service, ServiceId, Status, Transport, decode, encode_request,
    encode_response,
};
use proptest::prelude::*;
use vmcall_transport::{DOORBELL_PORT, IoDoorbell, PAGE_SIZE, TransportError, VmcallTransport};

/// The wire frame magic (`"HCP1"` little-endian) — `hypercall_proto`'s magic is private, so the
/// test mirrors it (same value the crate's `FRAME_MAGIC` mirrors). Used to forge response headers.
const FRAME_MAGIC: u32 = 0x3150_4348;

/// Per-test proptest config. Native runs keep the spec's full case counts (round-trip ≥256, the
/// adversarial bound-check probes 512). **Under Miri** two things change:
///
/// * **Cases are cut to 16.** The interpreter is ~10–100× slower, so the full counts would push
///   the suite into the tens of minutes while re-treading the same handful of `exchange` branches;
///   16 independent seeds still drive the bound-check and no-panic paths Miri is here to scrutinize
///   for UB. The reduction is Miri-only (`cfg!(miri)`); the native gate honors the ≥256 convention.
/// * **Failure persistence is disabled.** proptest's default persistence resolves a regression-file
///   path via `current_dir()` (getcwd), which Miri runs under filesystem isolation and rejects —
///   the suite would abort before testing anything. There is no regression-replay workflow under
///   the Miri gate, so dropping it is free; native runs keep the default file persistence.
fn config(native_cases: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { native_cases });
    if cfg!(miri) {
        cfg.failure_persistence = None;
    }
    cfg
}

/// A page-aligned, `PAGE_SIZE`-byte backing store reached **only** through a raw pointer whose
/// provenance is exposed — used directly as its GPA (the identity-mapped invariant the transport
/// requires).
///
/// Deliberately *not* a `Box<[u8; PAGE_SIZE]>`: a `Box`/`&mut`-owned page gives the allocation a
/// unique owner tag that Miri's aliasing model (Stacked **and** Tree Borrows) requires *every*
/// access — and the final deallocation — to go through. But here two "agents" reach the page
/// through independently int→ptr-recovered pointers: the guest transport (`self.req_page`/
/// `resp_page = gpa as *mut u8`) and the loopback host (`resp_gpa as *mut u8`), exactly as
/// hardware shares one physical page between guest and host. Owning the only path as a raw,
/// provenance-exposed allocation models that faithfully and lets both writers and the final
/// `dealloc` coexist — while Miri still catches genuine UB (the injected-OOB non-vacuity check in
/// IMPLEMENTATION.md confirms an out-of-bounds read past the page is flagged). This is the
/// production shape too: a real guest page is raw identity-mapped RAM, not a Rust `Box`.
struct Page {
    ptr: *mut u8,
}

impl Page {
    fn layout() -> std::alloc::Layout {
        // PAGE_SIZE is non-zero and 4096 is a power of two, so this is statically valid.
        std::alloc::Layout::from_size_align(PAGE_SIZE, 4096).expect("valid page layout")
    }

    fn zeroed() -> Self {
        // SAFETY: `layout()` has non-zero size (PAGE_SIZE) and power-of-two align.
        let ptr = unsafe { std::alloc::alloc_zeroed(Self::layout()) };
        assert!(!ptr.is_null(), "page allocation failed");
        Self { ptr }
    }

    /// The page's address, used directly as its GPA. Casting the raw pointer to an integer
    /// *exposes its provenance*, so the transport's and host's `gpa as *mut u8` round-trips
    /// resolve to this allocation under Miri.
    fn gpa(&self) -> u64 {
        self.ptr as u64
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `alloc_zeroed(layout())` (non-null, checked) and is freed
        // exactly once here through its original provenance.
        unsafe { std::alloc::dealloc(self.ptr, Self::layout()) }
    }
}

// ---------------------------------------------------------------------------
// Loopback host: a faithful stand-in for the vmm-core port-I/O exit handler.
// ---------------------------------------------------------------------------

/// `IoDoorbell` that emulates the §1 host: validates the doorbell port, then runs a real
/// `Dispatcher` over the request bytes the guest staged at the fixed request page and writes the
/// response frame into the fixed response page. It holds the fixed page GPAs out-of-band — exactly
/// as the production host knows them from the ABI rather than from the (pointer-free) doorbell.
struct LoopbackHost {
    dispatcher: Dispatcher,
    req_gpa: u64,
    resp_gpa: u64,
}

impl LoopbackHost {
    fn new(dispatcher: Dispatcher, req_gpa: u64, resp_gpa: u64) -> Self {
        Self {
            dispatcher,
            req_gpa,
            resp_gpa,
        }
    }
}

impl IoDoorbell for LoopbackHost {
    unsafe fn ring(&mut self, port: u16, req_len: u32) {
        // A wrong doorbell port, or a `req_len` past one page, is a malformed doorbell the host
        // does not recognize: write nothing, leaving the (zeroed) response page as the rejection
        // sentinel `exchange` reads as `HostRejected`. The `req_len > PAGE_SIZE` rejection matches
        // `Exit::Io { write: Some(len) }` — the host can expose at most one page.
        if port != DOORBELL_PORT || req_len as usize > PAGE_SIZE {
            return;
        }
        let n = req_len as usize;

        let mut req_local = [0_u8; PAGE_SIZE];
        // SAFETY: `req_gpa` is the fixed request-page address: a distinct, page-aligned,
        // `PAGE_SIZE`, test-owned page. We copy out `n <= PAGE_SIZE` bytes by raw pointer (no `&`
        // to the page held across anything) into a local before dispatching.
        unsafe {
            ptr::copy_nonoverlapping(self.req_gpa as *const u8, req_local.as_mut_ptr(), n);
        }

        let mut resp_local = [0_u8; PAGE_SIZE];
        // Dispatch only the **exposed** bytes (`&req_local[..n]`), not the zero-padded page: a
        // request shorter than its header-encoded frame is then seen as truncated and answered with
        // an error frame, faithful to the doorbell exposing only `len` bytes.
        let resp_n = self.dispatcher.dispatch(&req_local[..n], &mut resp_local);
        // `dispatch` never returns more than `PAGE_SIZE` (one frame), but clamp defensively so a
        // future contract change can never make the copy below run past the page.
        let resp_n = resp_n.min(PAGE_SIZE);

        // SAFETY: `resp_gpa` is the fixed response-page address (`PAGE_SIZE` bytes); `resp_n <=
        // PAGE_SIZE`, so the write stays in-page. Raw pointer write, no borrow held.
        unsafe {
            ptr::copy_nonoverlapping(resp_local.as_ptr(), self.resp_gpa as *mut u8, resp_n);
        }
    }
}

/// `IoDoorbell` that ignores the request and writes a scripted **raw response page** — the hostile
/// host used to probe the magic gate and the length bound. Whatever bytes the test crafts (a forged
/// header that lies about its length, a zeroed page, garbage) land in the fixed response page.
struct ScriptedHost {
    page: Vec<u8>,
    resp_gpa: u64,
}

impl IoDoorbell for ScriptedHost {
    unsafe fn ring(&mut self, _port: u16, _req_len: u32) {
        let n = self.page.len().min(PAGE_SIZE);
        // SAFETY: `resp_gpa` is the transport's response page (`PAGE_SIZE`); `n <= PAGE_SIZE`.
        unsafe {
            ptr::copy_nonoverlapping(self.page.as_ptr(), self.resp_gpa as *mut u8, n);
        }
    }
}

/// Build a `PAGE_SIZE` response page whose 24-byte header is a real (magic-correct) frame, then
/// **forge** its `payload_len` field (wire offset 16) to `payload_len_field` and place `body` after
/// the header. Lets a test craft a header that lies about its length to drive the bound check.
fn forged_resp_page(payload_len_field: u32, body: &[u8]) -> Vec<u8> {
    let mut page = vec![0_u8; PAGE_SIZE];
    // A real response header (correct magic), empty payload — gives us the right magic bytes.
    encode_response(ServiceId::Console, 1, 1, Status::Ok, &[], &mut page).expect("encode header");
    page[16..20].copy_from_slice(&payload_len_field.to_le_bytes());
    let n = body.len().min(PAGE_SIZE - HEADER_LEN);
    page[HEADER_LEN..HEADER_LEN + n].copy_from_slice(&body[..n]);
    page
}

/// Copy 4 bytes from the response page and read them as the little-endian frame magic.
fn resp_magic(resp_gpa: u64) -> u32 {
    let mut m = [0_u8; 4];
    // SAFETY: `resp_gpa` is a `PAGE_SIZE` page; reading 4 bytes stays in-page.
    unsafe {
        ptr::copy_nonoverlapping(resp_gpa as *const u8, m.as_mut_ptr(), 4);
    }
    u32::from_le_bytes(m)
}

/// Copy the response page out and decode its frame header (panics on a malformed frame).
fn decode_resp_header(resp_gpa: u64) -> FrameHeader {
    let mut page = [0_u8; PAGE_SIZE];
    // SAFETY: `resp_gpa` is a `PAGE_SIZE` page.
    unsafe {
        ptr::copy_nonoverlapping(resp_gpa as *const u8, page.as_mut_ptr(), PAGE_SIZE);
    }
    decode(&page).expect("valid response frame").0
}

// ---------------------------------------------------------------------------
// Recording services so the test can observe write-only services after a call.
// ---------------------------------------------------------------------------

/// Console sink that records into a handle the test keeps a clone of.
#[derive(Clone, Default)]
struct SharedConsole(Rc<RefCell<Vec<u8>>>);

impl Service for SharedConsole {
    fn handle(&mut self, opcode: u16, payload: &[u8], _resp: &mut [u8]) -> (Status, usize) {
        if opcode != 1 {
            return (Status::UnknownOpcode, 0);
        }
        self.0.borrow_mut().extend_from_slice(payload);
        (Status::Ok, 0)
    }
    fn save_state(&self) -> Vec<u8> {
        self.0.borrow().clone()
    }
    fn restore_state(&mut self, state: &[u8]) -> Result<(), ProtoError> {
        *self.0.borrow_mut() = state.to_vec();
        Ok(())
    }
}

/// Recorded `(event-id, data)` pairs, shared between the host service and the test.
type EventLog = Rc<RefCell<Vec<(u32, Vec<u8>)>>>;

/// Event sink that records `(id, data)` into a handle the test keeps a clone of.
#[derive(Clone, Default)]
struct SharedEvent(EventLog);

impl Service for SharedEvent {
    fn handle(&mut self, opcode: u16, payload: &[u8], _resp: &mut [u8]) -> (Status, usize) {
        if opcode != 1 {
            return (Status::UnknownOpcode, 0);
        }
        if payload.len() < 4 {
            return (Status::BadRequest, 0);
        }
        let id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        self.0.borrow_mut().push((id, payload[4..].to_vec()));
        (Status::Ok, 0)
    }
    fn save_state(&self) -> Vec<u8> {
        Vec::new()
    }
    fn restore_state(&mut self, _state: &[u8]) -> Result<(), ProtoError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Harness wiring pages + transport + the real task-01 Client together.
// ---------------------------------------------------------------------------

/// A live `Client<VmcallTransport<LoopbackHost>>` plus observation handles. The pages are
/// retained so their addresses (used as GPAs) stay valid for the client's lifetime.
struct Harness {
    client: Client<VmcallTransport<LoopbackHost>>,
    console: Rc<RefCell<Vec<u8>>>,
    events: EventLog,
    _req: Page,
    _resp: Page,
}

impl Harness {
    fn new(seed: u64, block_data: Vec<u8>) -> Self {
        let console = Rc::new(RefCell::new(Vec::new()));
        let events = Rc::new(RefCell::new(Vec::new()));

        let mut dispatcher = Dispatcher::new();
        dispatcher.register(ServiceId::Console, Box::new(SharedConsole(console.clone())));
        dispatcher.register(ServiceId::Entropy, Box::new(SeededEntropy::new(seed)));
        dispatcher.register(
            ServiceId::Block,
            Box::new(MemBlockDevice::new(block_data).expect("block data is sector-aligned")),
        );
        dispatcher.register(ServiceId::Event, Box::new(SharedEvent(events.clone())));

        let req = Page::zeroed();
        let resp = Page::zeroed();
        let (req_gpa, resp_gpa) = (req.gpa(), resp.gpa());
        let host = LoopbackHost::new(dispatcher, req_gpa, resp_gpa);

        // SAFETY: `req`/`resp` are distinct, page-aligned (`align(4096)`), `PAGE_SIZE`,
        // zero-initialized allocations; their addresses are stable while the pages live inside the
        // returned `Harness`, GPA == linear address, and nothing else aliases them (the pages are
        // reached only through the transport/host raw pointers — no `&`/`&mut` to the bytes exists).
        let transport = unsafe { VmcallTransport::with_doorbell(req_gpa, resp_gpa, host) };

        Self {
            client: Client::new(transport),
            console,
            events,
            _req: req,
            _resp: resp,
        }
    }
}

/// Independent reference for the deterministic entropy stream, mirroring the client's
/// `MAX_PAYLOAD` chunking so the stream advances identically.
fn entropy_reference(seed: u64, len: usize) -> Vec<u8> {
    let mut svc = SeededEntropy::new(seed);
    let mut out = vec![0_u8; len];
    let mut offset = 0;
    while offset < len {
        let n = (len - offset).min(MAX_PAYLOAD);
        let payload = (n as u32).to_le_bytes();
        let mut chunk = vec![0_u8; n];
        let (status, produced) = svc.handle(1, &payload, &mut chunk);
        assert_eq!(status, Status::Ok);
        assert_eq!(produced, n);
        out[offset..offset + n].copy_from_slice(&chunk);
        offset += n;
    }
    out
}

// ---------------------------------------------------------------------------
// The required end-to-end gate: all five client calls round-trip with no KVM.
// ---------------------------------------------------------------------------

#[test]
fn five_client_calls_round_trip_through_loopback() {
    let block_data: Vec<u8> = (0..16 * 512).map(|i| (i % 251) as u8).collect();
    let seed = 0xC0FF_EE12_3456_789A;
    let mut h = Harness::new(seed, block_data.clone());

    // console_write (guest -> host)
    let msg = b"hello, harmony \x00\x01\x02 end";
    h.client.console_write(msg).expect("console_write");
    assert_eq!(h.console.borrow().as_slice(), msg);

    // entropy_fill (host -> guest), length spanning >1 frame to exercise chunking
    let mut entropy = vec![0_u8; MAX_PAYLOAD + 37];
    h.client.entropy_fill(&mut entropy).expect("entropy_fill");
    assert_eq!(entropy, entropy_reference(seed, entropy.len()));

    // block_capacity (host -> guest)
    let capacity = h.client.block_capacity().expect("block_capacity");
    assert_eq!(capacity, 16);

    // block_read spanning >BLOCK_READ_MAX_SECTORS to exercise multi-call chunking
    let mut sectors = vec![0_u8; 11 * 512];
    h.client.block_read(2, &mut sectors).expect("block_read");
    assert_eq!(sectors.as_slice(), &block_data[2 * 512..(2 + 11) * 512]);

    // event_emit (guest -> host)
    h.client
        .event_emit(0xABCD, b"payload-bytes")
        .expect("event_emit");
    assert_eq!(
        h.events.borrow().as_slice(),
        &[(0xABCD_u32, b"payload-bytes".to_vec())]
    );
}

/// The loopback writes a frame only for the right doorbell port; a wrong port leaves the response
/// page unwritten (magic stays 0 → `exchange` maps it to `HostRejected`). Proves the gate would
/// catch a transport that rang the wrong port rather than rubber-stamp it. (The doorbell carries no
/// GPAs, so there is no GPA-mispassing failure mode to guard — the pages are fixed ABI constants.)
#[test]
fn loopback_rejects_bad_port() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Console, Box::new(SharedConsole::default()));
    let req = Page::zeroed();
    let resp = Page::zeroed();
    let (req_gpa, resp_gpa) = (req.gpa(), resp.gpa());

    // Stage a complete console_write request frame.
    let mut frame = [0_u8; PAGE_SIZE];
    let n = encode_request(ServiceId::Console, 1, 1, b"hi", &mut frame).expect("encode request");
    // SAFETY: `req_gpa` is a `PAGE_SIZE` page; `n <= PAGE_SIZE`.
    unsafe {
        ptr::copy_nonoverlapping(frame.as_ptr(), req_gpa as *mut u8, n);
    }
    let mut host = LoopbackHost::new(dispatcher, req_gpa, resp_gpa);

    // SAFETY: the host only reads/writes within the fixed pages it holds.
    unsafe {
        // Wrong port: nothing written, the zeroed response page keeps magic 0.
        host.ring(DOORBELL_PORT ^ 0x1, n as u32);
        assert_eq!(resp_magic(resp_gpa), 0, "wrong port writes nothing");

        // Correct port: a real response frame is written (magic present).
        host.ring(DOORBELL_PORT, n as u32);
        assert_eq!(
            resp_magic(resp_gpa),
            FRAME_MAGIC,
            "correct port writes a frame"
        );
    }
    drop((req, resp));
}

/// Fidelity: the loopback dispatches only the **exposed** request bytes, so a request whose rung
/// length is shorter than its encoded frame is seen as truncated (answered `BadRequest`), not
/// zero-padded into a valid-looking call — keeping the gate as strict as the ABI.
#[test]
fn loopback_dispatches_only_exposed_request_bytes() {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register(ServiceId::Console, Box::new(SharedConsole::default()));
    let req = Page::zeroed();
    let resp = Page::zeroed();
    let (req_gpa, resp_gpa) = (req.gpa(), resp.gpa());

    let mut frame = [0_u8; PAGE_SIZE];
    let n = encode_request(ServiceId::Console, 1, 7, b"hello", &mut frame).expect("encode request");
    assert!(
        n > HEADER_LEN,
        "frame has a payload so n-1 is still a full header"
    );
    // SAFETY: `req_gpa` is a `PAGE_SIZE` page; `n <= PAGE_SIZE`.
    unsafe {
        ptr::copy_nonoverlapping(frame.as_ptr(), req_gpa as *mut u8, n);
    }
    let mut host = LoopbackHost::new(dispatcher, req_gpa, resp_gpa);

    // SAFETY: the host only reads/writes within the fixed pages it holds.
    unsafe {
        // Full length: the frame decodes and the service runs (Status::Ok).
        host.ring(DOORBELL_PORT, n as u32);
        assert_eq!(decode_resp_header(resp_gpa).status, Status::Ok as u16);

        // Truncated length (n-1): the host sees one byte too few, so the frame decodes as
        // truncated -> BadRequest, NOT a zero-padded valid call.
        ptr::write_bytes(resp_gpa as *mut u8, 0, PAGE_SIZE);
        host.ring(DOORBELL_PORT, (n - 1) as u32);
        assert_eq!(
            decode_resp_header(resp_gpa).status,
            Status::BadRequest as u16,
            "a truncated request is seen as truncated, not zero-padded"
        );
    }
    drop((req, resp));
}

// ---------------------------------------------------------------------------
// Hostile-response rejection: the load-bearing magic gate + length bound, fixed cases.
// ---------------------------------------------------------------------------

/// Drive one `exchange` against a `ScriptedHost` that writes `page` into the response page.
/// `resp_buf_len` is the caller buffer size.
fn run_scripted(page: Vec<u8>, resp_buf_len: usize) -> Result<(usize, Vec<u8>), TransportError> {
    let req = Page::zeroed();
    let resp = Page::zeroed();
    let (req_gpa, resp_gpa) = (req.gpa(), resp.gpa());
    let host = ScriptedHost { page, resp_gpa };
    // SAFETY: pages are distinct, page-aligned, PAGE_SIZE, zeroed, owned, retained below.
    let mut transport = unsafe { VmcallTransport::with_doorbell(req_gpa, resp_gpa, host) };
    let mut out = vec![0_u8; resp_buf_len];
    let r = transport.exchange(&[], &mut out).map(|len| (len, out));
    drop((req, resp)); // keep pages alive across the call
    r
}

#[test]
fn hostile_response_is_rejected_without_panic_or_overcopy() {
    // No frame magic (zeroed page = what a rejecting host leaves behind): HostRejected.
    assert_eq!(
        run_scripted(vec![0_u8; PAGE_SIZE], 64).unwrap_err(),
        TransportError::HostRejected
    );

    // Valid magic but a header lying about its length — all bounded out, nothing copied.
    let max_payload = (PAGE_SIZE - HEADER_LEN) as u32;
    for &plen in &[
        max_payload + 1, // total = PAGE_SIZE + 1 > PAGE_SIZE
        u32::MAX,        // total = u32::MAX + HEADER_LEN, overflows u32 but fits u64
        0xFFFF_FFFF,
    ] {
        assert_eq!(
            run_scripted(forged_resp_page(plen, &[0xAB; 8]), PAGE_SIZE).unwrap_err(),
            TransportError::BadResponseLength,
            "payload_len={plen:#x} must be rejected",
        );
    }

    // Within the page but the frame is larger than the caller's buffer: rejected, not over-written.
    // total = HEADER_LEN + 50 = 74 > 64.
    assert_eq!(
        run_scripted(forged_resp_page(50, &[0xCD; 50]), 64).unwrap_err(),
        TransportError::BadResponseLength,
    );

    // Exactly at the caller-buffer boundary: accepted, exact frame bytes copied.
    // total = HEADER_LEN + 40 = 64 == buffer 64.
    let body: Vec<u8> = (0..40).map(|i| i as u8).collect();
    let page = forged_resp_page(40, &body);
    let (len, out) = run_scripted(page.clone(), 64).unwrap();
    assert_eq!(len, 64);
    assert_eq!(out, page[..64]);

    // Upper boundary: a full-page frame (total == PAGE_SIZE) with a full-page buffer.
    let body = vec![0x5A_u8; PAGE_SIZE - HEADER_LEN];
    let page = forged_resp_page(max_payload, &body);
    let (len, out) = run_scripted(page.clone(), PAGE_SIZE).unwrap();
    assert_eq!(len, PAGE_SIZE);
    assert_eq!(out, page);
}

#[test]
fn request_larger_than_page_is_rejected() {
    let req = Page::zeroed();
    let resp = Page::zeroed();
    let (req_gpa, resp_gpa) = (req.gpa(), resp.gpa());
    let host = ScriptedHost {
        page: forged_resp_page(0, &[]),
        resp_gpa,
    };
    // SAFETY: pages distinct/aligned/sized/owned, retained to end of function.
    let mut transport = unsafe { VmcallTransport::with_doorbell(req_gpa, resp_gpa, host) };
    let oversized = vec![0_u8; PAGE_SIZE + 1];
    let mut out = [0_u8; 64];
    assert_eq!(
        transport.exchange(&oversized, &mut out).unwrap_err(),
        TransportError::RequestTooLarge,
    );
}

// ---------------------------------------------------------------------------
// Property tests — the spec's required ≥256-case round-trip, plus adversarial
// coverage of the magic-gate / length-bound boundary (green gate is the floor).
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(config(256))]

    /// Round-trip arbitrary payloads through `Client<VmcallTransport<LoopbackHost>>` and assert the
    /// bytes returned equal what the stub services produced.
    #[test]
    fn round_trip_arbitrary_payloads(
        seed in any::<u64>(),
        console in proptest::collection::vec(any::<u8>(), 0..9000),
        entropy_len in 0_usize..9000,
        total_sectors in 1_usize..40,
        event_id in any::<u32>(),
        event_data in proptest::collection::vec(any::<u8>(), 0..(MAX_PAYLOAD - 4)),
    ) {
        let block_data: Vec<u8> =
            (0..total_sectors * 512).map(|i| (i.wrapping_mul(31) % 256) as u8).collect();
        let mut h = Harness::new(seed, block_data.clone());

        // console (guest -> host)
        h.client.console_write(&console).unwrap();
        let recorded_console = h.console.borrow().clone();
        prop_assert_eq!(recorded_console.as_slice(), console.as_slice());

        // entropy (host -> guest) vs independent reference stream
        let mut got = vec![0_u8; entropy_len];
        h.client.entropy_fill(&mut got).unwrap();
        prop_assert_eq!(&got, &entropy_reference(seed, entropy_len));

        // capacity (host -> guest)
        prop_assert_eq!(h.client.block_capacity().unwrap(), total_sectors as u64);

        // block_read of a random in-range span (host -> guest)
        let lba = (seed as usize) % total_sectors;
        let max_sectors = total_sectors - lba;
        let read_sectors = 1 + (event_id as usize % max_sectors);
        let mut sectors = vec![0_u8; read_sectors * 512];
        h.client.block_read(lba as u64, &mut sectors).unwrap();
        prop_assert_eq!(
            sectors.as_slice(),
            &block_data[lba * 512..(lba + read_sectors) * 512]
        );

        // event (guest -> host)
        h.client.event_emit(event_id, &event_data).unwrap();
        let recorded_events = h.events.borrow().clone();
        prop_assert_eq!(recorded_events.as_slice(), &[(event_id, event_data)]);
    }
}

proptest! {
    #![proptest_config(config(512))]

    /// For ANY host-written response page, `exchange` classifies exactly per spec, never panics,
    /// and never over-copies. `payload_len_field` ranges over the full `u32` domain (so
    /// `HEADER_LEN + payload_len` can exceed `u32::MAX`, exercising the `u64`
    /// bound-check-before-cast beyond any 32-bit value), and `valid_magic` toggles the rejection
    /// gate.
    #[test]
    fn exchange_classifies_any_response_page(
        valid_magic in any::<bool>(),
        payload_len_field in any::<u32>(),
        body in proptest::collection::vec(any::<u8>(), 0..=(PAGE_SIZE - HEADER_LEN)),
        resp_buf_len in 0_usize..=PAGE_SIZE,
    ) {
        let mut page = forged_resp_page(payload_len_field, &body);
        if !valid_magic {
            page[0] ^= 0xFF; // flip the low magic byte -> guaranteed != FRAME_MAGIC
        }
        let result = run_scripted(page.clone(), resp_buf_len);

        if !valid_magic {
            // No frame magic -> always rejected, regardless of the (unread) length.
            prop_assert_eq!(result.unwrap_err(), TransportError::HostRejected);
        } else {
            let total = HEADER_LEN as u64 + payload_len_field as u64;
            match result {
                Err(TransportError::BadResponseLength) => {
                    prop_assert!(total > PAGE_SIZE as u64 || total > resp_buf_len as u64);
                }
                Ok((len, out)) => {
                    prop_assert!(total <= PAGE_SIZE as u64 && total <= resp_buf_len as u64);
                    prop_assert_eq!(len, total as usize);
                    prop_assert_eq!(&out[..len], &page[..len]);
                }
                other => prop_assert!(false, "unexpected {:?} for valid magic", other),
            }
        }
    }
}

proptest! {
    #![proptest_config(config(512))]

    /// Decode-boundary fuzz: a hostile host writes an arbitrary response page (optionally with a
    /// valid magic + forged length); every task-01 `Client` call must yield `Ok` or a clean
    /// `ClientError` — never a panic/UB.
    #[test]
    fn client_survives_garbage_host(
        force_magic in any::<bool>(),
        payload_len_field in any::<u32>(),
        bytes in proptest::collection::vec(any::<u8>(), 0..=PAGE_SIZE),
    ) {
        let mut page = vec![0_u8; PAGE_SIZE];
        let n = bytes.len().min(PAGE_SIZE);
        page[..n].copy_from_slice(&bytes[..n]);
        if force_magic {
            page[0..4].copy_from_slice(&FRAME_MAGIC.to_le_bytes());
            page[16..20].copy_from_slice(&payload_len_field.to_le_bytes());
        }
        let req = Page::zeroed();
        let resp = Page::zeroed();
        let (req_gpa, resp_gpa) = (req.gpa(), resp.gpa());
        let host = ScriptedHost { page, resp_gpa };
        // SAFETY: pages distinct/aligned/sized/zeroed/owned; retained until end of case.
        let transport = unsafe { VmcallTransport::with_doorbell(req_gpa, resp_gpa, host) };
        let mut client = Client::new(transport);

        // Each call decodes host-controlled bytes; assert it returns (Ok or Err) without panic.
        let mut scratch = [0_u8; 512];
        let _: Result<(), ClientError<TransportError>> = client.console_write(b"x");
        let _: Result<(), ClientError<TransportError>> = client.entropy_fill(&mut scratch[..8]);
        let _: Result<u64, ClientError<TransportError>> = client.block_capacity();
        let _: Result<(), ClientError<TransportError>> = client.block_read(0, &mut scratch);
        let _: Result<(), ClientError<TransportError>> = client.event_emit(1, b"y");

        drop((req, resp));
        prop_assert!(true); // reaching here without panic is the property
    }
}
