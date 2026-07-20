# harmony-flow-agent — implementation notes (task 61)

The in-guest flow agent: the host decides a per-flow net policy (`net_decide`),
this agent enforces it on the intra-guest CNI. Full design + box-gate handoff live
in the repo-root **`docs/history/IMPLEMENTATION-task61.md`**; this file is the crate-local
summary conventions ask for.

## Layout

- `src/lib.rs` — the **brain** (portable, unit-tested on macOS, **zero `unsafe`**):
  `policy_from_answer` (`environment::Answer`/`Fault` → `flow::FlowPolicy`),
  `enforcement_commands` (`FlowPolicy` → deterministic `tc`/`nft` argv),
  `HostFlowDecider` (a `flow::FlowDecider` over the hypercall `Client`, failing
  closed to `Nominal` on any error), and the `selfcheck_agree` comparator.
- `src/main.rs` — the one-shot Linux binary: self-check witness → `net_decide` over
  the `hypercall-doorbell` doorbell → install enforcement. The privileged doorbell
  (`/dev/mem` mmap + `iopl`) is `cfg(linux + x86_64)` only.
- `tests/decider.rs` — the `HostFlowDecider` round-trip against the reference
  `NetDecider` over a loopback `Dispatcher` (proves the decider speaks the real
  wire protocol).
- `build-static.sh` — build the static musl binary for baking into the initramfs.

## Deviations / notes

- Own workspace (like `harmony-linux/sdk`), so the root `cargo deny`/`clippy` do not fold
  it in; run its gates from `harmony-linux/flow-agent/`.
- Embeds `flow`'s `FlowPolicy` vocab + `FlowDecider` seam, **not** the
  `ToxiproxyEngine` byte-proxy (integrator ruling 2). Fractional `NetLoss`
  (`den > 1`) is refused (`EnfError::FractionalLossUnsupported`) — it needs the
  deferred seeded-PRNG userspace proxy; full drop / partitions use `nft drop`.
- **`unsafe`** is confined to the box-only doorbell module (FFI for the named
  purpose of the hypercall doorbell, each block `// SAFETY:`-noted); the library is
  unsafe-free and the unsafe path is unreachable under Miri (darwin host).
- Guest-resident harmony-linux code, so the no-`cfg(target_os)` portability rule is
  waived (task spec Environment note); the brain is nonetheless target-agnostic and
  builds + tests on the dev host.
