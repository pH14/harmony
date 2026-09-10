<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Harmony CLI

Build from the repository with `cargo build --release -p harmony-cli`.
Run `target/release/harmony preflight` to inspect host support and guest artifacts.
See the [harmony-linux README](../consonance/harmony-linux/README.md) for guest image builds. Set `HARMONY_GUEST_DIR`
to the artifact directory when using an external build.

## Search packages

```sh
harmony search --package nes smb.nes --core quicknes_libretro.so
harmony search --package nes --backend native smb.nes --core quicknes_libretro.so
harmony search --package nes --backend consonance smb.nes \
  --kernel bzImage --base-initramfs initramfs-nes.cpio.gz
harmony search --package faults foo.oci --kernel bzImage \
  --base-initramfs initramfs.cpio.gz --fault-agent fault-agent --out run
```

NES identifies SMB or Nova by ROM hash and defaults to `native`. Supply the
pinned host QuickNES library with `--core` or `HARMONY_QUICKNES_CORE`. Consonance
execution uses a controlled kernel and the ROM-free image produced by
[`build-base-image.sh`](../workloads/nes-guest/build-base-image.sh); preparation
adds the ROM and launch command. It requires a supported Linux KVM host.

The faults package defaults to `consonance`. Its OCI image supplies
`/etc/harmony/bundle`, which names the workload's nodes, hooks, setup and
readiness commands in the [bundle format](../workloads/fault-agent/README.md),
plus the executables those lines run. Every node and the fault agent execute
inside one VM on one virtual CPU. Supply the controlled kernel with `--kernel`,
the Linux base image with `--base-initramfs`, and the static musl fault agent
with `--fault-agent` or `HARMONY_FAULT_AGENT`. Installed artifacts are
discovered through `HARMONY_GUEST_DIR`. Preparation injects the agent into the
staged image; its commands execute inside the guest.

`--horizon-ms` sets the guest time one fault action runs for and `--ram-mib` the
guest RAM. `--knobs "k=v k=v"` adds guest command-line words, `--places FILE`
lists the execution places the park action may hold a node at, and
`--wall-minutes` bounds a search in host time. `--replay INPUT.json --repeat N`
runs a recorded action list, such as a search's own `bug-1.json`, instead of
searching. Both modes write `report.json`.

`--seed`, `--workers`, `--executions`, and `--actions` bound the campaign's logical
work. `--out` selects a fresh output directory. Every package writes
`stream.jsonl` and `report.json`, retaining campaign choices and results; NES
adds `prepared.json` and `checkpoint.json`, and faults adds
`campaign-summary.json`, `progress.jsonl`, `first-bug-input.json`, and one
`bug-N.json` per bug. An explicit backend selection is checked before
execution.

## Investigating a finding

A faults run writes its findings, pinned artifact identities, and retained
evidence as a workspace. Open it with `-w` (or `--workspace`) and use the
read-only commands from any host:

```sh
harmony -w W findings
harmony -w W branches
harmony -w W inspect bug-1
harmony -w W fork bug-1 --rewind 3s --name trace
harmony -w W run trace --for 4s --request-id trace-1
harmony -w W exec trace --within 1s -- sh -c 'cat /run/service.out'
harmony -w W exec --at bug-1 --within 1s -- sh -c 'cat /run/service.out'
harmony -w W inspect trace@head console --matches 'ERROR:'
harmony -w W export bug-1 --out shared --evidence
```

`findings` lists recorded properties and verification scope. `branches` lists
saved continuation endpoints. `inspect` reads a workspace, finding, branch
point, or retained `console`, `events`, `command`, or `hash` view. `fork` starts a named
continuation, and `run` advances it by bounded guest virtual time; advancing
commands require Linux KVM. `exec BRANCH -- ARGV...` runs a guest command,
captures its output, and saves the resulting modified checkpoint. Use
`exec --at SELECTOR -- ARGV...` to run on an automatically named probe while
leaving the source point unchanged. Shell expressions need an explicit `sh -c`;
argv is preserved as individual words. `inspect` reports the retained command
status and evidence byte digests. A pending command resumes with `run`; a new
command is accepted after it finishes. `export` writes the recorded reproducer
and, with `--evidence`, investigation evidence separately.

`--json` emits one machine-readable document. Its `virtual_time` fields remain
numeric nanoseconds; text and JSON also include a human-readable duration.
Pass `--request-id ID` to `fork`, `run`, or `exec` so a retry returns the
committed result without constructing or running another guest. Global `--kernel`,
`--base-initramfs`, and `--fault-agent` select pinned artifacts when the
workspace's recorded artifacts are not installed.

`--for` and `--within` are virtual time, not host waiting. `--wall-seconds`
sets the total host seconds allowed for advancing the guest's action segments;
artifact preparation and endpoint capture are outside that advance budget.

## OCI execution

```
harmony oci run alpine:3 --seed 7 --timeout 60 --out run-7 -- /bin/echo hello
```

`oci run` accepts a registry image, OCI layout, or Docker image archive. It writes
`serial.log` and `run.json` on completion. `--console` streams the full boot log.
On timeout it preserves the partial serial log and returns an error without a
successful run digest.

On Linux x86, the timeout watchdog sets a host cancellation latch and interrupts
the owning KVM thread with reserved SIGUSR1. It repeats the interrupt after expiry
until the driver returns, covering a signal arriving just before KVM_RUN. It sends
no signals before expiry; canceled executions are abandoned. The timeout is a host
resource limit, not guest virtual time or replay state. The mechanism itself lives
in [`consonance-client`](../consonance/client/README.md), which the neutral session
also uses for its own host bound.

The CLI enables `harmony_pvclock` so the kernel uses virtual timing for entropy
mixing as well as timekeeping. The stock x86 virtual-time boot supplies Linux's `SETUP_RNG_SEED` record from the
VM's seeded entropy stream. This makes the CRNG ready without waiting for timing
jitter that cannot advance inside a non-exiting guest loop. The boot consumes 64
bytes from that same stream before guest execution. Hardware RNG instructions stay
hidden. The pinned Linux kernel must trust bootloader randomness (its default);
`random.trust_bootloader=off` disables this readiness mechanism.

The OCI CI gate reads `/dev/urandom` and checks byte-identical serial logs and
digests for repeated seeds, distinct output for different seeds, and cancellation
of a guest loop that performs no I/O.
