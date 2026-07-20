# Task 10 — `consonance/hypercall-doorbell`: guest-side VMCALL transport shim

Read `tasks/00-CONVENTIONS.md` first. Touch only `consonance/hypercall-doorbell/`.

This crate is the thin guest-side glue that makes a hypercall actually leave the guest. Task
01 (`hypercall-proto`) defined the wire protocol and a `Client<T: Transport>` that turns
service calls into request frames and decodes response frames — but left the `Transport`
abstract. This task implements that `Transport` for the real channel: marshal a request frame
into a shared guest-physical page, execute one `VMCALL`, and copy the response frame back out.
A `Client<VmcallTransport>` is then a complete, working guest hypercall client — and per
INTEGRATION.md §1 it "composes with the task 01 `Client` unchanged."

## Context

The mechanism is fixed by **INTEGRATION.md §1 (VMCALL transport ABI v0)** — read it; it is the
contract this crate implements. In brief:

- The guest reserves two 4 KiB-aligned guest-physical pages: a **request page** and a
  **response page**. (Bare-metal payloads — task 04 — use static page-aligned buffers; their
  guest-physical address equals their linear address under the payload's identity map.)
- To issue a hypercall the guest writes one complete request frame into the request page, then
  executes `VMCALL` with:
  - `RAX = 0x31504348` (`"HCP1"` little-endian — the same `u32` as the frame header magic),
  - `RBX = GPA` of the request page,
  - `RCX = GPA` of the response page.
- The host runs `Dispatcher::dispatch(request_page, response_page)` and, before resuming the
  vCPU, sets `RAX = response frame length`. **Transport-level failure** (bad magic in `RAX`,
  bad GPAs) sets `RAX = 0`; the guest must treat that as a transport error.
- The exchange is synchronous and single-in-flight: the vCPU is blocked for the whole call.

The host side of this ABI (the VM-exit handler in vmm-core) is **frontier work and out of
scope here** — which raises the one question this spec must answer: *how is a crate that
needs a hypervisor gate-tested on a laptop?* The answer is the **`VmExit` seam** below: the
deterministic page-marshalling logic is factored out from the privileged `vmcall` instruction,
so the whole marshalling path plus the real task-01 `Client` and `Dispatcher` round-trip can be
exercised in-process under `cargo test` with no `/dev/kvm`.

## Public API (contract — exact names, types, semantics)

`#![no_std]`. All items below are `pub` at the crate root.

```rust
/// Value placed in RAX to identify a hypercall VMCALL. Equals the frame header magic
/// ("HCP1" read little-endian) widened to 64 bits.
pub const VMCALL_MAGIC: u64 = 0x3150_4348;

/// Size in bytes of each shared page. Equals `hypercall_proto::MAX_FRAME`, so exactly one
/// frame fits in one page.
pub const PAGE_SIZE: usize = 4096;

/// The privileged hypercall-exit primitive, abstracted so the page-marshalling logic can be
/// driven by a host-side loopback in tests without a hypervisor.
///
/// Implementors perform the platform exit with the three register values and return the
/// host-set RAX: the response frame length, or 0 on transport-level rejection.
pub trait VmExit {
    /// Execute the hypercall exit. `req_gpa`/`resp_gpa` are the guest-physical addresses
    /// already written into the transport's pages. Returns host RAX.
    ///
    /// # Safety
    /// `req_gpa` and `resp_gpa` must name two distinct, page-aligned, `PAGE_SIZE`,
    /// guest-owned pages valid for the duration of the call.
    unsafe fn vmcall(&mut self, magic: u64, req_gpa: u64, resp_gpa: u64) -> u64;
}

/// Production `VmExit`: executes a real `vmcall` instruction. Meaningful only inside a VM.
pub struct RealVmcall;

/// Guest-side `hypercall_proto::Transport` over the §1 VMCALL ABI.
///
/// Generic over the exit primitive so tests can substitute a loopback host; defaults to
/// `RealVmcall` so production code writes `VmcallTransport`.
pub struct VmcallTransport<V: VmExit = RealVmcall> { /* private */ }

/// Errors surfaced by the transport. Becomes `ClientError::Transport(..)` in the task-01 client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    /// The request frame is larger than the request page (`req.len() > PAGE_SIZE`).
    RequestTooLarge,
    /// The host rejected the call (`RAX == 0`): bad magic or bad GPAs.
    HostRejected,
    /// The host-returned length exceeds `PAGE_SIZE` or the caller's `resp` buffer — a
    /// malformed or hostile host response; never partially copied.
    BadResponseLength,
}

impl RealVmcall {
    pub const fn new() -> Self;
}

impl VmcallTransport<RealVmcall> {
    /// Construct from the guest-physical addresses of the two reserved pages.
    ///
    /// # Safety
    /// `req_gpa` and `resp_gpa` must each name a distinct, page-aligned, `PAGE_SIZE`,
    /// guest-owned page mapped read+write for the lifetime of the transport. Each GPA must be
    /// **non-null and dereferenceable as a Rust pointer for `PAGE_SIZE` bytes** (GPA `0`,
    /// though it can be page-aligned and hardware-mapped, is not a valid Rust pointer).
    /// Because the transport dereferences these values directly (it accesses the pages by
    /// address), **each GPA must also equal the page's linear/virtual address** — i.e. the
    /// pages are
    /// identity-mapped, as under the task-04 payload map; a GPA that is not a valid linear
    /// address is UB. The pages must be **initialized byte storage** (real memory, not
    /// `MaybeUninit` — e.g. zeroed at reservation: the host may set `RAX = len` without
    /// writing all `len` bytes, and step 6 copies them regardless) and **exclusively owned**
    /// by this transport for its lifetime — no other live reference may alias them (the `req`
    /// and `resp` slices passed to `exchange` must not overlap them), since the host writes
    /// the response page out-of-band.
    pub unsafe fn from_gpas(req_gpa: u64, resp_gpa: u64) -> Self;
}

impl<V: VmExit> VmcallTransport<V> {
    /// Construct with an explicit exit primitive (the loopback host in tests).
    ///
    /// # Safety
    /// Same page requirements as `from_gpas`; `exit` must be consistent with those GPAs.
    pub unsafe fn with_exit(req_gpa: u64, resp_gpa: u64, exit: V) -> Self;
}

impl<V: VmExit> hypercall_proto::Transport for VmcallTransport<V> {
    type Error = TransportError;
    fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error>;
}
```

You may add private fields, helpers, and a `Debug` derive where they do not change the
contract. **Do not implement or derive `Default` for `VmcallTransport`** — a safe `Default`
would manufacture a transport without the unsafe GPA/page invariants, so safe `exchange()`
could then reach UB; `Default` is acceptable only for the invariant-free `RealVmcall`. Do not
rename or re-sign anything above.

### `exchange` semantics (normative, step by step)

`fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, TransportError>` must:

1. If `req.len() > PAGE_SIZE` → `Err(RequestTooLarge)` (never write past the page).
2. Copy `req` into the request page (first `req.len()` bytes), then **clear the page tail**
   (`req.len()..PAGE_SIZE` → `0`). The host derives the frame length from the header and reads
   that many bytes from the page; the task-01 `Client` always passes a complete frame whose
   header-encoded length equals `req.len()`, but a *direct* `Transport::exchange` caller could
   pass a `req` shorter than its encoded length, which without a tail-clear would let the host
   read **stale bytes from a previous call**. Zeroing the tail makes that misuse expose only
   zeros, never a prior frame — cheap defense in depth for a page the host reads by header
   length. (Document this as the chosen contract; the alternative — a hard precondition that
   `req` is a complete valid frame — is weaker and easy to violate.)
3. **Zero the response page**, then invoke
   `self.exit.vmcall(VMCALL_MAGIC, self.req_gpa, self.resp_gpa)` → `rax`. Clearing first means a
   host that sets `RAX = len` without writing all `len` bytes can only yield zeros in the
   unwritten span — never a *stale prior response* that might itself decode as a valid but
   replayed frame.
4. If `rax == 0` → `Err(HostRejected)`.
5. Bound-check **in `u64`, before any cast**: if `rax > PAGE_SIZE as u64` or
   `rax > resp.len() as u64` → `Err(BadResponseLength)` with nothing copied. Comparing before
   the `as usize` cast keeps the bound correct even on a target where `usize` is narrower than
   64 bits (a bare `let len = rax as usize` would truncate, e.g. `0x1_0000_0000 → 0`, and slip
   a huge `rax` past the check). Only after the check passes, `let len = rax as usize`. `rax`
   is attacker-controlled (whatever the host wrote into RAX); this bound check is the crate's
   load-bearing safety property — it must be impossible to make `exchange` read past the
   response page or write past `resp`, or to panic, for **any** `rax` value.
6. Copy `len` bytes from the response page into `resp[..len]`; return `Ok(len)`.

The task-01 `Client` then does its own `resp.get(..len)` and frame validation on top, so a
defective host yields a clean `ClientError`, never UB.

## The `VmExit` seam and the loopback gate

`RealVmcall::vmcall` does nothing but execute the instruction and read RAX — it does **not**
touch the pages (the host accesses them out-of-band by GPA). The transport writes the request
page *before* the call and reads the response page *after*. That factoring is what makes the
crate testable:

- **Production**: `VmcallTransport<RealVmcall>` + a real hypervisor.
- **Test**: `VmcallTransport<LoopbackHost>`, where `LoopbackHost: VmExit` plays the host —
  it reads the request page at `req_gpa`, runs a real `hypercall_proto::Dispatcher` (with stub
  `Service`s) into the response page at `resp_gpa`, and returns the dispatcher's length as RAX
  (returning 0 if `magic != VMCALL_MAGIC`, to exercise the rejection path). The loopback **must
  reach the pages via the numeric `req_gpa`/`resp_gpa` it is passed** (interpreting them as
  pointers to the test's page-aligned buffers, faithful to production) — it must **not** use
  captured handles that ignore those arguments. Otherwise a transport bug that puts the wrong
  GPA in `RBX`/`RCX`, or swaps them, would still pass the gate, defeating the point of testing
  register/GPA passing. (If it keeps a side table of expected page addresses, it must validate
  the received GPAs against it and reject a mismatch.)

The **required integration test** drives the real task-01 client end to end with no KVM:
construct `hypercall_proto::Client::new(VmcallTransport::with_exit(.., LoopbackHost::new(dispatcher)))`
and assert that `console_write`, `entropy_fill`, `block_read`, `block_capacity`, and
`event_emit` all round-trip correctly against stub services. This proves the marshalling, the
register/GPA passing, and composition with the unmodified task-01 `Client` in one gate.

## Dependencies, features, and grants

- **Depends on `hypercall_proto`** (the sibling crate). This is an explicit, deliberate
  exception to conventions rule 2: the entire purpose of this task is to implement
  `hypercall_proto::Transport` so a `Client<VmcallTransport>` composes — re-declaring the trait
  locally would produce a *different* trait that does not compose. Call this exception out in
  your `IMPLEMENTATION.md`; no other sibling dependency is permitted. No new third-party
  dependencies beyond the conventions whitelist (`proptest` for the round-trip tests is
  expected); ask-by-comment if you believe you need more.
- **Features (get this exactly right — it is the easiest way to fail the no_std gate)**:
  `hypercall_proto`'s default feature set is `["host"]`, which implies `std` and would break
  the bare-metal build. So depend on it as a **normal** dependency with
  `default-features = false, features = ["guest"]`, and pull the `host` `Dispatcher` **only as
  a dev-dependency**: `[dev-dependencies] hypercall-proto = { path = "...", features = ["host"] }`.
  Under the edition-2021/2024 feature resolver (v2), dev-dependency features are not unified
  into the normal `cargo build --target x86_64-unknown-none` build, so the library stays
  `no_std` while `cargo test` (host triple, `std`) gets the `Dispatcher`. The library uses only
  `hypercall_proto`'s guest-side no_std items (the `Transport` trait, `MAX_FRAME`, frame
  `encode`/`decode`); all host-only code lives under `#[cfg(test)]`.
- **`unsafe` is granted** for exactly two purposes, each block carrying a `// SAFETY:` comment
  that discharges the stated obligation: (a) the `vmcall` inline `asm!` in `RealVmcall`, and
  (b) reading/writing the shared pages through their GPAs (and, in the loopback, the same).
  No other `unsafe`.
- **`vmcall` `asm!` ordering (normative).** The host reads the request page and writes the
  response page *out-of-band* during the instruction — invisible to the compiler. So the
  `asm!` **must** carry a memory clobber that prevents reordering across it: do **not** use the
  `nomem`/`readonly`/`pure` options; the default (no `nomem`) "may read/write memory" semantics
  must hold so the request-page stores are not sunk past `vmcall` and the response-page loads
  are not hoisted before it. State this in the `// SAFETY:` comment. Also handle **RBX
  explicitly**: on x86-64 LLVM reserves RBX and rejects it as a named `in("rbx")` operand in
  some configurations (e.g. PIC) — pass the request GPA through a scratch register and
  `mov`/`xchg` it into `rbx` inside the `asm!` (restoring on exit), rather than binding `rbx`
  directly. RAX in = `magic`, RAX out = response length; RCX = response GPA.
- **No Rust reference to a shared page may be live across the exit (normative).** The host
  reads the request page and writes the response page out-of-band *during* `vmcall`, so a `&`
  or `&mut` to either page held across `VmExit::vmcall` is aliasing UB — the memory changes
  without going through the reference. Store the pages as raw pointers (`NonNull<u8>`/`*mut
  u8`), not as a `&mut [u8]` field, and touch them only with raw-pointer ops
  (`core::ptr::copy_nonoverlapping`, `core::ptr::write_bytes`) — or with borrows whose scope
  ends *before* the call and are re-derived *after* it. The same rule binds the loopback host.

## Determinism, safety, portability

- **Determinism**: the shim is pure marshalling — no `HashMap` iteration to output, no float,
  no wall-clock, no unseeded randomness. The externally-sourced inputs are the host's RAX
  (handled by step 5) **and the entire contents of the response page** — the host writes those
  `len` bytes, so they are equally untrusted/host-controlled. The transport copies them
  verbatim into `resp` (bounded by step 5); their *interpretation* is the task-01 `Client`'s
  job, which validates the frame and so turns a hostile/garbage response into a clean
  `ClientError` rather than UB or a wrong-but-accepted result. (Nondeterminism, if any, is the
  host's; not this crate's.) The `Client` validates frame *format*, not authenticity — a host
  that writes a well-formed but semantically false response is the host lying, which is outside
  this crate's trust model; the guarantee here is "no UB, no panic, no stale-frame replay
  (response page cleared per step 3)," not freshness or authenticity.
- **No panics on untrusted input** (rule 4): no indexing/slicing/arithmetic on `rax` or `req`
  that can panic. A hostile `rax` (`0`, `PAGE_SIZE+1`, `u64::MAX`, or one exceeding `resp.len()`)
  must yield the specified `Err`, never a panic or over-copy. Cover this explicitly in tests.
- **Portability** (rule 6): `VMCALL` is an x86-64 instruction. Gate the real `asm!` behind
  `#[cfg(target_arch = "x86_64")]`; on other architectures provide a `RealVmcall::vmcall` body
  that still compiles so the crate **builds on Apple-Silicon (aarch64) macOS** for the loopback
  tests. The off-arch body must be **deterministic and non-panicking**: return `0` (so
  `exchange` yields `Err(HostRejected)`), **not** `unreachable!`/`panic!`/`todo!`. `vmcall` is
  reachable through the *safe* `Transport::exchange` API, so "never reached off a VM" is a
  caller assumption, not a guarantee — a panic there would be a panic on the safe path. This
  single `cfg(target_arch)` split is permitted — it is intrinsic to the hardware instruction —
  and is **not** a `cfg(target_os)` logic fork. The loopback tests use `LoopbackHost`, not
  `RealVmcall`, so they run and pass on every host architecture.

## Gates (all must pass before you are done)

The standard conventions gates, plus:

1. `cargo build -p hypercall-doorbell --all-features` and with default features — both clean on
   macOS (incl. Apple Silicon) and Linux.
2. **no_std proof**: the library builds for a bare-metal target where `std` is unavailable —
   `rustup target add x86_64-unknown-none` then
   `cargo build -p hypercall-doorbell --target x86_64-unknown-none` succeeds (this also compiles
   the real `vmcall` `asm!`). A library that accidentally pulls `std` fails here.
3. `cargo test -p hypercall-doorbell --all-features` — includes the loopback round-trip
   integration test (all five client calls) and the hostile-`rax` rejection tests; runtime
   budget ~1 min.
4. A `proptest` (≥ 256 cases) that round-trips arbitrary service payloads through
   `Client<VmcallTransport<LoopbackHost>>` and asserts the bytes returned equal what the stub
   services produced.
5. `cargo clippy -p hypercall-doorbell --all-features --all-targets -- -D warnings` and
   `cargo fmt -p hypercall-doorbell -- --check` clean.

## Non-goals

The host-side VM-exit handler / GPA validation (vmm-core frontier work — this crate only
*assumes* the §1 host behavior, it does not implement it); the Linux guest driver that
allocates the pages at runtime (a later task — bare-metal static pages are sufficient here);
push-style host-initiated input (not part of the §1 ABI); multiple in-flight requests or
multi-vCPU concurrency (the ABI is single-in-flight by construction); any change to
`hypercall-proto` (you depend on it unmodified); reserving/placing the actual pages in a real
payload binary (task 04 / loader concern — you take the GPAs as constructor inputs).

## Deliverable

A branch `task/hypercall-doorbell` containing only `consonance/hypercall-doorbell/`, all gates green,
and a short `IMPLEMENTATION.md` noting: the rule-2 `hypercall-proto` dependency exception and
why; how the `VmExit` seam and loopback host are wired; the `cfg(target_arch)` handling for the
`vmcall` instruction; and any deviations considered and rejected.
