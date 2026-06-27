# Task 50 — network fault boundary: the host *decides*, the guest *enforces* (retire `pv-net`, add the `net_decide` flow seam)

> **DESIGN + CONTRACT-RESHAPE · supersedes the `pv-net` network locus in `docs/DISSONANCE.md`.**
> Reworks one merged crate (`dissonance/environment`, a breaking public-API change) and **retires**
> another (`dissonance/pv-net`). Land as one PR; it changes a public API and removes a workspace
> member, so it conflicts with any branch touching either crate — sequence it when the
> `dissonance/{environment,pv-net}` queue is clear.

Read `tasks/00-CONVENTIONS.md`, `docs/DISSONANCE.md` ("The guest control planes", "The guest fault
model", "Plane ≠ enforcement locus"), `tasks/24-environment.md`, `tasks/26-pv-net.md`, and
**`tasks/49-postgres-kubernetes-intra-guest.md`** (the empirical anchor — see Why) first.

## Why — the model and the shipped reality disagree

`docs/DISSONANCE.md` rules that inter-node traffic is routed **out of the guest** through a `net_tx`
hypercall to a **host L2 switch** (`pv-net`), which sees every frame and **performs** network faults
host-side on a V-time delivery schedule. That is the **one** place in the whole two-plane model where
the hypervisor *performs* a fault instead of *deciding* one — and it is the sole reason the awkward
"**Plane ≠ enforcement locus**" rule exists.

Two facts now make it wrong:

1. **The shipped topology doesn't route through the host.** Tasks 38/48/49 run the "nodes" of a
   distributed system as **containers/pods in one single-vCPU guest**, and their traffic transits the
   **in-guest CNI** (bridge + veth + netns) — it never leaves the guest. Task 49 states it outright:
   *"Networking stays intra-guest … it does not route through the hypervisor host, so pv-net is
   explicitly out of scope."* `pv-net` models a **multi-VM-bridged-across-host** world the project
   decided against. There is no host-side frame stream for it to switch.

2. **Intra-guest networking is already deterministic — with zero hypervisor data-path involvement.**
   Task 49's whole point is that pod-to-pod traffic over the real Linux stack replays bit-identically,
   because consonance already determinizes the only two things that could make it vary: **the clock**
   (guest TSC/LAPIC = V-time) and **entropy** (`/dev/urandom` fed by the entropy hypercall). The
   substrate's determinism contract *already* guarantees that anything the guest does — including
   running a network — is deterministic.

Put together: the host doesn't need to *see* the traffic (it's intra-guest), and it doesn't need to
*enforce* determinism on it (the substrate already does). So the host-side switch buys nothing, while
costing a per-frame hypercall on the data path, a stateful L2 switch + V-time scheduler in the
substrate, and a separate snapshot-serialization surface for the switch state.

**The fix — the host *decides*, the guest *enforces*.** A network fault becomes a per-**flow**
guest-plane decision: a harmony-linux guest utility asks the hypervisor *"what should I do with this
flow?"* (`net_decide`), the hypervisor **answers** (a flow `Fault` policy, recorded into the
`Moment`-keyed `Environment` so it replays), and the guest utility **enforces** the answer on the
intra-guest CNI using Linux's own mechanisms. This folds networking back into the
`decide(point) -> Answer` seam — exactly like block I/O and entropy — and **dissolves the
"Plane ≠ enforcement locus" exception**: every guest-plane fault is now decided by the host and
enforced by the guest.

## The ruling (record in `docs/DISSONANCE.md`, Deliverable D)

- **Two planes, by enforcement as well as decision:**
  - **Host plane** — the hypervisor decides **and** enforces, acting *on* the guest from outside
    (corrupt memory, skew the clock, modulate the CPU, inject an interrupt). No guest code is
    involved; there is no service point.
  - **Guest plane** — the hypervisor **decides** (answers a service the guest asked about, recorded
    by `Moment`); the **guest enforces** by acting on the answer. The hypervisor never performs the I/O.
- **Networking is a guest-plane decision, enforced in-guest.** Per-**flow**, not per-frame. The host
  is in the **control** path (low-frequency, recorded), never the **data** path.
- **`pv-net` (the host L2 switch + V-time delivery scheduler) is retired.** It modeled a host-routed
  multi-VM topology the project does not use.
- **"Plane ≠ enforcement locus" is removed.** It existed only to justify `pv-net`.

## Deliverable A — reshape the `dissonance/environment` network catalog (per-frame → per-flow)

`environment` is **merged** (task 24); this is a **breaking public-API change** — update
`tests/public-api.txt` + the codec/replay/golden tests in the same PR. It composes cleanly with
task 45 (which widens the recorded value to `Action = Host | Guest` on a `Moment` axis): the network
*Answer* slots into `Action::Guest` unchanged.

`DecisionClass::NetSend` (discriminant **4**) → **`NetFlow`**. **The discriminant 4 is preserved** —
`control-proto`'s `StopMask` bit 4 mirrors it (conventions rule 2), so the wire is unaffected by the
rename.

```rust
// BEFORE (task 24, per-frame — the switch saw every frame):
DecisionPoint::NetSend { src: NodeId, dst: NodeId, conn: ConnId, len: u32 }
Fault::NetDrop | NetDelay(VTime) | NetReorder | NetDup | NetCorrupt(CorruptSpec)

// AFTER (per-flow — the guest utility asks once per flow/connection):
pub enum FlowEvent { Open }            // fires when the utility first sees a flow; #[repr(u16)], extensible
DecisionPoint::NetFlow { src: NodeId, dst: NodeId, conn: ConnId, event: FlowEvent }

// the answer is a flow-level POLICY the guest enforces (Answer::Nominal = deliver normally):
Fault::NetLatency(VTime)               // add d of guest-time delay (netem) — d in V-time units
     | NetLoss { num: u16, den: u16 }  // drop a fraction (seeded); num/den, 1/1 = full drop
     | NetThrottle { bps: u32 }        // bandwidth cap (tbf)
     | NetReset                        // RST / refuse the connection
// Block / Process Fault variants unchanged.
```

- **Remove** the per-frame variants (`NetDrop`/`NetDelay`/`NetReorder`/`NetDup`/`NetCorrupt`) and
  `CorruptSpec`. Per-**message** faults (reorder/dup/corrupt a *specific* message) need message
  boundaries the network layer cannot see; they move to the **SDK/L7 tier** (a later task) — note this
  in the catalog doc, don't silently drop the capability.
- **`StandingFault` keeps `partition`/`throttle`** (correlated, V-time-windowed, link-level): a
  partition is "drop all on link a↔b in [t0,t1)", still recorded in `EnvSpec::Recorded.standing`, but
  **enforced guest-side** by the utility (e.g. an nftables rule for the window) — no host switch
  consults it.
- The `decide` logic is unchanged in nature: `SeededEnv`/`RecordedEnv` still answer a `NetFlow` point
  from seed/overrides. Only the catalog *shape* changes. `bump CATALOG_VERSION`.

## Deliverable B — retire `dissonance/pv-net`

- Remove the crate (`dissonance/pv-net/`): the `Switch`, the V-time delivery scheduler, the reorder
  buffer, `on_tx`/`due`, `set_partition`/`set_throttle` *enforcement*, `save_state`/`restore_state`.
  None of it has a consumer once enforcement is guest-side and traffic is intra-guest.
- The `members` glob (`dissonance/*`) auto-drops it on directory removal; confirm `cargo build` /
  `cargo deny` / `cargo public-api` are green with it gone and **no crate references it**
  (`git grep -nI 'pv[-_]net'` returns only intentional historical/doc references).
- Preserve, don't reinvent: the `NodeId`/`ConnId` newtypes already live in `environment`; the
  address↔node mapping the *decision* needs (which link a flow is on) is carried by
  `NetFlow { src, dst }` directly, so no host-side `NodeMap` survives. If any helper is worth keeping,
  fold it into `environment`; otherwise it lives in git history (returnable if host-side L2
  byte-fuzzing is ever wanted).

## Deliverable C — pin the `net_decide` request/response *shape* (contract only)

The in-band channel by which the guest utility asks. **Pin the shape here; the hypercall-service
wiring and the utility are out of scope** (frontier / next task — see Non-goals):

- **Request** (guest → host): a `NetFlow` decision point (`src, dst, conn, event`).
- **Response** (host → guest): an `Answer` for the `NetFlow` class — `Nominal` or `Fault(Net*)` above —
  encoded by `environment`'s existing `Answer::encode`/`decode` (the same opaque-bytes path
  `control-proto`'s `Answer(Vec<u8>)` and the in-band transport already carry). **No new codec.**
- **Frequency contract:** **one decision per flow/connection** (+ standing link policies consulted
  locally by the utility), **not per frame** — state this as the load-bearing difference from `pv-net`.

## Deliverable D — update `docs/DISSONANCE.md`

Rewrite the affected spots to the ruling above: the "Network locus: host-side `pv-net`" subsection
(→ guest-enforced, host-decided, per-flow), the "Plane ≠ enforcement locus" paragraph (→ removed, with
a one-line note that it existed only for `pv-net`), the guest-fault-model network table (→ flow-level
policy; partition still standing), the "What is still open" TCP note (→ now validated *end-to-end in
the guest* by tasks 38/49, not a host-schedule composition), and the Crates table (`pv-net` row
removed; `environment` owns the `NetFlow` seam). This PR already adds the `> Superseded by tasks/50`
pointer; replace it with the full rewrite.

## Determinism

- **The enforcement-determinism discipline (the new load-bearing contract).** Because the enforcer
  runs *in* the guest, it inherits the substrate's determinism **iff** it takes every input from a
  determinized source: delays measured in **guest time** (= V-time), random drops/loss from a
  **seeded** PRNG (seeded from the decision) or the entropy hypercall, never a host wall-clock or
  unseeded host RNG. It *cannot* reach a non-determinized source — consonance denies them — but the
  spec must state it so the future utility (next task) is held to it. Empirical proof the premise
  holds: task 49 runs a full k8s network stack intra-guest, deterministic-twice.
- The decision points are deterministic: a `net_decide` hypercall lands at a precise `Moment`
  (retired-instruction count); the same `Environment` answers it identically on replay. Property test
  (≥256 cases): a `NetFlow` decision sequence answered by a `RecordedEnv` reproduces bit-identically;
  the reshaped catalog round-trips through `EnvSpec::encode`/`decode`; no `HashMap` order reaches an
  `Answer` or an encoded byte.

## Acceptance gates

Beyond the standard suite (on the reshaped `environment`, with `pv-net` removed):

1. **Catalog replay + codec.** The `NetFlow`/flow-policy catalog round-trips; a recorded `NetFlow`
   answer sequence replays bit-identically; off-version / malformed bytes reject cleanly (never
   panic). Golden answers for at least one seed across the `NetFlow` class.
2. **Discriminant stability.** A test pins `DecisionClass::NetFlow as u16 == 4` so `control-proto`'s
   `StopMask` bit is unchanged; a round-trip through a `StopMask` arming the network class still
   selects it.
3. **`pv-net` is gone, cleanly.** Workspace `build` / `deny` / `public-api` green without it;
   `git grep -nI 'pv[-_]net'` returns only intentional historical/doc references.
4. **Doc ruling lands (D).** `docs/DISSONANCE.md` reflects host-decides/guest-enforces, the `pv-net`
   retirement, and the removed "Plane ≠ enforcement locus" rule.
5. **`IMPLEMENTATION.md`** records the catalog before/after, the per-frame→per-flow rationale, and the
   enforcement-determinism discipline.

## Non-goals

- **The guest flow utility itself** — *what* enforces the policy on the intra-guest CNI (an
  eBPF/tc-netem/nftables hook, or a userspace L4 proxy the pods route through), its config, and its
  placement. **This is the next task, deferred deliberately.** Task 50 defines the *seam and the
  ruling*, not the enforcer.
- **The `net_decide` hypercall-service wiring** (a new `ServiceId` in `hypercall-proto` + the
  `vmcall-transport`/`vmm-core` dispatch + the guest-side issuing path) — frontier glue, built with
  the utility.
- **Per-message (reorder/dup/corrupt) and L2 byte-corruption faults** — the SDK/L7 tier (a later
  task); noted in the catalog, not built here.
- Any change to the host plane (task 45), the explorer (task 12), or the determinization mechanism
  (RDTSC/RDRAND/V-time unchanged).
