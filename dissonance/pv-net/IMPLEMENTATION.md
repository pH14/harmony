# pv-net — implementation notes

The host-side L2 switch and V-time network-fault scheduler (task 26), the
network locus of dissonance's fault model. Pure logic: no `/dev/kvm`, no guest,
no real networking, no wall-clock, no host entropy, no sibling-crate
dependencies. Builds and passes every gate on macOS and Linux. No `unsafe`, so no
Miri obligation.

## What was built

- The public types exactly as the spec's Public API lists them: `VTime`,
  `NodeId`, `ConnId`, `FrameHdr`, `NodeMap`, `parse`, `NetSend`, `NetAnswer`,
  `NetOracle`, `NetDeliver`, `Switch` (`new`/`on_tx`/`due`/`set_partition`/
  `set_throttle`/`save_state`/`restore_state`), and `NetError`.
- The model: **delivery is scheduled in V-time and every fault is an operation
  on that schedule.** `on_tx` parses, consults standing faults then the oracle
  (per destination), and enqueues `0..N` deliveries; `due` drains everything due
  at or before `now` in `(at, seq)` order.
- Additions (allowed by conventions rule 3): `pub const REORDER_MAX`,
  `NodeMap::{new, insert_mac, insert_ip}`. The frozen public surface is in
  `tests/public-api.txt`.

### Module layout

`error.rs` (the `NetError` enum) · `types.rs` (the public plain data + the
`NetOracle` seam) · `parse.rs` (panic-free L2/L3/L4 parsing + the connection
identity) · `switch.rs` (the `Switch` state machine + the standing faults) ·
`codec.rs` (a strict little-endian `save_state`/`restore_state` with a
forward-only bounds-checked `Reader`).

## Key design decisions

- **Determinism by construction.** The schedule is a `BTreeMap<(VTime, seq),
  NetDeliver>`; ties at one `at` break by a monotonic `seq`, never by map order.
  Routing/broadcast use `BTreeMap`/`BTreeSet` so iteration order never reaches an
  answer (broadcast fans out in sorted `NodeId` order). No floats, no `HashMap`,
  no wall-clock, no unseeded RNG.

- **All V-time arithmetic saturates.** `T + L₀ + d` and the reorder horizon use
  `saturating_add`, so `Delay(u64::MAX)` or a `now` near `u64::MAX` clamps to
  `VTime(u64::MAX)` (delivered only if a Timeline ever reaches it) — never a debug
  panic, never a release wrap into the past. Asserted in debug-mode tests.

- **Reorder.** A `Reorder` answer holds the frame in a per-link FIFO buffer
  (link = directed `(src, dst)`). The next send on that link releases the
  held frame(s) *after* that send's own deliveries (they take the smaller seqs),
  anchored at the releasing send's **actual** scheduled time — `now + L₀`, or
  `now + L₀ + d` if that send was itself `Delay(d)`. Anchoring at the actual time
  (not merely the nominal one) is required: `seq` only tie-breaks at an equal
  `at`, so a held frame anchored at the nominal time would slip *ahead* of a
  delayed next frame, violating the reorder contract. With no later frame, `due`
  flushes the held frame exactly once at the bounded horizon
  `T + L₀ + REORDER_MAX`, so a last-frame reorder can never strand a Timeline.
  Only frames held *before* the current send are released, so two consecutive
  reorders on a link chain correctly (each released by the next).

- **Standing faults precede the oracle.** `on_tx` checks partition then throttle
  before consulting the oracle. A partition (undirected, normalized `a<=b`) drops
  matching sends in its window without an oracle call. A throttle (directed, a
  fixed-window rate limit / "clog") drops over-budget sends without an oracle
  call but lets admitted sends face the oracle normally — a rate limit on offered
  load, not a replacement decision. Both windows are **half-open `[start, end)`**.

- **Fault windows are half-open and counters are state.** A throttle's
  `(cur_index, count)` live in the switch and are serialized, so branch/replay
  reproduce the exact admit/clog boundary across a snapshot.

- **Parsing is minimal and total.** Ethernet addressing drives routing; a frame
  with a **complete** IPv4/TCP-or-UDP header additionally yields a
  direction-independent `ConnId` (FNV-1a over the sorted 5-tuple) for fault
  targeting only — it never affects routing. `conn` is `0` for anything else:
  non-IPv4, non-TCP/UDP IPv4 (e.g. ICMP), a non-first IPv4 fragment (fragment
  offset != 0 — its payload start is not the L4 header), an IHL/`total_length`
  that leaves no room for the L4 ports, or a packet truncated before its ports.
  Ports are read only when the **declared** IP `total_length` (not merely the
  captured bytes — a frame may carry trailing padding) reaches them. There is no
  ARP/bridge state machine (task non-goal). Every byte
  access is bounds-checked (`.get(..)`): malformed framing → `None`, an
  L2-routable-but-L3/L4-incomplete frame → `conn = 0`, never an out-of-bounds
  read. Node resolution is MAC first, then IPv4 address as a fallback (the spec's
  "MAC/IP ↔ NodeId").

- **`save_state` scope.** It serializes the *mutable* state: `l0`, the next
  `seq`, standing partitions/throttles (with counters), pending deliveries, and
  the held-reorder buffer — canonically (sorted `BTreeMap`/`BTreeSet` walk), so
  equal state ⇒ identical bytes and two equally-driven switches match. The
  `NodeMap` is config and is **not** in the blob: the integrator reconstructs the
  switch with the same `NodeMap` before `restore_state` (which leaves it intact).
  Decode is strict and total — bad magic/version, truncation, trailing bytes,
  non-canonical/duplicate sections, or a `seq >= next_seq` all yield
  `NetError::Malformed`, and a failed restore leaves the switch untouched (commit
  only after a fully valid parse).

## Deviations considered and rejected

- **A separate `jitter` field** (hinted in the spec's struct sketch). Omitted:
  per-send latency variation is already the oracle's `Delay(d)`, the one
  deterministic latency source. A second jitter knob would either duplicate it or
  reintroduce a non-deterministic-by-nature variance. The base latency `L₀` is
  the only construction-time latency parameter.

- **Not releasing a held reorder on a partition/throttle-dropped next frame.** A
  dropped send still *traversed the link* from the guest's view, so it counts as
  "the next frame"; releasing keeps behavior simple and the horizon is the
  backstop either way.

## Known limitations / integrator notes

- **"Real TCP replays under V-time"** remains the open, load-bearing assumption
  behind `pv-net` (see `docs/DISSONANCE.md`, "What is still open"); it needs a
  guest OS to validate. Until then this crate is gate-tested with synthetic
  frames, a fake oracle, and a fake V-time clock, exactly as the task scopes.

- **Frontier (vmm-core), not here:** the `net_tx` hypercall exit handler, the RX
  ring, raising the pv-NIC IRQ, and guest-memory frame copies; the pv-NIC guest
  driver; and the `Environment` itself (task 24, bound to `NetOracle`).

- **`ConnId` is opaque and may (astronomically rarely) hash-collide with the
  `0` "no L4" sentinel.** It only reaches the oracle for targeting, never routing
  or scheduling, so a collision cannot affect determinism.

- **`REORDER_MAX`** is a fixed constant (`1 << 20` V-time). It is part of the
  schedule, not per-send tunable; the integrator may re-pick the magnitude, which
  only changes how long a stranded last-frame reorder waits.

- **Fuzzing.** `fuzz/` is a self-contained cargo-fuzz project kept *inside* this
  crate's directory (conventions rule 1 — the repo-root `fuzz/` belongs to task
  19), with an empty `[workspace]` so the root workspace's `dissonance/*` glob
  and the `-p pv-net` gates ignore it. Target `parse_on_tx` fuzzes `parse`,
  `on_tx` (the guest-controlled entry — malformed frames must drop, every
  scheduling path must stay total), and `restore_state`. Run with the pinned
  nightly: `cargo +nightly-2026-06-16 fuzz run parse_on_tx`. The no-panic
  properties also run in the normal suite (`tests/no_panic.rs`, proptest) so the
  guarantee is gated without cargo-fuzz installed. **Ask-by-comment:**
  `libfuzzer-sys` (the standard cargo-fuzz harness crate) is outside the
  dependency whitelist; it is fuzz-only and never a library dependency.

- **CI wiring left to the integrator (root files are off-limits, rule 1):**
  the `public-api` job's `-p` list and a `fuzz` smoke job (task 19) would need
  `pv-net` added in `.github/workflows/quality.yml`. The `tests/public_api.rs`
  guard and `tests/public-api.txt` snapshot are in place and pass on the pinned
  nightly; the test skips cleanly when the tooling is absent. No `miri` entry is
  needed (no `unsafe`).

## Gates

`cargo build/nextest/clippy(-D warnings)/fmt -p pv-net --all-features` and
`cargo deny check` all pass; 44 tests + the (ignored, nightly) public-api guard.
Suite runtime ≈ 0.4 s. The clippy run also surfaces three *pre-existing*
workspace-`clippy.toml` meta-diagnostics (the `rand::*` disallowed-method paths
are unresolvable once proptest pulls `rand` into the dev dep graph); they are
emitted for every proptest-using crate, do not cite this crate's code, and do not
fail `-D warnings`.
