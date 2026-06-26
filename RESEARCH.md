# Deterministic Hypervisor — Research Notes

Research on Antithesis's deterministic hypervisor and the surrounding landscape, gathered
2026-06-10 via a multi-agent search/verify pass (24 sources, 120 extracted claims, 18
adversarially verified 3–0 against primary sources) plus synthesis. Citations inline.

## 1. How Antithesis works (verified)

### Lineage
- The approach descends directly from **FoundationDB's deterministic simulation testing**:
  before writing the database, the FDB team wrote a fully deterministic event-based network
  simulation; any failing run could be replayed indefinitely from its random seed.
  ([Antithesis blog](https://antithesis.com/blog/is_something_bugging_you/),
  [Strange Loop 2014](https://www.thestrangeloop.com/2014/testing-distributed-systems-w-slash-deterministic-simulation.html))
- FDB achieved determinism at the **language/library level** — disks, network links, and
  machines replaced with software mocks (Flow actor framework, single-threaded). The cost: you
  must write your whole system that way (FDB even replaced ZooKeeper to avoid an external
  dependency). Will Wilson (FDB engineer, Strange Loop 2014 speaker) co-founded Antithesis to
  remove that constraint.

### The core move
- Antithesis "went all out and wrote a hypervisor which emulates a deterministic *computer*",
  so **any unmodified software** inside it becomes deterministic — no rewrites, no mocks.
  ([blog](https://antithesis.com/blog/is_something_bugging_you/))
- Every bug found is perfectly reproducible, even across multiple networked services, because
  the entire distributed system runs inside the deterministic boundary.
- Determinism is dual-purpose: it enables reproduction **and** the bug-finding search itself
  ("our deterministic hypervisor isn't just about getting perfect reproducibility… it also
  helps us find the bugs in the first place" — [sdtalk](https://antithesis.com/blog/sdtalk/)).

### Implementation details (mostly from the BSDCan-era talk, youtube 0E6GBg13P60, and the FreeBSD Foundation writeup)
- **Base**: heavily modified **FreeBSD bhyve** ("the Determinator"), Intel VMX only at the
  time of the talk; project began ~2019 on FreeBSD 11. Chosen over KVM/Xen for its simple,
  strippable codebase.
  ([FreeBSD Foundation](https://freebsdfoundation.org/antithesis-pioneering-deterministic-hypervisors-with-freebsd-and-bhyve/))
- **Concurrency**: they don't solve deterministic multicore — they **disable concurrency**.
  One vCPU per VM; one VM per physical core for throughput. A distributed system is packed
  into one guest as **containers connected by a fault-injecting virtual network bridge**.
- **Time**: a single virtual time source ("V-time") feeds every guest-visible clock (TSC,
  HPET, …). V-time advances as a function of **work performed**, measured with Intel PMC
  performance counters — N units of guest work always yields the same clock reading.
- **I/O**: real device models removed. The only quasi-real device is an AHCI CD-ROM serving a
  read-only live-CD guest image. Everything else goes through a **side-effect-free hypercall
  channel built on VMCALL**. Host-injected interrupts are scheduled at chosen points in
  virtual time for push-style input. Guest `/dev/random`/`/dev/urandom` are fed through the
  channel.
- **Guest kernel**: modified where Linux itself is nondeterministic — e.g. lazy TLB
  invalidation causing spurious faults ("do the nice thing, not the fast thing").
- **Snapshots/branching**: copy-on-write via **Intel EPT**. All guest memory marked read-only;
  writes trap as EPT faults. An incremental snapshot stores only changed pages plus
  registers/VMCS/emulated-device state (a few KB). **Restore = remapping EPT entries**, not
  copying memory. Read-only snapshots (e.g. a ~20 GB post-boot image) are shared across many
  VMs on one machine through the common vmm kernel module — boot once, share everywhere.
- **Memory dedup**: CoW dedup across all parallel VMs (Wilson: "a thing that I don't know
  anybody else whose hypervisor can do").
  ([SE Radio 685](https://se-radio.net/2025/09/se-radio-685-will-wilson-on-deterministic-simulation-testing/))
- **Exploration layer**: when interesting/rare behavior is observed, copy the entire system
  state and explore many futures from that point; branching happens **thousands of times per
  test run**. ([how it works](https://antithesis.com/product/how_antithesis_works/))
- **Nondeterminism controlled at the VM boundary**: external network, disk I/O, system time,
  OS scheduling/concurrency, randomness. Determinism + recorded seed = perfect reproduction.

### Unverified but plausible (verification was cut off, not refuted)
- Each guest runs on a single core; parallelism comes from many deterministic VMs exploring
  different state-space regions (consistent with the verified one-vCPU-per-VM claim).
- The whole-VM snapshot save/restore is effectively instantaneous (consistent with the
  verified EPT-remap restore claim).

## 2. Prior art / landscape map

| System | Layer | Approach | Key lesson |
|---|---|---|---|
| FoundationDB simulation | Language/runtime | Single-threaded actor runtime, mocked net/disk/time, seed-driven | Gold standard for DST, but requires writing your software inside the harness |
| **Antithesis** | Hypervisor | Deterministic computer; unmodified guests | The VM boundary is the only place that covers *all* software |
| ReVirt (OSDI '02) | VMM | Log-and-replay: record async-event timing + external input, replay instruction-exact | Nondeterminism = (1) async event timing, (2) external input — that's the whole list |
| VMware deterministic replay / FT | VMM | Same record/replay idea, productized for fault tolerance (uniprocessor only) | Multiprocessor replay was abandoned — too expensive |
| XenTT | VMM (Xen) | Deterministic replay for Xen guests | Branch counters + landing-pad single-step to hit exact injection points |
| mozilla **rr** | ptrace/process | Record/replay of process trees; retired-conditional-branch PMU counter + signal interrupt to replay async events at exact points | The PMU "instruction position" technique; counter overcount/skid handling; single-core scheduling |
| Hermit (Meta) | ptrace sandbox | *Deterministic execution* (not replay) of unmodified Linux programs; virtualized time/rng/scheduling | Closest open-source analogue in spirit; process-level, fragile vs whole-VM |
| QEMU icount + record/replay | Emulator (TCG) | Virtual clock advances per translated instruction; record nondeterministic inputs | Determinism is nearly free under full emulation — at ~10-50x slowdown |
| Nyx / kAFL (Sec'21) | KVM + fuzzer | Fast VM snapshot/restore fuzzing of hypervisors/OSes, dirty-page reset, Intel-PT coverage | The snapshot-fuzzing loop mechanics: dirty-page reset at very high rates |
| Firecracker / Cloud Hypervisor | KVM VMM | Minimal device models, snapshot/restore (not deterministic) | rust-vmm building blocks; minimal-device philosophy |
| dOS, Determinator, DMP/Kendo/CoreDet/Dthreads | OS/runtime research | Deterministic multithreading via logical time / token passing | Deterministic *multicore* is possible but slow & complex — Antithesis was right to sidestep it |
| TigerBeetle VOPR, sled, turmoil, madsim, Shuttle, Loom | Language-level DST | Seed-driven simulation in Zig/Rust ecosystems | The application-level renaissance; all share the FDB constraint |
| WarpStream, Resonate, Polar Signals | Industry DST users | madsim-style or GOOS-based simulation of whole SaaS | DST works in production engineering, not just databases |

Useful aggregators: [awesome-deterministic-simulation-testing](https://github.com/ivanyu/awesome-deterministic-simulation-testing),
[Phil Eaton's DST notes](https://notes.eatonphil.com/2024-08-20-deterministic-simulation-testing.html),
[databases.systems open-source-Antithesis series](https://databases.systems/posts/open-source-antithesis-p1).

## 3. Design takeaways for a from-scratch build

1. **ReVirt's reduction is the spec**: a single-vCPU VM is deterministic iff (a) every
   nondeterministic-result instruction is trapped and given a deterministic answer, and
   (b) every asynchronous event (interrupt) is injected at a reproducible instruction
   boundary. Everything else follows.
2. **Don't fight multicore.** One vCPU per VM. Scale by running one VM per host core.
   Simulate distribution with containers + a deterministic, fault-injecting bridge inside
   the guest.
3. **Virtual time = f(work).** Derive every guest clock from one V-time counter advanced by a
   PMU count of guest progress (retired instructions/branches). Trap RDTSC; never let real
   time leak in.
4. **The precise-injection problem is the hard core.** PMU counters have skid/overcount;
   rr/XenTT solve it by programming the counter to overflow *early*, then single-stepping to
   the exact target count. Budget real effort here.
5. **Instructions to trap on x86/VMX**: RDTSC/RDTSCP (TSC exiting), RDRAND/RDSEED (secondary
   exec controls), CPUID (always exits), RDPMC, MONITOR/MWAIT/PAUSE-loop exiting, and avoid
   exposing anything you can't determinize (no waitpkg, no AMX timing oddities — control via
   CPUID filtering).
6. **No real devices.** Read-only boot medium + a paravirtual, side-effect-free hypercall
   channel (VMCALL doorbell) for console/entropy/block/external-net. Interrupts injected only
   at chosen V-times.
7. **Snapshots are EPT games, not memcpy.** Write-protect everything; dirty-page tracking
   gives you incremental snapshots of a few KB + changed pages; restore by remapping. Share
   the big post-boot image read-only across all VMs.
8. **Guest cooperation is allowed.** Antithesis patches the guest kernel where Linux is
   internally nondeterministic (lazy TLB). A custom minimal kernel config (no SMP, tsc=reliable,
   no watchdogs) eliminates whole classes of problems.
9. **Determinism is testable.** Run the same seed twice; hash registers + dirty pages at every
   exit (or every N instructions); bisect divergence by instruction count. Build this harness
   before building features.
10. **The fuzzer rides on top.** Coverage/"interestingness" signals via the hypercall channel;
    branch (snapshot + perturb seed) at interesting states; thousands of branches per run.
