<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# User-mode Linux profile

This directory builds the User-mode Linux (UML) profile: the pinned Linux
6.18.35 kernel compiled with `ARCH=um` as an ordinary host executable, a fixture
initramfs, and `profile.json`, which names the SHA-256 of every artifact. The
`uml` crate in `consonance/uml` verifies the profile and launches it.

UML runs without KVM, root, or ptrace. The guest kernel is a host process; each
guest address space is a host stub process that the kernel drives through a
SECCOMP filter and a shared page. Time comes from UML's `time-travel=inf-cpu`
mode, so guest time advances only when the guest kernel advances it.

## Deterministic execution

Every guest input comes from the profile, the boot arguments, or the host
bridge. A run with the same profile, seed and host CPU produces the same
sequence of guest events.

- Virtual time starts at zero and moves only by the cost model. Each guest
  system call costs `CONFIG_HARMONY_UML_SYSCALL_VNS` (100,000 ns) before it
  runs; `clock_gettime`, `gettimeofday`, `time` and each counter read cost
  `CONFIG_HARMONY_UML_CLOCK_READ_VNS` (1 ns). Kernel clock reads, host events
  and context switches cost nothing. An idle guest jumps to its next timer
  deadline, and timers due at the same moment fire in the order they were
  armed. `profile.json` records both costs under `virtual_time`.
- Preemption comes from the timer tick that these costs drive. A guest loop
  with no system calls never advances time, so it holds the CPU until the
  wall-clock limit stops it.
- `/dev/harmony` reaches the host through a socket that the launcher passes
  as `harmony_fd=3`. Each request carries the virtual time it was made at, and
  the guest waits for the answer, so the host can never deliver anything at a
  host-chosen point. The host answers entropy requests from the run seed and
  records each guest event with its virtual time. Any other service is
  refused.
- `harmony_seed=` carries a 32-byte boot seed derived from the run seed. It
  seeds the kernel random pool, so `getrandom`, `AT_RANDOM`, address-space
  layout and `/proc/sys/kernel/random/boot_id` follow the seed. UML no longer
  reads host entropy.
- On x86, each stub process sets `PR_SET_TSC` to fault on `rdtsc` and
  `rdtscp`. The kernel emulates both from virtual time.
- `/proc/cpuinfo` no longer shows the host `uname` line. The CPU flags and
  model still come from the host and `cpuid` runs natively, so the recording
  names both.
- Host stops and continues of stub processes no longer raise guest
  interrupts. A stub that dies without the kernel killing it panics the
  guest.
- Guest physical memory has a fixed layout. UML turns off host address
  randomization by re-executing itself, which Docker's default seccomp
  profile refuses. The range reserved for the host heap covers the host's
  whole heap randomization range and stays out of the guest's page
  allocator, so the guest sees the same memory either way.

## Building

```sh
nix run .#uml-images -- --output "$PWD/uml-output"
```

The flake supplies gcc 13 and static glibc, the pinned kernel and musl sources,
and a copy of the repository. `nix-build.sh` runs `build-uml.sh` in a fresh
temporary directory and copies four files to the output: `linux`, `config`,
`initramfs.cpio.gz`, and `profile.json`. Two builds from the same commit are
byte-identical; CI checks this on every pull request.

`build-uml.sh` also runs outside Nix on a Linux host with a C toolchain and the
static C library, after `make -C consonance/harmony-linux fetch`. It extracts a
source tree of its own under `GUEST_BUILD_ROOT`, applies `linux/patches/common`
and then `linux/patches/um`, configures `defconfig` with `config-fragment`
merged last, and asserts the symbols the profile depends on.

## Profile contents

- `linux` is statically linked, so the profile has no host library closure.
  Profile verification rejects an executable with an ELF interpreter and a
  profile that lists host libraries.
- `config` keeps one CPU, periodic 100 Hz ticks, and time-travel support. It
  removes host-backed devices: hostfs, the host random device, the management
  console, block and network drivers, the RTC, and every console channel
  except file descriptors and null.
- `initramfs.cpio.gz` holds `fixture-init.c` built against the pinned musl.
  The `harmony_fixture=` boot parameter selects a mode: `boot` forks 16
  children, checks their exit codes and powers off; `hang` spins without system
  calls; `flood` writes the console forever; `orphans` leaves 16 sleeping
  children and spins; `exit` returns from init so the kernel panics.
  Three modes write their observations to `/dev/harmony` as events: `values`
  reports clocks, counters, random values, addresses, IDs, the auxiliary
  vector and the `/proc` files a program reads at startup, then runs
  `fixture-registers.c` to report the startup registers and FPU state;
  `schedule` runs four workers that yield and exchange pipe messages;
  `timers` uses sleeps, interval timers, `timerfd`, `poll` and `select`.
- `linux/patches/um/` holds the UML series: SECCOMP userspace as the only
  mode, virtual time and its costs, the host bridge, the boot seed, counter
  emulation, host child signals, and the fixed memory layout.

## Host requirements

The profile runs on Linux hosts of the profile's architecture. Guest
instructions run natively, so a recording is valid only on the CPU that made
it. `HostIdentity` records the CPU model and feature flags, and replay refuses
a recording from a different CPU. Guests must be trusted: the UML
SECCOMP filter does not stop guest userspace from reading UML's memory.

The launcher sets `GLIBC_TUNABLES=glibc.pthread.rseq=0`. UML keeps its own
memory image, and a glibc-registered restartable-sequence area in that image
does not survive an in-place restore.

## Qualification

`harmony-uml-qualify --suite launch|replay --profile DIR` runs as an
ordinary user. It refuses to run as root or with effective capabilities and
installs a SECCOMP filter on itself that denies ptrace and every KVM ioctl.
The `launch` suite runs:

- 20 boot cycles (`--cycles`), each with a clean exit, a passing fixture, the
  SECCOMP startup check, and no leftover host process or work directory;
- a hanging guest stopped by the wall-clock limit;
- a flooding guest stopped by the console limit with a bounded console tail;
- 16 guest processes killed from outside, with every stub process reaped;
- a guest whose SECCOMP filter installation is denied, which must exit with
  the SECCOMP failure and never try ptrace;
- an init exit, which must end in a kernel panic with nothing left behind.

The `replay` suite runs each of `values`, `schedule` and `timers` 100 times
(`--replays`), spread over five host conditions: plain, four busy host
threads, the guest pinned to one CPU with a busy thread, the guest process
group stopped and continued every few milliseconds, and the guest denied the
`personality` call that turns off address randomization. Every run must end
cleanly with the same event hash. It then saves a recording at five cuts
(`--cuts`) through each run, reloads it, and replays it in a fresh process,
which must stop at the cut with the same hash. Finally it checks that a
different seed changes the `values` events and that a recording made on
another CPU is refused.

The event hash is SHA-256 over each event's virtual time, ID, length and
data.

The report records the effective UID, `CapEff`, the denial results, the host
CPU and kernel, and the verified profile.
