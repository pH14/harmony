# Task 61 — the net vertical: `net_decide` service + in-guest flow agent (first guest-plane path)

> **FRONTIER · the first true guest-plane fault path, end-to-end.** DISSONANCE.md's ruling —
> host *decides* a per-flow policy, guest *enforces* it on the intra-guest CNI — is fully
> designed and 0% wired: no `net_decide` hypercall service exists on either side
> (`hypercall-proto` stops at `ServiceId::Event=4`), `dissonance/flow` is an enforcer brain with
> no body (its own docs defer the proxy shell), and today's real-Linux path traverses **zero**
> hypercall seams (`docs/REVIEW-2026-07.md` gap #3). This task builds the vertical:
> a `Net` hypercall service (host side), an in-guest **flow agent** (the first real consumer of
> `vmcall-transport`), and enforcement on the CNI — proving one guest-plane fault decided by the
> host, enforced in the guest, recorded, and replayed bit-identically.
>
> Depends on **task 58** (server + recorded-env plumbing). Independent of 59/60 — can run in
> parallel after 58. **This is the largest Wave-4 task; the foreman may split it** (61a =
> service + agent skeleton + nominal round-trip; 61b = enforcement + fault gate) if one PR gets
> unwieldy.

Read first: `tasks/00-CONVENTIONS.md`, `docs/DISSONANCE.md` ("Networking: a per-flow guest-plane
decision…"), `tasks/50-net-fault-boundary.md` (the `NetFlow` catalog + `net_decide` shape),
`tasks/51-flow.md` (the FlowEngine brain + the deliberately-deferred proxy shell),
`dissonance/environment/src/catalog.rs` (`NetFlow`, `FlowEvent`, the flow-policy faults),
`consonance/hypercall-proto/src/lib.rs` (`ServiceId`, framing),
`consonance/vmcall-transport/src/lib.rs` (the guest-side doorbell — unused until now),
`guest/linux/k3s-init.sh` + `runc-init.sh` (CNI/netns layout the agent must live in).

## Environment

Host-side service + decision plumbing: mock-testable on macOS + Linux. Guest agent: builds as a
static Linux binary (it may be Linux-only — it *is* harmony-linux code; the no-`cfg(target_os)`
rule does not apply to guest-resident code, note this in the PR). End-to-end proof is
**box-only** on the runc-Postgres or k3s workload. Standard pinning + revert discipline.

## What to build

1. **Host: the `Net` hypercall service.** New `ServiceId::Net = 5` in `hypercall-proto`
   (additive; bump per the crate's versioning rules). The service handles `net_decide`: decode a
   `NetFlow { src, dst, conn, event }` decision point, consult the active `Environment`
   (`decide(point) -> Answer`), stamp the decision at its `Moment` into the `Recorded` env
   (task 45 plumbing), answer with the flow policy bytes. **One decision per flow/connection,
   never per frame** — the host is on the control path only.
2. **Guest: the flow agent.** A static binary baked into the workload initramfs, started by the
   init script: intercepts new flows on the intra-guest CNI (the task-51 design: iptables
   REDIRECT to a central L4 proxy; a simpler nftables-verdict prototype is acceptable if the
   redirect proves heavy — document the choice), asks `net_decide` over `vmcall-transport`, and
   enforces the answer with in-guest mechanisms per the catalog table: `NetLatency` → netem
   delay in **V-time-backed** guest time, `NetLoss` → seeded drop, `NetThrottle` → tbf,
   `NetReset` → RST, partitions → standing nftables rule. Embed `dissonance/flow`'s engine as
   the brain where it fits; where it doesn't, record the divergence in `IMPLEMENTATION.md` so
   task-51's abstractions get corrected rather than silently bypassed.
   **Enforcement-determinism discipline:** every input the agent acts on must come from a
   determinized source (guest clocks, seeded entropy, the answered policy) — it has no other
   sources by construction; assert, don't assume (a startup self-check that `/dev/urandom`,
   clock reads, and timerfds behave deterministically under two boots is cheap and worth it).
3. **Wire into one workload** (runc-Postgres client→server, or k3s pod→pod): the init script
   starts the agent before the workload; nominal path answers `Nominal` for every flow.

## Acceptance gates

1. **Portable:** service decode/decide/record round-trip proptests; agent brain logic (flow
   crate integration) unit-tested; `hypercall-proto` additive-versioning tests.
2. **Box gate A (nominal):** workload with the agent active and all-`Nominal` answers is
   deterministic-twice with `state_hash` equal to agent-active baseline (agent presence is
   deterministic), and every flow decision appears in the recorded env at a stable `Moment`
   across the two runs.
3. **Box gate B (fault):** a `NetLatency` and a full-drop (`NetLoss 1/1` or standing partition)
   policy on the workload's client→server flow: observable effect (retry/timeout markers in
   serial), deterministic-twice, and `replay` of the recorded env reproduces the identical
   `state_hash`. This is the first guest-plane fault ever landed — record the run table.
4. Standard suite green; agent-absent workloads byte-identical (everything is additive).

## Non-goals

Per-message faults (reorder/duplicate/corrupt — SDK/L7 tier, deferred by task 50); reactive
`run(resolve)` surfacing of net decisions to the explorer (`StopMask` stays empty — decisions are
seed/env-answered locally; the reactive path is a follow-on once a campaign wants to steer flows
interactively); block/entropy/process guest-plane services; harmony-linux crate-ification
(task 43 — but note in `IMPLEMENTATION.md` what the agent's existence implies for it: the agent +
init library + published catalog are the layer's first three real artifacts).
