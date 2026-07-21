# Task 15 — `consonance/vmm-core`: the deterministic VMM skeleton (loader + entry-state + device shims + event loop + M1/M2 gates)

Read `tasks/00-CONVENTIONS.md` first. Touch only `consonance/vmm-core/`. This task is spun out of
`docs/BRINGUP.md` ("Bring-up sequence" steps 3–6 and the M1/M2 milestone); BRINGUP is the
primary source and this spec implements it without renegotiating it. It also implements, and
does not negotiate with, `docs/R-BACKEND.md`, `docs/CPU-MSR-CONTRACT.md`, and
`docs/R1-DEVICE-MODEL.md`. Where this spec and those docs disagree, **those docs win** — raise a
`[question]`, do not resolve silently.

`vmm-core` is the deterministic VMM that sits **above** the `Backend` trait (task 14,
`consonance/vmm-backend`) and **compiles against that trait alone**. It is the Multiboot loader, the
32-bit-protected-mode entry-state setup, the guest memory map, the CPUID/MSR-filter **policy**
(data from `docs/CPU-MSR-CONTRACT.md`), the bring-up device shims (8250 UART, isa-debug-exit),
and the **event loop**. The event loop drives the vCPU **only** through `Backend::run()` /
`run_until()` and matches on the returned `Exit`; it **never issues `KVM_RUN` itself**. `KVM_RUN`
is a KVM-specific ioctl that lives **inside `KvmBackend::run()`** in `vmm-backend`, *below* the
trait — that is precisely what makes "nothing above the trait branches on the impl" literally
true. The **one** place a concrete backend is named is the binary's composition root
(`fn main` selects `KvmBackend` vs `PatchedKvmBackend` and injects it). Get this seam right: any
`#[cfg]` on a backend impl, any `KVM_*` constant, or any `kvm-ioctls`/`kvm-bindings` import inside
the `vmm-core` **library** is a layering bug.

## Environment

> *Requires: Linux bare-metal Intel x86-64 with VMX, `/dev/kvm`, and `perf_event` access; does
> not run on macOS or under nested virtualization. The pure-logic portions (loader, UART model,
> contract policy, event loop) are Mac-unit-testable against a mock `Backend`; live `KVM_RUN`
> (instantiating `KvmBackend` and running it) is **box-only**.*

- **Pure-logic portions** (the bulk of the library): build, lint, and unit-test on **macOS and
  Linux** with no `/dev/kvm` — driven against a **mock `Backend`** (see "Mock-backend testing").
  These obey conventions rule 6 portability in full.
- **Live run** (M1, M2): **Linux bare-metal Intel only** — the determinism box, reached as
  `ssh <det-box>`. Nested virtualization does **not** satisfy this. The vCPU thread is
  **CPU-pinned** per `docs/BOX-PINNING.md` (dedicated physical core, SMT sibling idle) — every
  determinism run on the box obeys that rule. macOS is your terminal/editor only; the live gates
  build and run on the box. Live tests are `#[cfg(target_os = "linux")]` integration tests
  (`consonance/vmm-core/tests/`), gated to skip-with-a-loud-message on a host lacking `/dev/kvm`
  (never silently pass).

## Context — where this sits in the bring-up

`docs/BRINGUP.md` splits the frontier into two crates: `vmm-backend` (task 14 — the trap
apparatus: the `Backend` trait + `Exit`/`Event`/`VcpuState` value types + `KvmBackend`) and
`vmm-core` (this task — everything above the trait). **Task 14 leads task 15**: this crate
depends on `vmm-backend` for the trait and its value types (a declared exception to conventions
rule 2 — see "Dependencies, grants, and the rule-2/rule-5 exceptions").

The milestone (BRINGUP "stretch into determinism"):

- **M1 — boots & prints.** Flat-load the task-04 `hello` payload, run it on `KvmBackend`, and
  reproduce `consonance/acceptance-suite/golden/hello.txt` byte-for-byte over the emulated serial port, then exit
  clean (a clean isa-debug-exit `PASS`).
- **M2 — deterministic twice.** Drive `hello` **and** `compute` through the `unison`
  `Machine`/`MachineFactory` adapter; the canonical `state_hash` over **all observable state**
  is identical across two runs of each. These two payloads are **RDTSC/RDRAND-free by audit**, so
  they meet the gate on **stock `KvmBackend`** — no kernel patch needed (M3 / `PatchedKvmBackend`
  is a parallel track, out of scope here).

Prior art to crib from (read-only, not a dependency): `preestablished/determinism-hypervisor`,
a working stock-KVM deterministic VMM (see the `prior-art-det-hypervisors` memory).

## The entry contract (task 04 — replicate QEMU `-kernel`)

Verified against `consonance/acceptance-suite/payloads/common/src/boot.s` and `consonance/acceptance-suite/payloads/linker.ld`. The loader
must reproduce exactly this handoff (BRINGUP "The entry contract"):

| What | Value |
|---|---|
| Load image | Flat-load the Multiboot ELF **honoring the file offset**. The loadable segment is **not** at file offset 0 (the ELF/program headers precede it), so copy from the PT_LOAD's `p_offset` — **`p_offset = 0x1000`** for the current payloads (the Multiboot header sits at file offset `0x1000`). Use the Multiboot **address-override** formula `file_off = mb_header_file_offset − (header_addr − load_addr)`. For these payloads `header_addr == load_addr == 0x100000`, so the `(header_addr − load_addr)` term is `0` and `file_off = 0x1000`. **Do not** drop the `mb_header_file_offset` term and use `header_addr − load_addr` alone (that gives `0` — the bug), and **do not** copy from the start of the file or treat `load_addr` as a file offset. Copy `load_end_addr − load_addr` bytes into GPA `load_addr = 0x10_0000` (1 MiB), then **zero BSS** from `load_end_addr` up to `bss_end_addr`. |
| Entry point | `entry_addr` (= `_start`, in the 1 MiB load region); set `RIP`/`EIP` there. |
| CPU mode | **32-bit protected mode**, paging **off** (`CR0.PE=1`, `CR0.PG=0`), A20 on. |
| Segments | flat 32-bit **CS** (base 0, limit 4 GiB, code/exec-read, DPL 0) + flat **DS/ES/SS/FS/GS** (base 0, limit 4 GiB, data/read-write). Granularity 4 KiB, `D/B=1` (32-bit). |
| GPRs | `EAX = 0x2BADB002` (the Multiboot **bootloader** magic the loader passes at entry — **not** `0x1BADB002`, which is the *header* magic embedded in the image, value `MB_MAGIC` in `boot.s`); `EBX` → a **minimal Multiboot info struct** in guest RAM (the shim doesn't read it, but set a valid non-null pointer into mapped RAM); `EFLAGS = 0x0000_0002` (reserved bit 1 set, **`IF = 0`**). Other GPRs `0`. |
| Console | polled **8250 UART, port `0x3F8`** (115200 8N1). Guest spins on LSR (`0x3FD`) for THR-empty, then writes bytes to THR (`0x3F8`). |
| Halt/exit | write `u8` to port **`0xF4`** (isa-debug-exit): `0` = PASS, `1` = FAIL; falls back to a `hlt` loop if absent. |
| Oracle | `consonance/acceptance-suite/golden/<name>.txt` — byte-exact expected serial output. |

The shim itself enables PAE/long-mode and loads a 64-bit GDT *after* entry (`boot.s`), so the
host only nails the **Multiboot 32-bit-PM handoff** — nothing more. (Header magic in `boot.s` is
`0x1BADB002`; the loader passes `0x2BADB002` in `EAX` — getting these two swapped is the classic
bug the loader test must catch.)

## Public API (contract — exact names, types, semantics)

`std` crate (this crate is host-side and box-only at runtime; it is not `no_std`). All items below
are `pub` at the named module path. Signatures are the contract (conventions rule 3); you may add
private items, helper methods, and `Debug`/`Clone` derives that do not change meaning. Types owned
by task 14 are referenced as `vmm_backend::{Backend, Exit, VcpuState, Gpa, Event, CpuidModel,
CpuidEntry, MsrFilter, MsrRange, ExitCounts, BackendError}` and **must not be redefined here**.

### `multiboot` — loader (pure logic, Mac-testable, trust boundary)

```rust
/// The Multiboot v1 **header** magic embedded in the payload image (`boot.s` `MB_MAGIC`).
pub const MULTIBOOT_HEADER_MAGIC: u32 = 0x1BAD_B002;
/// The Multiboot v1 **bootloader** magic the loader passes to the guest in EAX at entry.
pub const MULTIBOOT_BOOTLOADER_MAGIC: u32 = 0x2BAD_B002;
/// Guest-physical load address of the payload (1 MiB) — `linker.ld` `. = 1M`.
pub const PAYLOAD_LOAD_GPA: u32 = 0x0010_0000;
/// Max bytes scanned for the Multiboot header (Multiboot v1 requires it in the first 8 KiB).
pub const MULTIBOOT_SEARCH_LEN: usize = 8192;

/// The address-override fields parsed out of the Multiboot header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MultibootHeader {
    pub header_file_offset: u32, // offset of the magic within the image file
    pub header_addr: u32,
    pub load_addr: u32,
    pub load_end_addr: u32,
    pub bss_end_addr: u32,
    pub entry_addr: u32,
}

/// Result of flat-loading a payload into a guest-RAM slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoadedImage {
    pub entry_addr: u32,
    pub load_addr: u32,
    pub load_end_addr: u32,
    pub bss_end_addr: u32,
}

/// Errors the loader returns instead of panicking. The image is **untrusted input**
/// (conventions rule 4 / no-panic-on-untrusted-input): every malformed image yields one of
/// these, never a panic, slice-index OOB, or arithmetic overflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LoadError {
    #[error("no Multiboot v1 header (magic) found in the first {MULTIBOOT_SEARCH_LEN} bytes")]
    NoHeader,
    #[error("Multiboot header checksum invalid")]
    BadChecksum,
    #[error("Multiboot header lacks the address-override flag (bit 16)")]
    NoAddressOverride,
    #[error("address fields inconsistent (load_end < load_addr, or bss_end < load_end)")]
    BadAddressFields,
    #[error("computed file offset or load span exceeds the image")]
    ImageTooSmall,
    #[error("load/bss region [{0:#x}..{1:#x}) does not fit in guest RAM")]
    OutOfRange(u64, u64),
}

/// Locate and parse the Multiboot v1 header: scan the first `MULTIBOOT_SEARCH_LEN` bytes at
/// 4-byte alignment for `MULTIBOOT_HEADER_MAGIC`, verify `magic + flags + checksum == 0 (mod
/// 2^32)`, and require the address-override flag (bit 16). Returns the parsed fields or a
/// `LoadError`. Pure; never panics on arbitrary bytes.
pub fn parse_header(image: &[u8]) -> Result<MultibootHeader, LoadError>;

/// Flat-load `image` into `guest_ram` (the host-side backing for GPA `0`):
/// 1. `parse_header`;
/// 2. `file_off = header_file_offset − (header_addr − load_addr)` (checked; underflow ⇒ error);
/// 3. copy `image[file_off .. file_off + (load_end_addr − load_addr)]` into
///    `guest_ram[load_addr .. load_end_addr]`;
/// 4. zero `guest_ram[load_end_addr .. bss_end_addr]`.
/// All indexing is bounds-checked against both `image` and `guest_ram`; any overflow is the
/// corresponding `LoadError`. Returns the `LoadedImage` describing the placement.
pub fn load(image: &[u8], guest_ram: &mut [u8]) -> Result<LoadedImage, LoadError>;
```

### `entry` — 32-bit-PM entry state (pure logic, Mac-testable)

```rust
/// Build the architectural entry state for the Multiboot 32-bit-PM handoff as a
/// `vmm_backend::VcpuState`: flat CS/DS/ES/SS/FS/GS, `CR0.PE=1` / `CR0.PG=0`, A20 on,
/// `RIP = entry_addr`, `EAX = MULTIBOOT_BOOTLOADER_MAGIC`, `EBX = mbi_gpa`,
/// `EFLAGS = 0x2` (IF cleared), all other GPRs `0`. The returned state is handed to
/// `Backend::restore()` before the first run.
pub fn protected_mode_entry(entry_addr: u32, mbi_gpa: u32) -> vmm_backend::VcpuState;

/// Guest-physical address of the minimal Multiboot info struct (a fixed low-RAM page outside
/// the payload load region and the boot page-tables; below 1 MiB). Documented constant.
pub const BOOT_INFO_GPA: u32 = 0x0000_9000;

/// Write a minimal Multiboot info struct (`flags = 0`; the rest zeroed) into `guest_ram` at
/// `BOOT_INFO_GPA` and return that GPA for `EBX`. The task-04 shims do not read it; this only
/// guarantees `EBX` points at valid, mapped, zeroed RAM. Errors if it would not fit.
pub fn write_boot_info(guest_ram: &mut [u8]) -> Result<u32, LoadError>;
```

### `contract` — CPUID model + MSR-filter policy (pure logic, Mac-testable)

The **data** comes from `docs/CPU-MSR-CONTRACT.md` / its canonical mirror
`docs/cpu-msr-contract.toml`. `vmm-core` owns the **policy** (the *what*); the install mechanism
(`KVM_SET_CPUID2`, `KVM_X86_SET_MSR_FILTER`, `KVM_CAP_X86_USER_SPACE_MSR`) is KVM-specific and
lives **below the trait** in `vmm-backend` — `vmm-core` produces backend-agnostic policy values
and the composition root hands them to the backend (see "What this needs from task 14").

```rust
/// The mask `vmm-backend` must enable on `KVM_CAP_X86_USER_SPACE_MSR` **before installing the
/// MSR filter** (CPU-MSR-CONTRACT §1; api.rst §4.97 ordering): FILTER | UNKNOWN | INVAL. Enabling
/// the cap first is load-bearing — otherwise a denied/unknown/invalid MSR becomes a silent
/// in-kernel #GP instead of a loud `KVM_EXIT_X86_RDMSR/WRMSR`. These are the contract's bit
/// values, named here as policy; the backend maps them onto the `KVM_MSR_EXIT_REASON_*` constants.
pub const USER_SPACE_MSR_MASK: u64 = MSR_EXIT_REASON_FILTER
    | MSR_EXIT_REASON_UNKNOWN
    | MSR_EXIT_REASON_INVAL;
pub const MSR_EXIT_REASON_FILTER:  u64 = 1 << 0;
pub const MSR_EXIT_REASON_UNKNOWN: u64 = 1 << 1;
pub const MSR_EXIT_REASON_INVAL:   u64 = 1 << 2;

/// The frozen CPUID model from §2 of the contract, in canonical (leaf, subleaf) order, as the
/// backend-owned `vmm_backend::CpuidModel` (task 14 — `CpuidEntry` is **not** redefined here) so
/// it feeds straight into `Backend::set_cpuid`. Installed once via `KVM_SET_CPUID2` so CPUID is
/// answered **in-kernel** from this model (no host leaves are ever inherited). Masks
/// `X86_FEATURE_X2APIC` (CPUID.1:ECX[21]) and the TSC-deadline bit (CPUID.1:ECX[24]) and hides all
/// PV leaves (`0x4000_00xx`) and the vPMU, per R1.
///
/// This is the **frozen base only**. The contract has three **dynamic** cells that are a function
/// of live guest state, not constants: `CPUID.1:ECX[27]` (OSXSAVE) mirrors `CR4.OSXSAVE`; leaf
/// `0xB`/`0x1F` `ECX[7:0]` echoes the input subleaf and `ECX[15:8]` the level type; leaf
/// `0xD.0:EBX` is the XSAVE-area size for the live `XCR0`. For **stock `KvmBackend`** KVM applies
/// these **in-kernel** (`arch/x86/kvm/cpuid.c`), so the frozen table is correct and no CPUID exit
/// fires. A backend that surfaces a userspace `Exit::Cpuid` (patched/direct) MUST overlay them
/// from guest state via `resolve_cpuid` — a fixed table alone returns stale OSXSAVE/level/`XCR0`
/// cells once the guest writes `CR4`/`XCR0`.
pub fn cpuid_model() -> vmm_backend::CpuidModel;

/// Overlay the three dynamic CPUID cells (see `cpuid_model`) onto the frozen `base` entry when
/// servicing a userspace `Exit::Cpuid`, from the guest's live `CR4`/`XCR0` (`base.leaf`/
/// `base.subleaf` select which rule applies). Never called for stock `KvmBackend` (CPUID is
/// in-kernel); it exists so the patched/direct path stays contract-correct. Pure.
pub fn resolve_cpuid(base: vmm_backend::CpuidEntry, cr4: u64, xcr0: u64) -> vmm_backend::CpuidEntry;

/// Per-direction disposition of an MSR access (the §3 vocabulary the skeleton needs).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MsrDisposition {
    /// Architecturally guest-writable; KVM virtualizes it — placed in the filter **allow** set,
    /// so it is serviced in-kernel and never reaches a userspace exit.
    AllowStateful,
    /// Read returns this constant (read-only rows); write is denied.
    AllowFixed(u64),
    /// `emulate-vtime` rows (CPU-MSR-CONTRACT §3): `MSR_IA32_TSC` (0x10) and `MSR_IA32_TSC_ADJUST`
    /// (0x3b), read **and** write. A read resolves to `VClock::tsc(work)`; a write rebases
    /// `tsc_base`/`TSC_ADJUST` — serviced from **V-time**, never the host counter. V-time is not
    /// wired in this skeleton (a later phase), so the disposition is **represented faithfully**
    /// here, but an actual `0x10`/`0x3b` access is a loud `ContractViolation` until V-time lands;
    /// the audited M1/M2 payloads touch neither, so it never fires for them. (Folding these into
    /// `AllowFixed`/`DenyGp` would silently break the contract — hence the explicit variant.)
    EmulateVtime,
    /// Trapped, logged loudly, then #GP injected.
    DenyGp,
    /// Write dropped after a loud log (never silent); read side is never this.
    DenyIgnoreWrite,
}

/// The MSR-filter allow set: exactly the `allow-stateful` rows — the only MSRs KVM keeps
/// servicing in-kernel — as the backend-owned `vmm_backend::MsrFilter` (task 14, not redefined
/// here) so it feeds straight into `Backend::set_msr_filter`. The allow-stateful rows are
/// guest-writable AND KVM-virtualized, hence **bidirectional** (read+write in-kernel); every other
/// disposition (`AllowFixed`/`EmulateVtime`/`DenyGp`/`DenyIgnoreWrite`) is **left out on purpose**
/// so the access surfaces to a userspace `Exit::Rdmsr`/`Wrmsr`. Ranges are canonical, sorted, and
/// non-overlapping; the backend installs them under `KVM_MSR_FILTER_DEFAULT_DENY` with both the
/// READ and WRITE flags (well within KVM's 16-ranges-per-direction limit).
pub fn msr_filter_allow() -> vmm_backend::MsrFilter;

/// Compute the contractual disposition of a guest read of `index` (default `DenyGp`).
pub fn rdmsr_disposition(index: u32) -> MsrDisposition;
/// Compute the contractual disposition of a guest write of `value` to `index` (default `DenyGp`).
pub fn wrmsr_disposition(index: u32, value: u64) -> MsrDisposition;

/// SHA-256 of the canonical serialized contract this policy was built from (§6 `contract_hash`).
/// A gate asserts it equals the hash in `docs/cpu-msr-contract.toml` so policy can never drift
/// from the ratified contract. **Prerequisite ([question] if absent):** that gate needs the §6
/// `(contract-version, body-hash)` registry to actually carry a `contract_hash` in
/// `docs/cpu-msr-contract.toml` — CPU-MSR-CONTRACT §6 notes the registry may not be committed yet.
/// If the field is missing when this task is implemented, raise a `[question]` and treat committing
/// it as a prerequisite; **do not invent an unratified hash**.
pub fn contract_hash() -> [u8; 32];
```

### `devices` — bring-up device shims (pure logic, Mac-testable)

```rust
pub const UART_PORT_BASE: u16 = 0x3F8;   // 8250 base: THR/RBR when LCR.DLAB=0, DLL when DLAB=1
pub const UART_PORT_LCR:  u16 = 0x3FB;   // line control register (bit 7 = DLAB)
pub const UART_PORT_LSR:  u16 = 0x3FD;   // line status register
pub const ISA_DEBUG_EXIT_PORT: u16 = 0x00F4;
/// `LCR.DLAB` (bit 7). When set, `UART_PORT_BASE` (and `+1`) address the divisor latch (DLL/DLM),
/// **not** THR/RBR/IER. The model must track it.
pub const UART_LCR_DLAB: u8 = 0x80;
/// LSR value reported on read: THR-empty + transmitter-empty (bits 5 and 6) so the guest's
/// polled-write loop always makes progress. No data-ready bit (we never feed input).
pub const UART_LSR_THR_EMPTY: u8 = 0x60;

/// Minimal 8250: accepts init writes (IER/FCR/LCR/MCR/divisor) without modeling baud; LSR reads
/// return `UART_LSR_THR_EMPTY`. It **tracks `LCR.DLAB`** (port `UART_PORT_LCR`, bit 7): a write to
/// `UART_PORT_BASE` is appended to the capture buffer **only when DLAB is clear** (a real THR
/// transmit). With DLAB set, that port is the divisor-latch-low byte — task-04's UART init sets
/// DLAB and writes the `0x01` baud divisor to `0x3F8`, which is **not** serial output; capturing
/// it would prepend a stray `\x01` and fail the M1 golden byte-for-byte. Pure; no I/O.
pub struct Uart8250 { /* private: capture buffer + LCR/DLAB + benign register shadows */ }
impl Uart8250 {
    pub fn new() -> Self;
    /// Service a guest port write (incl. `LCR`, which updates DLAB). Returns whether the port
    /// belonged to this device. A `UART_PORT_BASE` write appends to `capture()` **only if
    /// `LCR.DLAB == 0`**; with DLAB set it is the divisor latch — shadowed, not captured.
    pub fn write(&mut self, port: u16, value: u8) -> bool;
    /// Service a guest port read; `Some(byte)` if this device owns `port`, else `None`
    /// (LSR → `UART_LSR_THR_EMPTY`; `UART_PORT_BASE` with DLAB set → the divisor-latch shadow).
    pub fn read(&self, port: u16) -> Option<u8>;
    /// The bytes written to THR (DLAB clear) so far — the serial capture buffer, in order.
    pub fn capture(&self) -> &[u8];
}
```

### `vmm` — the event loop (drives the `Backend` trait only)

```rust
/// Why a run stopped. M1 requires `DebugExit { code: 0 }` specifically — **not** `Hlt`
/// (the payload's fallback) and **not** a non-zero code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalReason {
    /// isa-debug-exit (`0xF4`) wrote `code`. PASS = 0, FAIL = 1.
    DebugExit { code: u8 },
    /// `HLT` (the payload's fallback when isa-debug-exit is absent) — terminal here.
    Hlt,
    /// Backend `Shutdown` (triple fault / explicit shutdown).
    Shutdown,
}

/// Errors that abort a run. A `ContractViolation` is the default-deny posture made loud: an exit
/// the skeleton does not model (an unmodeled port/MMIO/hypercall, a backend-dependent
/// RDTSC/RDRAND on a non-deterministic-claiming backend) fails closed here — never silently.
#[derive(Debug, thiserror::Error)]
pub enum VmmError {
    #[error("backend error")] Backend(#[from] vmm_backend::BackendError),
    #[error("load error")] Load(#[from] crate::multiboot::LoadError),
    #[error("contract violation: {0}")] ContractViolation(String),
}

/// One serviced exit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step { Continued, Terminal(TerminalReason) }

/// What a completed run produced (and what the M2 hash is taken over).
pub struct RunResult {
    pub reason: TerminalReason,
    pub serial: Vec<u8>,
    /// Per-exit-reason counts read from the backend (R-Backend normative observability),
    /// surfaced for the unison report. The backend-owned type (task 14), not redefined here.
    pub exit_counts: vmm_backend::ExitCounts,
}

/// Owned, pinned host backing for guest RAM. The backend registers a pointer **into this buffer**
/// via the `unsafe Backend::map_memory` (task 14), and `Vmm` owns it so the backing **outlives
/// every `run`** (the backend's safety precondition) and `state_blob` can re-read materialized
/// memory for the M2 hash. Allocated once and never reallocated after mapping — a `Vec<u8>` heap
/// allocation (mock/Mac) or an `mmap` region (box) keeps its address across a move of the owner,
/// so moving the `GuestRam` into `Vmm` does not invalidate the mapped pointer.
pub struct GuestRam { /* private: Vec<u8> (mock/Mac) or mmap-backed region (box) */ }
impl GuestRam {
    /// Allocate `len` bytes (a multiple of 4 KiB) of zeroed, pinned backing.
    pub fn new(len: usize) -> Result<Self, VmmError>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    /// The materialized guest bytes — read by `state_blob` for the M2 hash.
    pub fn as_bytes(&self) -> &[u8];
    /// Mutable view for the loader / `write_boot_info` / `map_memory` (before the first run).
    pub fn as_mut_bytes(&mut self) -> &mut [u8];
}

/// The deterministic VMM. Generic over `B: Backend`, so the same loop runs against `KvmBackend`
/// on the box and the mock backend in unit tests. **No method here mentions a concrete backend.**
pub struct Vmm<B: vmm_backend::Backend> { /* backend, owned GuestRam, devices, serial, terminal */ }

impl<B: vmm_backend::Backend> Vmm<B> {
    /// Construct over an already-configured backend (CPUID/MSR-filter installed, entry state
    /// restored) **and the `GuestRam` it owns** — `bringup::boot` has already mapped that backing
    /// into the backend via `unsafe map_memory`, and `Vmm` now holds it so the backing outlives
    /// every `run` and `state_blob` can hash materialized memory. (Taking only a length, as an
    /// earlier draft did, left the backing unowned: the live KVM memslot would dangle and
    /// `state_blob` would have nothing to read.)
    pub fn new(backend: B, guest_ram: GuestRam) -> Self;

    /// Drive the vCPU for exactly one exit: `backend.run()`, then dispatch the returned `Exit`:
    /// UART/​debug-exit ports → `devices`; `Rdmsr`/`Wrmsr` → `contract` disposition; `Cpuid` →
    /// frozen model (normally answered in-kernel; a userspace `Cpuid` exit is serviced from the
    /// model); `Hlt`/`Shutdown` → terminal; any unmodeled exit → `ContractViolation` (loud).
    /// Data-returning exits (port/MMIO read, `Rdmsr`, `Cpuid`) are resolved back to the backend
    /// (see "What this needs from task 14").
    pub fn step(&mut self) -> Result<Step, VmmError>;

    /// `step()` to a `Terminal`. Returns the serial capture, terminal reason, and exit counts.
    pub fn run(&mut self) -> Result<RunResult, VmmError>;

    /// Canonical, length-prefixed, domain-tagged serialization of **all observable state**:
    /// materialized guest memory ‖ `Backend::save()` (`VcpuState`) ‖ serial capture buffer ‖
    /// device + terminal state (UART register shadows, isa-debug-exit code, `TerminalReason`).
    /// Pure (no `HashMap` iteration into bytes, no float, no wall-clock); calling it twice is
    /// identical. **Superseded by task 09's `vm_state` codec when it lands** (BRINGUP) — at which
    /// point the ad-hoc encoding is replaced and device state folds into that blob.
    pub fn state_blob(&self) -> Vec<u8>;
    /// `sha256(state_blob())` — the M2 determinism hash and the unison `state_hash`.
    pub fn state_hash(&self) -> [u8; 32];
}
```

### `bringup` — composition helpers (the live-run wiring)

A thin layer that allocates the owned `GuestRam`, installs the `contract` policy on the backend,
runs the loader + `write_boot_info`, builds + restores the entry state, maps the RAM, and returns a
`Vmm<B>` ready to `run()`. It takes the `Backend` **by value** (constructed bare at the composition
root — `KvmBackend::new()` with no policy) so the **only** place naming a concrete backend is the
binary's `fn main`; policy goes in **through the trait** (`set_cpuid`/`set_msr_filter`), not a
concrete constructor. Order is load-bearing:

1. `backend.set_cpuid(&contract::cpuid_model())`, then `set_msr_filter(&contract::msr_filter_allow())`
   (the backend enables the `USER_SPACE_MSR_MASK` cap first) — **before** the first run (task 14 §config);
2. `let mut ram = GuestRam::new(guest_ram_len)?;` then `multiboot::load(payload, ram.as_mut_bytes())`
   and `entry::write_boot_info(ram.as_mut_bytes())`;
3. `unsafe { backend.map_memory(Gpa(0), ram.as_mut_bytes())? }` — the backend now retains a pointer
   into `ram` (its `# Safety` contract); `ram` moves into `Vmm` in step 5 and its heap does not
   move, so the pointer stays valid for the backend's lifetime;
4. `backend.restore(&entry::protected_mode_entry(loaded.entry_addr, BOOT_INFO_GPA))?`;
5. `Ok(Vmm::new(backend, ram))`.

Exact signature is the implementer's, e.g.:

```rust
/// Allocate `guest_ram_len` of owned host-backed guest RAM, install the contract policy via the
/// trait, flat-load `payload`, write the boot-info struct, build + restore the 32-bit-PM entry
/// state, `unsafe`-map the RAM, and return a ready `Vmm` that owns the backing. Pure-logic except
/// `backend` calls — drivable against the mock backend.
pub fn boot<B: vmm_backend::Backend>(backend: B, payload: &[u8], guest_ram_len: usize)
    -> Result<Vmm<B>, VmmError>;
```

You may add private items and helpers freely; do not rename or re-sign anything above.

## Event-loop dispatch (normative)

For each `Exit` from `Backend::run()`:

| `Exit` | Disposition |
|---|---|
| `Io { port: 0x3F8..=0x3FF, write: Some(b) }` | `Uart8250::write(port, b)` — the device tracks `LCR` (0x3FB) DLAB and appends to the capture buffer **only** for a `0x3F8` write with `DLAB == 0` (a THR transmit). A `0x3F8` write with DLAB set is the divisor latch (task-04 init's `0x01`) → shadowed, **not** captured; other init writes (IER/FCR/LCR/MCR) update benign shadows. |
| `Io { port: 0x3F8 or 0x3FD, write: None }` | `Uart8250::read(port)` → LSR = `UART_LSR_THR_EMPTY`; `0x3F8` with DLAB set = divisor-latch shadow. Resolve the value back to the backend. |
| `Io { port: 0xF4, write: Some(c) }` | isa-debug-exit → `Terminal(DebugExit { code: c })`. |
| `Io { other port }` | **Not modeled by M1/M2 payloads** → loud `ContractViolation` (default-deny; do **not** silently return 0/0xFF). PIC/PIT/CMOS ports arrive with their payloads (`interrupts`, later) — not this milestone. |
| `Mmio { .. }` | xAPIC page (R1) is deferred to task 13; M1/M2 payloads touch no MMIO → loud `ContractViolation`. |
| `Hypercall { .. }` | VMCALL host handler (INTEGRATION.md §1) is deferred; M1/M2 payloads issue none → loud `ContractViolation`. |
| `Cpuid { leaf, subleaf }` | Answer from `contract::cpuid_model()` (normally pre-installed via `KVM_SET_CPUID2`, so on stock `KvmBackend` this exit does not fire). If a backend surfaces it: look up the frozen entry **and overlay the dynamic cells** via `contract::resolve_cpuid(entry, cr4, xcr0)` — never from host CPUID. |
| `Rdmsr { index }` | `contract::rdmsr_disposition(index)` → `AllowFixed` returns its constant; `DenyGp` injects #GP; `EmulateVtime` (0x10/0x3b) is a loud `ContractViolation` until V-time lands (audited M1/M2 payloads touch neither). Log loudly **before** any effect. |
| `Wrmsr { index, value }` | `contract::wrmsr_disposition(index, value)` → `DenyIgnoreWrite` drops-with-log; `DenyGp` injects #GP; `EmulateVtime` is a loud `ContractViolation` (as above). Log loudly first. |
| `Rdtsc` / `Rdtscp` / `Rdrand { .. }` / `Rdseed { .. }` | Backend-dependent (contract §1 — **all four** stock-KVM holes: RDTSC/RDTSCP and RDRAND/RDSEED). On `KvmBackend` none surface (M2 payloads are RDTSC/RDTSCP/RDRAND/RDSEED-free by audit); should any ever surface (a patched backend, no V-time wired yet), it is a loud `ContractViolation` here — M3 routes them to V-time / seeded PRNG, out of scope. |
| `Hlt` | `Terminal(Hlt)`. |
| `Shutdown` | `Terminal(Shutdown)`. |

"Loud" = a host-side log line (direction, port/index, value, guest RIP, exit reason, disposition)
emitted **before** any architectural effect, per CPU-MSR-CONTRACT §1's loud-event policy. Logging
is host-side only and never perturbs guest-visible state.

## What this needs from task 14 (`vmm-backend`) — the trait surface relied on

Task 14 (`tasks/14-backend.md`) is now concrete; this event loop is written against its API. The
three things this loop needs, and where task 14 provides them (flag any divergence as a
`[question]` rather than reaching below the trait):

1. **Install-time policy application — via trait methods.** Task 14 puts `set_cpuid(&CpuidModel)`
   and `set_msr_filter(&MsrFilter)` **on the `Backend` trait** (the impl enables
   `KVM_CAP_X86_USER_SPACE_MSR` with the `FILTER|UNKNOWN|INVAL` mask before `KVM_X86_SET_MSR_FILTER`,
   and `KVM_SET_CPUID2`, all below the trait). `vmm-core` supplies the data
   (`contract::cpuid_model() -> vmm_backend::CpuidModel`, `contract::msr_filter_allow() ->
   vmm_backend::MsrFilter`) and `bringup::boot` calls the trait methods **before the first run** —
   so policy stays **above** the trait and `vmm-core` never names a KVM ioctl. (This supersedes an
   earlier draft's "constructor injection" recommendation: task 14 chose trait methods, which keep
   `vmm-core` impl-agnostic without the composition root having to carry the policy.)
2. **Exit completion — the read/MSR/hypercall round-trip.** Task 14 exposes `complete_read(value)`
   (IO/MMIO/MSR and instruction-read exits), `complete_fault()` (`deny-gp` → inject #GP), and
   `complete_hypercall(rax)`, each valid exactly once before the next `run` (a missed completion is
   `BackendError::PendingCompletion`, fail-closed). `vmm-core` computes the value (port/MMIO read
   byte, `Rdmsr` value or #GP, `Cpuid` quad) and calls the matching completion — it never touches
   `kvm_run`.
3. **`Exit::Hlt` distinct from `Exit::Shutdown`.** Task 14's `Exit` has both, so M1 distinguishes a
   clean isa-debug-exit `PASS` (`DebugExit { code: 0 }`) from the payload's `HLT` fallback and from
   a `Shutdown`.

Also consumed: `exit_counts() -> vmm_backend::ExitCounts` (per-exit-reason counters) for
`RunResult.exit_counts`.

## Mock-backend testing (how the pure logic is gate-tested on a Mac)

The event loop, loader, entry state, UART, and contract policy are exercised with **no
`/dev/kvm`** by a `MockBackend` test fixture implementing `vmm_backend::Backend`:

- `run()` returns the next `Exit` from a **scripted queue** the test loads (e.g.
  `[Io{0x3FD,read}, Io{0x3F8,write:'H'}, …, Io{0xF4,write:0}]`), so a test can drive a whole
  `hello`-shaped serial+exit sequence and assert `RunResult.serial` and `TerminalReason`.
- `map_memory`/`save`/`restore` record into an in-memory `VcpuState` + RAM buffer, so
  `state_blob`/`state_hash` and the entry-state setup are checkable end-to-end.
- The mock must faithfully reflect what it is given — a read exit gets resolved with the value
  `vmm-core` supplies, and the test asserts that value (so a transport/dispatch bug that resolves
  the wrong exit is caught, not masked).

This is the `hypercall-doorbell` loopback pattern (task 10) applied to the backend seam: the
deterministic logic is factored above the privileged primitive so it runs under `cargo test` on
every host. The mock lives in `#[cfg(test)]` / `tests/`; it is **not** a `pub` part of the crate.

## Dependencies, grants, and the rule-2 / rule-5 exceptions

- **`vmm-backend` (task 14) — declared rule-2 exception.** The entire point of this crate is to
  be the deterministic VMM *above* the `Backend` trait, so it depends on `vmm-backend` for
  `Backend`, `Exit`, `VcpuState`, `Gpa`, `Event`, `CpuidModel`, `MsrFilter`, `ExitCounts`, and
  `BackendError`. Re-declaring the trait
  locally would produce a *different* trait that the binary's `KvmBackend` does not implement.
  This is the one permitted sibling dependency (BRINGUP: "14 leads 15"). **Hard prerequisite:** task
  14 must land first — this spec compiles against `set_cpuid`/`set_msr_filter`/`complete_read`/
  `complete_fault`/`complete_ok`/`complete_hypercall`/`exit_counts`, which are task-14 *refinements*
  of the checked-in R-BACKEND sketch (not yet present in it). Do **not** start task 15 until
  `tasks/14-backend.md` is merged and `consonance/vmm-backend` exposes that surface; treat any
  divergence as a `[question]`. Call it out in `IMPLEMENTATION.md`. **No `kvm-ioctls`/`kvm-bindings`/`vm-memory` here** — those are
  `vmm-backend`'s Linux-only deps, *below* the trait; this library must build on macOS (trait +
  value types only).
- **`unison` (task 03) — dev-dependency only.** The M2 `Machine`/`MachineFactory` adapter
  lives in the **box-only integration test** (`#[cfg(target_os = "linux")]`), so `unison` is a
  `[dev-dependencies]` entry, not a library dep. When the production unison adapter is needed
  (a later phase, with real V-time / `run_to(target)`), it graduates into the library; for the
  M1/M2 skeleton, test-only coupling is correct and keeps the library's sibling surface minimal.
- **rule-5 whitelist.** Library deps stay within the conventions whitelist: `thiserror`,
  `zerocopy` (parsing the Multiboot header / writing the boot-info struct without `unsafe`),
  `sha2` (the state hash + contract hash). `clap` for the bin only. `memmap2`/`rustix` only in the
  box path for host-backed guest RAM (see `unsafe` below). **The `kvm-*`/`vm-memory` frontier deps
  are a reviewed rule-5 exception recorded in *task 14's* spec/PR, not here** — `vmm-core` never
  imports them. **Open question (ask-by-comment):** ingesting `docs/cpu-msr-contract.toml` at
  build time may want a `toml` parser (not whitelisted) — prefer a checked-in generated module or
  `build.rs` codegen to avoid a runtime dep; if a parser is unavoidable, ask-by-comment in the PR.
- **`unsafe`** is granted for two related purposes in the `bringup`/box path, each with a
  `// SAFETY:` comment: (1) allocating the pinned host-backed `GuestRam` (mmap via
  `memmap2`/`rustix`); and (2) the **call** to task 14's `unsafe Backend::map_memory` — `bringup`
  upholds its `# Safety` contract by keeping the `GuestRam` owned by the returned `Vmm` (so the
  backing outlives every `run`) and never aliasing it while a `run` is in flight. On the mock
  backend `map_memory` is still `unsafe` to call but records only the slice, so the unit tests
  exercise the same call shape. The loader, entry state, UART, contract policy, event loop, and
  state hashing are **safe**. The live-`KVM_RUN` `unsafe` lives in `vmm-backend`, not here.

## Determinism (conventions rule 4)

- No `HashMap`/`HashSet` iteration into the state hash or any output — `state_blob` is a fixed,
  length-prefixed, domain-tagged byte layout (memory ‖ vcpu ‖ serial ‖ device/terminal); the
  `contract` tables are `BTreeMap`/sorted-`Vec`. No floating point. No wall-clock, no unseeded
  randomness — the skeleton introduces no time source (V-time arrives in a later phase).
- `state_hash` is a **pure function of state** (unison contract): calling it twice is
  identical, and it covers **all observable state** so an output-only or wrong-exit-code
  divergence with identical memory/registers still breaks the hash (BRINGUP determinism gate).
- The loader is a **trust boundary**: arbitrary `image` bytes never panic — they yield a
  `LoadError`. A `proptest`/fuzz-shaped test feeds random bytes and asserts `Ok | Err`, never a
  panic (conventions no-panic-on-untrusted-input).
- The gate is the *forcing function*, not a proof: `hello`/`compute` pass on stock `KvmBackend`
  because they are **RDTSC/RDRAND-free by audit**; the skeleton claims determinism only for that
  audited subset, exactly as BRINGUP / R-BACKEND require. A backend-dependent exit on stock KVM is
  a loud failure, never a laundered host value.

## Acceptance gates

The standard conventions gates (`build`, `nextest`, `clippy -D warnings`, `fmt`, `cargo deny`) on
**macOS and Linux** for the pure-logic library, **plus**:

**Miri (required — this crate grants `unsafe`).** Per the `unsafe ⇒ Miri` rule
(`tasks/00-CONVENTIONS.md` / `AGENTS.md`, merged in quality-g/#24), `cargo +nightly miri test -p
vmm-core` must run **clean**, and `vmm-core` must be added to the `miri` job's crate set in
`.github/workflows/quality.yml` (and `scripts/install-quality-tools.sh`'s `MIRI_CRATES`). The granted
`unsafe` is box-only — the `GuestRam` `mmap` and the `unsafe Backend::map_memory` *call* — so it is
`#[cfg(not(miri))]`-excluded: under Miri (and on the Mac) `GuestRam` falls back to a `Vec<u8>`
backing, and `MockBackend`'s `map_memory` performs no unsafe operation. Miri then exercises the
**pointer / lifetime / bounds logic** of the loader, event loop, and `state_blob` against the mock —
exactly the surface the rule targets. Document the exclusion and why it is sound in
`IMPLEMENTATION.md` (the `hypercall-doorbell` precedent).

**Pure-logic unit/property tests (run on the Mac, against `MockBackend` — no `/dev/kvm`):**

1. **Loader — file offset.** `parse_header` + `load` of the real `hello` payload place
   `entry_addr`/`load_addr` correctly and copy from `file_off = 0x1000` (BSS zeroed). A
   constructed image with `header_addr ≠ load_addr` exercises the **full** override formula
   (`file_off = mb_header_file_offset − (header_addr − load_addr)`), proving the
   `mb_header_file_offset` term is not dropped.
2. **Loader — no panic on untrusted input.** ≥ 256 `proptest` cases of arbitrary bytes (and
   truncated/again-aligned-magic images): every result is `Ok | Err(LoadError)`, never a panic or
   OOB.
3. **Entry state.** `protected_mode_entry` yields `EAX = 0x2BADB002` (**not** `0x1BADB002`),
   `EBX = BOOT_INFO_GPA`, `RIP = entry_addr`, flat CS/DS/ES/SS (base 0, 4 GiB, 32-bit),
   `CR0.PE=1`/`CR0.PG=0`, `EFLAGS.IF=0`.
4. **UART model + DLAB.** LSR read = `0x60`; THR writes (DLAB clear) append in order to
   `capture()`; init writes are accepted and do not corrupt the capture buffer. **Critically:**
   replay the task-04 init order — set `LCR.DLAB` (write `0x80` to `0x3FB`), write the `0x01`
   divisor to `0x3F8`, clear DLAB, then write data — and assert `capture()` contains **only** the
   data bytes (no leading `\x01`). A `0x3F8` write with DLAB set must not be captured.
5. **isa-debug-exit.** A `0xF4` write of `c` yields `Terminal(DebugExit { code: c })`; `PASS`(0)
   and `FAIL`(1) distinguished.
6. **Contract policy.** `USER_SPACE_MSR_MASK == FILTER|UNKNOWN|INVAL`; `msr_filter_allow()`
   contains exactly the `allow-stateful` rows (≤ 16 ranges, installed both directions); sampled
   `rdmsr/wrmsr` dispositions match the contract (default `DenyGp`; a known `allow-fixed` returns
   its constant; **`0x10`/`0x3b` → `EmulateVtime` for both read and write**, not `AllowFixed`/`DenyGp`);
   `cpuid_model()` masks X2APIC (1:ECX[21]) + TSC-deadline (1:ECX[24]) and hides PV/​vPMU leaves;
   **`resolve_cpuid` overlays the dynamic cells** (`CPUID.1:ECX[27]` follows `CR4.OSXSAVE`,
   `0xB`/`0x1F:ECX` echoes the subleaf, `0xD.0:EBX` follows `XCR0`); and **`contract_hash()` equals the
   `contract_hash` in `docs/cpu-msr-contract.toml`** (policy cannot drift from the ratified contract).
   **Prerequisite — this sub-gate is blocked until the §6 registry lands:** `docs/cpu-msr-contract.toml`
   does not yet carry a `contract_hash` field (CPU-MSR-CONTRACT §6's `(contract-version, body-hash)`
   registry is uncommitted). Committing that field is **out of this task's scope** (which is
   `consonance/vmm-core/` only), so until it lands the implementer must raise a `[question]` and may
   leave this sub-gate pending — **never fabricate an unratified hash**. The rest of gate 6 stands.
7. **Event loop (scripted).** A `MockBackend` scripted with the `hello` serial+exit sequence makes
   `Vmm::run()` produce `RunResult.serial == b"PAYLOAD hello START\nPAYLOAD hello PASS\n"` and
   `reason == DebugExit { code: 0 }`. An unmodeled `Io`/`Mmio`/`Hypercall`/`Rdtsc` exit yields
   `Err(VmmError::ContractViolation(_))`, never a silent default.
8. **`state_hash` purity & coverage.** Called twice on the same state ⇒ identical; flipping any
   one component (a guest-RAM byte, a `VcpuState` register, a serial byte, the debug-exit code)
   ⇒ different hash.

**Box-only live integration tests (`#[cfg(target_os = "linux")]`, on `ssh <det-box>`, CPU-pinned
per `docs/BOX-PINNING.md`, against the real `KvmBackend`):**

9. **M1 — boots & prints.** `boot(KvmBackend::new(), hello_image, ram_len)` — a **bare** backend;
   `boot` installs the `contract` policy through `set_cpuid`/`set_msr_filter` (never a
   policy-carrying constructor, which would bypass the trait seam) — then `run()`:
   `RunResult.serial` equals `consonance/acceptance-suite/golden/hello.txt` **byte-for-byte**, **and**
   `reason == TerminalReason::DebugExit { code: 0 }` — explicitly **not** `Hlt` and **not** a
   non-zero code (a payload can print `PASS` then exit non-clean; the terminal reason is checked,
   not just the serial).
10. **M2 — deterministic twice.** A `unison::MachineFactory` whose `spawn` builds a fresh
    `Vmm<KvmBackend>` for a given payload, and a `Machine` impl whose `state_hash` is
    `Vmm::state_hash` and whose `run_to`/`work` run the payload to terminal (work-counting /
    `run_to(target)` bisection is a later-phase concern — see open questions). For **both** `hello`
    and `compute`: run twice and assert the two `state_hash`es are identical **and** the two serial
    captures are identical; `compute`'s serial also equals `consonance/acceptance-suite/golden/compute.txt`. The hash is
    taken over all observable state (memory ‖ `VcpuState` ‖ serial ‖ device/terminal), so an
    output-only divergence still breaks it.
11. **Fail-fast host check.** The live tests detect a host without `/dev/kvm` / not Intel-VMX and
    skip with a loud message naming what is missing and where to run it — never a silent pass.

## Non-goals (deferred — see BRINGUP "What later phases pick up")

- **Interrupts / LAPIC / PIC / PIT.** `KVM_IRQCHIP_NONE` with no interrupt delivery (R1). The
  userspace xAPIC MMIO handling is **task 13** (`lapic`) integration; PIC/PIT stubs and the
  `interrupts` payload are a later phase. M1/M2 payloads need none.
- **V-time / PMU / `run_until` deadlines.** The retired-branch counter, `Backend::run_until`,
  single-step injection, and the INTEGRATION.md §2 planner inversion are a later phase. The event
  loop's `step()` seam is shaped to accept them, but this milestone runs payloads to terminal only.
- **Hypercall host handler.** The VMCALL exit handler (INTEGRATION.md §1, the host side of task
  10) — deferred; M1/M2 payloads issue no hypercalls.
- **Snapshot / `vm_state` codec.** Task 09's canonical blob replaces the ad-hoc `state_blob`
  encoding when it lands; snapshot/restore (tasks 02/08) is a later phase.
- **`PatchedKvmBackend` / RDTSC / RDRAND / RDSEED (M3).** A parallel track (R-BACKEND); routing
  those exits to V-time / seeded PRNG is out of scope. The skeleton fails closed on them.
- **bzImage / real-Linux loader and the guest driver** — Phase 3.
- **`vmm-backend` itself** (the `Backend` trait + `KvmBackend` + `Exit` surface + per-exit
  counters) is **task 14**; this crate consumes it.

## Deliverable

A branch `task/vmm-core` containing only `consonance/vmm-core/`, the pure-logic gates green on macOS
and Linux, the box-only M1/M2 gates green on `ssh <det-box>` (CPU-pinned), and a short
`IMPLEMENTATION.md` noting: the rule-2 `vmm-backend` dependency exception and the `unison`
dev-dependency; how the mock-backend seam is wired; the exact task-14 surface relied on (the three
coordination items above) and any `[question]` raised against R-BACKEND while wiring the real
`KVM_RUN` loop; the contract-ingestion mechanism chosen (generated module vs. `build.rs` vs. an
asked-for `toml` dep) and the `contract_hash` gate; and any deviations considered and rejected.