<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# User-mode Linux profile

This directory builds the User-mode Linux (UML) profile: a pinned Linux kernel
compiled with `ARCH=um` as an ordinary host executable, a fixture initramfs,
and `profile.json`, which names the SHA-256 of every artifact. The `uml` crate
in `consonance/uml` verifies the profile and launches it.

The x86-64 profile builds Linux 6.18.35. Linux has no arm64 UML port in a
release, so the arm64 profile builds the RFC port at
`zalexdev/linux-um-arm64` commit `8897487c52233cd00cf2850008ca068892f1ae91`
(7.2-rc4), pinned by hash in `linux/versions.lock`.

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
  host-chosen point. The host answers entropy requests from the run seed,
  records each guest event with its virtual time, and answers the SDK's
  standing-service questions, coverage requests and payload pulls with the
  code the KVM backend uses. Any other service is refused.
- `harmony_seed=` carries a 32-byte boot seed derived from the run seed. It
  seeds the kernel random pool, so `getrandom`, `AT_RANDOM`, address-space
  layout and `/proc/sys/kernel/random/boot_id` follow the seed. UML no longer
  reads host entropy.
- Each stub process sets `PR_SET_TSC`, so guest counter reads fault. The
  kernel emulates them from virtual time: `rdtsc` and `rdtscp` on x86, and
  `cntvct_el0` and `cntvctss_el0` on arm64, each returning nanoseconds.
  `cntfrq_el0` returns 1 GHz. The arm64 vDSO has no clock functions, so C
  library clock calls are system calls and follow virtual time.
- `/proc/cpuinfo` no longer shows the host `uname` line. The CPU flags and
  model still come from the host and `cpuid` runs natively, so the recording
  names both. A guest program that executes `rdrand` or `rdseed` gets host
  randomness, and one that reads the APIC ID from `cpuid` (leaf 1 EBX bits
  31:24, or the x2APIC ID in leaves 0xB and 0x1F) gets the ID of the host
  CPU it happens to run on; workloads must not use them.
- Host stops and continues of stub processes no longer raise guest
  interrupts. A stub that dies without the kernel killing it panics the
  guest.
- Guest physical memory has a fixed layout. UML turns off host address
  randomization by re-executing itself, which Docker's default seccomp
  profile refuses. The range reserved for the host heap covers the host's
  whole heap randomization range and stays out of the guest's page
  allocator, so the guest sees the same memory either way.

## Checkpoints

The host creates the guest's physical memory file and passes it as descriptor
4 with `harmony_physmem=4`. The host takes a checkpoint while the guest kernel
waits for a `/dev/harmony` answer. Instead of the answer, the host sends a
control message with a file descriptor for the image. The kernel parks on the
host stack with every host signal blocked and reports the physical pages
written since the last capture or restore, or that every page may have
changed. The host copies those pages while the guest waits, then sends
`CONTINUE`. The kernel writes the image and acknowledges it with the image
size. Nothing is flushed or drained and virtual time does not move. The guest
then waits for the answer again.

The kernel tracks its own writes by mapping the physical memory range
read-only after each capture or restore. The first write to a page faults, and
the host signal handler records the page and makes it writable again. A guest
program's page is mapped writable in its stub process only once the page is
recorded, so the first write faults into the kernel, which records it without
counting a page fault for the program. A capture takes write access away from
the stubs again. Kernel stacks stay writable, because host system calls write
into them, and are reported at every command. So are stub data pages and
vmalloc pages, which are written outside tracking. After 16384 pages and
stacks are made writable in one interval, the kernel makes the whole range
writable and reports every page.

An image holds:

- captured: the UML process's private memory other than its code and the host
  stack (the binary's data, read-only data and BSS, the heap, anonymous
  mappings and the C library's thread data), the host mappings of the
  physical memory file outside the main physical memory range (vmalloc
  space), the vDSO address and the thread pointer;
- checked, not copied: the executable mappings, the binary's end, the
  physical memory range, a single host thread, and the open host descriptors
  by number, type and access mode, excluding stub sockets.

A restore checks the image before it changes anything and refuses an image
that does not match. It then reports the pages written since the last
command as a bitmap, sent in 64 KiB messages so that any guest size fits the
socket buffer, and the host writes back each page that differs from the target
checkpoint. After `CONTINUE` the kernel kills the stub processes, removes the
epoll registrations, copies the image back (writable ranges keep their
protection, because the arm64 binary's writable segment is also executable),
moves the vDSO to its captured address,
and resumes at the capture point. There the kernel rebuilds what lives outside
the image: a new stub process and socket for each guest address space, with
every present page marked for remapping, and the epoll registrations with
`O_ASYNC` ownership for the current process. The host stack, the initial jump
buffer, the environment pointer and the descriptors stay those of the running
process. The kernel acknowledges the restore and waits for the answer that was
pending at the capture.

The host stores the image and the physical memory of every checkpoint as
pages in one `snapshot-store` store, each checkpoint a delta on its parent.

With `harmony_restore` on the command line, the kernel asks the host for an
image at the end of boot and restores it into the fresh process. The fresh
process must use the same profile, memory size and host CPU. The host keeps
its own part of the checkpoint: the entropy state, the recorded events, the
latest virtual time and the pending answer.

## Building

```sh
nix run .#uml-images -- --output "$PWD/uml-output"
```

The flake supplies gcc 13 and static glibc, the pinned kernel source for the
host architecture, musl, and a copy of the repository. `nix-build.sh` runs `build-uml.sh` in a fresh
temporary directory and copies four files to the output: `linux`, `config`,
`initramfs.cpio.gz`, and `profile.json`. Two builds from the same commit are
byte-identical; CI checks this on every pull request.

`build-uml.sh` also runs outside Nix on a Linux host with a C toolchain and the
static C library, after `make -C consonance/harmony-linux fetch` on x86-64 or
`make -C consonance/harmony-linux fetch-uml-arm64` on arm64. It builds for the
host architecture. It extracts a source tree of its own under
`GUEST_BUILD_ROOT`, applies `linux/patches/common` and then
`linux/patches/um` or `linux/patches/um-arm64`, configures `defconfig` with
`config-fragment` merged last (and `config-fragment-arm64` after it on arm64),
and asserts the symbols the profile depends on.

## Profile contents

- `linux` is statically linked, so the profile has no host library closure.
  Profile verification rejects an executable with an ELF interpreter and a
  profile that lists host libraries.
- `config` keeps one CPU, periodic 100 Hz ticks, and time-travel support. It
  removes host-backed devices: hostfs, the host random device, the management
  console, block and network drivers, the RTC, and every console channel
  except file descriptors and null. It enables what the OCI runtime and the
  supervisor use: PID, IPC, UTS, network and mount namespaces, cgroup v2 with
  the pids, devices, CPU and freezer controllers, BPF device filters without
  the JIT, Unix and IP sockets, POSIX timers, sysctl, and
  `/proc/<pid>/task/<tid>/children`. Kernel stacks come from the direct map
  (`VMAP_STACK` off): UML backs each vmalloc area with a host mapping of its
  own, a guest with many threads would exceed the 512 mappings an image
  holds, and capture would fail with `E2BIG`.
- `initramfs.cpio.gz` holds `fixture-init.c` built against the pinned musl.
  The `harmony_fixture=` boot parameter selects a mode: `boot` forks 16
  children, checks their exit codes and powers off; `hang` spins without system
  calls; `flood` writes the console forever; `orphans` leaves 16 sleeping
  children and spins; `exit` returns from init so the kernel panics.
  Three modes write their observations to `/dev/harmony` as events: `values`
  reports clocks, counters, random values, addresses, IDs, the auxiliary
  vector and the `/proc` files a program reads at startup, then runs
  `fixture-registers.c` to report the startup registers and the
  floating-point and vector state. Six times along the way it writes the
  same 12,288 pages and reports its thread CPU time, so a run that continues
  after a checkpoint takes a tracked write fault on every page and shows
  whether those faults moved the scheduling clock;
  `schedule` runs four workers that yield and exchange pipe messages;
  `timers` uses sleeps, interval timers, `timerfd`, `poll` and `select`;
  `counter` reads the CPU counter (`rdtsc` or `cntvct_el0`) around system
  calls, and on arm64 the counter frequency.
- `linux/patches/um/` holds the UML series: SECCOMP userspace as the only
  mode, virtual time and its costs, the host bridge, the boot seed, counter
  emulation, host child signals, the fixed memory layout, checkpoints, and
  the initial FPU state for every new program, as x86 Linux does; UML
  otherwise copies the vector registers of a stub process, which hold
  leftovers of host strings such as the command line. Its last patch stops
  `flush_tlb_mm` and the checkpoint walks at the `XA_ZERO_ENTRY` that a
  failed `dup_mmap` leaves in the child's VMA tree; without it, a kill that
  lands while a process forks panics the guest. Mainline Linux no longer
  leaves the marker, so the arm64 series, built on 7.2-rc4, does not carry it.
  `linux/patches/um-arm64/` ports the same series to the arm64 RFC. Its
  counter emulation also answers the frequency register, and the stub waits
  for the kernel without the RFC's counter-bounded spin, because its own
  counter reads fault. It adds two build fixes for the RFC: the host headers
  compile against current glibc, and the stub has no frame pointer, so
  `rt_sigreturn` finds the signal frame where the host kernel wrote it. It
  also starts every guest program with zeroed vector registers, FPSR and FPCR,
  as arm64 Linux does; UML otherwise copies them from a stub process whose
  values depend on host address randomization. It copies the host's
  `AT_PLATFORM` string at boot, because arm64 gives it to every guest program
  and the original lives on the host stack, which a restore in a fresh process
  replaces. It kills and reaps a destroyed address space's stub before
  releasing its socket and pages, as the x86 port does; the RFC released them
  when the host's SIGCHLD arrived, so page frees and open descriptors followed
  host timing. Its linker script puts the code in a read and execute segment of its own,
  as x86 does; the RFC linked the whole binary as one writable segment, so
  every checkpoint copied the code.
- `config-fragment-arm64` selects 4 KiB pages.

## Host requirements

The profile runs on Linux hosts of the profile's architecture. arm64 hosts
must run Linux 6.12 or later, the first release where `PR_SET_TSC` traps
counter reads, and must use 4 KiB pages: a guest page is a host mapping, and
the arm64 profile uses 4 KiB pages. Guest
instructions run natively, so a recording is valid only on the CPU that made
it. `HostIdentity` records the CPU model and feature flags, and replay refuses
a recording from a different CPU. Guests must be trusted: the UML
SECCOMP filter does not stop guest userspace from reading UML's memory.

The launcher sets `GLIBC_TUNABLES=glibc.pthread.rseq=0`. UML keeps its own
memory image, and a glibc-registered restartable-sequence area in that image
does not survive an in-place restore.

## Qualification

`harmony-uml-qualify --suite launch|replay|checkpoint --profile DIR` runs as an
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
which must stop at the cut with the same hash. The `counter` fixture gets the
same replays. Finally it checks that a different seed changes the `values`
events and that a recording made on another CPU is refused.

The `checkpoint` suite runs each fixture once to get the cold event hash,
then runs six diamonds (`--diamonds`), half of them with the `personality`
call denied. Each diamond captures at a quarter and at half of the events,
runs to three quarters, restores the first image in place, runs again to
half, restores the second image, and runs to the end. Each image is then
restored into a fresh process, with and without address randomization. A
search publishes checkpoints as page deltas from the setup checkpoint and
imports them onto a setup with the same digest, so the suite also captures
at a quarter, half and three quarters, rebuilds the half image from its
delta on the quarter image, and runs the result in place and in a fresh
process. It reports whether a second process reaches a byte-identical
quarter image. Every run must end with the cold hash. Two planted omissions
must fail: an image captured without host memory, and a restore that keeps
the host's bridge state. The first two diamonds compare every physical page
with the stored checkpoint after each capture and restore, and fail on a
page that changed without being reported. The report gives capture and
restore times from the other diamonds, image sizes and the bytes each
checkpoint owns in the store.

The event hash is SHA-256 over each event's virtual time, ID, length and
data.

The report records the effective UID, `CapEff`, the denial results, the host
CPU and kernel, and the verified profile.

`harmony-uml-qualify exec [--report FILE] -- COMMAND...` makes the same
credential checks, installs the same filter, writes the credentials, denial
results, host and command to the report, and executes the command under the
filter. Campaigns run `harmony search --backend uml` this way.
