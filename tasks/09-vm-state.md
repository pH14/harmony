# Task 09 — `consonance/vm-state`: versioned `vm_state` snapshot codec

Read `tasks/00-CONVENTIONS.md` first. Touch only `consonance/vm-state/`.

## Environment

Runs on: macOS and Linux. Requires: Rust only. Does not require: `/dev/kvm`, Intel CPU,
QEMU, root. Pure-logic serialization crate — no syscalls, no ioctls, no time.

## Context

A snapshot of a running VM has two parts: the **guest memory** (owned by `snapshot-store`,
task 02, as copy-on-write page layers) and an **opaque `vm_state` blob** that captures
everything else able to influence future guest-visible behavior. `snapshot-store` already
treats that blob as `Vec<u8>` (`BaseBuilder::seal(vm_state)` / `Store::vm_state() -> &[u8]`).
This task builds the **codec for that blob**: a versioned, deterministic, round-trip-tested
binary encoding of the non-memory machine state.

Ruling **R1** (`docs/R1-DEVICE-MODEL.md` §"Consequence 1") is the prerequisite that made this
specifiable: with `KVM_IRQCHIP_NONE` and a userspace xAPIC, the device portion of the blob is a
concrete, fully VMM-owned field set with **no coupling to KVM's `kvm_lapic_state`/`kvm_irqchip`/
`kvm_pit_state2` ABI**. INTEGRATION.md §4 ("Snapshot contents checklist") and R1 §"Consequence 1"
together enumerate the exact contents.

**This crate does not touch `/dev/kvm`.** The vmm-core adapter (frontier) reads the live machine
via ioctls and **populates these plain-data structs**; this crate's job is purely to encode them
to bytes and decode back, byte-deterministically and round-trip-exactly, with a version stamp.
Per convention rule #2 it depends on **no sibling crate**: V-time, timer, hypercall, and device
state are all mirrored here as local plain-data structs / opaque byte sections, exactly as
`snapshot-store` treats `vm_state` as opaque bytes.

> **Scope note for this task (decided with the integrator):** the **device-emulation section**
> (LAPIC + PIC stub + PIT stub) is carried as an **opaque, length-delimited placeholder** for now
> — see §"Device section: placeholder" below. Sketch and lock the rest of the blob first. Task 13
> (`consonance/lapic`) defines the real `LapicState`; wiring it in is a deliberate follow-up so this
> task isn't blocked on 13's struct freezing.

## Why a versioned codec (not ad-hoc serde_json)

The blob is hashed into the determinism gate, holds a ~4 KiB binary XSAVE image, and must encode
**byte-identically for identical state across machines and toolchains**. So: an explicitly
specified little-endian binary container (house style — cf. `hypercall-proto`'s frames), not a
text format and not a format whose byte layout is an implementation detail of a third-party crate.
The format **version is part of the determinism contract**: a decoder rejects a version it does
not understand rather than silently misreading.

## Format

A **TLV (tag-length-value) container** with a fixed header:

- **Header**: magic `VM_STATE_MAGIC: u32 = 0x31534D56` (`"VMS1"` read little-endian; distinct
  from the hypercall magic `0x31504348`), then `version: u16 = VM_STATE_VERSION`, then a `u16`
  section count. All multi-byte integers little-endian.
- **Sections**: each is `tag: u16`, `len: u32`, then `len` bytes of payload. Sections are emitted
  in **ascending tag order** (deterministic); **every v1 tag is present exactly once** — a v1 blob
  carries the full field set, there are no optional sections. An unknown tag, a duplicate tag, an
  out-of-order tag, a **missing required tag** (`MissingSection`), or a `len` past end-of-buffer is
  a decode error. (Every record has a `Default`, so a decoder that tolerated a missing section would
  silently restore that machine state as zero — hence all v1 tags are required.)
- Decoding is **strict and total**: it never panics on arbitrary input (rule #4) — every
  malformed blob yields a `VmStateError`, every valid blob round-trips.

Fixed-layout records (GPRs, the segment/control block, XCR0, debug regs, the V-time block) should
use `zerocopy` POD structs with explicit `#[repr(C)]` little-endian fields; variable-length parts
(the MSR list, the XSAVE image, the timer-queue entries, the hypercall blob, the device
placeholder) are length-prefixed within their section.

## Public API

std crate (uses `Vec`/`BTreeMap`). `zerocopy` for the fixed records; no `unsafe` beyond what
`zerocopy`'s derives generate (none hand-written). No `serde` is required for the format itself.

```rust
pub const VM_STATE_MAGIC: u32 = 0x3153_4D56; // "VMS1" LE
pub const VM_STATE_VERSION: u16 = 1;

/// The complete non-memory machine snapshot. The vmm-core adapter fills this from
/// KVM ioctls + the V-time/hypercall/device subsystems; this crate encodes it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct VmState {
    pub regs:        VcpuRegs,        // KVM_GET_REGS  — GPRs, RIP, RFLAGS
    pub sregs:       VcpuSregs,       // KVM_GET_SREGS2 — segments, CRs, IA32_APIC_BASE, EFER
    pub xcrs:        Xcrs,            // KVM_GET_XCRS  — XCR0 (state image is in `xsave`, XCR0 is not)
    pub debugregs:   DebugRegs,       // KVM_GET_DEBUGREGS — DR0..3, DR6, DR7
    pub events:      VcpuEvents,      // KVM_GET_VCPU_EVENTS — pending exc/NMI/SMI, interrupt shadow
    pub mp_state:    MpState,         // KVM_GET_MP_STATE — Runnable vs Halted (HLT quiescent point)
    pub msrs:        MsrBlock,        // KVM_GET_MSRS over the contract's allow-stateful set
    pub xsave:       XsaveImage,      // KVM_GET_XSAVE2 — FPU/XSAVE state image (per contract §2 XCR0 policy)
    pub vtime:       VtimeState,      // VClock snapshot_vns + ratio config (mirror of vtime types)
    pub timers:      TimerQueueState, // absolute-V-time timer-queue contents
    pub hypercall:   Vec<u8>,         // hypercall-proto Dispatcher::save_state() bytes (opaque here)
    pub devices:     DeviceBlob,      // LAPIC + PIC + PIT — PLACEHOLDER, see below
    pub contract_hash: [u8; 32],      // SHA-256 of the ratified CPU/MSR contract this snapshot was
                                      // taken under (CPU-MSR-CONTRACT §6). Stored so the restorer can
                                      // reject a blob whose CPUID/MSR behavior has since changed —
                                      // without it, a contract change silently diverges guest state.
}

impl VmState {
    /// Encode to the versioned TLV blob. Deterministic: equal `VmState` ⇒ equal bytes.
    /// **Fallible**: rejects a `VmState` that cannot be restored exactly — currently a
    /// `VtimeState` with `ratio_den != 1` (`FractionalRatio`, INTEGRATION.md §4) — so an
    /// un-restorable blob is never produced. (Rule 3: this signature is the contract; the
    /// `FractionalRatio` gate below *requires* the `Result` — a `Vec<u8>` return could only
    /// satisfy it by panicking.)
    pub fn encode(&self) -> Result<Vec<u8>, VmStateError>;
    /// Decode a blob produced by `encode`. Strict: validates magic/version/sections.
    pub fn decode(bytes: &[u8]) -> Result<VmState, VmStateError>;
    /// The format version a blob was written with (peek without full decode).
    pub fn peek_version(bytes: &[u8]) -> Result<u16, VmStateError>;
}

// --- fixed-layout records (zerocopy POD, little-endian) ---

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VcpuRegs { pub rax: u64, pub rbx: u64, /* ...r8..r15... */ pub rsp: u64,
                      pub rbp: u64, pub rip: u64, pub rflags: u64 }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Segment { pub base: u64, pub limit: u32, pub selector: u16,
                     pub type_: u8, pub present_dpl_s: u8, pub flags: u8 /* l/db/g/avl/unusable */ }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VcpuSregs { pub cs: Segment, /* ds es fs gs ss tr ldt */ pub gdt_base: u64,
                       pub gdt_limit: u16, pub idt_base: u64, pub idt_limit: u16,
                       pub cr0: u64, pub cr2: u64, pub cr3: u64, pub cr4: u64, pub cr8: u64,
                       pub efer: u64, pub apic_base: u64 }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Xcrs { pub xcr0: u64 } // the live XCR0; guest may XSETBV within the §2-masked menu

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DebugRegs { pub db: [u64; 4], pub dr6: u64, pub dr7: u64 }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VcpuEvents { pub exception_pending: bool, pub exception_vector: u8,
                        pub exception_error_code: u32, pub nmi_pending: bool, pub smi_pending: bool,
                        pub interrupt_shadow: u8 /* STI / MOV-SS blocking bits */ }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MpState { #[default] Runnable, Halted } // KVM_MP_STATE_RUNNABLE / _HALTED

// --- variable-length sections ---

/// MSRs captured over the contract's `allow-stateful` set. `BTreeMap` keyed by MSR
/// index so iteration order (and thus encoded bytes) is deterministic (rule #4).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MsrBlock(pub alloc_btreemap_u32_u64);   // BTreeMap<u32, u64> — sorted by index

/// The XSAVE state image (header + components per the guest's XCR0). Opaque bytes
/// to this crate; length-prefixed. (Typically up to ~4 KiB for the contract's menu.)
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct XsaveImage(pub Vec<u8>);

/// Mirror of `vtime::VClockConfig` + the captured `snapshot_vns`. Plain data; this
/// crate does NOT depend on vtime. **Snapshot-bearing configs must use an integer
/// ratio** (`ratio_den == 1`) per INTEGRATION.md §4 — `encode` rejects a fractional
/// ratio with `VmStateError::FractionalRatio` so a bad blob can't be written.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VtimeState { pub ratio_num: u64, pub ratio_den: u64, pub tsc_hz: u64,
                        pub tsc_base: u64, pub snapshot_vns: u64 }

/// Timer-queue contents: absolute V-time deadlines (survive restore unchanged), each tagged
/// with its task-05 insertion sequence `seq`. Stored sorted by **(deadline_vns, seq)** — NOT by
/// token: task 05's `TimerQueue` fires same-deadline timers in **FIFO insertion order**, so the
/// snapshot must carry `seq` to reproduce it (sorting ties by token would reorder e.g. token 2
/// inserted before token 1 at the same deadline). `next_seq` is the queue's monotonic counter,
/// snapshotted so a restored queue keeps issuing non-colliding sequence numbers. Timer state is
/// therefore genuinely insertion-order-dependent — unlike the MSR `BTreeMap`, two queues built by
/// different insertion orders are *different* states and encode differently, as they must.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TimerQueueState { pub entries: Vec<TimerEntry>, pub next_seq: u64 }
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TimerEntry { pub deadline_vns: u64, pub seq: u64, pub token: u64,
                        pub period_vns: u64 /* 0 = one-shot */ }

#[derive(Clone, PartialEq, Eq, Debug)]   // no `Default`: an error enum has no meaningful default,
pub enum VmStateError {                  // and a fieldless-default-less enum can't derive it anyway
    BadMagic(u32), UnsupportedVersion(u16), Truncated, TrailingBytes,
    UnknownTag(u16), DuplicateTag(u16), SectionOrder(u16),
    MissingSection(u16),     // a required v1 section tag is absent (decode must see every v1 tag)
    FractionalRatio,         // ratio_den != 1 in a snapshot-bearing config
    InvalidField,            // e.g. MpState/MsrBlock value out of range
}
```

`alloc_btreemap_u32_u64` is shorthand for `std::collections::BTreeMap<u32, u64>` — write the real
type; it is named here only to flag the **sorted-map** requirement (never a `HashMap`, rule #4).

## Device section: placeholder (the one deferred seam)

`DeviceBlob` carries the LAPIC + PIC stub + PIT stub state. Per the scope note, encode it now as
**opaque, length-delimited bytes** so the rest of the format is locked without waiting on task 13:

```rust
/// PLACEHOLDER. Will hold `lapic::LapicState` (task 13) + PIC/PIT stub state once
/// those structs exist. For now an opaque, length-prefixed byte section so the
/// container format and version are stable. The vmm-core adapter passes through
/// whatever the device models emit; this crate does not interpret it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DeviceBlob(pub Vec<u8>); // TODO(task-13): replace with a typed { lapic, pic, pit } record
```

When task 13 lands, the typed `LapicState` is folded in (either `vm-state` gains an optional
`lapic` dependency for the `LapicState` type, or `lapic` exposes a stable byte encoding `vm-state`
nests — that choice is the follow-up, not this task). Document the placeholder honestly in
`IMPLEMENTATION.md`: the format is forward-compatible because the device section is one tag whose
internal layout can gain a typed encoding under a **bumped `VM_STATE_VERSION`** without disturbing
any other section.

## Semantics that must hold

- **Round-trip identity**: `VmState::decode(&s.encode()?) == Ok(s)` for every constructible,
  *encodable* `VmState` (one with `ratio_den == 1`; a fractional ratio is rejected at `encode`).
- **Determinism**: `s.encode() == s.encode()` always, and two `VmState`s that are `==` encode to
  identical bytes — no map-iteration-order, no padding-byte, no float nondeterminism reaches the
  output (MSRs via `BTreeMap`; timer entries in canonical `(deadline_vns, seq)` order; `zerocopy`
  records fully initialized incl. any reserved/pad bytes zeroed).
- **Strict decode**: bad magic, unknown/duplicate/out-of-order/**missing** tag, truncated section,
  trailing bytes, or an unsupported version all return the matching `VmStateError`; **no panic** on
  any input (fuzz it — see gates). `section_count` must equal the v1 tag count and **every** v1 tag
  must appear — a blob with `section_count = 0` (or any dropped section) is `MissingSection`, never
  a zero-filled best-effort restore.
- **Versioning**: `peek_version` reads the header without decoding the body; `decode` rejects
  `version != VM_STATE_VERSION` with `UnsupportedVersion` (no silent best-effort).
- **Integer-ratio invariant**: `encode` refuses a `VtimeState` with `ratio_den != 1`
  (`FractionalRatio`) — enforcing INTEGRATION.md §4 at the codec boundary so an
  un-restorable-exactly timeline can never be written. (This is why `encode` is fallible.)
- **`contract_hash` is carried, not verified here**: the codec round-trips the 32-byte
  `contract_hash` like any field; **comparing** it against the current contract on restore (and
  rejecting a mismatch) is vmm-core's job, the same division of labor as the quiescent-point
  assertion below. The field merely guarantees the value is *present* in every blob.
- **Quiescent-point invariant is the *caller's* (assert, don't serialize)**: there is no
  armed-but-unfired injection-plan field — INTEGRATION.md §4 says vmm-core only snapshots at a
  quiescent point and enforces it with an assertion. Document that this crate deliberately has no
  such field.

## Acceptance gates

Beyond the standard gates (build/nextest/clippy `-D warnings`/fmt/deny):

1. **Round-trip proptest (core gate)**: `proptest`-generate arbitrary `VmState` (random GPRs,
   segments, CRs, an arbitrary `allow-stateful` MSR map, a random-length XSAVE image, random timer
   entries with distinct `(deadline_vns, seq)`, an integer-ratio `VtimeState`, a random 32-byte
   `contract_hash`, a random device placeholder, random hypercall bytes) ⇒
   `decode(&encode(s).unwrap()) == Ok(s)`. ≥ 256 cases.
2. **Determinism test**: `encode(s).unwrap()` twice ⇒ identical bytes; build two `==` `VmState`s by
   inserting the **MSR map** in different orders ⇒ identical bytes (the `BTreeMap` canonicalizes).
   Timer entries are insertion-order-dependent by design — assert they encode in canonical
   `(deadline_vns, seq)` order and that two queues with the same fire-order are byte-identical.
3. **Strict-decode / fuzz-robustness test**: feed truncations, bit-flips, bad magic, wrong version,
   duplicated/reordered tags, a **dropped required section** (and `section_count = 0`), and oversized
   `len` fields ⇒ each yields the right `VmStateError` (`MissingSection` for a dropped tag), never a
   panic. Drive with `proptest` over arbitrary byte vectors **and** over mutated valid blobs (the
   `arbitrary` dev-dep is whitelisted for this).
4. **Golden-stability test**: a fixed, fully-populated `VmState` encodes to a **recorded byte
   vector** checked into the test (hex or a `blake3` digest). This catches accidental format drift
   — any layout change must consciously update the golden **and** bump `VM_STATE_VERSION`.
5. **Version-rejection test**: a blob whose header version is `VM_STATE_VERSION + 1` decodes to
   `UnsupportedVersion`; `peek_version` still returns it.
6. **Integer-ratio rejection test**: a `VmState` with `ratio_den = 2` fails `encode` with
   `FractionalRatio`.

Property tests ≥ 256 cases; keep total `cargo test` under ~3 minutes.

## Non-goals

Reading `/dev/kvm` / issuing ioctls (frontier `vmm-core` fills the structs); guest *memory* (owned
by `snapshot-store`); compression/persistence across process restarts; the real typed device
encoding (placeholder now — task 13 follow-up); deciding *which* MSRs are in the `allow-stateful`
set (that's `docs/CPU-MSR-CONTRACT.md`; this crate encodes whatever map it's handed); cross-version
migration of old blobs (strict same-version decode is the v1 contract). Do not depend on `vtime`,
`hypercall-proto`, `snapshot-store`, or `lapic` — mirror their state as local plain data (rule #2).
