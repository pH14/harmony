# Task 61 — the net vertical: `net_decide` service + in-guest flow agent

**FRONTIER — the first true guest-plane fault path, end to end.** The host
*decides* a per-flow network policy (`net_decide`); the in-guest flow agent
*enforces* it on the intra-guest CNI. This document is the design + gate evidence
+ box-gate handoff.

## Surfaces touched (frontier surface list)

| Surface | What changed | Gates |
|---|---|---|
| `consonance/hypercall-proto` | `ServiceId::Net = 5` + `net_decide` (op 1) wire seam; guest `Client::net_decide`; host reference `NetDecider`/`NetFlowPoint` | portable (build/test/clippy/fmt/deny) |
| `consonance/vmm-core` | host `Net` doorbell arm: decode → `Environment::decide` → record → encode; `NetChannel` + `enable_net` + `net_decisions` | portable + **box** |
| `guest/flow-agent` (new, own workspace) | the agent: brain (answer→policy, enforcement plan, decider seam) + Linux doorbell/enforce bin | portable + **box** |
| `guest/linux/{k3s-init.sh,build-k3s-image.sh}`, `guest/flow-agent/build-static.sh` | bake + start the agent in the k3s pod→pod workload | **box** |
| `docs/INTEGRATION.md` | §1.2 ServiceId registry: `Net = 5` documented | — |

Commits (branch `task/net-vertical`, off `main`): hypercall-proto service · vmm-core
host wiring · flow-agent crate · k3s workload wiring.

## Integrator rulings (2026-07-05, confirmed before implementation)

1. **Net wire shape = environment-free proto + opaque answer bytes.**
   `hypercall-proto` is `consonance` substrate and must **not** depend on the
   `dissonance/environment` catalog (a layering inversion). So: opcode 1 =
   `net_decide`; request payload = fixed **18-byte LE** `NetFlow { src:u32,
   dst:u32, conn:u64, event:u16 }`; response = the **opaque** environment-encoded
   `Answer` bytes, which the proto frames + ferries but never interprets. The
   production host (`vmm-core`) does the decode → `DecisionPoint::NetFlow` →
   `env.decide` → record → `Answer::encode`, inline in `dispatch_doorbell`,
   mirroring the task-73 `decide_buggify` arm (production HAND-ROUTES doorbell
   frames — there is no `Dispatcher::register` in prod; the `Dispatcher`/`Service`
   path is test-only). The guest agent depends on `environment` to decode.

2. **CNI enforcement = nftables/tc-verdict prototype**, not the task-51 userspace
   L4 proxy. The agent asks `net_decide` once per flow, then installs deterministic
   in-kernel enforcement (`tc netem delay`/`tbf`, `nftables` drop/reject). It
   embeds `flow`'s `FlowPolicy` vocabulary + `FlowDecider` seam as the brain, **not**
   the `ToxiproxyEngine` byte-proxy. Consequence: **fractional** seeded-PRNG
   `NetLoss` (`den > 1`) is deferred to the proxy shell (a follow-on); full drop
   (`1/1`) and partitions are enforced as a standing `nft drop`. Box gate B
   (NetLatency + full-drop) is fully covered deterministically.

## The wire contract (`ServiceId::Net = 5`, op 1 `net_decide`)

- **Request** (18 bytes, little-endian): `src:u32 | dst:u32 | conn:u64 | event:u16`.
  `event = 0` is `FlowEvent::Open` (the sole event today; extensible).
- **Response**: the environment-encoded `Answer` (a 1-byte `Nominal` tag, or a
  small `NetLatency`/`NetLoss`/`NetThrottle`/`NetReset` fault encoding). Opaque to
  `hypercall-proto`; bounded to the response buffer, never interpreted.
- **Additive**: `Net` fills the reserved id 5 without renumbering any released
  service (`service_ids_are_a_stable_additive_registry` asserts it). Unknown
  opcode on `Net` → `UnknownOpcode`; malformed payload → `BadRequest`; unwired
  service → `UnknownService` — never a silent drop.

## Determinism discipline

Every input the agent acts on comes from a determinized source **by construction**
— the guest clock (V-time-backed), the host-answered policy — and consonance
denies it any other source. Concretely:

- `NetLatency` → `tc netem delay <µs>` in the guest's own (V-time-backed) time.
- full `NetLoss` (`num ≥ den`) / partition → a standing `nft drop` (no RNG).
- `NetReset` → `nft reject with tcp reset` (no RNG).
- `NetThrottle` → `tbf rate` (deterministic pacing).
- **fractional `NetLoss` (`den > 1`) is refused** (`EnfError::FractionalLossUnsupported`),
  because `tc netem loss` draws from the kernel's own **unseeded** PRNG — exactly
  the non-determinism this project eliminates. The seeded-PRNG path lives in the
  deferred userspace proxy (task 51's shell); the agent leaves such a flow nominal
  and logs the reason rather than mis-enforcing.

**Startup self-check.** The agent emits a determinism witness to serial
(`selfcheck urandom=<hex16> monotonic_ns=<n>` — a `/dev/urandom` read + a
`CLOCK_MONOTONIC` read) and asserts two immediate clock reads are non-decreasing.
The cross-boot equality of these witness lines is asserted by the box gate (the
bit-identical serial), and the `state_hash` proves the rest.

**Host-side isolation.** The `NetChannel` (like the SDK channel) is **never folded
into the `state_hash`**, so an agent-absent workload is byte-for-byte unchanged
(acceptance gate 4). Net decisions are host-side observation; the reproducer
carries the flow policy as seed + `FaultPolicy` (a seeded run) or as recorded
overrides (a replay).

## Divergence from task-51's abstractions (recorded per the spec)

Task 51 designed one central userspace L4 proxy (iptables REDIRECT →
`ToxiproxyEngine`) modeling delivery as a byte-stream `Deliver`/`Reset` schedule
with seeded-PRNG fractional loss in userspace. This vertical instead installs
**in-kernel** enforcement (the "nftables-verdict prototype" the spec permits).
What that corrects in task-51's abstractions:

- The `ToxiproxyEngine` byte-proxy is **unused** here; only the `FlowPolicy`
  vocabulary and the `FlowDecider` seam are embedded. The engine remains the right
  home for the deferred fractional-loss path.
- `FlowEvent::Open` carrying no V-time means a purely-idle `Reset` flow schedules
  nothing in the engine model — irrelevant to the in-kernel `nft reject`, which is
  standing. If a future proxy shell wants reset-at-accept, `Open` needs a V-time
  (a spec-level decision for the proxy-shell builder, already flagged in
  `flow/IMPLEMENTATION.md`).
- The agent hands out a **fresh monotonic `conn`** per flow (the init script
  passes `--conn`), honoring the flow-crate frontier invariant (no reusable
  5-tuple hash), so no evict-on-`Close` is needed.

## Portable gate evidence (all green on macOS dev host)

- `hypercall-proto` (`--all-features`): **25 tests** incl. `golden_net_decide_request_bytes`,
  `net_decide_round_trips_the_flow_policy`, `net_decide_rejects_an_undersized_out_buffer`,
  `net_decide_without_the_service_is_a_clean_status`, `net_decider_state_round_trips`,
  `net_decider_rejects_bad_opcode_and_payload`, `service_ids_are_a_stable_additive_registry`,
  `net_flow_point_decodes_the_fixed_wire_form`, and the `net_decide_round_trip_is_faithful`
  proptest (≥256 cases). clippy/fmt/deny clean.
- `vmm-core` (`--all-features`): **405 tests**, incl. the host record→replay closure
  `net_doorbell_decides_records_and_replays` (a fresh materialize reproduces the
  identical answer at the identical `Moment`) and `net_doorbell_rejects_malformed_requests`.
  clippy/fmt/deny clean.
- `guest/flow-agent` (own workspace): **11 tests** (8 lib + 3 decider integration:
  the `HostFlowDecider` round-trip against the reference `NetDecider` over a
  loopback `Dispatcher`, plus fail-closed-to-nominal on missing service / supply
  answer). clippy/fmt/deny clean. Bin smoke: `flow-agent … --assume-nominal
  --dry-run` prints the self-check witness + decision + (empty) plan.

## Box gates (handed to the foreman — box access fluctuates; see the memory
`harmony-box-only-gates`)

Discipline: pin every workload to a dedicated core (`box-window.sh acquire <name>`
prints the leased core; **never** `insmod`/`rmmod` KVM directly), and the last
lease out reverts to stock KVM `1396736` and prints `REVERT OK`.

Build the image with the agent baked in:

```sh
FLOW_AGENT_BIN="$(guest/flow-agent/build-static.sh)" sudo guest/linux/build-k3s-image.sh
# gate B additionally needs `nft` + `tc` binaries baked into the image.
```

- **Gate A (nominal).** Run the k3s workload with the agent active and the Net
  channel answering `Nominal` for every flow. Assert: deterministic-twice with
  `state_hash` **equal to the agent-active baseline** (agent presence is
  deterministic), and the `net_decisions()` capture shows the client→postgres flow
  decision at a **stable `Moment`** across the two runs. The `K8S61:` /
  `flow-agent: selfcheck …` serial lines must be byte-identical across runs.
- **Gate B (fault).** Configure a `FaultPolicy` making the client→postgres
  `NetFlow` sample a `NetLatency` (then, separately, a full-drop `NetLoss 1/1` or a
  standing partition). Assert: observable effect in the client pod's serial
  (retry/timeout markers), deterministic-twice, and `replay` of the recorded env
  reproduces the identical `state_hash`. **Record the run table** — this is the
  first guest-plane fault ever landed.

The host record→replay half of both gates is already proven portably by
`net_doorbell_decides_records_and_replays`; the box gates add the live CNI
enforcement + `state_hash` determinism.

## Deviations considered and rejected

- **A typed `Net` answer in `hypercall-proto`** (instead of opaque bytes) — rejected
  (ruling 1): it would force the substrate to depend on the `dissonance`
  environment catalog, inverting the layering. Opaque bytes keep the proto a pure
  framer.
- **Full userspace L4 proxy now** — rejected (ruling 2): the byte-splice shell is
  the heavier task-51 work and unnecessary for the first vertical's box gate B
  (latency + full-drop). Deferred with a clean seam.
- **A parallel `NetChannel` vs. reusing the SDK channel's `RecordedEnv`** — chose a
  parallel channel: the SDK channel is buggify-shaped (buggify-only policy gate,
  snapshot state), and a separate `NetChannel` keeps the change additive and the
  net path independent of SDK enablement (they compose on one doorbell).
- **`tc netem loss` for fractional drop** — rejected: its PRNG is unseeded/
  non-deterministic. Refused explicitly rather than silently mis-enforced.

## Known limitations / integrator notes

- **`unsafe`.** The library (`src/lib.rs`) is **unsafe-free**. The only `unsafe` is
  in the binary's `cfg(target_os = "linux", target_arch = "x86_64")` doorbell
  module — box-only FFI for the named purpose of the hypercall doorbell (`/dev/mem`
  mmap of the fixed REQ/RESP pages + `iopl` for the `OUT` port) and one
  `clock_gettime`. Each block carries a `// SAFETY:` note. It is **unreachable
  under Miri** (Miri runs on the darwin host, where the linux-gated code does not
  compile), and the crate is its own workspace (outside the root `quality.yml`
  miri `-p` list). Flagging for the integrator: the doorbell inherently needs
  privileged FFI; if a hard "no unsafe without an explicit grant" reading applies,
  the box-only module is the single place to review.
- **`libc`** is added as a `cfg(target_os = "linux")`-only dependency (on the
  dependency whitelist). `clap` for the bin (whitelist). No other new deps.
- **Fractional `NetLoss` enforcement** is deferred to the userspace proxy shell.
- **`StopMask` stays empty** (non-goal): net decisions are seed/env-answered
  locally; the reactive `run(resolve)` path is a follow-on.

## What the agent implies for harmony-linux crate-ification (task 43)

The agent + the init-script integration + the published `NetFlow` catalog are the
**harmony-linux layer's first three real artifacts**. Task 43 should lift the
brain (`harmony_flow_agent`'s `policy_from_answer` / `enforcement_commands` /
`HostFlowDecider`) and the init-library glue into the crated layer; the enforcement
mechanism table (answer → `tc`/`nft`) is the layer's first published contract.
