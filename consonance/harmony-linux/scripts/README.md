# Platform scripts

The instruction scanners reject unsupported instructions in shipped guest
executables. Their planted negative controls must continue to fail. The N6
entropy scanner reads executable ELF load segments, because section headers can
mislabel code without changing the bytes or permissions the guest loader maps.
On x86 it decodes contained allocated sections from their starts and scans the
gaps between them, so headers or padding cannot shift an instruction boundary
or create a false hit across a section boundary. Arm64 is checked as aligned
words across the whole executable segment, regardless of section lengths.
Its hidden-section negative control clears the `.text` executable flag while
leaving `PT_LOAD` executable and must still be rejected.

The arm64 AA-4 and AA-5 scanners compare loaded section addresses and file
offsets with their executable `PT_LOAD` mapping. They reject overlapping
executable segments, duplicate or overlapping loaded sections, a non-executable
`.text`, an entrypoint in data, and unreviewed data-section names. Both AA-4
and AA-5 check every aligned word in the 4 KiB page-rounded file mapping of a `PF_X` load
segment, including words labeled as data or lying outside its declared
`p_filesz`. The ARM kernel builder requires 4 KiB pages to match that scan.
The scanners reject load segments whose file or virtual pages
overlap an executable load's pages, so a separate data segment cannot supply
an executable alias through Linux's page rounding.
ARM user binaries link their read-only data into page-separated non-executable
`PT_LOAD` segments so incidental exclusive-shaped metadata remains usable.
The pinned `vmlinux` has a different mapping: its page tables execute
`_stext.._etext` and `__inittext_begin..__inittext_end`, while its early
identity map also executes the `.rodata.text` page. The explicit
`--kernel-vmlinux <vmlinux> <Image>` AA-4 mode validates those linker boundaries,
requires each range to start in a file-backed executable ELF load, and scans
the exact boot Image bytes across all three runtime-executable ranges. The
Image scan covers zero-filled alignment tails omitted by ELF `p_filesz` and
does not trust section or segment flags to identify executable bytes. The arm64
linker patch places `.altinstructions` and unwind metadata on NX init-data
pages, and the early identity map makes the ordinary read-only interval NX
without blocking its special identity-map code. The vDSO debug ELF uses
ordinary user-page scanning. The exact stripped `vdso.so` is embedded whole
and mapped executable through its final page, so `--embedded-vdso` scans every
aligned word in the complete file, including the section table beyond `PT_LOAD`.
These are static artifact checks; they do not trap later executable mappings,
`mprotect`, generated code, or writes to executable files.

The counter scanner also rejects physical and virtual timer-value reads
(`CNTP_TVAL_EL0` and `CNTV_TVAL_EL0`). These expose a remaining interval derived
from the live counter even when no timer is programmed. The raw executable-word
walk covers both ordinary instructions and words marked as data by mapping
symbols. Timer programming remains prohibited; owned code uses the Harmony
clock and clockevent instead.

The scanner also rejects physical and virtual timer control reads and writes.
The owned arm64 kernel no longer touches those controls: its virtual timer
shutdown disarms the Harmony MMIO clockevent, and its unreachable physical
timer paths stop if selected. A control write could enable a host-time timer;
KVM also omits physical timer control from its saved vCPU state.

`build-platform-runtime.sh` builds the native architecture's kernel, static
supervisor, OCI runtime, and OCI smoke fixture, then seals their manifest. Run
`make -C consonance/harmony-linux fetch` first. Install the pinned
`nightly-2026-06-16` Rust toolchain with `rust-src` and the architecture's Linux
musl target. ARM builds rebuild the Rust standard library against LSE musl and
scan the complete shipped binaries for unsupported instructions.

`runtime-artifacts.py` records and verifies the canonical kernel, OCI runtime,
and platform fixture. `source-key` hashes platform build sources and pinned
contracts; generated build/download directories are excluded. `seal` records
that source digest and every required artifact digest in `runtime-manifest.json`.
Run it only after building the artifacts from those sources.

`verify` reports one of four qualification scopes:

- `exact-input`: all artifact digests match, and their source digest matches the checkout.
- `host-only`: the artifacts match their manifest, but were built from different sources.
- `inconclusive`: the artifact directory has no manifest.
- `unavailable`: required artifacts are missing, corrupt, or incompatible.

Only a successful hardware smoke with `exact-input` artifacts qualifies a guest
platform change. A successful smoke with `host-only` artifacts checks host
compatibility. The other scopes fail prerequisite verification and cannot pass
qualification. CI retains the machine-readable result with the smoke logs.

```sh
python3 consonance/harmony-linux/scripts/runtime-artifacts.py verify \
  --architecture aarch64 --artifacts /path/to/aarch64 --output /path/to/provenance.json
python3 consonance/harmony-linux/scripts/test_runtime_artifacts.py
```

### Packaging the Nix platform

The OCI producer builds its Rust payloads once, builds the Nix kernel/runtime
once, then packages the generic fixture without rebuilding the kernel:

```sh
consonance/harmony-linux/scripts/build-platform-runtime.sh --runtime-payloads "$PWD/runtime-payloads"
export HARMONY_NIX_RUNTIME_INIT="$PWD/runtime-payloads/init.sh"
export HARMONY_NIX_RUNTIME_SUPERVISOR="$PWD/runtime-payloads/harmony-supervisor"
export HARMONY_NIX_RUNTIME_MANIFEST="$PWD/runtime-payloads/runtime-payloads.json"
nix run .#platform-guest-images -- --oci-runtime --output "$PWD/nix-guest-output"
consonance/harmony-linux/scripts/build-platform-runtime.sh --from-nix "$PWD/nix-guest-output"
```

Both output directories must be new. `--runtime-payloads` builds the supervisor
with the pinned Rust/remap settings and copies canonical `runtime/init.sh`.
Its manifest binds source, architecture, toolchain/target and payload hashes.
Nix verifies the external payloads against its actual copied source before and
after assembly, rejects source changes, and records all produced file hashes.

`--from-nix` verifies those source/output bindings, builds only `runtime-fixture`,
and preserves the exact kernel, OCI archive and direct `initramfs.cpio.gz`.
The direct fixture is required; absent bytes are unavailable, not reconstructed.
The canonical `runtime-manifest.json` seals these files and the original OCI
manifest, with the Nix build/payload records under `build-provenance/`.

This bridge is required by the source-correct producer/consumer handoff: the
Nix assembler consumes externally built Rust payloads, and quick consumers
require the canonical artifact layout. Removing the binding could label stale
payloads or kernel bytes with the current source key; using the ordinary builder
instead would rebuild the kernel. Records rely on the trusted builder and do
not provide signatures or instruction admission. The ordinary native and ARM
build paths remain available.

```sh
python3 consonance/harmony-linux/scripts/test_nix_runtime_artifacts.py
python3 consonance/harmony-linux/scripts/test_runtime_artifacts.py
```
