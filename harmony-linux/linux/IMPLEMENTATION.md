# harmony-linux/linux — implementation notes

## Task 48 — `runc` *actually* runs the Postgres OCI container (no `unshare` shim)

### What landed (the money-shot, and the overturn of task 38's load-bearing finding)

Task 38's headline result was a **deadlock finding**: under the single-vCPU /
V-time VMM, `runc`'s Go container-init never completes the create→exec handshake
(the container reaches `"created"` but never execs its command), so task 38 fell
back to building the container with `unshare`/`chroot`/`setpriv` — plain syscalls,
no Go init. Task 38's own "Deviations" closed with: *"a future VMM with a
forced-preemption/exit mechanism could revisit runc."* **Task 47 (PR #15) is that
mechanism** — `run_until` (PMU-overflow + single-step preemption at the V-time LAPIC
deadline) preempts a busy-spinning Go runtime at a seed-deterministic instant — and
**task 48 revisits runc with it**: the **actual `runc` binary** (`runc run`) now
launches the official `postgres:17` OCI container, the task-42
`gen_random_uuid()`/`clock_timestamp()` workload runs against it, and (the gate) it
comes out **bit-identical across two same-seed runs**.

New/changed files, all additive over task 38:

- **`runc-init.sh`** (new) — a second guest `/init`, baked as `/runc-init` and
  selected by the kernel `rdinit=/runc-init` cmdline param. It brings up the kernel
  filesystems + cgroup-v2 (identical to `docker-init.sh`) and then runs
  `runc run --bundle /oci pg-container` — the **real `runc`**, on the **same `/oci`
  bundle + `config.json`** task 38's `build-docker-image.sh` already generated (it was
  always runc-ready: `runc spec` template + allow-all devices + `terminal=false` +
  `process.args = /run-workload.sh`). The task-38 `unshare` `/init` (`docker-init.sh`)
  stays the default, so its gate is unchanged and the two paths are directly
  comparable.
- **`build-docker-image.sh`** — one added line: `install … runc-init.sh
  "$DKROOT/runc-init"`. No change to the OCI bundle, the workload, the pre-baked
  PGDATA, or the task-38 path.
- **`consonance/vmm-core/tests/live_runc_postgres.rs`** (new) — the three box gates
  (`r1`/`r2`/`r3`), `#[cfg(target_os = "linux")]` + `#[ignore]`, modeled on
  `live_postgres_docker.rs` but with `rdinit=/runc-init` and runc-path proof markers.

**No `src/` change.** The preemption path task 47 added is wired and *auto-fires* on
the patched Linux boot: `Vmm::step` calls `preemption_deadline()`
(`vmm-core/src/vmm.rs`), which returns `Some` whenever the backend has a deterministic
retired-branch counter (patched KVM) **and** the LAPIC timer is armed (the Linux boot
path, set up in `bringup.rs`); on `Some` it drives `Backend::run_until(deadline)`
instead of bare `run()`, so a busy-spinning runc/Go thread is preempted at the V-time
deadline. Task 48 only *exercises* this on the real container-runtime stack — it is a
guest-image + gate-test task.

### Why `runc` now makes progress (the determinism is preemption-driven)

`runc`/its container-init busy-spin (`procyield`/`osyield`) with no natural VM-exit.
Task 38 froze there because V-time only advanced at RDTSC-bearing exits, so the LAPIC
tick never fired and the Go scheduler never ran. Now `run_until` arms the PMU to
overflow at the timer's V-time deadline (expressed in **retired branches** = a pure
function of the seed) and single-steps the skid to land at *exactly* that count, then
the VMM injects the LAPIC timer vector. The Go runtime is preempted on time, its
scheduler runs, the create→exec/exec-fifo handshake completes, and `runc run` execs
the container's `/run-workload.sh`. Because the preemption instant is seed-deterministic
(not wall-clock), the whole interleaving — including the Go runtime's now-genuinely-running
goroutine schedule — is a pure function of the seed. That is the delta over task 38:
task 38 *bypassed* the Go runtime; task 48 *runs* it, deterministically.

The entropy/clock determinism is unchanged from task 37/38: `gen_random_uuid()` →
`pg_strong_random` → the seeded CRNG (RDRAND/RDSEED trap to the seeded stream),
`clock_timestamp()` → V-time. `runc`/Go reading `AT_RANDOM`/`getrandom` at startup
rides the same seeded CRNG (task 38 verified this via `boot_id`; `runc-init.sh` keeps
the `boot_id` witness).

### The gates (`live_runc_postgres.rs`) — what each proves

- **r1 — real `runc` runs Postgres + streams.** One patched boot must launch the
  container **through the real `runc` binary** — asserted by the `RUNC48: … via REAL
  runc: runc run` banner **present** AND the task-38 `DK38:`/`unshare(…)` shim markers
  **absent** (proof `rdinit=/runc-init` selected the runc path, not the shim) AND
  `runc run` **exiting 0** — then postgres ready, the workload's `row|20|20|210|<uuid>|<t>`
  reaching `ttyS0` with all 20 UUIDs valid + distinct, `GUEST_READY`, clean terminal.
- **r2 — deterministic-twice (the milestone).** Two same-seed patched boots through
  real `runc` → **bit-identical** serial (incl. the UUIDs/timestamps) **and**
  `state_hash`. Each run is independently re-asserted to have gone through real runc
  and reached `GUEST_READY`, so two identical-but-stranded boots can't pass vacuously.
- **r3 — seed-sensitivity.** A different seed ⇒ different UUIDs through the container
  (the seeded CRNG), both quoted.

### Box-run instructions (FOR THE FOREMAN — these gates are box-only; not yet box-verified)

These gates need the patched KVM + Intel PMU (the `run_until` preemption path uses
`perf_event` overflow + `KVM_SET_GUEST_DEBUG` single-step) and the built Docker image —
none available on the dev Mac, and `ssh hetzner` is denied in this worker's session, so
**the live path is delivered + fully Mac-verified but the box gates are handed to the
foreman to run verbatim** (per the box-only-gate convention). On `ssh hetzner`, pin per
`docs/BOX-PINNING.md`, **always revert KVM to stock `1396736` + verify**:

```sh
# 1. build the image (needs network for the pinned postgres image + docker bundle)
make -C harmony-linux fetch && make -C harmony-linux/linux docker-image     # -> harmony-linux/build/initramfs-docker.cpio.gz
# 2. load the patched kvm.ko/kvm-intel.ko (vermagic must match `uname -r`), then, CPU-pinned:
taskset -c 4 timeout 4200 cargo test -p vmm-core --test live_runc_postgres -- \
    --ignored --nocapture --test-threads=1 r1_runc_postgres_runs_and_streams_patched
taskset -c 4 timeout 5400 cargo test -p vmm-core --test live_runc_postgres -- \
    --ignored --nocapture --test-threads=1 r2_runc_postgres_deterministic_twice_patched
taskset -c 4 timeout 5400 cargo test -p vmm-core --test live_runc_postgres -- \
    --ignored --nocapture --test-threads=1 r3_runc_postgres_seed_sensitivity_patched
# 3. ALWAYS revert + verify:  rmmod kvm_intel kvm; modprobe kvm kvm_intel; lsmod | grep '^kvm '  # 1396736
```

Wall budgets are generous (real `runc`/Go + the preemption single-stepping is heavier
than task 38's `unshare` shim and than a fresh boot); `WALL_BUDGET_SECS` overrides the
in-test watchdog if a run needs longer. Capture the `r2` equal digests + a sample
UUID/timestamp and the `r3` two UUIDs into this file's "Acceptance-gate evidence" once
run. **Note:** re-shipping the worktree with `rm -rf` wipes the built guest payloads —
rebuild the image before any gate-2 run.

### If a genuinely NEW blocker surfaces (honest frontier, per the task spec)

Preemption removes the *one* blocker task 38 documented (runc-init never execs). The
kernel capabilities real `runc` additionally needs are all present in the task-36 Kata
base (verified by reading the config): `CONFIG_SECCOMP_FILTER` (runc applies the
`runc spec` default seccomp profile), `CONFIG_POSIX_MQUEUE` (the `/dev/mqueue` mount),
full cgroup-v2 with all controllers + `CONFIG_CGROUP_BPF`, and the namespace set; the
device-cgroup eBPF default-deny that would kill PID 1 is avoided by the bundle's
allow-all `linux.resources.devices` (already in task 38's `config.json`), and the
ramdisk-rootfs `pivot_root` EINVAL by the baked `--no-pivot` runc wrapper. So no further
blocker is *anticipated*. **If one does surface on the box** (a Go path preemption alone
doesn't determinize → `r2` serials differ same-seed, or runc failing on a kernel/config
gap → `r1` `runc run` rc≠0), it is a **real finding**: report it with the precise
failure (the `RUNC48: runc run exited rc=…` line + the divergent bytes), implement as far
as the primitive carries, and document it as the next frontier — do **not** relax a gate
or fall back to `unshare` (that is task 38, kept available for comparison, not this gate).

## Task 42 — Postgres workload v2: `gen_random_uuid()` + time, still deterministic-twice

### What landed

The shared bare(37)+OCI(38) Postgres workload (`workload.sql`, generated in both
`build-postgres-image.sh` and `build-docker-image.sh`) now populates each row with a
**`gen_random_uuid()`** id (column `DEFAULT`) and a **`clock_timestamp()`**
wall-clock column, streamed as `row|i|count|sum|uuid|t`:

```sql
CREATE TABLE ledger(id uuid PRIMARY KEY DEFAULT gen_random_uuid(), i int, t timestamptz);
-- each of N=20 iterations:
INSERT INTO ledger(i,t) VALUES ($i, clock_timestamp());
SELECT 'row', i, (SELECT count(*) FROM ledger), (SELECT sum(i) FROM ledger), id, t FROM ledger WHERE i=$i;
```

The headline: a random UUID and a per-call wall-clock timestamp *look* nondeterministic,
but come out **bit-identical across two same-seed runs** — because `gen_random_uuid()`
draws from `pg_strong_random` → the seeded CRNG (task 37's verified path), and
`clock_timestamp()` reads the system clock, which is V-time-driven. It is a sharper
determinism demo *and* a stress test of the RNG/clock determinization: if any path
escaped, the deterministic-twice gate would fail and that would be a real
determinization finding. **It did not escape — both bare and OCI pass
deterministic-twice.**

The `count`/`sum` prefix stays a pure function of the loop index (`row|20|20|210|` for
the final row: count=20, sum(1..20)=210) — the **deterministic anchor** the gates match.
The uuid + t are seed-derived (deterministic but not predictable), so the gates check
them by **shape** (`is_uuid`/`is_timestamp`) and prove **seed-sensitivity** at a second
seed, rather than pinning a literal.

### `gen_random_uuid()` needs no extension

It is built into PostgreSQL **core since v13** (PG17 here — both the Debian `.deb`s of the
bare image and the official `postgres:17` OCI image), so **no `CREATE EXTENSION
pgcrypto`** at build time. Confirmed empirically: the workload runs clean under `psql -v
ON_ERROR_STOP=1` (a missing function would abort the run and the final row would never
reach the serial — yet it did, in every run below).

### Determinism closure (the whole point — each traces to the seed / V-time)

- **`gen_random_uuid()` → seeded CRNG.** Core `gen_random_uuid()` fills 16 bytes from
  `pg_strong_random()` → `getrandom(2)` → the kernel CRNG. Under the patched backend,
  RDRAND/RDSEED trap to the **seeded entropy stream** and credit the CRNG
  deterministically (the same root as task 37's cancel keys and task 38's `AT_RANDOM`).
  So the 20 UUIDs are a deterministic function of the seed: identical across two same-seed
  runs, **different across different seeds** (Gate 3).
- **`clock_timestamp()` → V-time.** It reads `CLOCK_REALTIME` (gettimeofday), whose base
  is the VMM's deterministic persistent-clock value and whose advance is the TSC-derived
  monotonic clock — both V-time-driven. Empirically the wall-clock base is a fixed epoch
  (`1999-11-30 00:00:00`) and the per-row sub-second field **advances** across the 20 rows
  — a live, advancing clock that is nonetheless bit-identical across same-seed runs.
- **Text rendering pinned.** `timestamptz` text depends on `timezone`/locale; both are
  pinned (`timezone='UTC'`, `LC_ALL=C.UTF-8`, default ISO `DateStyle`), so the rendered
  bytes are stable. The serial bit-identity (Gate 2) is the ground truth either way.

### The gates (both files, plus the task-40 regression)

`live_postgres.rs` and `live_postgres_docker.rs` dropped the old
`FINAL_ROW = row|20|407|20|3010` literal (no longer valid) and now assert:

1. **Deterministic-twice** (Gate 2) — two same-seed patched runs: bit-identical serial
   (incl. the UUIDs + timestamps) **and** identical `state_hash`. *This* is the proof the
   UUIDs/timestamps are bit-identical.
2. **Shape** (in Gates 1+2) — the final row carries a valid UUID + timestamp, and all 20
   per-iteration UUIDs are **distinct** (not a frozen constant within a run).
3. **Seed-sensitivity** (Gate 3, a new `p3_*` test in each file) — a second seed produces
   **different** UUIDs (genuinely seed-driven, not a constant).

`live_branching_demo.rs` (task 40) shares the image; its workload-complete marker was moved
from the old literal to the new anchor `row|20|20|210|` so it does not regress. The bare
runs confirm that exact byte sequence reaches the serial, and the branching demo uses the
SAME image + the same `find(serial, …)` check, so the marker matches by construction (the
demo was not re-run — it is many boots, and no behavior of it changed beyond the marker).
`pg-init.sh`'s header comment was updated to the new determinism rationale.

**No production code changed** (no kernel / `devices.rs` / contract / hashing), so M1/M2/P6
+ the acceptance-suite goldens and the `state_hash` schema are byte-unchanged by construction;
host gates (build, `nextest` 226 passed, clippy, fmt, `deny`) are green and the
`#[cfg(target_os="linux")]`-gated test bodies cross-check + cross-clippy clean under
`--target x86_64-unknown-linux-gnu`.

### Acceptance-gate evidence (box `ssh hetzner`, `taskset -c 2`, reverted to stock `1396736`)

Built on the box (cached `.deb`s / OCI tar / docker bundle + the unchanged task-36
`bzImage` reused from ht38; `GUEST_BUILD_ROOT` under `/tmp` so the build-time `initdb` —
run as an unprivileged uid — can traverse it, and isolated from task 41). Every patched run
reverted to stock KVM (`lsmod | grep '^kvm '` = `1396736`, `kvm_intel users=0`) and was
verified by the `run-patched-ht42.sh` trap; lsmod was checked **before** each load to
coordinate with task 41 (core 4).

**Bare (task 37 path), `live_postgres.rs`:**

- **Gate 1** (`p1_postgres_runs_and_streams_patched`): `pg_ready workload_done final_row
  GUEST_READY` all true, 20 distinct UUIDs, clean `Hlt` terminal, 167701 steps, `ok`.
  Sample final row: `uuid=c001b5cb-c6c4-41e9-9c80-6deee886ab99 t=1999-11-30 00:00:00.374345+00`.
- **Gate 2 — deterministic-twice** (`p2_postgres_deterministic_twice_patched`):
  ```
  [p2 run A] steps=167701 ... all true   uuid=c001b5cb-c6c4-41e9-9c80-6deee886ab99 t=1999-11-30 00:00:00.374345+00
  [p2 run B] steps=167701 ... all true   uuid=c001b5cb-c6c4-41e9-9c80-6deee886ab99 t=1999-11-30 00:00:00.374345+00
  serial A == serial B  (16063 bytes, incl. the UUIDs + timestamps)
  state_hash A = state_hash B = 794e3565aebf018b5330a1428d15b196664af452e26fcaef2f070ec7ff833a7f
  ```
  The random-looking UUID + wall-clock timestamp are **bit-identical** across the two runs.
- **Gate 3 — seed-sensitivity** (`p3_postgres_seed_sensitivity_patched`):
  ```
  seed 0x0028c0ffee5eedc0 -> c001b5cb-c6c4-41e9-9c80-6deee886ab99
  seed 0x9e1fb946911491d5 -> 67a3ff14-f485-4424-8319-ec693058d058
  ```
  Different seed ⇒ **different UUID** (also a different step count, 167701 vs 167699, and a
  different timestamp — genuinely a different entropy stream). `test result: ok. 2 passed`,
  `REVERT OK`, `lsmod kvm = 1396736`.

**OCI (task 38 path), `live_postgres_docker.rs`** — same three gates through the full
container stack (the official `postgres:17` OCI image, namespace/cgroup-isolated):

- **Gate 1** (`p1_docker_postgres_runs_and_streams_patched`): `container_up pg_ready
  workload_done final_row GUEST_READY` all true, 20 distinct UUIDs, clean `Hlt` terminal,
  167750 steps, `ok`. Sample: `uuid=6c8b2ac4-1b3b-4cd2-9246-8b267304a394 t=1999-11-30 00:00:01.827954+00`.
- **Gate 2 — deterministic-twice** (`p2_docker_postgres_deterministic_twice_patched`):
  ```
  [p2 run A] steps=167750 ... all true   uuid=6c8b2ac4-1b3b-4cd2-9246-8b267304a394 t=1999-11-30 00:00:01.827954+00
  [p2 run B] steps=167750 ... all true   uuid=6c8b2ac4-1b3b-4cd2-9246-8b267304a394 t=1999-11-30 00:00:01.827954+00
  serial A == serial B  (17344 bytes, incl. the UUIDs + timestamps)
  state_hash A = state_hash B = 0266abce246253ed6b8e10695de49b064d9074a1bc9e8b5d30eb9aa467adaf30
  ```
- **Gate 3 — seed-sensitivity** (`p3_docker_postgres_seed_sensitivity_patched`):
  ```
  seed 0x0028c0ffee5eedc0 -> 6c8b2ac4-1b3b-4cd2-9246-8b267304a394
  seed 0x9e1fb946911491d5 -> d1c1360d-fe72-486c-8bc2-21819805f7ca
  ```
  `test result: ok. 3 passed; finished in 1855s`, `gate rc=0`, `REVERT OK`, `lsmod kvm =
  1396736` (`kvm_intel users=0`).

The container's UUIDs/timestamps differ from the bare path's at the *same* seed (e.g.
`6c8b2ac4…` vs `c001b5cb…`, and the clock has reached `00:00:01.8` vs `00:00:00.3`) —
expected: the container surface consumes entropy + V-time differently before the workload,
so the CRNG and clock are at a different point. Each path is bit-identical to *itself*
across same-seed runs, which is the determinism property the gate proves.

### Deviations considered / limitations

- **`clock_timestamp()` over `now()`.** The spec allows either; `clock_timestamp()` is the
  stronger test (it reads the wall clock on *every* call, not once per transaction), so the
  per-row timestamps advance — exercising the live clock 20× per run rather than freezing it.
- **Single combined SELECT** (not `INSERT … RETURNING` + a separate SELECT). One streamed
  `row|…` line per iteration carries the uuid + t + the running aggregate — fewer lines, a
  cleaner golden, and a single deterministic anchor to match; fully satisfies "use both
  `gen_random_uuid()` and a time function, stream id + t + a running aggregate".
- **Shape, not value, for uuid/t.** A 122-bit random UUID and a V-time timestamp are
  deterministic but not predictable without running, so the gates can't pin a literal; they
  match the deterministic count/sum anchor + validate uuid/t by shape + prove distinctness +
  seed-sensitivity. The serial bit-identity (Gate 2) is what actually pins them.
- **No durability-fault surface** (RAM-backed PGDATA; deferred D1) — unchanged from 37/38.
- **No determinization-mechanism change** (per the spec non-goals): this task *exercises*
  the existing RNG/clock determinization; it found no gap.

## Task 38 — Postgres as an OCI container, deterministic-twice

### What landed

The **Postgres-in-a-container workload image** (consonance workload stream, step
3 of 3 — the credibility money-shot): the *unchanged* task-36 container-class
`bzImage` + a new `initramfs-docker.cpio.gz` that runs the **official
`postgres:17` OCI image** as a real container — namespace + cgroup isolation of
the image's own rootfs — and drives the SAME fixed insert/select workload as task
37, **bit-identically twice** on the patched backend. New files:
`build-docker-image.sh`, `docker-init.sh`, `container-setup.sh`,
`pg-container-run.sh`; `versions.lock`/`fetch.sh` pin+fetch the Docker static
bundle and the postgres image; the box gates live in
`consonance/vmm-core/tests/live_postgres_docker.rs`.

> **Framing (per the integrator):** the goal is **running OCI images**, not
> docker-the-daemon. So this runs the official postgres **OCI image** in a real
> container (the same image `docker run postgres` pulls), with real isolation —
> just not via the docker daemon, for the load-bearing reason below.

### The load-bearing finding: dockerd AND runc both deadlock under the V-time VMM

> **This is the most important result of the task**, and it is a general one
> about container runtimes vs. a work-driven deterministic VM.

Under consonance's single-vCPU / V-time model, **V-time advances only when the
guest executes `RDTSC`/`RDTSCP` (or `RDMSR(IA32_TSC)`)** — those are the only
exits that update the skid-free `last_intercept_work` anchor the V-time LAPIC
timer reads (`vmm.rs`: `complete_tsc` / `rdmsr_vtime`). Plain IO/MMIO exits,
syscalls, and userspace loops do **not** advance it. So any guest code that
busy-waits *without doing RDTSC* freezes V-time → the periodic tick never fires →
the single vCPU is never preempted → permanent deadlock (core pinned at 99.9 %,
serial frozen). This is task 37's *"a busy spin starves everything; there is no
preemption tick"*, made precise.

Two consequences, both observed empirically on the box:

1. **`dockerd` deadlocks.** Its embedded `containerd` boots (Go programs *can*
   run + produce output here), but then dockerd's Go runtime **busy-spins with no
   RDTSC** while creating its containerd client (gRPC) — frozen at *"containerd
   successfully booted"*. A long-running Go **daemon** assumes a free-running
   clock that advances independently of guest progress; consonance's clock does
   not. (This is why the user/integrator agreed dockerd is the wrong primitive.)

2. **`runc` itself deadlocks** — the deeper finding. `runc` is not a daemon, but
   its **container-init (Go) deadlocks the create→exec transition**: the
   container reaches state `"created"` but **never execs its command**. Verified
   with even a trivial `/bin/sh -c 'echo …'` as the container command — the marker
   never prints. runc's Go init + the exec-fifo handshake between `runc run`
   (parent) and the container-init child needs a free-running clock to make
   progress; under frozen V-time it never completes. We tried hard to drive
   V-time from the guest init around it — `runc state`/`runc exec` polls (they
   *hang*, taking runc's per-container lock that the spinning init holds), and a
   `cat /proc/uptime` RDTSC-driver loop — none unstick it, because runc-init's
   wait is internal to a process we don't control.

**Why `unshare` works.** We therefore build the container's isolation
**directly with `unshare` + `chroot` + `setpriv`** — plain *syscalls*, no Go init,
no exec-fifo. The container's PID 1 is a busybox shell that sets up the namespace
view and execs the **cooperative task-37 flow inside the container**: start the
postgres binary, drive it with a local `psql` `SELECT 1` loop, run the workload,
`pg_ctl` stop. That loop advances V-time for exactly the reason task 37's did — a
blocking `psql` connect yields the vCPU to the starting postmaster, whose RDTSCs
(log timestamps, sched_clock) trap → VM-exits → V-time advances → the tick fires →
postgres is scheduled and reaches "ready". The guest `/init` just `unshare`s the
container and `wait`s; the container is the only runnable work and is busy
throughout (no idle-HLT), so nothing has to be driven from outside. **The full
Docker/runc stack is still baked into the rootfs** (the OCI runtime is present —
the finding is that it can't *run* here), and a valid OCI bundle (`/oci` with
`config.json` + the image rootfs) is generated for the record.

### Build (`build-docker-image.sh`, root + Linux only)

1. **Docker static bundle** (`versions.lock`, sha256-verified like the
   kernel/busybox): `dockerd`/`containerd`/shim/`runc`/`ctr` — all statically
   linked, baked under `/usr/local/bin` (present, but unused at runtime per the
   finding). A `--no-pivot` runc wrapper is baked too (it was the fix for an
   earlier runc bring-up: the initramfs root mount has no parent so runc's
   `pivot_root` `EINVAL`s — moot now that runc isn't run).
2. **Official postgres image → rootfs.** `fetch.sh` pulls `postgres:17` **by
   registry digest** (content-addressed — the integrity anchor) with the box's
   `ctr` and exports a `docker load`-format tar; the build extracts the image's
   layers (in order, best-effort whiteouts) into `/oci/rootfs`. NO glibc closure
   to copy (unlike task 37) — the image ships its own userland.
3. **Pre-`initdb`'d PGDATA baked in** (like task 37, and load-bearing the same
   way). Running the image's *entrypoint* would `initdb` at container start — both
   crushingly slow under the single-stepping VMM AND it re-execs through `gosu`,
   a Go program whose runtime busy-spins (the dockerd failure mode again). So
   build-time `initdb` runs once into the rootfs's PGDATA (`chroot --userspec=999`
   + the host /dev/proc bound in; a fixed, snapshotted cluster system id), and the
   container runs the `postgres` binary directly — a cooperative C workload, no
   Go. A determinism overlay (socket-only, pinned TZ/locale, `[pg %p]` pid prefix,
   autovacuum off) is appended to `postgresql.conf`, mirroring task 37.
4. **Ownership preserved in the cpio** (no `--owner=0:0`): the guest-side files
   are root-owned, while the OCI rootfs keeps the image's ownerships + PGDATA
   owned by uid 999, which the container's postgres (uid 999) needs.
5. The workload (`/workload.sql`, == task 37) and the two scripts are baked:
   `pg-container-run.sh` → `/oci/rootfs/run-workload.sh` (runs *inside* the
   container) and `container-setup.sh` → `/container-setup.sh` (the unshared
   PID 1, *before* chroot).

### The container (`container-setup.sh` + `pg-container-run.sh`)

`docker-init.sh` mounts the kernel filesystems + cgroup-v2, creates a
`pg-container` cgroup and moves init into it, then:

```
unshare --mount --uts --ipc --net --pid -f --propagation private  sh /container-setup.sh
```

— fresh **mount / UTS / IPC / NET / PID** namespaces (`-f` forks so the script is
PID 1 of the new pid ns; `--net` with no veth = **`--network none`**, loopback
only). `container-setup.sh` (PID 1) mounts the container's own `/proc` (new pid
ns), `--rbind`s the host devtmpfs into `/dev` (a `nodev` tmpfs can't host mknod'd
nodes), mounts a fresh `/dev/shm` (postgres POSIX shm) + `/tmp`, then
`chroot`s into the image rootfs and `setpriv`s to uid 999 (the image's own
`setpriv` — C, no `/proc/self/exe`) to exec `run-workload.sh`. That script is the
task-37 flow verbatim, now inside the container: `postgres -D PGDATA &`, the
cooperative `psql SELECT 1` readiness loop, the insert/select workload (over the
container-local unix socket), `pg_ctl -m fast -W stop` + `wait`.

### Determinism closure (each item traces to the seed / V-time)

- **The Go-runtime entropy path is on the seeded CRNG** (the spec's load-bearing
  item). `runc`/`unshare`-adjacent code and the kernel read `AT_RANDOM`/
  `getrandom` at process startup to seed map-iteration randomization; under the
  patched backend RDRAND/RDSEED trap to the **seeded stream** and credit the
  kernel CRNG deterministically (the same root as task 37's `pg_strong_random` and
  the build-time `initdb`). The kernel CRNG mixes `random_get_entropy()` = the TSC
  at add-time, which is the **V-time TSC** (every in-guest RDTSC, incl. the vDSO,
  traps — never a laundered host value). `docker-init.sh` prints `boot_id` (the
  CRNG's UUID) as the explicit identical-twice witness; the bit-identical serial
  proves the rest.
- **Namespace + cgroup setup is a pure function of guest execution** (syscalls
  under a single vCPU, no probe-spin), and the postgres flow is multiprocess but
  deterministic by construction (single vCPU kills SMP races, sequential fork
  PIDs, V-time-driven background workers) — exactly as task 37 documented.
- **The terminal is a forced triple-fault** (`reboot=t,force` + `reboot -f`);
  note it takes a bounded-but-non-trivial number of VM-steps to reach the `Hlt`
  terminal after `GUEST_READY` (the kernel reboot path), all deterministic.

### Blame boundary (the spec's gate 3)

Task 37 (bare Postgres) isolates the **database** determinism surface; this task
adds only the **container surface** — the namespace/cgroup isolation + the
container-internal driving — on top of it. The DB workload, locale, and
final-row golden are **identical to task 37 by construction**, so a divergence
localizes cleanly to a layer: if the `row|…` values match task 37 but the run
diverges, the fault is in the container surface, not the DB.

### Acceptance-gate evidence (box, `ssh hetzner`, core-2-pinned, reverted to stock 1396736)

Built with `make -C harmony-linux fetch && make -C harmony-linux/linux docker-image` on the box;
patched 6.12.90 proxy modules loaded, `taskset -c 2`, **always reverted to stock
KVM (`1396736`) after each run and verified** (`run-patched-ht38.sh`'s trap).

- **Gate 1 — Dockerized Postgres runs + streams** (`p1_docker_postgres_runs_and_streams_patched`):
  one patched boot brought up the OCI container, postgres announced ready, the
  workload streamed, `GUEST_READY`, clean terminal — `test result: ok`:

  ```
  [p1] steps=162728 terminal=Some(Hlt) container_up=true pg_ready=true
       workload_done=true final_row=true GUEST_READY=true step_error=None
  ```
  (final_row = `row|20|407|20|3010`, the same pure-function-of-the-index row task
  37 pins; ~162.7k steps, ≈ task 37's 162.6k — the container surface adds almost
  no VM-steps.)

- **Gate 2 — deterministic twice (the milestone)** (`p2_docker_postgres_deterministic_twice_patched`):
  two same-seed patched boots through the full container surface → **bit-identical**
  serial (incl. the `row|…` query output) **and** `state_hash`:

  ```
  [p2 run A] steps=162728 terminal=Some(Hlt)  container_up pg_ready workload_done final_row GUEST_READY = all true
  [p2 run B] steps=162728 terminal=Some(Hlt)  container_up pg_ready workload_done final_row GUEST_READY = all true
  serial A == serial B  (16094 bytes, including the row|… query output)
  state_hash A = state_hash B =
    ab6635f93cd65d9a5c647507482849b22959cd4c377082b41a544a1a16b362a0
  ```
  (Run A and Run B retire the *same* 162728 VM-steps — the container surface is
  fully deterministic — and `test result: ok` confirms the bit-identical serial +
  `state_hash`.)

- **Gate 3 — blame boundary documented** (above): task 37 isolates the DB
  surface, this task adds only the container surface.

- **No regression / box hygiene:** only `harmony-linux/linux/` + the box-only
  `live_postgres_docker.rs` changed; the kernel / minimal image / `devices.rs` /
  contract are untouched, so M1/M2/P6 + the acceptance-suite goldens + the `state_hash`
  schema are byte-unchanged. Every patched run reverted to stock KVM (`1396736`)
  and was verified.

### Deviations considered / limitations

- **Containers via `unshare`, not runc/dockerd** — forced by the deadlock finding
  above (runc-init never execs under frozen V-time), confirmed by the integrator's
  "OCI images, not docker" framing. It is still the official postgres OCI image in
  real namespace + cgroup isolation; only the OCI *tooling* is bypassed. The full
  docker/runc stack stays baked and a valid OCI bundle is generated, so the
  finding is reproducible and a future VMM with a forced-preemption/exit mechanism
  could revisit runc.
- **Pre-baked PGDATA + direct `postgres`** (not the image entrypoint): the
  entrypoint's runtime `initdb` + `gosu` (Go) deadlock/slow the VMM; pre-baking is
  task 37's proven pattern. The image is otherwise off-the-shelf.
- **Image not byte-reproducible across separate builds** (registry export +
  build-time initdb system id) — runtime determinism, the gate, is unaffected; the
  integrity anchor is the digest-pinned pull.
- **`--network none`** drops the entire bridge/netfilter surface (config *and*
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
   **(Superseded by task 42** — the workload now populates each row with a
   `gen_random_uuid()` id + a `clock_timestamp()` column; see the Task 42 section at
   the top. The determinism closure below is unchanged and is exactly what makes those
   random/wall-clock columns come out bit-identical.)

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

Built with the repo `make -C harmony-linux/linux postgres-image` (kernel reused from task 36;
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

- **Gate 3 — no regression:** only `harmony-linux/linux/` (+ the box-only `live_postgres.rs`
  test) changed; the kernel/minimal-image/`devices.rs`/contract are untouched, so
  M1/M2/P6 + the acceptance-suite goldens and `state_hash` schema are byte-unchanged.
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
GUEST_BUILD_ROOT=/tmp/ht36-guest-build make -C harmony-linux/linux image     # bzImage + initramfs
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

## Task 60 — the Postgres-campaign image (planted, fault-triggerable bug)

`build-campaign-image.sh` (Makefile target `campaign-image`) builds
`initramfs-campaign.cpio.gz`: the task-37 bare-Postgres image **plus** a static supervised process
`campaign-super` (compiled from `campaign-super.c`) and the `campaign-init.sh` `/init` that runs it.
Everything else — the pinned PostgreSQL 17 .debs, the determinism overlay, the fixed-UUID ext4, the
reproducible cpio packing — is `build-postgres-image.sh` verbatim, so the campaign image inherits task
37's determinism closure. **No kernel change**: the companion bzImage is the unchanged task-36
container-class kernel (`mmap`/`mlock` are available; **`ioperm`/`iopl`/`/dev/port` are NOT** — this
kata-derived kernel has no `CONFIG_X86_IOPL_IOPERM` / `CONFIG_DEVPORT`, which the box proved and shaped
the terminal mapping below), so the kernel golden (`MANIFEST.sha256`) is untouched.

**The planted bug** (the campaign's target, task 60). `campaign-super` keeps a small **ledger** (a
canary + a signed retry budget) in a fixed-address, `mlock`'d, `volatile` guest page — a deterministic
guest-physical address (nokaslr + `MAP_FIXED` + `MAP_POPULATE`) the campaign's `CorruptMemory` fault can
find by searching. It prints `CAMPAIGN_READY` (the base-snapshot marker — mid-workload, post the ambient
Postgres workload), then runs a **long** (`ITERS = 2×10⁸`), bounded, deterministic retry loop whose
bookkeeping invariant (canary intact, `0 ≤ budget < BUDGET_MAX`) holds on every nominal iteration. The
loop is long so the mid-workload base snapshot seals **inside** it (a short loop the seal overshoots
leaves the fault target unreachable — the box proved this). A **single-event upset** — a host
`CorruptMemory` that flips the canary (or the budget's sign bit) at a `Moment` inside the loop — is the
only way to reach the guarded branch; the supervisor detects the impossible state, prints `CAMPAIGN_BUG:`
to the serial, and exits non-zero.

**Terminal mapping (as shipped in `campaign-init.sh`).** A guest *process* cannot reach an I/O port on
this kernel, so the bug does **not** signal `Crash{Panic}` via isa-debug-exit (`campaign-super`'s boot
self-test reports `CAMPAIGN_IOPERM`/`IOPL`/`DEVPORT` all FAILED). Instead `/init` maps the outcome to two
distinct guest terminals the *kernel* produces:

- **bug** (`campaign-super` exits non-zero) → `reboot -f` → triple-fault → `KVM_EXIT_SHUTDOWN` →
  **`StopReason::Crash{Shutdown}`** — the reportable bug;
- **clean** (exits 0, `CAMPAIGN_DONE`) → `halt -f` → the boot CPU HLTs → **`StopReason::Quiescent`** —
  the benign terminal.

Both use `-f` (force), skipping `device_shutdown` (which strands once block I/O is used — the pg-init
lesson). So the campaign oracle is the standard **"any `Crash`/`Assertion` is the bug; `Quiescent` is
clean."** The full trigger conditions, the oracle, the window/`ScheduleUnsatisfiable` bound, the
search-space tuning, and the box runbook + PASS result are in
**`dissonance/conductor/IMPLEMENTATION.md` §"Task 60"**.

Bring-up aid: boot with `CAMPAIGN_DEBUG=1` in the environment and `campaign-super` prints
`CAMPAIGN_LEDGER_GPA:` (read via `/proc/self/pagemap`, needs root/CAP_SYS_ADMIN), so the operator can
scope `conductor campaign box --gpa-*` tightly. This is box-built and box-validated by the foreman
(the C is Linux-only; the shell is shellcheck-clean).

## Task 69 — the benchmark bugs (ii) ordering-interrupt and (iii) rare-entropy

Task 69 extends task 60's single planted bug into a **benchmark** of three bugs of
distinct classes (the shared fixture tasks 71/72/75 extend). Bug (i) is task 60's
`campaign-super` above, **reused verbatim**. Two new supervised processes, built to
the same conventions (static `cc -static -O2`; fixed-address, `volatile`
bookkeeping; `ORDER_READY`/`UUID_READY` base-snapshot markers; the isa-debug-exit
terminal with the same kernel-terminal fallback `campaign-init.sh` documents):

- **`order-super.c` — bug (ii), ordering / interrupt-timing.** A two-word invariant
  `mirror == ~primary` updated **non-atomically** inside a fixed per-iteration
  window. KVM delivers a task-59 `InjectInterrupt { vector }` to the guest **kernel
  IDT**, not as a userspace signal, so the reachable, userspace-observable effect is
  a **kernel reschedule**: an injected reschedule-class interrupt landing in the
  window preempts the process mid-update (an **involuntary context switch**, counted
  in `rusage.ru_nivcsw`), leaving the pair torn while descheduled. The process
  samples `ru_nivcsw` across the window; a change means it was preempted inside it →
  it aborts with **`ORDER_BUG:`** (crash code `0x62`). Outside the window the same
  preemption is harmless. Window width is the time-to-find dial (~256 branches); the
  manifest vector `0x81` is wired to a reschedule-class vector at box bring-up.
  Manifest: `benchmark` `BugId(2)`. *(An earlier draft modeled the injected
  interrupt as a POSIX `SIGUSR1` handler — wrong, since KVM never turns an IDT
  interrupt into a userspace signal; the milestone-1 review caught it.)*
- **`uuid-super.c` — bug (iii), rare-entropy-value.** Draws its run value from the
  VMM's **RDRAND intercept** (the seeded-entropy service — task 42's
  `gen_random_uuid()` path), executed **after the `UUID_READY` snapshot marker** so
  each branch's EnvSpec seed actually varies it. The source must be RDRAND, not a
  pre-snapshot `getenv("SEED")`: an env var is baked into the process before the
  base seal, so branching could never vary it and the search was a no-op (the
  round-2 review's deeper P1 — moving the *draw* after the marker was not enough,
  the *source* had to be post-snapshot and campaign-controlled). The RDRAND word
  **is** the draw (the first word of `SeededEntropy::new(EnvSpec.seed)`, the
  xorshift64* stream in `consonance/hypercall-proto`); `benchmark::trigger::
  entropy_draw` replicates that **exact** function so the guest and the model agree
  bit-for-bit on which seeds fire (the round-3 stream-matching fix — an earlier
  draft re-hashed with a splitmix64 the model did not share). A branch taken only
  when the draw's top `PREFIX_BITS = 8`
  bits match `0xA5` emits **`UUID_BUG:`** (crash code `0x63`) **before** poisoning a
  pointer and dereferencing it (so the attribution gate always sees the marker for
  the bug the deref then crashes on). Fire probability `2^-8` ⇒ ~256 branches.
  `PREFIX_BITS` is the dial. Manifest: `benchmark` `BugId(3)`.

Each bug has a **distinct serial marker** (`CAMPAIGN_BUG` / `ORDER_BUG` /
`UUID_BUG`) and crash code (`0x60`/`0x62`/`0x63`) so fingerprints attribute finds
per-bug (spec gate 2). The portable **toy** stand-in for all three — the trigger
predicates, unit-tested to fire 100% / nominal never — lives in
`dissonance/benchmark/src/trigger.rs`, and the record-emitting toy machine that
drives the signal-configured campaign portably is
`dissonance/conductor/src/benchcampaign.rs::BenchToyMachine`.

**Status (task 69 milestone 1):** the C payloads are committed **source**; their
box images (`build-campaign-image.sh` analogues) and the full ≥20-seed real-KVM
campaign that produces the GO/NO-GO ruling are **milestone 2** — these two files are
Linux-only and are box-built/box-validated by the foreman there. The trigger logic
they implement is already validated portably (the `benchmark` toy predicates +
`conductor::benchcampaign` gates).
