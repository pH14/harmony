# Box CPU-pinning discipline

> **The box alias.** `<det-box>` throughout these docs is a placeholder for *your* determinism
> box's ssh alias (defined in `~/.ssh/config`). The repo hard-codes no host; the CI scripts read
> it from the **`DET_BOX_SSH`** environment variable (`export DET_BOX_SSH=<your-alias>`), and a dev
> may keep a `Bash(ssh <your-alias>:*)` permission rule in the gitignored
> `.claude/settings.local.json`.

**Rule: every workload that runs on the determinism box (`ssh <det-box>`) must be
CPU-pinned to a dedicated physical core with its SMT sibling left idle.** No exceptions.
The box exists to produce reproducible measurements; an unpinned process shares a core
(or a hyperthread) with whatever else the scheduler put there, and that contention is a
source of non-determinism we control for free with `taskset`.

This is a determinism-hygiene rule, not a performance rule. It is the host-homogeneity
contract (see `docs/CPU-MSR-CONTRACT.md`) applied at the scheduling layer: identical CPU
+ microcode is necessary but not sufficient if two workloads fight over one core's
execution resources.

## The box

Intel Core i9-9900K — 8 physical cores, 16 threads (SMT/hyperthreading on), 1 socket,
1 NUMA node, 3.6 GHz base. SMT sibling map (`cpuN` / its sibling share one physical core):

| Physical core | Threads (`taskset -c`) |
|---|---|
| 0 | 0, 8 |
| 1 | 1, 9 |
| 2 | 2, 10 |
| 3 | 3, 11 |
| 4 | 4, 12 |
| 5 | 5, 13 |
| 6 | 6, 14 |
| 7 | 7, 15 |

To pin a workload to physical core *N* with no hyperthread contention, run it on the
**lower** thread of the pair and leave the **upper** thread (N+8) idle — e.g.
`taskset -c 2 <cmd>` uses core 2 and leaves cpu10 unused.

## Standing core assignments

Keep workloads on distinct physical cores so concurrent spikes never contend:

| Use | Pin to | Sibling left idle | Notes |
|---|---|---|---|
| OS / ssh / housekeeping | core 0 (cpu0) | cpu8 | default landing spot |
| Task 07 — PMU skid measurement | `taskset -c 2` | cpu10 | most pinning-sensitive workload |
| Task 08 — snapshot/restore latency | `taskset -c 4` (or `-c 4,1`) | cpu12 (and cpu9) | KVM vCPU ± driver thread — keep off the CI cores |
| **Self-hosted CI runner** | **cores 5,6,7** (`AllowedCPUs=5-7,13-15`) | — (uses both threads) | systemd `ci.slice` cpuset; see "CI runner isolation" below |

Cores 1,3 are spare. When a new box workload appears, give it a spare core and record it here.

## How to pin

Prefix the remote command, not the ssh:

```sh
ssh <det-box> 'taskset -c 2 cargo run --release -p pmu-count -- ...'
```

For latency-sensitive measurement you may additionally raise scheduling priority to cut
preemption jitter:

```sh
ssh <det-box> 'taskset -c 2 chrt -f 1 ./measure ...'
```

Always **record the pinned core(s), the cpufreq governor, and `no_turbo`** in the spike's
confound capture / results, so a number can be reproduced exactly.

## Frequency state

**Frequency is not a determinism concern.** The V-time clock is retired branches, which is
frequency-independent — turbo, C-states, and governor change only *wall-clock* timing, never
the count. So nothing about frequency is a requirement for correct, reproducible *V-time*
replay. It matters only for one thing: lowering run-to-run variance in **wall-clock
benchmarks** (e.g. a restore-latency table in µs).

Box-wide default (set live, non-persistent — re-apply after any reboot):

- **cpufreq governor = `performance`** (was `powersave`). Cheap, box-wide, harmless: removes
  the idle→load frequency ramp as a confound for everything. Keep it.
  `echo performance | sudo tee /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor`
- **`no_turbo = 0`** (box default — turbo enabled). We do **not** disable turbo box-wide:
  it's a global/package-level switch with no per-core form, so disabling it slows every
  build and workload on the box to buy variance reduction that only one kind of measurement
  even wants. (An earlier `no_turbo=1` was reverted as overreach.)

**Per-core frequency pinning — a benchmarking knob, opt-in, owned by the harness.** When a
spike measures *wall-clock* timing and wants a flat core, cap *that core's* max frequency to
base for the duration of the run — don't touch the rest of the box. `scaling_max_freq` is
per-core (unlike `no_turbo`):

```sh
# pin core 4 (+ its SMT sibling 12) to base 3.6 GHz for a wall-clock benchmark
for c in 4 12; do echo 3600000 | sudo tee /sys/devices/system/cpu/cpu$c/cpufreq/scaling_max_freq; done
# ... run the benchmark on -c 4 ... then restore:
for c in 4 12; do cat /sys/devices/system/cpu/cpu$c/cpufreq/cpuinfo_max_freq | sudo tee /sys/devices/system/cpu/cpu$c/cpufreq/scaling_max_freq; done
```

The harness applies this on its own core, **records the cap (and governor / no_turbo) as a
confound**, and restores afterward — exactly as it already records the pinned core. A
retired-count-only spike (task 07's skid) needs none of this; it pins and ignores frequency.

## Self-hosted CI runner isolation

The box doubles as a **self-hosted GitHub Actions runner** (GHA-hosted minutes are paused;
self-hosted runners consume none). **Provisioned idempotently by `scripts/setup-ci-runner.sh`**
(non-root `runner` user, `ci.slice` cpuset, registration, service, toolchain — that script is
the authoritative record of the setup). To keep CI from perturbing the determinism measurements,
the runner is **cpuset-confined to cores 5,6,7**, off the measurement cores (2, 4) and
housekeeping (0):

```ini
# /etc/systemd/system/ci.slice  (cgroup v2 cpuset)
[Slice]
AllowedCPUs=5-7,13-15
# optional resource bounds:
# MemoryMax=8G
# IOWeight=50
```
Run the runner service in that slice (`Slice=ci.slice`). **Every job the runner spawns
inherits the cpuset** — unlike `taskset`, a child cannot escape it. So no CI build ever
shares a physical core or SMT sibling with a measurement.

**The residual that can't be pinned away:** the i9-9900K is single-socket with one shared L3
(~16 MB) and one memory controller, and as a *client* part has **no Intel CAT** (cache
partitioning is Xeon-only). So a heavy CI build still shares L3 + memory bandwidth with a
concurrent measurement. cpuset can't fix that. Why it's fine in practice:

- **Correctness gates are contention-immune.** build, test, clippy, Miri, and the determinism
  gate (same seed twice ⇒ identical *hash*) are *content* checks — a cache-thrashed slower run
  produces the same artifact and the same hash. They don't measure time, so CI can run anytime
  without affecting its own verdict or anything's determinism *correctness*.
- **Only the latency/skid spikes (07, 08) want a quiet box.** Those are deliberate, infrequent
  runs, not on-push. Discipline: **don't run a measurement spike concurrently with CI** (pause
  the runner for that run, or just don't push during one). V-time *counts* are
  contention-independent regardless; it's only those two spikes' wall-clock numbers that care.

If even that residual ever proves to matter for the measurements, the reboot-gated `isolcpus`
on cores 2/4 below is the next lever.

## Deeper isolation (reboot-gated — not yet applied)

`taskset` pins *our* process to a core but does not stop the kernel from scheduling
*other* work (interrupts, RCU callbacks, the timer tick) onto that core. For the
strongest isolation — needed only if pinning alone proves insufficient for the PMU skid
spike — boot the box with the workload cores carved out of the scheduler:

```
isolcpus=2,4,6,10,12,14 nohz_full=2,4,6,10,12,14 rcu_nocbs=2,4,6,10,12,14
```

This requires editing the bootloader and **rebooting** (which kills any running spike), so
it is a deliberate, operator-authorized step — not something a foreman or worker applies
on its own. The current `/proc/cmdline` has none of these. Adopt only if a measurement
shows residual scheduler-contention noise that pinning + idle-sibling can't remove.

### Isolation vs. the real hypervisor (not just measurement)

When the actual hypervisor runs (not just the spikes), the same isolation is **wanted on
the vCPU-carrying cores — but as a margin measure, not as the determinism guarantee**.

The V-time clock is retired *guest* branches. A host timer tick / IPI / device IRQ that
fires while the guest is in VMX non-root mode forces a VM-exit; the host services it and
re-enters. Determinism survives this **iff**:

1. **Count-neutral exits** — the branch-retired PMU counter is scoped to guest-mode and
   *frozen across every VM-exit/entry* (Intel: VMCS `PERF_GLOBAL_CTRL` save/restore +
   guest/host MSR load lists), so host execution (IRQ handler, RCU, scheduler) adds zero
   guest branches. The interrupt then changes wall-clock time but **not** the branch count.
2. **Injection discipline** — a host interrupt never becomes a *guest-visible* event
   except at a V-time-deterministic point. A host tick reflected into the guest at a
   wall-clock-determined moment would inject non-determinism; host events stay invisible.

`nohz_full` / `rcu_nocbs` / `isolcpus` do not provide either guarantee — they reduce how
*often* the machinery is exercised (fewer involuntary exits ⇒ fewer boundary-leakage
chances ⇒ tighter skid margin, smaller invariant surface, less wall-clock jitter). This is
standard for production deterministic-VM cores (KVM-RT, the `cpu-partitioning` tuned
profile). But the model must never *depend* on it: unmaskable interrupts (NMI, MCE) can't
be eliminated, so a design where one stray interrupt breaks replay would be fatally
fragile. Isolation buys margin on top of a model that is count-neutral by construction.

Normative split: the *guest-visible* side — guest may not read/program any PMU MSR, so it
can neither observe nor perturb the V-time counter — already lives in the CPU/MSR contract
(`docs/CPU-MSR-CONTRACT.md`, `msr-pmu` class: host owns the PMU, perfmon v0, RDPMC→#GP).
The *host-side* count-neutral + injection-discipline requirements are a vmm-core/vtime
concern (the guest-only retired-branch `perf_event` counter and how it's frozen across
VM-exits) and belong in the vmm-core determinism spec, not the guest-visible contract.
Task 07's PMU skid spike validates the boundary behavior empirically.
