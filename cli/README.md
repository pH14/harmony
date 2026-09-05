<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Harmony CLI

Build from the repository with `cargo build --release -p harmony-cli`.
Run `target/release/harmony preflight` to inspect host support and guest artifacts.
See [BUILDING](../docs/BUILDING.md) for guest image builds. Set `HARMONY_GUEST_DIR`
to the artifact directory when using an external build.

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
resource limit, not guest virtual time or replay state.

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
