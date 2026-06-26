// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]
//! Guest-side hypercall-doorbell transport for the Harmony hypercall channel.
//!
//! Task 01 (`hypercall-proto`) defined the wire protocol and a `Client<T: Transport>` but left
//! the `Transport` abstract. This crate implements that `Transport` over the **INTEGRATION.md §1
//! hypercall-doorbell ABI**: it marshals a request frame into a shared, page-aligned
//! guest-physical request page, rings a magic **port-I/O doorbell** ([`DOORBELL_PORT`]) with a
//! single `OUT`, and reads the host's response frame back out of the response page — its length
//! taken from the frame header and bounded so a hostile host can never make the shim read past a
//! page, write past the caller's buffer, or panic. A `Client<VmcallTransport>` is then a complete
//! guest hypercall client that composes with the task-01 `Client` unchanged.
//!
//! ## Why a port-I/O doorbell, not `VMCALL` (integrator ruling, 2026-06-23)
//!
//! The original ABI (task 10) used `VMCALL` with the request/response GPAs in `RBX`/`RCX`.
//! **Stock KVM services `VMCALL` in-kernel**: for our magic number (`0x3150_4348`) its
//! `kvm_emulate_hypercall` returns `-ENOSYS` to the guest and resumes — it never surfaces a
//! `KVM_EXIT_HYPERCALL` to userspace (only `KVM_HC_MAP_GPA_RANGE` does). So a `VMCALL` doorbell
//! needs the patched/direct-VMX backend (task 21). A port `OUT` to a magic port, by contrast,
//! **is** surfaced by stock KVM as `KVM_EXIT_IO` — so this hypercall channel works with **zero**
//! kernel patch. (RDTSC/RNG interception still needs the patched backend; that is separate — this
//! crate is only the doorbell.)
//!
//! ## The doorbell protocol (single `OUT` — atomic)
//!
//! An `OUT` cannot carry two 64-bit GPAs, so the doorbell carries **no pointer**: the request and
//! response frames live in two **fixed** guest-physical pages the contract reserves and the VMM
//! maps ([`REQ_GPA`] / [`RESP_GPA`]). One exchange is a **single VM exit**:
//!
//! 1. The guest writes its request frame into the [`REQ_GPA`] page.
//! 2. `OUT DOORBELL_PORT, EAX` with `EAX` = the request length → the host gets
//!    `Exit::Io { port, size, write: Some(len) }`, reads [`REQ_GPA`], services it through the
//!    task-01 `Dispatcher`, writes the response **frame** into [`RESP_GPA`], and resumes the guest
//!    at the next instruction. An `OUT` needs no completion.
//! 3. The guest reads the response **length** straight from the response-frame header in
//!    [`RESP_GPA`] (`HEADER_LEN + payload_len`, the frame being self-describing) and copies that
//!    many bytes out. A response page that does not begin with the frame magic — e.g. the host
//!    wrote nothing — is a rejection (`HostRejected`).
//!
//! **Atomicity / single-in-flight.** The whole exchange is **one** `OUT` exit: the host fully
//! services it and writes the response before resuming, holding **no pending state across a guest
//! resume** — exactly like the old single-`VMCALL` doorbell. This is why the response length is
//! folded into the frame header rather than returned by a second `IN` exit: a two-exit `OUT`/`IN`
//! doorbell resumes the guest *between* the exits while the host still owes a response length, so
//! an interrupt injected in that window whose handler re-enters the doorbell would clobber the
//! fixed pages and the pending length. One exit removes that window.
//!
//! The privileged `OUT` doorbell is abstracted behind the [`IoDoorbell`] seam so the whole
//! marshalling path — plus the real task-01 `Client` and `Dispatcher` — can be exercised
//! in-process under `cargo test` with no hypervisor (see the loopback tests).
//!
//! > **Name note.** The package and the [`VmcallTransport`] type keep their task-10 names to
//! > avoid churn (the spec defers the `io-transport` rename); despite the name, the mechanism is
//! > now the port-I/O doorbell described above, **not** `VMCALL`.

use core::ptr;

use hypercall_proto::HEADER_LEN;

/// The magic 16-bit I/O port the guest rings to signal a hypercall (the **doorbell**). An `OUT` to
/// this port is surfaced by **stock KVM** as `KVM_EXIT_IO` (INTEGRATION.md §1) — unlike `VMCALL`,
/// which stock KVM services in-kernel and never forwards to userspace for our magic number. Chosen
/// to avoid the legacy ISA/PCI port map (PIT `0x40`–`0x43`, PIC `0x20`/`0xA0`, PS/2 `0x60`/`0x64`,
/// PCI-config `0xCF8`/`0xCFC`, …); the VMM reserves it. Because it is `> 0xFF` the real doorbell
/// addresses it through `DX`, not an immediate.
pub const DOORBELL_PORT: u16 = 0x0CA1;

/// Guest-physical address of the fixed **request page** (4 KiB). The doorbell carries no pointer
/// (an `OUT` cannot pass two 64-bit GPAs), so the request frame lives at this fixed GPA the
/// contract reserves and the VMM maps. Page-aligned; distinct from [`RESP_GPA`]. (Exact placement
/// is the loader/vmm-core's to finalize and reserve in the guest e820 / task-04 payload map; what
/// this ABI pins is that the pages are two fixed, page-aligned, VMM-reserved guest-RAM pages.)
pub const REQ_GPA: u64 = 0x0000_E000;

/// Guest-physical address of the fixed **response page** (4 KiB). See [`REQ_GPA`].
pub const RESP_GPA: u64 = 0x0000_F000;

/// Size in bytes of each shared page. Equals `hypercall_proto::MAX_FRAME`, so exactly one frame
/// fits in one page.
pub const PAGE_SIZE: usize = 4096;

/// The wire frame-header magic (`"HCP1"` little-endian), equal to `hypercall_proto`'s private
/// frame magic. The transport reads it from the response page to tell a host-written frame from a
/// rejected (unwritten → zeroed) page; it is **not** the public doorbell magic (that is the port).
const FRAME_MAGIC: u32 = 0x3150_4348;

// One frame must fit in exactly one page; if the wire format's cap ever diverges from the page
// size this stops compiling rather than silently truncating frames.
const _: () = assert!(PAGE_SIZE == hypercall_proto::MAX_FRAME);

// The header (read in full to extract magic + payload_len) must fit within a page.
const _: () = assert!(HEADER_LEN <= PAGE_SIZE);

// The request/response pages are distinct, page-aligned, and one page apart — a static guard on
// the ABI constants so a typo can never alias them or mis-align them.
const _: () = assert!(REQ_GPA.is_multiple_of(PAGE_SIZE as u64));
const _: () = assert!(RESP_GPA.is_multiple_of(PAGE_SIZE as u64));
const _: () = assert!(REQ_GPA != RESP_GPA);

/// The privileged hypercall-doorbell primitive, abstracted so the page-marshalling logic can be
/// driven by a host-side loopback in tests without a hypervisor.
///
/// Implementors perform the doorbell on `port`: a single `OUT` carrying `req_len` (the
/// request-frame length the guest staged in [`REQ_GPA`]). There is no return value — the host
/// writes the response frame into [`RESP_GPA`] out-of-band before resuming, and the guest reads
/// the response length from that frame's header. One exit ⇒ the exchange is atomic with respect to
/// injected interrupts.
pub trait IoDoorbell {
    /// Ring the doorbell: a single `OUT port, req_len`. `req_len` is the number of valid request
    /// bytes already staged in the request page.
    ///
    /// # Safety
    /// The fixed request/response pages the host services out-of-band must name distinct,
    /// page-aligned, `PAGE_SIZE`, guest-owned pages valid for the duration of the call (the host
    /// reads the request page and writes the response page while the doorbell is in flight).
    unsafe fn ring(&mut self, port: u16, req_len: u32);
}

/// Production [`IoDoorbell`]: executes a real `OUT` doorbell. Meaningful only inside a VM.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RealIoDoorbell;

impl RealIoDoorbell {
    /// Construct the production doorbell primitive.
    pub const fn new() -> Self {
        Self
    }
}

impl IoDoorbell for RealIoDoorbell {
    /// Execute a real `OUT` doorbell on x86-64; a deterministic no-op elsewhere.
    ///
    /// Excluded under Miri (`not(miri)`): Miri interprets MIR and cannot execute inline `asm!`,
    /// so the real doorbell is unavailable there. This is sound for the Miri gate because the
    /// gate's purpose is the *pointer/bound-check* logic in [`VmcallTransport::exchange`], which
    /// the loopback [`IoDoorbell`] impls drive with no asm; the privileged instruction itself is
    /// out of Miri's scope by construction (see this crate's IMPLEMENTATION.md, "Miri").
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    unsafe fn ring(&mut self, port: u16, req_len: u32) {
        // SAFETY: a single `out` traps to the host, which reads the request page and writes the
        // response page *out-of-band* by GPA — invisible to the compiler — before resuming the
        // guest at the next instruction. One exit ⇒ the host holds no pending state across a guest
        // resume, so the exchange is atomic w.r.t. injected interrupts. The default (no `nomem`/
        // `readonly`/`pure`) "may read or write any memory" semantics are required and intentional:
        // they keep the request-page stores (in `exchange`) from sinking past this `out`.
        // `preserves_flags` is deliberately omitted — the host owns guest state across an exit
        // except as specified, so we do not assume RFLAGS survives. `nostack` is accurate (`out`
        // touches no guest stack). The port (`> 0xFF`) is carried in DX; the request length in EAX.
        // Caller (`exchange`) upholds the fixed-page invariants in `IoDoorbell::ring`'s contract.
        unsafe {
            core::arch::asm!(
                "out dx, eax",
                in("dx") port,
                in("eax") req_len,
                options(nostack),
            );
        }
    }

    /// Off `x86_64` there is no port-`OUT` instruction we can target. This path is reachable
    /// through the *safe* `Transport::exchange` API, so it must be deterministic and
    /// non-panicking: do nothing. `exchange` then reads the response page it zeroed (step 3),
    /// finds no frame magic, and returns [`TransportError::HostRejected`]. (Port I/O being an
    /// x86-64 facility, this `cfg(target_arch)` split is intrinsic to the hardware, not a
    /// `cfg(target_os)` logic fork; it lets the loopback tests build and run on Apple-Silicon
    /// macOS.)
    ///
    /// This same no-op stub also stands in **under Miri on x86-64** (`cfg(miri)`), where the real
    /// `asm!` above is excluded — Miri cannot interpret inline asm. The safe API stays total under
    /// Miri even on the production target; the Miri gate never depends on this path (it drives the
    /// unsafe logic through the loopback [`IoDoorbell`]).
    #[cfg(any(not(target_arch = "x86_64"), miri))]
    unsafe fn ring(&mut self, _port: u16, _req_len: u32) {}
}

/// Errors surfaced by the transport. Becomes `ClientError::Transport(..)` in the task-01 client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    /// The request frame is larger than the request page (`req.len() > PAGE_SIZE`).
    RequestTooLarge,
    /// The host rejected the call: the response page does not begin with the frame magic (the
    /// host wrote no frame — e.g. a bad doorbell, or an off-VM build).
    HostRejected,
    /// The response frame's header-declared length (`HEADER_LEN + payload_len`) exceeds
    /// `PAGE_SIZE` or the caller's `resp` buffer — a malformed or hostile host response; never
    /// partially copied.
    BadResponseLength,
}

impl core::fmt::Display for TransportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            Self::RequestTooLarge => "request frame larger than page",
            Self::HostRejected => "host rejected the hypercall",
            Self::BadResponseLength => "host-returned response length out of bounds",
        };
        f.write_str(text)
    }
}

/// Guest-side `hypercall_proto::Transport` over the §1 hypercall-doorbell ABI.
///
/// Generic over the doorbell primitive so tests can substitute a loopback host; defaults to
/// [`RealIoDoorbell`] so production code writes `VmcallTransport`. (Type name retained from task
/// 10 to avoid public-API churn; the mechanism is the port-I/O doorbell, not `VMCALL` — see the
/// crate docs.)
///
/// The two shared pages are held as **raw pointers**, never as `&mut [u8]` fields: the host
/// writes the response page out-of-band during the doorbell, so a Rust reference held across the
/// call would be aliasing UB. The pages are touched only with raw-pointer ops whose borrows end
/// before the call and are re-derived after it.
#[derive(Debug)]
pub struct VmcallTransport<D: IoDoorbell = RealIoDoorbell> {
    /// Guest-physical == linear address of the request page (the host reads it on the doorbell).
    req_page: *mut u8,
    /// Guest-physical == linear address of the response page (the host writes it on the doorbell).
    resp_page: *mut u8,
    doorbell: D,
}

impl VmcallTransport<RealIoDoorbell> {
    /// Construct the production transport over the fixed ABI pages [`REQ_GPA`] / [`RESP_GPA`].
    ///
    /// # Safety
    /// The ABI pages must satisfy the [`from_gpas`](VmcallTransport::from_gpas) page contract for
    /// `req_gpa = REQ_GPA`, `resp_gpa = RESP_GPA` (mapped read+write, identity-mapped,
    /// zero-initialized, exclusively owned, valid for the transport's lifetime).
    pub unsafe fn new() -> Self {
        // SAFETY: forwarded to `from_gpas` with the ABI-fixed GPAs; the caller upholds the page
        // contract for those GPAs.
        unsafe { Self::from_gpas(REQ_GPA, RESP_GPA) }
    }

    /// Construct from explicit request/response page GPAs. Production passes the ABI constants
    /// (or uses [`new`](VmcallTransport::new)); the explicit form exists for a loader that places
    /// the pages elsewhere and for tests.
    ///
    /// # Safety
    /// `req_gpa` and `resp_gpa` must each name a distinct, page-aligned, `PAGE_SIZE`, guest-owned
    /// page mapped read+write for the lifetime of the transport. Each GPA must be **non-null and
    /// dereferenceable as a Rust pointer for `PAGE_SIZE` bytes** (GPA `0`, though it can be
    /// page-aligned and hardware-mapped, is not a valid Rust pointer). Because the transport
    /// dereferences these values directly (it accesses the pages by address), **each GPA must also
    /// equal the page's linear/virtual address** — i.e. the pages are identity-mapped, as under
    /// the task-04 payload map; a GPA that is not a valid linear address is UB. The pages must be
    /// **initialized byte storage** (real memory, not `MaybeUninit` — e.g. zeroed at reservation:
    /// the host may write a response shorter than the page, and step 3 zeroes the page so the
    /// untouched tail and a rejected page read as zeros) and **exclusively owned** by this
    /// transport for its lifetime — no other live reference may alias them (the `req` and `resp`
    /// slices passed to `exchange` must not overlap them), since the host writes the response page
    /// out-of-band.
    pub unsafe fn from_gpas(req_gpa: u64, resp_gpa: u64) -> Self {
        // SAFETY: forwarded to `with_doorbell`; `RealIoDoorbell` carries no invariants of its own
        // and is consistent with any GPAs (it only executes the `OUT` doorbell).
        unsafe { Self::with_doorbell(req_gpa, resp_gpa, RealIoDoorbell::new()) }
    }
}

impl<D: IoDoorbell> VmcallTransport<D> {
    /// Construct with an explicit doorbell primitive (the loopback host in tests).
    ///
    /// # Safety
    /// Same page requirements as [`VmcallTransport::from_gpas`]; `doorbell` must be consistent
    /// with those GPAs (it services the same fixed pages out-of-band).
    pub unsafe fn with_doorbell(req_gpa: u64, resp_gpa: u64, doorbell: D) -> Self {
        // GPA == linear address (see safety contract), so the numeric GPA *is* the page pointer.
        Self {
            req_page: req_gpa as *mut u8,
            resp_page: resp_gpa as *mut u8,
            doorbell,
        }
    }
}

impl<D: IoDoorbell> hypercall_proto::Transport for VmcallTransport<D> {
    type Error = TransportError;

    /// Marshal `req` through the shared pages and one `OUT` doorbell, copying the response into
    /// `resp`.
    ///
    /// The response length is read from the host-written response-frame header; it is
    /// attacker-controlled, so the `u64` bound check (before any `as usize` cast) is the
    /// load-bearing safety property — no header value can make this read past the response page,
    /// write past `resp`, or panic.
    fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error> {
        // Step 1: never write past the request page.
        if req.len() > PAGE_SIZE {
            return Err(TransportError::RequestTooLarge);
        }

        // Steps 2 & 3: stage the request and clear both pages. Raw-pointer ops only — no `&`/`&mut`
        // to either page is created here, and none is live across the doorbell below.
        //
        // Step 2 clears the request-page tail (`req.len()..PAGE_SIZE`) so a direct `exchange`
        // caller passing a `req` shorter than its header-encoded length exposes only zeros to the
        // host, never stale bytes from a previous call (the host reads by the length we ring). Step
        // 3 zeroes the response page so its untouched tail reads as zeros and — critically — a host
        // that writes nothing (a rejection) leaves the magic field zero, which step 5 maps to
        // `HostRejected` instead of decoding a stale prior frame.
        //
        // SAFETY: `req_page`/`resp_page` name distinct, page-aligned, `PAGE_SIZE`, exclusively
        // owned pages (constructor contract); `req` does not overlap them (same contract), so the
        // copy is non-overlapping. All offsets stay within `PAGE_SIZE`.
        unsafe {
            ptr::copy_nonoverlapping(req.as_ptr(), self.req_page, req.len());
            ptr::write_bytes(self.req_page.add(req.len()), 0, PAGE_SIZE - req.len());
            ptr::write_bytes(self.resp_page, 0, PAGE_SIZE);
        }

        // Step 4: ring the doorbell (single `OUT`). No reference to either page is live across this
        // call; the host reads the request page and writes the response page out-of-band here, then
        // resumes — atomically, holding no pending state. `req.len() <= PAGE_SIZE` (step 1), so the
        // `as u32` length cast is lossless.
        //
        // SAFETY: the pages are distinct, page-aligned, `PAGE_SIZE`, guest-owned, and valid for the
        // duration of the call (constructor contract); the doorbell services exactly those pages.
        unsafe {
            self.doorbell.ring(DOORBELL_PORT, req.len() as u32);
        }

        // Step 5: read the response-frame header straight out of the response page. `HEADER_LEN <=
        // PAGE_SIZE` (static assert), so this fixed-size read is always in-page; the bytes are
        // host-controlled, so the magic gate and the `u64` length bound below are load-bearing. No
        // host write is in flight after `ring` returns (the host resumed before this point), so the
        // raw read aliases nothing.
        //
        // SAFETY: `resp_page` is a `PAGE_SIZE` page (constructor contract) and `HEADER_LEN <=
        // PAGE_SIZE`, so the read stays in-page; no `&`/`&mut` to the page outlives this block.
        let mut header = [0_u8; HEADER_LEN];
        unsafe {
            ptr::copy_nonoverlapping(self.resp_page, header.as_mut_ptr(), HEADER_LEN);
        }

        // Step 6: transport-level rejection. A response page that does not begin with the frame
        // magic carries no frame — e.g. the host wrote nothing, so step 3's zeros remain. (Wire
        // format is little-endian.)
        let magic = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        if magic != FRAME_MAGIC {
            return Err(TransportError::HostRejected);
        }

        // Step 7: derive the response length from the self-describing header (`payload_len` lives
        // at wire offset 16) and bound-check it in `u64` BEFORE any cast. `payload_len` is
        // host-controlled (up to `u32::MAX`), so `HEADER_LEN + payload_len` can exceed `u32::MAX`;
        // computing and checking in `u64` is load-bearing — a bare `as usize` could truncate (a
        // 16-bit `usize` truncates even a 32-bit length) and slip an out-of-range value past the
        // check. Nothing is copied on failure.
        let payload_len = u32::from_le_bytes([header[16], header[17], header[18], header[19]]);
        let total = HEADER_LEN as u64 + payload_len as u64;
        if total > PAGE_SIZE as u64 || total > resp.len() as u64 {
            return Err(TransportError::BadResponseLength);
        }
        // Bound check passed: `total <= PAGE_SIZE` and `total <= resp.len()`, both `usize`-bounded.
        let len = total as usize;

        // Step 8: copy exactly `len` validated bytes out of the response page.
        //
        // SAFETY: `len <= PAGE_SIZE`, so the read stays within the response page; `len <=
        // resp.len()`, so the write stays within `resp`. `resp` does not overlap the response page
        // (constructor contract), so the copy is non-overlapping. No `&`/`&mut` to the page
        // outlives this block.
        unsafe {
            ptr::copy_nonoverlapping(self.resp_page, resp.as_mut_ptr(), len);
        }
        Ok(len)
    }
}
