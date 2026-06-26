# vm-state — implementation notes

Versioned, deterministic TLV codec for the non-memory `vm_state` snapshot blob
(task 09). Pure logic: no syscalls, no `/dev/kvm`, no time, no sibling-crate
dependencies. Builds and passes every gate on macOS and Linux.

## What was built

- `VmState` and its plain-data sub-structs, exactly as the spec's Public API
  lists them (`VcpuRegs`, `Segment`, `VcpuSregs`, `Xcrs`, `DebugRegs`,
  `VcpuEvents`, `MpState`, `MsrBlock`, `XsaveImage`, `VtimeState`,
  `TimerQueueState`/`TimerEntry`, `DeviceBlob`, `contract_hash: [u8; 32]`).
- `VmState::{encode, decode, peek_version}` and `VmStateError` with all ten
  specified variants.
- A little-endian TLV container: 8-byte header (`magic`, `version`,
  `section_count`) then 13 sections (`tag: u16`, `len: u32`, payload) in
  ascending tag order, every v1 tag present exactly once.
- Fixed-layout records use `zerocopy` POD wire structs (`src/wire.rs`); the only
  `unsafe` is what `zerocopy`'s derives generate.

### Module layout

`error.rs` (the error enum) · `types.rs` (public plain data) · `wire.rs`
(`#[repr(C)]` zerocopy records + total conversions) · `codec.rs`
(encode/decode/peek + a strict forward-only `Reader`) · `lib.rs` (crate doc,
re-exports, constants, the `VmState` struct).

## Key design decisions

- **No padding, hence no nondeterminism from padding.** Every wire record uses
  `zerocopy::little_endian::{U16,U32,U64}` (alignment 1) plus `u8`, so each
  `#[repr(C)]` struct is alignment-1 with no padding. The `IntoBytes` derive
  *enforces* this at compile time, so there are no reserved/pad bytes that could
  differ between machines or toolchains.

- **Timer invariants (the one subtle round-trip point).** A `TimerQueueState`
  must obey three task-05 `TimerQueue` invariants, all enforced as **value
  invariants** by a shared `validate_timers` helper that `encode` runs before
  writing and `decode` re-runs after reading (a violation is `InvalidField`,
  never a silent fix-up):
  1. entries strictly ascending and unique by `(deadline_vns, seq)` — the firing
     order task-05 replays (same-deadline timers fire in `seq`/FIFO order, **not**
     token order);
  2. `token`s unique across entries — task-05 keys a `token -> entry` index, so a
     duplicate would misdirect a later cancel/reschedule;
  3. every `seq < next_seq` — else a restored queue's next same-deadline insertion
     would reuse a live `seq` and collide.

  `encode` deliberately does **not** silently sort/dedup/clamp: silent
  canonicalization would break `decode(encode(s)?) == s` (decode would hand back a
  different queue), and a duplicate-key Vec would emit a blob `decode` then
  rejects. Validating instead makes round-trip identity hold for *every* `VmState`
  `encode` accepts. (Invariant 1 was the first PR #36 review fix — the earlier
  draft sorted; invariants 2–3 were the P2 completion in the re-review.) The
  round-trip proptest generates faithful queues (distinct seqs/tokens,
  `next_seq` above every seq); `encode_accepts_iff_timers_valid` asserts `encode`
  accepts a queue **iff** all three hold and that every accepted queue
  round-trips; `encode_rejects_duplicate_token_or_high_seq` injects each of
  invariants 2 and 3 into an otherwise-valid queue and asserts rejection.

- **MSRs via `BTreeMap`.** Iteration is sorted by index, so insertion order never
  reaches the bytes. `decode` also validates strictly-ascending indices, which
  both rejects malformed blobs and guarantees the map round-trips exactly.

- **`encode` is fallible by contract.** It rejects `ratio_den != 1`
  (`FractionalRatio`) at the codec boundary so an un-restorable-exactly timeline
  can never be written (INTEGRATION.md §4). `ratio_den == 0` is `!= 1`, so the
  same gate covers it — note this means a `VmState::default()` is *not* encodable
  as-is (its `ratio_den` is 0). It also returns `InvalidField` if a
  variable-length section would exceed `u32::MAX` bytes (unreachable for real
  state, but kept total rather than truncating silently).

- **Strict, total decode.** Bad magic → `BadMagic`; unknown version →
  `UnsupportedVersion`; short buffer / oversized `len` → `Truncated`; leftover
  bytes → `TrailingBytes`; a repeated tag → `DuplicateTag`; a non-ascending tag →
  `SectionOrder`; an unknown tag → `UnknownTag`; any absent required tag →
  `MissingSection`; an out-of-range `MpState`/bool byte, a non-ascending MSR or
  timer list, or a fixed record of the wrong length → `InvalidField`. Fuzzed over
  arbitrary byte vectors and single-byte mutations of a valid blob: never panics.

- **`peek_version`** reads only the header, validates the magic, and returns the
  raw version even when it is unsupported — so a caller can tell an old/new blob
  apart from a corrupt one. (`decode` of that same blob returns
  `UnsupportedVersion`.)

## Deviations considered and rejected

- **Putting `zerocopy` LE types directly in the public structs.** Rejected: the
  spec's Public API uses plain `u64`/`u32`/`u16`/`bool`, which is the contract
  (rule #3). The LE wire types stay private (`src/wire.rs`) with total
  conversions; the public API is exactly as specified.

- **`#![forbid(unsafe_code)]`.** Rejected: it would reject the `unsafe impl`s that
  `zerocopy`'s derives generate. The "no hand-written `unsafe`" property holds
  simply because this crate writes no `unsafe` blocks itself. Note that
  derive-generated `unsafe` *still* puts the crate under the unsafe⇒Miri rule (a
  PR #36 review fix corrected an earlier "no Miri" assumption) — see the Miri gate
  below.

- **Tolerating a missing section as a zero-filled default.** Rejected per spec:
  every record has a `Default`, so a tolerant decoder would silently restore that
  machine state as zero. All v1 tags are required; a dropped one (or
  `section_count = 0`) is `MissingSection`.

- **A decode-side `ratio_den == 1` check.** Not added: `FractionalRatio` is an
  encode-time gate; `decode` faithfully round-trips whatever ratio bytes a blob
  carries (a blob produced by `encode` always has `ratio_den == 1` anyway).

## Known limitations / things the integrator must know

- **Device section is a placeholder.** `DeviceBlob(Vec<u8>)` is opaque,
  length-delimited bytes — the deliberate deferred seam (scope note + R1
  §"Consequence 1"). When task 13's `lapic::LapicState` (+ PIC/PIT stubs) lands,
  fold a typed `{ lapic, pic, pit }` record into this one section under a **bumped
  `VM_STATE_VERSION`**; no other section is disturbed. The format is
  forward-compatible precisely because the device payload's internal layout is
  isolated behind one tag.

- **`contract_hash` is carried, not verified here** — and that is the whole job
  for this field. Comparing it against the live contract on restore (and rejecting
  a mismatch) is vmm-core's responsibility (CPU-MSR-CONTRACT §6), the same
  division of labor as the quiescent-point assertion.

- **No armed-but-unfired injection-plan field.** By design: vmm-core snapshots
  only at a quiescent point and enforces it with an assertion (INTEGRATION.md §4),
  so there is nothing to serialize.

- **Golden test pins the exact bytes.** `tests/golden.rs` holds the full hex of a
  fixed `fully_populated()` `VmState`. Any layout change fails it; if intentional,
  regenerate with `cargo test -p vm-state --test golden -- --ignored --nocapture
  print_golden` **and** bump `VM_STATE_VERSION`.

- **Miri gate (PR #36 review fix).** `zerocopy`'s derives generate `unsafe impl`s,
  so this crate is under the unsafe⇒Miri rule. `cargo +nightly-2026-06-16 miri
  test -p vm-state` (MIRIFLAGS `-Zmiri-permissive-provenance`) is clean; Miri
  validates the manual TLV byte-parsing and the `zerocopy` record reads. The crate
  is added to the `miri` job in `.github/workflows/quality.yml` and to
  `MIRI_CRATES` in `.githooks/pre-push`. Proptest case counts drop to 16 under
  `cfg!(miri)` (and failure persistence is disabled, since Miri's filesystem
  isolation rejects the regression-file `getcwd`), so the interpreted suite stays
  ~3 min; native runs keep the full ≥256/512/1024 counts.

- **Public API is frozen (PR #36 review fix).** `tests/public_api.rs` +
  `tests/public-api.txt` snapshot the surface via `cargo public-api`, and
  `-p vm-state` is wired into the `public-api` CI job. Refresh an intentional
  change with `UPDATE_PUBLIC_API=1 cargo test -p vm-state --test public_api`.

- **`Cargo.lock`** is intentionally not tracked by this repo (see `deny.toml`); a
  local build generates one. It is not part of this change.

## [question] for the integrator — `contract_hash` and `docs/cpu-msr-contract.toml`

The acceptance sub-gate that would tie `contract_hash` to the actual ratified
contract needs `docs/cpu-msr-contract.toml` to carry a `contract_hash` field
(the §6 registry value). That value is **not committed yet** (a pending user
decision), so I have **not fabricated a hash**: `vm-state` simply round-trips the
32-byte field, and all six task gates exercise it with random / fixed bytes (none
of them reads the contract file). This is consistent with the spec — computing the
hash from the canonical contract form and *comparing* it on restore is explicitly
vmm-core's job, not this crate's. Nothing here is blocked; the only open item is
upstream: once the contract artifact records its `contract_hash`, vmm-core (not
`vm-state`) is where it gets computed and checked. Flagging so the integrator
confirms that division of labor rather than expecting `vm-state` to derive it.

## Gate results

`cargo build`, `cargo nextest run` (31 tests: round-trip ≥512 cases, determinism
incl. the timer accept-iff-valid and invariant-injection proptests, strict-decode/
fuzz ≥1024 cases, golden, version-rejection, integer-ratio rejection),
`cargo clippy --all-targets
-- -D warnings`, `cargo fmt --check`, `cargo deny check`, and
`cargo +nightly-2026-06-16 miri test -p vm-state` all pass. Native test runtime is
well under a second; the Miri suite (reduced cases) ~3 min. The `public_api`
snapshot test passes against the committed `tests/public-api.txt` on the pinned
nightly.
