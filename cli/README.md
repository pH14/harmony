<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Harmony CLI

Build from the repository with `cargo build --release -p harmony-cli`.
Run `target/release/harmony preflight` to inspect host support and guest
artifacts. Add `--bundle path/to/bundle` to report what a workload image
declares about itself and what an investigation of it would find missing.
With `--bundle` the exit status answers about the bundle, so a complete one
still succeeds on a machine with no hypervisor or guest artifacts installed.
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

A faults search leaves its `--out` directory as a workspace: the findings it
recorded, the checkpoints and evidence behind them, and a journal of every
change since. `-w W` opens that directory. It does not attach to a running VM.
Each command opens the workspace, does bounded work, commits its result, and
exits; guest time is frozen in between.

```sh
harmony search --package faults pgcic-14.3.oci --out pg --seed 7 --executions 100
harmony -w pg findings
harmony -w pg inspect bug-1
harmony -w pg fork bug-1 --rewind 3s --name trace
harmony -w pg exec trace --within 200ms -- sh -c 'cat /run/amcheck.out'
harmony -w pg run trace --until assertion:2:fail --within 4s --extend
harmony -w pg inspect trace@head console --since m-0007 --matches 'ERROR:'
harmony -w pg export bug-1 --out shared --evidence
```

| Command | What it does |
| --- | --- |
| `findings` | Every recorded failure, the properties it violated, what they mean, and whether a replay reproduced it |
| `branches` | Named continuations, their saved endpoints, and any command still in flight |
| `inspect` | The workspace, a finding, a branch point, or a retained view of one |
| `fork SOURCE --rewind D --name N` | A continuation starting before the source, inheriting its remaining recorded inputs |
| `run BRANCH --for D` | Advance the branch by at most `D` more virtual time and save its endpoint |
| `run BRANCH --until COND --within D` | Advance until a new guest report matches, bounded by `D` more virtual time |
| `exec BRANCH -- ARGV...` | Run a guest command, retain its output and the resulting checkpoint |
| `exec --at SELECTOR -- ARGV...` | The same on an automatically named probe, leaving the source where it is |
| `export FINDING --out DIR` | Write the recorded reproducer; `--evidence` adds investigation evidence with its own provenance |

Selectors name points: `bug-1` a finding, `trace@head` a branch's latest saved
endpoint, `trace@12.3s` an absolute virtual time in its history, and `m-0007`
an immutable moment a previous reply returned. A selector that resolves to
nothing is refused; a nearby point is never substituted for it.

`--for` and `--within` are virtual time, not host waiting; `--wall-seconds`
(default 30) is the separate host watchdog. Reaching a bound without the
watched condition reports `stop: virtual_deadline` and `condition_met: false`,
which is not a passing property. Advancement stops at the source
continuation's recorded end unless `--extend` allows running past it under its
final environment. Watching `assertion:2:fail` waits for a *new* failed
evaluation of property 2 after the run starts; the failure already in the
history does not satisfy it.

`exec` marks its branch modified, because command delivery is not part of the
reproducer record. Its checkpoint is retained, because the recorded inputs
alone cannot reconstruct it. A modified branch cannot mint a reproducer; the
original finding stays recorded and reproducible. Pass `--request-id ID` and a
retry returns the committed result instead of running the command twice. A
command whose bound expires stays in flight: the next `run` on that branch
finishes it, and a second `exec` is refused until it does.

Shell expressions need an explicit `sh -c`; argv is delivered verbatim
otherwise. Every reply carries the same facts in text and under `--json`:
moment, branch, virtual time, history, stop reason, command completion and
exit status, property evaluation, verification scope, evidence references, and
truncation. Each also names commands that are valid next steps.

Inspection views are `console`, `events`, `command`, and `hash`. `--since
MOMENT`, `--limit`, `--offset`, and `--matches TEXT` bound and page them. The
`regs` and `read GPA LEN` views described in
[the investigation plan](../docs/BUG-INVESTIGATION-PLAN.md) are not
implemented; `inspect` names the views this build has when asked for another.

Advancing verbs need a Linux KVM host. `findings`, `branches`, `inspect`, and
`export` read the retained history on any host.

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
