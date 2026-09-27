# Command line

Use `harmony --help`, `harmony oci run --help`, and `harmony search --help` for the installed binary's option list. This reference describes the current source version.

## `harmony preflight [--json]`

Reports OS, architecture, nesting/container detection, hypervisor availability, support-matrix classification, run-loop support, and discovered guest artifacts. `--json` makes that report machine-readable.

Exit status is zero only when `ready` is true. Preflight checks filenames and host availability; it does not boot the runtime or validate application compatibility.

## `harmony oci run IMAGE [OPTIONS] [-- COMMAND...]`

| Option | Default | Meaning |
| --- | --- | --- |
| `--seed N` | `0` | Guest execution seed |
| `--out DIR` | Temporary `harmony-run-<pid>` directory | Location of `serial.log` and completed `run.json` |
| `--ram-mib N` | `512` | Guest memory in MiB |
| `--timeout SECONDS` | `900` | Host wall-time budget for guest execution, excluding image preparation |
| `--console` | Off | Stream the full boot log |
| `--allow-untested` | Off | Permit an `expected` support-matrix host |
| `-- COMMAND...` | Image entrypoint and command | Replace the entire application command |

The CLI returns zero only after observing application exit status zero. An application failure returns CLI status 1, not necessarily the application's numeric exit code. Read `container_rc` for that value. Argument parsing errors are also nonzero.

Use a fresh output directory: OCI runs may overwrite files in an existing directory.

## `harmony search --package PACKAGE INPUT [OPTIONS]`

`PACKAGE` is required: `faults` or `nes`. Faults takes an OCI image containing a bundle. NES takes a supported ROM.

| Option | Default | Applies to / meaning |
| --- | --- | --- |
| `--backend native\|consonance` | `native` for NES; `consonance` for faults | Execution backend; faults does not support native |
| `--seed N` | `0` | Campaign seed; retain it for replay |
| `--workers N` | `1` | Campaign workers; use one on macOS fault campaigns |
| `--executions N` | `1000` | Logical campaign execution budget |
| `--actions N` | `128` | Action budget per generated execution |
| `--out DIR` | `harmony-search` | Campaign directory; choose a fresh path |
| `--core FILE` | `HARMONY_QUICKNES_CORE` | Native NES host library |
| `--kernel FILE` | Discovered guest kernel | VM execution |
| `--base-initramfs FILE` | Discovered OCI initramfs | VM execution |
| `--image PATH` | `HARMONY_NES_IMAGE` | NES OCI image for VM execution |
| `--ram-mib N` | `1024` | Faults guest RAM |
| `--knobs "k=v k=v"` | None | Faults: additional guest kernel command-line words |
| `--wall-minutes N` | None | Faults: host-time campaign bound |
| `--replay INPUT.json` | None | Faults only: execute a recorded action list instead of searching |
| `--repeat N` | `1` | Faults replay repetition count |

Some package-specific flags are accepted by the common parser but unused by another package. In particular, `--replay` is not a native NES replay interface, and `--wall-minutes` does not bound NES search. Faults requires positive workers, executions, and actions.

A completed search or replay returns zero even if it found a bug. In automation, inspect `report.json`'s `bug_found`, replay outcomes, and failure counters rather than treating CLI success as a passing application test.

## Environment variables and artifact discovery

| Variable | Purpose |
| --- | --- |
| `HARMONY_GUEST_DIR` | Directory directly containing the host architecture's kernel and initramfs |
| `HARMONY_QUICKNES_CORE` | Default host QuickNES library for native NES |
| `HARMONY_NES_IMAGE` | Default OCI image path for NES VM execution |

Guest discovery checks, in order:

1. `HARMONY_GUEST_DIR`.
2. `share/harmony/guest/<isa>/` below the installed executable's prefix.
3. `consonance/harmony-linux/build/<isa>/` relative to the current working directory.

`<isa>` is `x86_64` or `aarch64`. Discovery selects the first directory containing a recognized kernel; it does not merge a kernel from one directory with an initramfs from another. Kernel discovery checks `Image` before `bzImage`. The OCI initramfs filename is exactly `initramfs-oci.cpio.gz`.

Explicit `--kernel` and `--base-initramfs` overrides are available for search, not `oci run`.
