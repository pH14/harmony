# `vmcall-transport` — implementation notes

Guest-side `hypercall_proto::Transport` over the INTEGRATION.md §1 **hypercall-doorbell ABI v1**.
A `Client<VmcallTransport>` is a complete, working guest hypercall client that composes with the
task-01 `Client` unchanged.

**Task 20 reworked this crate** from the v0 `VMCALL` doorbell (task 10) to a **port-I/O doorbell**
so the hypercall channel works on **stock KVM with no kernel patch**. The `exchange()` signature,
the `TransportError` set, and the load-bearing `u64`-bounds-check-before-cast invariant are
preserved verbatim; only the doorbell primitive changed.

## Chosen mechanism — port-I/O doorbell (not VMCALL, not MMIO)

**Why not `VMCALL` (integrator ruling, 2026-06-23).** Stock KVM services `VMCALL` *in-kernel*:
`kvm_emulate_hypercall` returns `-ENOSYS` to the guest for our magic number (`0x3150_4348`) and
resumes — it never surfaces a `KVM_EXIT_HYPERCALL` to userspace for a custom number (only
`KVM_HC_MAP_GPA_RANGE` exits). So a `VMCALL` doorbell needs the patched/direct-VMX backend (task
21). A port `OUT` to a magic port, by contrast, **is** surfaced by stock KVM as
`KVM_EXIT_IO` → the existing `Exit::Io` — so the channel works with **zero** kernel patch.

**Why port-I/O over MMIO** (the spec's default; MMIO only if "materially cleaner" — it isn't):

- `OUT` is a single instruction and `Exit::Io { port, size, write }` carries exactly what the
  doorbell needs: the `OUT` value *is* the request length (`write: Some(len)`). The protocol maps
  1:1 onto the backend with no new exit variant and no decoding.
- MMIO would need a *separate* magic GPA that must be **left unmapped** so the access faults to
  userspace — fiddlier to place next to the two mapped data pages, and it overloads the data
  region with MMIO semantics. A port keeps the request/response pages as plain RAM (simplest for
  the future guest driver) and keeps the doorbell a distinct, unmistakable signal.

No `[question]` was raised: port-I/O is the spec's recommended default and MMIO is not cleaner.

**Single `OUT`, no `IN` (revised in PR #44 review — atomicity).** An earlier revision used a
two-exit `OUT`/`IN` doorbell (the `IN` returned the response length via `complete_read`). The
codex cross-model pass flagged this as a real atomicity regression: the guest resumes *between*
the two exits while the host still owes a response length, so an interrupt injected in that
window whose handler re-enters the doorbell clobbers the fixed pages and the pending length — not
single-in-flight, unlike the atomic single-`VMCALL`. The fix (the spec's own offered alternative)
**drops the `IN`**: the response length is already self-describing in the frame header
(`HEADER_LEN + payload_len`), so `exchange` reads it straight from `RESP_GPA` after the single
`OUT`. One exit ⇒ atomic again, no host-side pending state across a guest resume.

## The finalized ABI (pinned in `src/lib.rs`, documented in INTEGRATION.md §1)

| Constant | Value | Role |
|---|---|---|
| `DOORBELL_PORT` | `0x0CA1` | the magic 16-bit port the guest rings (`> 0xFF` → addressed via `DX`) |
| `REQ_GPA` | `0x0000_E000` | fixed request-page GPA (4 KiB, VMM-reserved, identity-mapped) |
| `RESP_GPA` | `0x0000_F000` | fixed response-page GPA (4 KiB, VMM-reserved, identity-mapped) |

The doorbell carries **no pointer** (an `OUT` cannot pass two 64-bit GPAs), so the two frame pages
live at fixed GPAs the contract reserves and the VMM maps. One exchange is a **single `OUT` VM
exit** (synchronous, single in-flight, **wait-free** — one exit, no spinning, no retries):

1. The guest writes its request frame into `REQ_GPA`.
2. `OUT DOORBELL_PORT, EAX` with `EAX` = request length → `Exit::Io { write: Some(len) }`; the host
   reads `len` bytes from `REQ_GPA`, runs `Dispatcher::dispatch`, writes the response **frame** into
   `RESP_GPA`, and resumes at the next instruction (an `OUT` needs no completion).
3. `exchange` reads the response length straight from the response-frame header in `RESP_GPA`
   (`HEADER_LEN + payload_len`) and copies that many bytes out. A response page that does not begin
   with the frame magic (the host wrote nothing → step 3's zeros remain) ⇒ `HostRejected`.

**Atomicity.** One exit means the host fully services the exchange and holds **no pending state
across a guest resume** — exactly like the old single-`VMCALL` doorbell, and the reason the length
is read from the header rather than via a second `IN` exit (see "Single `OUT`, no `IN`" above).

**Rejection signal.** With no `IN` to return `0`, `HostRejected` is signalled in-band: `exchange`
zeroes the response page before the `OUT` (step 3), so a host that writes no frame leaves the
magic field `0`; `exchange` reads the 24-byte header and returns `HostRejected` when
`magic != 0x31504348`. This keeps the `TransportError` set meaningful and is the cleanest in-band
reject (a zeroed/garbage page is caught at the transport boundary, not deferred to the `Client`).

**Width / the load-bearing bound check.** The `OUT` carries the request length in 32-bit `EAX`
(`size = 4`), the natural fit for `Exit::Io { write: u32 }`. The response length is
`HEADER_LEN + payload_len` where `payload_len` is the host-controlled `u32` at wire offset 16 — so
the sum can exceed `u32::MAX`. `exchange` computes it as `HEADER_LEN as u64 + payload_len as u64`
and bound-checks (`≤ PAGE_SIZE` **and** `≤ resp.len()`) **in `u64` before any `as usize` cast** —
the task-10 invariant, now *more* exercised than before (a bare cast could truncate the > `u32`
sum on a narrow `usize`). Reading the fixed 24-byte header is always in-page (`HEADER_LEN ≤
PAGE_SIZE`, static-asserted); only the post-header copy length is dynamic and bounded.

`from_gpas`/`with_doorbell`/`new` are the constructors (`new()` uses the ABI constants;
`from_gpas`/`with_doorbell` take explicit GPAs for a relocating loader and for tests).

## `Exit::Hypercall` disposition (vmm-backend — **not** edited)

The task flagged that `Exit::Hypercall` "may become vestigial — note which; do not silently break
the trait." Decision: **`Exit::Hypercall` is kept, and `consonance/vmm-backend` is not touched.**

- The stock-KVM doorbell maps to the **existing `Exit::Io`** variant — which stock `KvmBackend`
  already surfaces. No new exit variant, no backend code change, the `Backend` trait is unchanged.
- `Exit::Hypercall(HypercallRegs)` stays for the **patched/direct-VMX backend** (task 21), where a
  `VMCALL` doorbell carries the same frame semantics (RAX = magic, RBX/RCX = page GPAs, host sets
  RAX = response length). Its doc comment in `vmm-backend/src/exit.rs` *already* states "Not
  surfaced by stock `KvmBackend` … exists for `PatchedKvmBackend`/`DirectVmxBackend`", so it is
  already aligned with this rework and needs no edit.

So `Exit::Hypercall` is vestigial **for the stock backend** but live **for the patched backend**;
it is neither removed nor broken. (Editing `vmm-backend` is also out of this task's lane — rule 1 —
and its gates are not in scope here; nothing there needed to change.)

## Determinism note — the doorbell constants are NOT hashed (confirmed)

`DOORBELL_PORT`/`REQ_GPA`/`RESP_GPA` are transport-ABI constants that live in INTEGRATION.md §1
and `src/lib.rs` **only**. They are deliberately **not** rows in `docs/cpu-msr-contract.toml`, so
they **never enter the §6 canonical form or `contract_hash`** — they carry no per-host or
hidden-µarch input. **Verified:** the contract edits are prose-only (markdown Rationale/Citation +
a TOML *comment*, which the §6 serializer does not read); `cargo test -p vmm-core` still passes
`contract_hash_matches_committed_registry` and `canonical_form_matches_golden` (hash unchanged at
`e01f0835…`). The VMCALL *instruction* row's normative fields (`vmx-exit(vmcall-unconditional)` /
`hypercall-dispatch` / `intercept`) are left untouched — that is the genuine disposition of the
`VMCALL` instruction under the ratified determinism backend; only the *prose* claim that VMCALL is
*stock-serviceable* was wrong and is corrected.

## The rule-2 `hypercall-proto` dependency exception (unchanged)

Conventions rule 2 says "do not depend on any sibling crate." This crate is the spec-sanctioned
exception: its entire purpose is to *implement* `hypercall_proto::Transport` so a
`Client<VmcallTransport>` composes. Re-declaring the trait locally would produce a *different*
trait the task-01 `Client` does not accept. So we depend on `hypercall-proto` directly; no other
sibling dependency is taken.

### Feature wiring (the no_std-critical part)

`hypercall-proto`'s default feature set is `["host"]`, which implies `std` and would break the
bare-metal build. The wiring (unchanged from task 10):

- **Normal dependency**: `default-features = false, features = ["guest"]` — pulls only the
  guest-side, no_std items (`Transport`, `MAX_FRAME`).
- **Dev-dependency**: `features = ["host"]` — pulls the `Dispatcher` and reference services used
  only by the loopback tests.

Under the edition-2024 / resolver-v2 feature resolver, dev-dependency features are **not** unified
into a non-test build, so `cargo build --target x86_64-unknown-none` compiles the library with
`guest` only (stays `no_std`); `cargo test` (host triple, `std`) additionally gets `host`. An
explicit `version = "0.1.0"` on both entries satisfies `cargo deny`'s wildcard ban. A
`const _: () = assert!(PAGE_SIZE == hypercall_proto::MAX_FRAME);` makes the "one frame per page"
assumption a compile-time error if the wire cap ever diverges; two further `const` asserts pin the
ABI GPAs page-aligned and distinct.

## How the `IoDoorbell` seam and loopback host are wired

`RealIoDoorbell::ring` does nothing but execute the single `OUT` — it never touches the pages (the
host reaches them out-of-band by their fixed GPA). All deterministic page-marshalling lives in
`VmcallTransport::exchange`, which writes the request page *before* the doorbell and reads the
response page (header + bounded copy) *after* it. That factoring is what makes the privileged path
testable with no `/dev/kvm`:

- **Production**: `VmcallTransport<RealIoDoorbell>` + a real hypervisor.
- **Test**: `VmcallTransport<LoopbackHost>` (`tests/loopback.rs`). `LoopbackHost: IoDoorbell` plays
  the host: it holds the fixed page GPAs out-of-band (exactly as the production host knows them
  from the ABI, **not** from the pointer-free doorbell), validates the doorbell **port**, reads the
  `req_len` bytes the guest rang (**only** those bytes — see fidelity below), runs a real
  `hypercall_proto::Dispatcher` with stub services, and writes the response frame into the response
  page. On a wrong port (or `req_len > PAGE_SIZE`) it writes **nothing**, leaving the zeroed page as
  the rejection sentinel `exchange` reads as `HostRejected`.

The required end-to-end test (`five_client_calls_round_trip_through_loopback`) drives all five
service calls — `console_write`, `entropy_fill`, `block_read`, `block_capacity`, `event_emit` —
asserting each round-trips against the stub services. This proves the marshalling, the port/length
passing, and composition with the unmodified task-01 `Client` in one gate, with no KVM.

**Loopback fidelity (PR #44 [suggestion]).** The mock dispatches only the **exposed** request bytes
(`&req_local[..req_len]`), not the zero-padded page, and rejects `req_len > PAGE_SIZE` — so a
request whose rung length is shorter than its encoded frame is seen as *truncated* (answered
`BadRequest`), matching `Exit::Io { write: Some(len) }` exposing only `len` bytes, instead of being
silently zero-padded into a valid-looking call. `loopback_dispatches_only_exposed_request_bytes`
asserts the truncated case yields `BadRequest`; `loopback_rejects_bad_port` asserts a wrong port
leaves the response page unwritten (so `exchange` would reject it).

**Lost vs task 10:** the v0 ABI passed the GPAs in `RBX`/`RCX`, so the loopback could catch a
transport that swapped or mis-set them. The doorbell carries **no pointer** (pages are fixed ABI
constants the host knows independently), so that GPA-mispassing failure mode no longer exists —
`loopback_rejects_bad_port` guards the one thing that *can* go wrong (a wrong port → rejection).

## `cfg(target_arch)` handling for the `OUT` doorbell

Port I/O is an x86-64 facility, so `RealIoDoorbell::ring` has two bodies behind a
`target_arch`/`miri` split (intrinsic to the hardware — **not** a `cfg(target_os)` logic fork). The
real `asm!` body is gated `#[cfg(all(target_arch = "x86_64", not(miri)))]`; the no-op stub is gated
`#[cfg(any(not(target_arch = "x86_64"), miri))]`, so the `not(miri)` clause routes the **Miri**
build (even on the x86-64 box) to the stub — Miri interprets MIR and cannot execute inline asm.

- **x86-64 (non-Miri)**: a single `out dx, eax`. The port (`> 0xFF`) is carried in `DX`; `EAX`
  carries the request length. The `asm!` keeps the **default** (no `nomem`/`readonly`/`pure`) "may
  read/write memory" semantics so the request-page stores are not sunk past the `out` — the host
  reads/writes the pages out-of-band, invisibly to the compiler. `preserves_flags` is deliberately
  **omitted** (the host owns guest state across an exit except as specified, so we do not assume
  RFLAGS survives); `nostack` is accurate (`out` touches no guest stack).
- **off-arch (e.g. Apple-Silicon aarch64) or under Miri**: a no-op stub. `exchange` then reads the
  response page it zeroed (step 3), finds no frame magic, and returns `TransportError::HostRejected`.
  `ring` is reachable through the *safe* `Transport::exchange` API, so "never reached off a VM" is a
  caller assumption, not a guarantee — a `panic!`/`unreachable!`/`todo!` there would be a panic on
  the safe path; doing nothing keeps the safe API total and deterministic.

## Safety model

- **Pages held as raw pointers, never `&mut [u8]` fields.** The host writes the response page
  out-of-band during the doorbell, so a Rust reference held across `IoDoorbell::ring` would be
  aliasing UB. `VmcallTransport` stores `*mut u8` and touches the pages only with
  `core::ptr::{copy_nonoverlapping, write_bytes}`; no borrow to either page is live across the
  call. The same rule binds the loopback host.
- **The `u64` bound check is the load-bearing property.** The response length comes from the
  host-written frame header: `total = HEADER_LEN as u64 + payload_len as u64`, where `payload_len`
  is an attacker-controlled `u32`. `exchange` compares `total > PAGE_SIZE` and `total > resp.len()`
  **in `u64`, before any `as usize` cast** — `total` can exceed `u32::MAX`, so a bare `as usize`
  would truncate on a narrow-`usize` target and slip an out-of-range value past the check. Reading
  the fixed 24-byte header is itself always in-page (`HEADER_LEN ≤ PAGE_SIZE`, static-asserted); no
  header value can make `exchange` read past the response page, write past `resp`, or panic; on
  failure nothing is copied.
- **Magic gate before length.** `exchange` rejects (`HostRejected`) when the response page does not
  begin with the frame magic, *before* trusting `payload_len` — a zeroed (unwritten) page is a
  reject, not a zero-length frame.
- **Defense-in-depth zeroing.** Step 2 clears the request-page tail so a direct `exchange` caller
  passing a short `req` exposes only zeros (never stale bytes from a prior call) to a host that
  reads by the rung length. Step 3 zeroes the whole response page before the doorbell so the
  rejection sentinel is well-defined (an unwritten page reads magic 0) and a host that writes a
  response shorter than the page leaves zeros in the tail, never a stale prior frame.
- **`unsafe` is confined to the two granted purposes**, each with a `// SAFETY:` comment: (a) the
  `OUT` `asm!` in `RealIoDoorbell`, and (b) reading/writing the shared pages through their GPAs (in
  `exchange` — including the in-page header read — and the same in the loopback/scripted test
  hosts).
- **No `Default` for `VmcallTransport`** (only for the invariant-free `RealIoDoorbell`): a safe
  `Default` would manufacture a transport without the unsafe GPA/page invariants, after which safe
  `exchange()` could reach UB.

## Test coverage beyond the happy path (green gate is the floor)

Case counts below are the **native** numbers; under Miri a `cfg!(miri)` `config()` helper cuts
every property test to 16 cases.

- `round_trip_arbitrary_payloads` (proptest, 256 cases) — the required gate: arbitrary console,
  entropy (multi-frame), block, and event payloads through `Client<VmcallTransport<LoopbackHost>>`,
  entropy checked against an independent reference stream.
- `exchange_classifies_any_response_page` (proptest, 512 cases) — for **any** host-written response
  page (`valid_magic` toggled, `payload_len_field` over the full `u32` domain, arbitrary body,
  arbitrary caller-buffer length), `exchange` classifies exactly per spec, never panics, never
  over-copies; on `Ok` the copied bytes equal the page contents. Because `total = HEADER_LEN +
  payload_len` can exceed `u32::MAX`, this exercises the `u64` bound-check-before-cast beyond any
  32-bit value; `!valid_magic` must always yield `HostRejected`.
- `client_survives_garbage_host` (proptest, 512 cases) — a hostile host writes an arbitrary response
  page (optionally a valid magic + forged length); every task-01 `Client` call yields `Ok` or a
  clean `ClientError`, never a panic/UB.
- `hostile_response_is_rejected_without_panic_or_overcopy` — fixed cases: a no-magic (zeroed) page →
  `HostRejected`; forged `payload_len` fields (`MAX_PAYLOAD+1`, `u32::MAX`, `0xFFFF_FFFF`,
  exceeds-buffer) → `BadResponseLength`; plus the exact accept boundaries (caller-buffer edge and a
  full-page frame).
- `loopback_rejects_bad_port` — proves the loopback writes a frame only for the right doorbell port
  (a wrong port leaves the page unwritten → `exchange` would reject it).
- `loopback_dispatches_only_exposed_request_bytes` — the PR #44 fidelity fix: a request rung with a
  length one byte short of its encoded frame is answered `BadRequest` (seen as truncated), not
  zero-padded into a valid call.

## Miri (UB validation — quality-g)

The crate carries raw-pointer `unsafe`, so it runs under the **Miri** gate (the unsafe⇒Miri
review-bar rule). Behavioral tests cannot see UB that does not surface as a wrong value or a panic;
Miri can. The crate is *designed* for it: the privileged `OUT` sits behind the `IoDoorbell` seam,
so the loopback suite drives **all** the unsafe pointer code (including the in-page header read)
with no inline asm (which Miri cannot interpret).

**Command** (matches the `miri` job in `.github/workflows/quality.yml` and `.githooks/pre-push`,
where `vmcall-transport` is already in the `-p` list — the crate name is unchanged by this task):

```sh
MIRIFLAGS=-Zmiri-permissive-provenance \
  cargo +nightly-2026-06-16 miri test -p vmcall-transport
# => test result: ok. 8 passed; 0 failed
```

What each piece is and why:

- **Pinned nightly (`nightly-2026-06-16`).** Miri is nightly-only; the pin matches the suite.
- **`RealIoDoorbell` asm excluded under Miri.** The real `OUT` `asm!` is
  `#[cfg(all(target_arch = "x86_64", not(miri)))]`; the no-op stub is
  `#[cfg(any(not(target_arch = "x86_64"), miri))]`. So even on the x86-64 box the Miri build routes
  `RealIoDoorbell` to the stub. **Honest limitation:** the real privileged instruction is therefore
  *not* Miri-covered; only the pointer/bound-check logic in `exchange` is (driven by
  `LoopbackHost`/`ScriptedHost`). That logic is the entire reason this crate carries `unsafe`.
- **Reduced proptest cases under `cfg!(miri)`.** 16 cases (vs native 256/512) and failure
  persistence disabled — its default resolves a regression path via `current_dir()` (getcwd), which
  Miri's filesystem isolation rejects.
- **`-Zmiri-permissive-provenance`.** The transport models hardware GPAs as integers and
  round-trips them to pointers (`gpa as *mut u8`) by design. This flag silences the benign
  int→ptr-cast *warning*; it does **not** weaken Miri's bounds/aliasing/UB checking.
- **Test pages are raw provenance-exposed `alloc_zeroed`, not `Box`.** A `Box`-owned page gives the
  allocation a unique owner tag that Miri's aliasing model requires every access *and the final
  dealloc* to go through — but two agents (guest transport, loopback host) reach the page through
  independently int→ptr-recovered pointers (exactly as hardware shares a physical page). Backing it
  with `alloc_zeroed` and touching it only through the exposed raw pointer models the production
  shape (raw identity-mapped RAM) and is sound under the **default** Stacked Borrows. See the `Page`
  doc comment in `tests/loopback.rs`.

**Non-vacuity proof (Miri actually catches UB).** Temporarily injecting a one-byte out-of-bounds
read past the response page into `exchange`'s copy-out step
(`let _ = core::hint::black_box(*self.resp_page.add(PAGE_SIZE));`) — behaviorally invisible — is
flagged by Miri immediately as "dangling pointer (it has no provenance)". Reverted; the committed
`exchange` reads only the in-page header and exactly `len <= PAGE_SIZE` bytes. The gate is not
vacuous.

## Deviations considered and rejected

- **MMIO doorbell instead of port-I/O** — rejected. Port-I/O is the spec's recommended default;
  MMIO is not materially cleaner (it needs a separate *unmapped* magic GPA and overloads the data
  region with MMIO semantics), so no `[question]` was raised. See "Chosen mechanism".
- **Two-exit `OUT`/`IN` doorbell (length returned by the `IN`)** — **adopted then reverted in PR
  #44 review.** It was initially chosen to keep `exchange` frame-format-agnostic, but the cross-model
  pass showed it is *not atomic*: the guest resumes between the two exits while the host owes a
  length, so a nested doorbell from an injected-interrupt handler clobbers the fixed pages and the
  pending length. Folding the length into the frame header (which the wire format already carries)
  and reading it from `RESP_GPA` after a single `OUT` restores the single-`VMCALL` atomicity. The
  small cost — `exchange` now knows the header's magic + `payload_len` offsets — is worth atomicity,
  and the `u64`-bounds-check-before-cast is *more* exercised (the header sum can exceed `u32::MAX`).
- **A `cli`/`sti` window around the two-exit exchange** (the review's fallback) — rejected in favor
  of the single-`OUT` header-length design: masking interrupts is a heavier, guest-cooperation
  requirement, whereas one exit removes the reentrancy window structurally.
- **16-bit `AX` doorbell (the spec's literal example)** — rejected in favor of 32-bit `EAX`: `EAX`
  is the natural fit for `Exit::Io { write: u32 }` carrying the request length. (The response length
  is no longer on the wire — it is read from the frame header — so its width is the header's `u32`
  `payload_len`, checked in `u64`.)
- **Renaming the crate/type to `io-transport` / `IoTransport`** — deferred per the spec ("not in
  this task; keep the package name to avoid churn"). The `vmcall`-derived names are kept; the docs
  make the port-I/O mechanism unmistakable.
- **Editing `vmm-backend` (new exit variant or removing `Exit::Hypercall`)** — rejected/unneeded:
  the doorbell maps to the existing `Exit::Io`, and `Exit::Hypercall` is already correctly
  documented as patched-backend-only. The `Backend` trait is unchanged; rule 1 keeps me out of that
  crate's source anyway.
- **Changing the toml VMCALL normative fields / `contract_hash`** — rejected: the `VMCALL`
  *instruction*'s disposition under the ratified backend is genuinely unchanged (exit-and-dispatch);
  only the *stock-serviceable* prose was wrong. Touching normative fields would change the §6
  canonical form, breaking a hash hard-coded in `vmm-core` source (out of lane), and the determinism
  note explicitly forbids the port/GPA from entering the hash. Prose-only correction instead.
- **Storing pages as `&mut [u8]` / `NonNull<[u8]>` slices** — rejected: a reference live across the
  out-of-band host write is aliasing UB. Raw `*mut u8` + pointer ops only.
- **`total as usize` then bound-check** — rejected: `total = HEADER_LEN + payload_len` can exceed
  `u32::MAX` and would truncate on narrow-`usize` targets. Check in `u64` first; cast only after it
  passes.
- **`options(nomem/readonly/pure)` / `preserves_flags` on the `asm!`** — rejected: would let the
  compiler reorder the request-page stores around the `out`, and the host may modify RFLAGS across
  the exit. Default memory semantics; `nostack` is the only option set.
- **`panic!`/`unreachable!` in the off-arch `ring`** — rejected: that path is reachable through the
  safe `exchange` API. The stub does nothing; `exchange` then finds no frame magic on the zeroed
  page and returns `HostRejected`.

## Known limitations / notes for the integrator

- The crate **assumes** the §1 host behavior; it does not implement the host-side port-I/O exit
  handler (vmm-core frontier work, out of scope). It also does not allocate/place or reserve the
  shared pages — it takes their GPAs as constructor inputs and exports `REQ_GPA`/`RESP_GPA` as the
  ABI defaults. **The loader/vmm-core must reserve the two pages** (mark `reserved` in the guest
  e820 / task-04 payload map; never place kernel/initrd/cmdline there) and identity-map them; exact
  placement may be finalized at bring-up (what the ABI pins is "two fixed, page-aligned,
  VMM-reserved, identity-mapped pages"). The caller must uphold the documented `new`/`from_gpas`/
  `with_doorbell` safety contract.
- The ABI is single-in-flight and **atomic** by construction (one `OUT` exit, no host-side pending
  state across a guest resume); no multi-request or multi-vCPU concurrency is modeled.
- **Host-side rejection contract:** to reject a doorbell, the vmm-core handler must leave the
  response page **without a valid frame** (the guest shim zeroes it before the `OUT`, so writing
  nothing suffices → magic field 0 → `HostRejected`). On success it writes a complete response
  frame via `Dispatcher::dispatch`; the shim reads the length from that frame's header.
- `hypercall-proto` is depended on **unmodified**; the only files changed outside
  `consonance/vmcall-transport/` are the three sanctioned doc surfaces (INTEGRATION.md,
  CPU-MSR-CONTRACT.md, cpu-msr-contract.toml).
