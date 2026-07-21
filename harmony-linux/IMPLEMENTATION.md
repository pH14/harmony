# `harmony-linux` implementation record

SPDX-License-Identifier: AGPL-3.0-or-later

Task 43 (`hm-ciz`) moved the former guest-environment tree here, with the R-L4
carve-out: `payloads/` and `golden/` moved instead to
`consonance/acceptance-suite/{payloads,golden}`. `flow-agent`, `play-agent`, and
the internal SDK moved with Linux because they are environment-side plumbing.
The root Cargo workspace excludes `harmony-linux`; each Rust component retains
its explicit standalone workspace boundary.

The task-43 glossary window also renamed `consonance/det-corpus` to
`consonance/acceptance-suite`, its selector enum to `OracleKind`, and
`consonance/vmcall-transport` to `consonance/hypercall-doorbell`. The Explorer
`Oracle` trait and the transport's public `VmcallTransport` type are intentionally
unchanged.

## New environment ABI

`linux/patches/x86/0002-x86-harmony-character-device.patch` adds a built-in misc
driver at `/dev/harmony`. It is Linux kernel code carried only as a GPL-2.0 diff.
The driver rides the existing fixed-page/OUT doorbell; consonance and the wire
protocol do not change. A global mutex makes every exchange single-in-flight,
and the sequence counter plus per-open pending entropy bytes live in guest memory,
so snapshot/replay captures all future-affecting state.

`libvoidstar/` is an AGPL clean-room implementation based only on the public SDK
ABI. Its JSON, entropy, flush, legacy coverage, and sanitizer coverage symbols are
ABI-tested. Coverage hooks are deterministic no-ops until a coverage service is
specified. Every image builder installs the same fixed-path build artifact at
`/usr/lib/libvoidstar.so`.

## Reproducibility decision

The driver changes `bzImage` and the library changes every initramfs, so keeping
the previous manifest was impossible. Per task 90's ruling, the already-required
rebaseline also removes the stale `/tmp/hypervizor-guest-build` and `hypervizor`
Kbuild identity strings. The new fixed inputs are `/tmp/harmony-linux-build` and
`KBUILD_BUILD_{USER,HOST}=harmony`. `GUEST_BUILD_ROOT` remains the supported
override. `linux/run-tests.sh` must reproduce both builds byte-for-byte before it
updates `linux/MANIFEST.sha256`; no digest is hand-edited.

The workload-boundary findings and deferred actions are recorded in
`docs/CONSONANCE-WORKLOAD-AUDIT.md`. Detailed kernel/image history remains in
`linux/IMPLEMENTATION.md`; component-specific decisions live beside each agent,
SDK, and compatibility library.

## Rebaseline result

On 2026-07-20, `make -C harmony-linux test-linux` passed on the determinism box
(native x86_64) inside the pinned Debian build container
`sha256:475844e1d00c30c8c247706e8887379d3b503e036844e827749285625239c7e0`
(gcc 14.2.0-19). The gate completed two clean builds, compared them byte-for-byte,
and then booted the result under QEMU through `GUEST_READY` with exit status 0.
The full run transcript is `linux/rebaseline-2026-07-20.txt`.

This rerun closes review finding F1: `linux/clean-artifacts.sh` now also removes
`$BUILD_ROOT/libvoidstar-build`, so `libvoidstar.so` is compiled from source in
*both* reproducibility legs (transcript: two compiler invocations, zero cached
reuse) instead of the second leg silently reusing the first leg's cached library.
The `bzImage` digest is unchanged — the kernel is untouched by the driver-adjacent
fixes, and the box-native build reproduces the earlier digest byte-for-byte — while
`initramfs.cpio.gz` changes because the unspecced `HARMONY_DEVICE_PATH` override was
removed from `libvoidstar` (finding F12), altering the packed library. The generated
`linux/MANIFEST.sha256` records:

- `bzImage`: `91b092c56b18df883d3289bafa536e12ab5227dc94235500f6f634c9e2d89c7b`
- `initramfs.cpio.gz`: `7218d705ba8e856518f7e8754e9ede92d9cbd7db79c64f5c9ce1334e4612a652`
