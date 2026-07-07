# control-proto — implementation notes

The out-of-band control-plane wire protocol (task 25): the versioned,
length-delimited request/response codec dissonance's explorer (R2) uses to drive
a VM as a black box — `snapshot`/`branch`/`replay`/`run`/`hash`. **Protocol layer
only.** Pure host `std` logic: no `/dev/kvm`, no guest, no real socket, no
wall-clock, no host entropy, no `HashMap`/`HashSet`, no floating point, no sibling
crate dependencies. Builds and passes every gate on macOS and Linux. No `unsafe`,
so no Miri obligation.

## What was built

- The public types **exactly** as the spec's Public API lists them: the opaque
  carried units (`Environment`, `Answer`), handles (`SnapId`, `VTime`,
  `DecisionId`), the verbs (`Request`, `Reply`), run control (`StopConditions`,
  `StopMask`, `HashScope`), the guest-observable outcomes (`StopReason` and its
  `CrashInfo`/`EventRef` payloads), the two error categories (`ControlError`,
  `ProtocolError`), negotiation (`Caps`, `CoverageGeometry`, `CapFlags`), the
  consts `PROTO_VERSION`/`MAX_FRAME_LEN`, and the four codec functions
  `encode_request`/`encode_reply`/`decode_request`/`decode_reply`.
- **The wire format.** A frame is `magic("CTL1") · version:u16 · seq:u32 ·
  len:u32 · body[len]`, all integers little-endian (14-byte header). The body is a
  tagged encoding of a `Request` or a `Result<Reply, ControlError>`; every
  variable-length field is `u32`-length-prefixed. The encoding is **canonical**
  (one byte form per value; `len` always equals the body's natural size), so
  `encode(decode(x)) == x` for every accepted frame. Decoding is **strict and
  total**: bounds-checked against the actual buffer, an over-cap `len` rejected
  from the header alone, a body that doesn't consume exactly `len` bytes rejected,
  never a panic or an out-of-bounds read.

### Task 45 — the host-plane `perturb` verb

Task 45 (the host control plane) added the spec's control-transport verb
`perturb(fault: HostFault, at: Moment) -> Result<(), ControlError>` (task-45 spec
lines 41–42). It mirrors the existing `Branch` verb pattern:

- **`Request::Perturb { fault: HostFault, at: Moment }`** stages a host-plane fault
  at a `Moment`, replied to with `Reply::Unit` (an ack, like `Branch`/`Replay`).
  The host plane rides this **out-of-band** channel — the guest never sees it; the
  backend decodes `fault` and applies it at its `Moment` during a `Run`.
- **`HostFault(pub Vec<u8>)`** is a new opaque carried unit — the host-plane
  analogue of `Answer`, schema-blind (its structure is `environment::HostFault`'s
  contract; the backend decodes it, never the codec). **`Moment(pub u64)`** mirrors
  `environment::Moment` (rule 2, defined locally).
- **Wire format:** body = `REQ_PERTURB(8) · fault(u32-len-prefixed bytes) · at:u64`,
  canonical and length-delimited like every other verb. Round-trip, golden
  (`req_perturb`), loopback (a staged host fault over the wire), and
  adversarial/streaming coverage all extend to it; the `public-api.txt` snapshot
  is refreshed.

### Task 80 — the observation verbs `read` / `regs`

Task 80 (the resolution observation surface) adds the two **observation** verbs the
control protocol was missing — it could `hash(scope)` guest state but never *look
at* it:

- **`Request::Read { gpa: u64, len: u32 } → Reply::Bytes(Vec<u8>)`** — guest
  physical memory. Body = `REQ_READ(10) · gpa:u64 · len:u32`. `len` is bounded by
  the new **`READ_CAP` = 64 KiB** const; the backend answers a loud
  `ControlError::ReadTooLarge { len, cap }` for an over-cap request and
  `ControlError::ReadOutOfRange { gpa, len, ram_len }` for a `[gpa, gpa+len)` past
  guest RAM — **never** a truncated success.
- **`Request::Regs → Reply::Regs(RegsView)`** — a **versioned** register view
  (`RegsView::VERSION = 1`): the 16 GPRs (canonical order
  `rax rbx rcx rdx rsi rdi rbp rsp r8..r15`), `rip`, `rflags`, the segment
  selectors (`cs ss ds es fs gs`), `cr0`/`cr3`/`cr4`, and the current `Moment`/
  V-time. A **view, not** the save/restore format: additive-evolution, no
  round-trip obligation — a `VERSION` bump appends fields (encoded after `vtime`),
  and an older reader still consumes the prefix it knows. Body = `REQ_REGS(11)`;
  the reply's `RegsView` is written field-by-field, fixed order, little-endian.

The `RegsView` shape mirrors `dissonance/resolution`'s **local** view (PR #82,
`server.rs`) field-for-field (rule 2 — resolution defined it locally against this
spec while 80 was unmerged), so the integrator's reconciliation is a rename, not a
reshape. `resolution`'s local view additionally derives `serde::{Serialize,
Deserialize}` for its transcript; this crate stays serde-free (the wire codec *is*
the serialization) — when the integrator collapses the two, add serde derives to
`RegsView` (serde is whitelisted) or a `From` conversion. The two read range guards
(`ReadOutOfRange`/`ReadTooLarge`) collapse onto resolution's client-local
`SessionError` variants, same field shapes.

`APP_PROTOCOL_VERSION` bumped **4 → 5** (new verbs + reply tags + error tags):
additive to the codec, but a stale peer must reject **at `hello`** rather than hit
a mid-session `ShortFrame` on a tag it does not know. Golden bytes (`req_read`,
`req_regs`, `reply_bytes`, `reply_regs`, `err_read_out_of_range`,
`err_read_too_large`), the round-trip/adversarial/streaming proptest strategies,
the loopback ("every verb") session, and `public-api.txt` all extend to the new
surface; the decoder's new discriminant arms are reached by the existing
arbitrary-bytes fuzz target.

### Module layout

`error.rs` (`ControlError` / `ProtocolError`) · `types.rs` (the plain wire data +
the `class_bit` discriminants + `StopMask`/`CapFlags` helpers) · `codec.rs` (the
strict little-endian framing with a forward-only, bounds-checked `Reader`) ·
`lib.rs` (crate doc, re-exports, `PROTO_VERSION`/`MAX_FRAME_LEN`).

## Key design decisions

- **Three independent "versions", only one validated by the codec.** The frame
  header `version` is the *framing* version (`PROTO_VERSION`); a mismatch is
  `ProtocolError::BadVersion`. `Caps.protocol_version` is the *negotiated*
  application version, inspected by the backend (gate 4) — the codec never
  validates it. `Environment.blob_version` is carried verbatim and never
  validated, so an off-version blob still decodes and the backend (not the codec)
  answers `ControlError::BadEnvVersion`. This separation is what lets R2 be coded
  ahead of the fault catalog (schema-blind carry).

- **`ShortFrame` is the body-malformation error.** The spec freezes
  `ProtocolError` at four variants (`ShortFrame`/`BadMagic`/`BadVersion`/`BadLength`);
  adding one would expand a pinned public contract (conventions rule 3), so a
  *complete* frame whose body is undecodable — an unknown discriminant, an inner
  length that overruns the body, or trailing bytes inside the declared `len` — is
  reported as `ShortFrame` (documented on the enum). A frame that is merely
  not-yet-fully-received is **not** an error: `decode_*` returns `Ok(None)`
  ("need more"), which is what makes byte-at-a-time streaming correct (gate 5).

- **`BadLength` before buffering.** `decode_*` rejects a header advertising
  `len > MAX_FRAME_LEN` from the 14-byte header alone, before slicing or
  allocating any body — so an untrusted length can never force unbounded
  buffering. The cap is inclusive (`len == MAX_FRAME_LEN` is accepted; only `>` is
  rejected). `encode_*` mirror this: an over-cap body returns `BadLength` and
  leaves `buf` unchanged (the body is built in a scratch `Vec` and size-checked
  before any byte is appended to `buf`).

- **`StopMask` bit = `1 << class_bit`.** The integrator-pinned mapping: the armed
  bit for a class is `1 << DecisionClass`-discriminant. `arm`/`armed` use a
  checked shift, so a `class_bit ≥ 32` is a total no-op (never a shift-overflow
  panic); the real discriminants are `1..=6`. The `class_bit` module mirrors
  `environment::DecisionClass`'s discriminants locally (conventions rule 2 — the
  one shared contract between this crate and the fault catalog; they must stay in
  sync or different decisions surface and replay breaks).

- **`CrashInfo`/`CrashKind` made concrete.** The spec left `CrashInfo`'s fields
  open (`/* kind: panic/triple-fault/shutdown + detail */`); implemented as a
  `CrashKind` enum (`Panic`/`TripleFault`/`Shutdown`) plus an opaque `detail:
  Vec<u8>`, matching the sketch.

- **No `Host` `StopReason`.** Per the spec: an in-band hypercall is serviced by
  the consonance plane and the run continues; anything R2 must react to arrives as
  `Decision`/`SnapshotPoint`/`Assertion`. This keeps `StopReason` representable by
  the explorer surface (task 12) and preserves the two-result-category rule.

## Additions (allowed by conventions rule 3)

- `pub mod class_bit` — named `DecisionClass` discriminants (`1..=6`) for
  `StopMask::arm`, mirroring `environment::DecisionClass`.
- `CrashKind` enum (the `CrashInfo.kind` field type).
- `CapFlags::{NONE, GUEST_HAS_SDK, contains, with}` and `StopMask::NONE`
  (`StopMask::{NONE, arm, armed}` are spec-mandated). The bit meanings of
  `CapFlags`/`StopMask` are the backend's contract; the codec only round-trips the
  `u32`. The frozen public surface is in `tests/public-api.txt`.

## Deviations considered and rejected

- **A `kind` byte in the frame header** to distinguish request vs reply frames.
  Rejected: the spec frame is exactly `magic·version·seq·len·body`, and
  `decode_request`/`decode_reply` are separate entry points (each side knows which
  it is decoding). Adding a discriminant would diverge from the pinned frame
  sketch for no protocol benefit.

- **A fifth `ProtocolError` variant** (e.g. `Malformed`) for undecodable bodies.
  Rejected — the four-variant enum is the contract (rule 3); `ShortFrame` is the
  documented catch-all.

- **Validating `Environment.blob_version` / `Caps.protocol_version` in the
  codec.** Rejected: gate 4 requires the codec to *carry* them so the backend can
  answer `BadEnvVersion` / negotiate. The codec validates only the framing
  version.

## Known limitations / integrator notes

- **Frontier (vmm-core), not here:** the unix `SOCK_STREAM` itself, the
  verb→`Backend`/`snapshot-store`/`Dispatcher` binding, the stage-and-re-enter run
  suspension (the suspended hypercall re-entered with the staged `resolve`
  answer), the internal structure of `Environment`/`Answer` (task 24), and the
  coverage map bytes (shmem — only its geometry crosses the socket, never the
  bytes). The `MalformedEnvironment`/`MalformedAnswer`/`BadEnvVersion` checks are
  the backend's: this crate decodes the frame and hands the opaque blob over; it
  never parses it.

- **The `StopMask` ↔ `DecisionClass` bit mapping is the one shared contract** with
  the fault catalog. `class_bit`'s consts must stay equal to
  `environment::DecisionClass`'s discriminants; a divergence would silently change
  which decisions surface (broken replay). Both crates compute the identical bit
  (`1 << discriminant`).

- **Fuzzing.** `fuzz/` is a self-contained cargo-fuzz project kept *inside* this
  crate's directory (conventions rule 1 — the repo-root `fuzz/` belongs to task
  19), with an empty `[workspace]` so the root workspace's `dissonance/*` glob and
  the `-p control-proto` gates ignore it. Target `decode_frame` fuzzes
  `decode_request`/`decode_reply` on arbitrary bytes (and on the same bytes
  wrapped in a valid header, to reach the body parser): no panic, no over-read,
  and every accepted frame round-trips canonically (`encode(decode(x)) ==
  x[..consumed]`). Run with the pinned nightly:
  `cargo +nightly-2026-06-16 fuzz run decode_frame`. The no-panic + round-trip
  properties also run in the normal suite (`tests/adversarial.rs`,
  `tests/roundtrip.rs`, proptest ≥256 cases) so the guarantee is gated without
  cargo-fuzz installed. **Ask-by-comment** (conventions rule 5): `libfuzzer-sys`
  (the standard cargo-fuzz harness crate) is outside the dependency whitelist; it
  is fuzz-only and never a library dependency.

- **CI wiring left to the integrator (root files are off-limits, rule 1):** add
  `-p control-proto` to the `public-api` job's list in
  `.github/workflows/quality.yml` (as was done post-merge for `pv-net`/`environment`),
  and `control-proto` to any task-19 fuzz smoke job. The `tests/public_api.rs`
  guard and `tests/public-api.txt` snapshot are in place and pass on the pinned
  nightly (`nightly-2026-06-16`); the test skips cleanly when the tooling is
  absent. No `miri` entry is needed (no `unsafe`). The root `Cargo.lock` is not
  committed here — the repo does not maintain it per-PR (it is even missing the
  `environment` entry from #54); the integrator regenerates it.

## Gates

`cargo build/nextest/clippy(-D warnings)/fmt -p control-proto --all-features` and
`cargo deny check` all pass; 52 tests across golden bytes (gate 1), round-trip
(gate 2), adversarial decode (gate 3), version negotiation (gate 4), streaming
framing (gate 5), and loopback (gate 6), plus the lib unit tests and the
(ignored, nightly) public-api guard. Suite runtime ≈ 0.3 s. The clippy run also
surfaces three *pre-existing* workspace-`clippy.toml` meta-diagnostics (the
`rand::*` disallowed-method paths become unresolvable once proptest pulls `rand`
into the dev-dep graph); they are emitted for every proptest-using crate, cite
`clippy.toml` not this crate's code, and do not fail `-D warnings`.
