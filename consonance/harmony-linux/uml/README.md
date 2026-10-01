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
- `linux/patches/um/0001-um-harmony-require-seccomp.patch` makes SECCOMP
  userspace the only mode. `seccomp=` accepts only `on`, boot aborts when the
  filter cannot be installed, and the ptrace startup checks are removed.

## Host requirements

The profile runs on Linux hosts of the profile's architecture. Guest
instructions run natively, so a recording is valid only on the CPU model that
made it; `HostIdentity` reads that model. Guests must be trusted: the UML
SECCOMP filter does not stop guest userspace from reading UML's memory.

The launcher sets `GLIBC_TUNABLES=glibc.pthread.rseq=0`. UML keeps its own
memory image, and a glibc-registered restartable-sequence area in that image
does not survive an in-place restore.

## Qualification

`harmony-uml-qualify --profile DIR` runs the boot and cleanup checks as an
ordinary user. It refuses to run as root or with effective capabilities,
installs a SECCOMP filter on itself that denies ptrace and every KVM ioctl,
and then runs:

- 20 boot cycles (`--cycles`), each with a clean exit, a passing fixture, the
  SECCOMP startup check, and no leftover host process or work directory;
- a hanging guest stopped by the wall-clock limit;
- a flooding guest stopped by the console limit with a bounded console tail;
- 16 guest processes killed from outside, with every stub process reaped;
- a guest whose SECCOMP filter installation is denied, which must exit with
  the SECCOMP failure and never try ptrace;
- an init exit, which must end in a kernel panic with nothing left behind.

The report records the effective UID, `CapEff`, the denial results, the host
CPU model and kernel, and the verified profile.
