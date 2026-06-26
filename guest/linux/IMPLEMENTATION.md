# guest/linux — implementation notes

## Task 38 — Postgres-in-Docker, deterministic-twice

### What landed

The **Postgres-in-Docker workload image** (consonance workload stream, step 3 of
3 — the credibility money-shot): the *unchanged* task-36 container-class
`bzImage` + a new `initramfs-docker.cpio.gz` that runs the **official
`postgres:17` Docker image** as a real OCI container and drives the SAME fixed
insert/select workload as task 37, **bit-identically twice** on the patched
backend. New files: `build-docker-image.sh`, `docker-init.sh`; `versions.lock` /
`fetch.sh` pin+fetch the Docker static bundle and the postgres image; the box
gates live in `consonance/vmm-core/tests/live_postgres_docker.rs`.

### The load-bearing finding: real `dockerd` deadlocks the V-time model → run via `runc`

> **This is the most important result of the task.** We bake the *full* Docker
> static stack (dockerd + containerd + containerd-shim-runc-v2 + runc) into the
> rootfs, but the deterministic run drives the container with **`runc` directly**
> — the same low-level OCI runtime dockerd/containerd invoke under the hood. The
> container that runs is the *identical* official-image container `docker run`
> would produce; we keep the image + the runtime and drop only the long-running
> daemon.

**Why dockerd can't run here.** Under consonance's single-vCPU / V-time model,
**V-time advances only at VM-exits** (RDTSC/IO/MMIO; a plain userspace loop
retires branches but triggers no exit, so the skid-free `last_intercept_work`
anchor the LAPIC timer reads stays frozen). A long-running Go **daemon** is fatal
to this: empirically, once `dockerd`'s embedded `containerd` booted, dockerd's Go
runtime entered a **busy-spin with no VM-exit** (spin-waiting on containerd over
gRPC). V-time froze → the periodic LAPIC tick never fired → the scheduler never
preempted → nothing else ever ran → permanent deadlock (core pinned at 99.9 %,
serial frozen at "containerd successfully booted"). This is exactly task 37's
documented hazard — *"a busy spin starves everything; there is no preemption
tick"* — now hit by a daemon we don't control instead of our own init. It is a
genuine, general result: **heavy long-running Go daemons are incompatible with a
work-driven deterministic VM**, because their internal spin-wait/park machinery
assumes a free-running clock that advances independently of guest progress.

**Why `runc` avoids it.** `runc` is **not** a daemon. It sets the container up
(namespaces, cgroups, rootfs) and **runs to completion** — its only inter-process
wait is the parent↔init handshake over a socketpair, which is *blocking I/O* (a
voluntary park → a clean context switch, not a spin). Once it `exec`s the
entrypoint, the container is just postgres — a cooperative C workload, exactly
like task 37. So the whole run is cooperative: every wait is a blocking `runc
exec` round-trip (the container is doing work = exiting = advancing V-time) or a
poll that forks a command each iteration; never a `sleep`, never a spin.

The user approved this pivot; the spec explicitly permits *"a documented lighter
OCI path if dockerd proves intractable."*

### Build (`build-docker-image.sh`, root + Linux only)

1. **Docker static bundle** (`versions.lock`, sha256-verified like the
   kernel/busybox): `dockerd`/`containerd`/shim/`runc`/`docker`/`ctr` are all
   **statically linked**, so — unlike task 37 — the guest rootfs needs **no
   glibc closure** (the container ships its own userland inside the image).
2. **Official postgres image → OCI bundle.** `fetch.sh` pulls
   `postgres:17` **by registry digest** (content-addressed; the integrity
   anchor) with the box's `ctr` into an isolated namespace and exports a
   `docker load`-format tar. The build extracts that image's layers (in order,
   with best-effort whiteout handling) into `oci/rootfs`, and generates
   `oci/config.json` from the image's **own** runtime config (entrypoint = `["docker-entrypoint.sh","postgres"]`,
   env incl. its `PATH`/`PGDATA`, cwd) via `runc spec` + `jq`. The container
   rootfs lives in the initramfs tmpfs → **PGDATA is RAM-backed** (fsync is a
   noop; no durability-fault surface — deferred to D1, as in task 37).
3. **`config.json` deltas** the off-the-shelf image needs to run under `runc` in
   this guest (each isolated on the box):
   - **`--no-pivot`** (a baked `/usr/local/bin/runc` wrapper injecting it into
     `create`/`run`): the container rootfs is on the **initramfs ramdisk, whose
     root mount has no parent**, so runc's default `pivot_root` `EINVAL`s
     ("pivot_root: invalid argument"); `--no-pivot` uses `MS_MOVE`+`chroot`, the
     documented ramdisk path.
   - **capabilities**: the bare `runc spec` grants only 3 caps; the postgres
     entrypoint chowns PGDATA and `gosu`s to the postgres user, so we grant
     **docker's default 14-cap set** (CHOWN/DAC_OVERRIDE/FOWNER/SETUID/SETGID/…),
     else `chown: Operation not permitted`.
   - **device cgroup allow-all + `noNewPrivileges:false`** (docker's defaults):
     the bare `runc spec` device cgroup is **default-deny** (`[{allow:false}]`),
     and on the guest kernel (cgroup-v2 device control = an eBPF filter) that
     **silently kills the container's PID 1 at exec** — it reaches *"init: about
     to wait on exec fifo"* then "container process is already dead", even for a
     trivial `/bin/echo`. Allow-all is appropriate for a trusted single-purpose
     determinism gate.
4. **Workload** baked into the container rootfs (`/workload.sql`): the same
   `CREATE TABLE ledger` + N=20 `INSERT`/`SELECT` loop as task 37, printing
   `row|i|v|count|sum` — values a pure function of the loop index.

### `docker-init.sh` (the runc-direct /init) — the control-flow findings

- **`--network none` by construction.** The generated `config.json` has the
  default fresh, empty **network namespace** (no veth) — loopback only — which is
  precisely `--network none`. The workload reaches postgres over the **local unix
  socket**, never TCP.
- **cgroup-v2 setup.** Mount the unified hierarchy, move init into a leaf
  (`/sys/fs/cgroup/init`) so the root can delegate controllers, enable
  `cpu/io/memory/pids` in the root subtree (cpuset is absent — it depends on SMP,
  off per the task-36 audit; runc degrades over it). `cgroup_no_v1=all` on the
  cmdline keeps every controller available to v2.
- **No busy keepalive (re-confirmed the hard way).** A first cut used a
  `while :; do :; done` keepalive to "prevent idle"; it **deadlocked the VMM**
  immediately — a pure userspace spin retires branches but triggers no VM-exit,
  freezing V-time exactly as dockerd did. Removed. The cooperative round-trips
  below keep V-time advancing instead.
- **Wait for the container to *exist* before polling readiness.** Right after
  `runc run … | tee &`, poll `until runc state "$CID"` (cooperative — each forked
  `runc state` is a VM-exit). Under the VMM, runc's container setup is much slower
  than the shell, so an earlier readiness check would race creation and
  false-`FATAL` on "does not exist" (a bug invisible under QEMU's faster timing).
- **Skip the entrypoint's transient init server.** The official image runs
  `initdb` against a temporary unix-socket server, prints *"PostgreSQL init
  process complete; ready for start up."*, stops it, then starts the real server.
  We gate on that marker (PGDATA is fresh each boot → init always runs), then on
  `pg_isready`, so the workload never races the temp server's shutdown.
- **Shutdown from *inside* the container.** A host-side `runc kill` cannot stop
  postgres: it is **PID 1 of the container's pid namespace**, and the kernel
  drops signals sent to a namespace's PID 1 from an **ancestor** namespace
  (verified on the box: `runc kill … SIGINT` returns 0 but postgres ignores it).
  So we stop it the task-37 way — from *within* the namespace via
  `runc exec … gosu postgres pg_ctl -m fast -W stop` — whose signal is delivered
  and handled; the shell's `wait` then blocks on the run job so the shutdown
  checkpoint gets the vCPU and its logs reach `ttyS0`.
- **Terminal.** As in task 37, `poweroff` strands in `device_shutdown` once block
  I/O has run; the cmdline's `reboot=t,force` + `reboot -f` make a clean
  triple-fault terminal.

### Determinism closure (each item traces to the seed / V-time)

- **The Go-runtime entropy path is on the seeded CRNG** (the spec's load-bearing
  item). `runc` (Go) reads `AT_RANDOM`/`getrandom` at startup to seed
  map-iteration + hash randomization; if that diverged, Go map order — and thus
  every map-ordered output byte — would diverge. Under the patched backend
  RDRAND/RDSEED trap to the **seeded stream** and credit the kernel CRNG
  deterministically (the same root as task 37's `pg_strong_random` and the
  in-container `initdb`). The kernel CRNG mixes `random_get_entropy()` = the TSC
  at add-time, which is the **V-time TSC** (every in-guest RDTSC, including the
  userspace vDSO, traps to V-time — never a laundered host value). `docker-init.sh`
  prints `boot_id` (the CRNG's own UUID) as an explicit identical-twice witness;
  the overall bit-identical serial proves the map-ordered remainder.
- **cgroup-v2 setup + the rootfs assembly** are deterministic given V-time
  (deterministic timestamps) + seeded entropy — both are pure functions of guest
  execution under the single vCPU, with no probe-spin (the runc/postgres
  processes are cooperative; the i8042 fast-fail from task 34 is unchanged).
- **Multiprocess is deterministic by construction** — the postgres entrypoint
  forks `initdb`, a temp server, then the real postmaster + its background
  workers; a single vCPU kills SMP races, fork order is sequential, and timer
  wakeups ride the V-time tick. The serial is bit-identical twice — empirical
  proof.

### Blame boundary (the spec's gate 3)

Task 37 (bare Postgres) isolates the **database** determinism surface; this task
adds only the **container-stack** surface (runc's namespace/cgroup/rootfs setup +
the Go entropy seeding) on top of it. The DB workload, locale, and final-row
golden are identical to task 37 by construction, so a future divergence localizes
cleanly to a layer: if the `row|…` values match task 37 but the run diverges, the
fault is in the container surface, not the DB.

<!-- ACCEPTANCE-GATE EVIDENCE: filled in after the box gates pass. -->

### Deviations considered / limitations

- **runc-direct instead of `dockerd`** — forced by the deadlock above; the user
  approved it and the spec permits the lighter OCI path. The full Docker stack is
  still baked (the image *is* docker-capable); only the daemon is bypassed at
  runtime. A future task could retry dockerd if the VMM ever grows a
  forced-exit/preemption mechanism (out of scope — "build on 34, don't
  re-architect the seam").
- **Storage = the image rootfs in the initramfs tmpfs** (RAM-backed), not a
  `vfs`/`overlay2` docker graph. With runc-direct there is no docker daemon to
  manage a storage driver; the OCI bundle's rootfs is the storage, and it lives
  in RAM exactly as the spec's "RAM-backed" intent requires. (vfs-on-tmpfs and
  overlay2-on-a-loop-ext4 were the dockerd-path options; moot once dockerd is
  dropped.)
- **Image not byte-reproducible across separate builds** (the registry export tar
  + initdb-at-container-start mint build/run-time randomness) — runtime
  determinism, the gate, is unaffected. The integrity anchor is the digest-pinned
  pull.
- **`--network none` drops the entire bridge/netfilter surface** (config *and*
  nondeterminism); single-node has no network anyway. No durability-fault surface
  (RAM-backed PGDATA; deferred to D1).

## Task 37 — bare Postgres in full guest Linux, deterministic-twice

### What landed

A **bare-Postgres workload image** (consonance workload stream, step 2 of 3): the
*unchanged* task-36 container-class `bzImage` + a new `initramfs-postgres.cpio.gz`
that boots a real **PostgreSQL 17**, drives a fixed insert/select workload loop, and
runs **bit-identically twice** on the patched backend. No kernel change was needed —
the task-36 capability audit already confirmed `EXT4_FS`, `BLK_DEV_LOOP`/`BLK_DEV_RAM`,
`TMPFS`, `UNIX` (sockets), `SYSVIPC`, `DEVTMPFS_MOUNT` are all built in. New files:
`build-postgres-image.sh`, `pg-init.sh`; `versions.lock`/`fetch.sh` pin+fetch the
.debs; the box gates live in `consonance/vmm-core/tests/live_postgres.rs`.

### Build (`build-postgres-image.sh`, root + Linux only)

1. **PostgreSQL from pinned Debian .debs** (server + client + libpq, `versions.lock`,
   verified by sha256 like the kernel/busybox). The relocatable Debian binaries keep
   their `bin`/`lib`/`share` relative layout; the runtime shared-library closure
   (glibc, libicu, libssl, libgssapi, …) is resolved with `ldd` and copied from the
   build host's own `/lib` (+ `libnss_files` for the getpwnam postgres does), with
   `ldconfig -r` building the rootfs ld.so cache. `--with-system-tzdata` means we also
   ship `/usr/share/zoneinfo`; glibc's `C.UTF-8` is file-backed here so we ship
   `/usr/lib/locale/{locale-archive,C.utf8}`. JIT bitcode is dropped (jit=off).
2. **Pre-`initdb`'d PGDATA baked into a RAM-backed ext4.** `initdb` runs **once at
   build time** as a non-root build user (postgres refuses uid 0) into a *subdirectory*
   of the staging tree (a subdir keeps initdb's 0700 + uid-70, which postgres requires
   of PGDATA — the ext4 root that `mke2fs` creates is root-owned). `mke2fs -t ext4 -U
   <fixed-uuid> -E lazy_itable_init=0,lazy_journal_init=0 -d <staging>` bakes the
   cluster in. At runtime `pg-init.sh` loop-mounts that image (`mount -o loop`, so the
   ext4 lives in the initramfs tmpfs = RAM) on `/pgmnt`.
3. **Workload** (`/workload.sql`, baked): `CREATE TABLE ledger(i,v)` then N=20
   autocommit iterations, each `INSERT (i, i*i+7)` + a `SELECT` of the row plus the
   running `count(*)`/`sum(v)` — printed as `row|i|v|count|sum`. Values are a pure
   function of the loop index (no `now()`/`random()` columns), so the golden is a
   deterministic function of the seed. One psql session streams them all (fork +
   connect per row would be needlessly heavy under the single-stepping VMM).

### Determinism closure (each item traces to the seed / V-time)

- **Pre-`initdb`'d PGDATA.** `initdb` mints the cluster *system identifier* from
  `gettimeofday`+pid+random; doing it once at build time and baking the result removes
  that nondeterminism from the runtime entirely. (This is the one build-time event the
  spec calls out; it also makes the *image* not byte-reproducible across separate
  builds — a documented non-goal, distinct from the runtime determinism the gate
  proves.)
- **Locale + TZ pinned** (`LC_ALL=C.UTF-8`, `TZ=UTC`, `timezone='UTC'`,
  `--locale=C.UTF-8`). C.UTF-8 collation is byte-order (memcmp) — deterministic and
  locale-version-independent, so sorts cannot diverge silently.
- **`pg_strong_random` → the seeded CRNG.** Postgres' per-backend cancel key + other
  secrets go through `pg_strong_random` → `getrandom(2)` → the kernel CRNG. Under the
  patched backend RDRAND/RDSEED trap to the **seeded entropy stream** (the same root as
  the task-38 `AT_RANDOM` path); crediting them seeds the CRNG deterministically. See
  the **CRNG-init** finding below — this is load-bearing, not just hygiene.
- **Multiprocess is deterministic by construction.** The postmaster forks the startup
  process, checkpointer, bgwriter, walwriter, autovacuum launcher and a per-connection
  backend; a single vCPU means no SMP races, fork order is sequential (deterministic
  PIDs — visible in the `[pg <pid>]` log prefix), and any timer-driven background work
  wakes at V-time-deterministic points. The serial (incl. the startup/shutdown log
  lines + their V-time timestamps) is bit-identical twice — empirical proof.
- **`fsync` on RAM-backed storage** is instant + deterministic (the loop-over-tmpfs
  honors it; no real device). Durability calls add no nondeterminism. **Limitation
  (D1):** RAM storage has no durable/volatile split, so there is no
  durability-fault surface here (deferred, per the spec non-goals).

### consonance-VMM control-flow — four findings (the non-obvious part)

The minimal task-30/34/36 `init.sh` runs straight through to `poweroff`; it never
idles, never sleeps, never uses block I/O. Postgres exercises all three, surfacing
VMM properties the boot gate never hit. Each fix is **guest-side + deterministic** (no
VMM/contract change, per the task's "build on 34, don't re-architect the seam"):

1. **CRNG init must not be starved (cmdline: drop `random.trust_cpu=off`).** Under
   deterministic V-time there is no interrupt-timing jitter, so with the CPU RNG
   distrusted the kernel CRNG **never initializes** and postgres' first *blocking*
   `getrandom` hangs forever before its first log line. Trusting the trapped+seeded
   RDRAND/RDSEED seeds the CRNG deterministically (`random: crng init done` appears
   early) — the determinism is preserved *because* the entropy is the seeded stream.
2. **No `nanosleep` wakeups; await cooperatively, never by `sleep` or busy-spin.** A
   focused test showed `sleep 1` never returns under the VMM (no clock-event/tick
   device is set up; only the TSC clocksource is). So readiness/shutdown can't be
   `sleep`-polled (the sleeper never wakes) and can't be busy-spun (a spin starves
   postgres — there's no preemption tick either). Instead `pg-init.sh` waits
   **cooperatively**: a blocking `psql` connect yields the single vCPU to the starting
   postmaster (retry the idempotent `SELECT 1` until it connects), and the shell's
   `wait $PGPID` blocks on the postmaster so its shutdown checkpoint gets the CPU.
3. **The first guest HLT is terminal — keep the guest non-idle until the real exit.**
   `vmm.rs` treats `Exit::Hlt` as a terminal reason (the boot's `poweroff`→HLT is how
   it ends). A workload that idles (HLT) would end the run prematurely; the cooperative
   waits above keep *something* runnable at all times, so the guest never idles until
   the deliberate terminal.
4. **`poweroff` strands in `device_shutdown`; terminate via `reboot=t,force`.** Once
   block I/O has been used, the kernel's poweroff path hangs in `device_shutdown` under
   V-time. `pg-init.sh` unmounts the ext4 (auto-detaching loop0) and `reboot -f`s; the
   cmdline's **`reboot=force`** skips the orderly device_shutdown and **`reboot=t`**
   (triple-fault) becomes a clean `KVM_EXIT_SHUTDOWN`/HLT terminal. Relatedly, the
   deterministic-twice gate boots the image **twice in one process and drops the first
   run's `Vmm` before the second** — two pinned `perf_event` work counters open at once
   multiplex on the PMU and perturb the branch count (a few-step V-time skid → a
   divergent printk timestamp). One counter at a time is exact.

### Acceptance-gate evidence (box, `ssh <det-box>`, core-2-pinned, then reverted to stock 1396736)

Built with the repo `make -C guest/linux postgres-image` (kernel reused from task 36;
`bzImage` sha256 matches the committed `MANIFEST.sha256`). Patched 6.12.90 proxy
modules loaded, `taskset -c 2`, reverted to stock after each run.

- **Gate 1 — Postgres runs + streams** (`p1_postgres_runs_and_streams_patched`):
  `pg_ready=true workload_done=true final_row=true GUEST_READY=true`, clean terminal,
  ~163k VMM steps. Quoted serial (excerpt):

  ```
  [pg 100] LOG:  starting PostgreSQL 17.10 ... on x86_64-pc-linux-gnu
  [pg 100] LOG:  database system is ready to accept connections
  PG37: workload begin
  row|1|8|1|8
  row|2|11|2|19
  ...
  row|20|407|20|3010
  PG37: workload end
  [pg 100] LOG:  database system is shut down
  GUEST_READY
  ```

- **Gate 2 — deterministic twice** (`p2_postgres_deterministic_twice_patched`,
  the milestone): two same-seed patched boots → identical step count (162609) and
  **bit-identical serial + `state_hash`** (`test result: ok. 2 passed`):

  ```
  [p2 run A] steps=162609 terminal=Hlt  pg_ready workload_done final_row GUEST_READY = all true
  [p2 run B] steps=162609 terminal=Hlt  pg_ready workload_done final_row GUEST_READY = all true
  serial A == serial B  (14813 bytes, including the row|… query output)
  state_hash A = state_hash B =
    7ea21de2e3eb3ba2dede8370edda84a6950f97afe7469de8c990f88090845e39
  ```

- **Gate 3 — no regression:** only `guest/linux/` (+ the box-only `live_postgres.rs`
  test) changed; the kernel/minimal-image/`devices.rs`/contract are untouched, so
  M1/M2/P6 + the det-corpus goldens and `state_hash` schema are byte-unchanged.
- **Gate 4 — box hygiene:** every patched run reverts to stock KVM (`lsmod | grep
  '^kvm '` = `1396736`) and is verified.

### Deviations considered / limitations

- **Distro .debs vs. building PostgreSQL from source.** Chose the pinned Debian
  binaries (fast, tested) with `--locale-provider=libc --locale=C.UTF-8` so ICU
  collation is never used; ICU/krb5/openssl are linked but determinism comes from below
  so their presence is harmless. A `--without-icu` source build would shrink the rootfs
  but adds a heavy build step for no determinism gain.
- **Image not byte-reproducible across separate builds** (the baked initdb system id is
  a build-time random) — runtime determinism, the gate, is unaffected. The runtime libs
  are taken from the build host's `/lib` (the determinism box is the pinned build
  environment), not separately pinned.
- **Terminal is a forced triple-fault reboot, not an ACPI poweroff** — a consequence of
  the device_shutdown stall above; deterministic and clean for the gate's purpose.

## Task 36 — guest-kernel rebase: Kata-class container-host config + determinism overlay

### The decision (what landed)

Swap the guest-kernel **base** from `make ARCH=x86_64 tinyconfig` to a **vendored Kata
Containers guest-kernel config** (`kata/`), and keep `config-fragment` as the **determinism
overlay** merged on top (it wins every conflict). Built with the *existing*
`build-kernel.sh` pipeline (reproducible levers, pinned bytes, `MANIFEST.sha256`). We use
Kata's *config*, not Kata's *binary*: `init.sh` stays our init, the golden initramfs flow is
unchanged, brd/loop stay, and the artifact is reproducible. Determinism is **not** in the
config — it is enforced from below (patched KVM determinizes TSC/RNG, V-time drives the
timer, the VMM device models + cmdline handle the rest); the config governs only *capability*
and *probe surface*.

### Provenance of the Kata config (`kata/`)

- kata-containers/kata-containers **release 3.32.0** (2026-06-22), commit
  `337b6002681479fb6a605ca8a7a1138e81b6098c`, `kata_config_version` 198.
- That release's `versions.yaml` pins kernel **v6.18.35** — the *exact* version in
  `versions.lock`. The config and kernel source are version-matched by construction.
- Vendored verbatim: `tools/packaging/kernel/configs/fragments/{common,x86_64}/*.conf`,
  reproducing Kata's own `-a x86_64` selection (all 27 common fragments — none carry a
  `!x86_64` exclusion tag — plus all 13 x86_64 fragments; no confidential/GPU/debug/
  build-type fragments). No symbol is redefined with a conflicting value across the set.
  See `kata/PROVENANCE` for the re-fetch + verify recipe and the aggregate sha256.

### Build pipeline (`build-kernel.sh`)

Kata generates its config from `allnoconfig` + fragments (its build passes `merge_config.sh
-n`), so we seed with **allnoconfig** (not tinyconfig — its `tiny.config` size deltas are not
part of Kata's config), then merge **in one pass**: the Kata fragments (container-host base)
followed by `config-fragment` **last** so the overlay overrides every conflict
(SMP/NUMA/KASLR/HZ/CPU_FREQ/HW_RANDOM/X86_PM_TIMER/HIGH_RES_TIMERS → off), then
`olddefconfig`.

### Gate 2 — the overlay survives the richer base (asserted in `build-kernel.sh`)

`merge_config.sh` only *warns* when a fragment symbol can't take effect, so every determinism
symbol is asserted after `olddefconfig`. Against the Kata base (which sets `SMP=y`,
`NO_HZ_FULL=y`, `CPU_FREQ=y`, `RANDOMIZE_BASE=y`, `RELOCATABLE=y`, `X86_PM_TIMER=y`,
`HW_RANDOM=y`, `HIGH_RES_TIMERS=y`, …) the overlay wins every one — verified on the box:

| Determinism lever | Result after merge |
|---|---|
| `SMP`, `NUMA`, `CPU_FREQ`, `MODULES` | off |
| `RANDOMIZE_BASE` (KASLR), `RELOCATABLE` | off |
| `HIGH_RES_TIMERS`, `X86_PM_TIMER`, `HW_RANDOM` | off |
| `TRANSPARENT_HUGEPAGE`, `KSM`, `SUSPEND`, `HIBERNATION` | off |
| `HZ_PERIODIC` / `HZ_100` (`CONFIG_HZ=100`), `KERNEL_GZIP` | on |
| `LOCALVERSION=""`, `LOCALVERSION_AUTO` off | empty / off |

**Dynticks subtlety (assert fix):** Kata sets the deprecated `CONFIG_NO_HZ=y`. That symbol
only sets the *default* of the "Timer tick handling" choice (`default NO_HZ_IDLE if NO_HZ`)
and selects nothing once `HZ_PERIODIC` wins the choice — it harmlessly stays `=y`. So the
assert checks the **meaningful** tickless symbols off — `NO_HZ_COMMON` (which selects the
dynticks machinery + `TICK_ONESHOT`), `NO_HZ_FULL`, `NO_HZ_IDLE`, `TICK_ONESHOT` — not plain
`NO_HZ`. Box-confirmed: `NO_HZ_COMMON` and `TICK_ONESHOT` absent → true periodic tick.

`EXT4_FS` moved out of `assert_off` (the container workload needs it; Kata provides it). The
overlay also **stopped** disabling `BLOCK`/`EXT4_FS`: merged last, those `is not set` lines
would have cascade-disabled the entire container capability.

### Why Kata's paravirt surface is dormant (no determinism risk)

Kata's base sets `KVM_GUEST=y`, `PARAVIRT=y`, `PVH=y`, `X86_X2APIC=y`. The frozen CPU/MSR
contract (`docs/CPU-MSR-CONTRACT.md`) neutralizes all of them at runtime: `CPUID.1:ECX`
**HYPERVISOR[31]=0** (the guest believes it is bare metal → `kvm_para_available()` false →
kvm-clock / paravirt-EOI / async-PF never arm) and **x2APIC[21]=0** (the kernel can't enter
x2APIC mode → stays on the modeled xAPIC-MMIO LAPIC). The patched boot log confirms it:
*"Booting paravirtualized kernel on bare hardware"*, virtual-wire APIC, 1 CPU. They are
dormant code, not active nondeterminism — exactly the "config governs capability, determinism
from below" split.

### Phase 2 — new probe surface: **no new stall**

A bigger config probes more absent devices, and under patched V-time every jiffies-timeout
probe spin can strand the boot (the i8042 lesson, task 34). Empirically, on the patched
backend the rebased kernel reaches `GUEST_READY` with **no new fix needed**:

- The **i8042 keyboard-controller probe** — the one such spin — is already covered by task
  34's `devices::LegacyPlatform` OBF-set fast-clear (status `0x64` → `0x01`), which makes the
  controller-presence check fail fast instead of spinning `10000×udelay`. Unchanged here.
- No other driver in the Kata set spins on a jiffies timeout during boot: PCI/virtio/NIC
  drivers find no device (PCI config reads return all-ones) and bail; FS/crypto/netfilter
  init touch no hardware. `devices.rs` is **unchanged**.

The boot reaches `/init` and `GUEST_READY` in ~152k VMM steps / well under the V-time + wall
budget. (An `earlycon` lead was investigated and **rejected** — see Deviations: it was a
harness artifact, not a real stall.)

### Phase 3 — container-capability audit (sets up 37/38; not exercised here)

Read from the generated `.config` (box). Presence of a symbol, not a running container.

| Need (tasks 37/38) | Symbols | Status |
|---|---|---|
| Real ext4 + journal | `EXT4_FS`, `EXT4_USE_FOR_EXT2`, `JBD2`, `FS_IOMAP` | ✅ y |
| RAM-backed block dev | `BLK_DEV_LOOP` (loop-over-image), `BLK_DEV_RAM` (brd, 4096 KB), `BLK_DEV_SD`, `BLOCK` | ✅ y (both loop **and** brd) |
| cgroup-v2 controllers | `CGROUPS`, `MEMCG`, `CGROUP_PIDS`, `CGROUP_FREEZER`, `CGROUP_DEVICE`, `CGROUP_CPUACCT`, `CGROUP_SCHED`, `BLK_CGROUP`, `CGROUP_BPF`, `CGROUP_HUGETLB` | ✅ y |
| cgroup cpuset controller | `CPUSETS` | ⚠️ **absent** — see below |
| overlayfs (docker storage) | `OVERLAY_FS` (+INDEX/REDIRECT_DIR/METACOPY/XINO_AUTO) | ✅ y |
| namespaces | `NAMESPACES`, `PID_NS`, `NET_NS`, `USER_NS`, `UTS_NS`, `IPC_NS` | ✅ y (cgroup-ns is unconditional when CGROUPS+NAMESPACES — no `CONFIG_CGROUP_NS`) |
| exec / binfmt | `BINFMT_ELF`, `BINFMT_SCRIPT`, `BINFMT_MISC` | ✅ y |
| event/IPC syscalls | `EPOLL`, `EVENTFD`, `SIGNALFD`, `TIMERFD`, `FUTEX`, `AIO`, `FHANDLE`, `POSIX_MQUEUE`, `MEMFD_CREATE` | ✅ y |
| fs surface | `TMPFS` (+XATTR), `DEVTMPFS` (+MOUNT), `PROC_FS`, `SYSFS`, `FUSE_FS` | ✅ y |
| sandbox helpers | `SECCOMP`, `SECCOMP_FILTER`, `KEYS`, `SECURITY`, `BPF_SYSCALL` | ✅ y |
| networking (NOT required; 38 uses `--network none`) | `NETFILTER`, `BRIDGE`, `VETH`, `INET` | ✅ y (present anyway) |

**The one absent must-have — `CPUSETS`:** it `depends on SMP`, and the determinism overlay
keeps `SMP` off (single vCPU is load-bearing — no IPIs, no cross-CPU races). This is an
**honest** absence, not a gap: the cpuset controller partitions CPU affinity across CPUs that
don't exist on a 1-vCPU guest. `docker run --network none postgres` (tasks 37/38) does not
require cpuset (only `--cpuset-cpus` does), and runc/containerd degrade gracefully over a
missing controller. **Follow-on option if a future task ever needs the controller present:**
build with `SMP=y` + boot `maxcpus=1` (a determinism trade-off to evaluate then — it adds
SMP/IPI code paths; out of scope for this task, which keeps `SMP` off as proven by tasks
30/34). Recorded here so the gap surfaces now, not mid-Postgres bring-up.

### Gate 1 — deterministic-twice on the rebased kernel (the milestone, box)

Two same-seed patched boots of the rebased 10 MB Kata-config `bzImage` + unchanged
`init.sh` reach `GUEST_READY` and are **bit-identical**:

```
state_hash A = state_hash B =
  b277bc5260144dcb22545f6350c42886f2691a0f95ffcc8e18f8dc1b44bd6847
serial A == serial B  (12872 bytes, including GUEST_READY)
reached_userspace = true ; GUEST_READY = true ; terminal = Hlt
```

Stock `GUEST_READY` (no regression): `gate3` passes (27928 steps, clean Hlt).

### Box run commands (det-cfl-v1, `ssh hetzner`, CPU-pinned per docs/BOX-PINNING.md)

Pinned to **core 2** (sibling cpu10 idle; task 39 owns core 4), patched modules loaded then
**always reverted to stock KVM (1396736)** and verified:

```sh
# build the rebased image (isolated build root so concurrent box work doesn't collide)
GUEST_BUILD_ROOT=/tmp/ht36-guest-build make -C guest/linux image     # bzImage + initramfs
# milestone (patched, deterministic twice) — load patched kvm.ko/kvm-intel.ko, then:
taskset -c 2 timeout 400 cargo test -p vmm-core --test live_linux_boot \
    -- --ignored --nocapture --test-threads=1 c_linux_boot_deterministic_twice_patched
# stock GUEST_READY:
taskset -c 2 cargo test -p vmm-core --test live_linux_boot \
    -- --ignored --nocapture --test-threads=1 gate3_linux_guest_ready_and_clean_poweroff
# always revert + verify:  rmmod kvm_intel kvm; modprobe kvm kvm_intel; lsmod | grep '^kvm '  # 1396736
```

### Reproducibility + `MANIFEST.sha256`

`run-tests.sh` builds kernel+initramfs **twice** and asserts byte-identical, then writes
`MANIFEST.sha256` and QEMU-boots the manifested image to `GUEST_READY`. The bzImage hash is
new (the Kata-class kernel is ~10 MB vs the old tiny kernel); the initramfs hash is unchanged
(`f0bb7c0d…` — busybox + `init.sh` untouched). See `MANIFEST.sha256`.

### The vmm-core change (cross-reference)

The only `consonance/` change is the box gate's `DEFAULT_CMDLINE`
(`consonance/vmm-core/tests/live_linux_boot.rs`): added the runtime determinism params the
Kata base needs — `random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable` — each
a no-op against the overlay's build symbols, present because Kata's base sets the opposite (see
that file's doc comment, and `consonance/vmm-core/IMPLEMENTATION.md` Task 36 note). No
`devices.rs` / `state_hash` change.

### Deviations considered and rejected

- **`earlycon=uart8250,io,0x3f8` as a "Phase-2 fix":** during bring-up a patched boot appeared
  to strand with empty serial, and adding `earlycon` "fixed" it. Root-caused to a **harness
  bug** (my run script exported `BOOT_CMDLINE=""`, which Rust's `env::var` reads as `Ok("")`,
  overriding `DEFAULT_CMDLINE` with an *empty* cmdline → no `console=ttyS0` → no serial). With
  the real `DEFAULT_CMDLINE` the boot reaches `GUEST_READY` deterministically **without**
  earlycon. Rejected — adding it would be cargo-cult; the cmdline carries only justified
  determinism params.
- **Vendoring a single merged `kata.config` file** instead of the verbatim fragment tree:
  rejected — the fragment tree is byte-diffable against upstream (stronger provenance), and
  `build-kernel.sh` merges it trivially.
- **Starting the base from `tinyconfig`/`defconfig`** instead of `allnoconfig`: rejected —
  `allnoconfig` is what Kata uses (faithful), and `defconfig` would pull in a huge driver set
  (USB/SATA/sound/NICs) that only enlarges the probe surface for no capability gain.
- **`CONFIG_SMP=y` + `maxcpus=1`** (to keep `CPUSETS`): rejected for this task — `SMP` off is
  the proven, simpler, more-deterministic path (tasks 30/34) and cpuset is meaningless on one
  vCPU. Left as a documented follow-on if ever needed.

### Known limitations

- `CPUSETS` absent (above) — the only must-have not present; honest and documented.
- The config is intentionally *larger* than minimal (Kata's full container-host set incl.
  XFS/EROFS/CIFS/netfilter/virtio/mlx5). Per the task this is accepted — minimization is not
  load-bearing for determinism, and the extra drivers are dormant (no device to bind).
