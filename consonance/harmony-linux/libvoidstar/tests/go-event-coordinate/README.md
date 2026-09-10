<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Instrumented Go event-coordinate fixture

This fixture is gate 2 for Harmony's Antithesis event coordinate. Build it
through the pinned `antithesis-go-toolexec`; a normal `go build` is an explicit
negative control and must not satisfy the gate.

The harness passes `HARMONY_FIXTURE_START_FD` in addition to libvoidstar's
control and diagnostic report descriptors. The fixture starts two goroutines,
waits until both are blocked on their input channels, prints `READY`, and waits
for one release byte. The harness sends that byte only after the bridge has
acknowledged the selected event ordinal. All later application work alternates
between the workers through channel rendezvous and prints the state before each
round.

For several ordinals, the gate starts multiple fresh guests and requires all
runs to produce the same generated edge id and the same sequence of
`GO_EVENT_COORDINATE_*` state markers before synchronous `SIGKILL`. It resolves
the edge id against the `.sym.tsv` metadata produced by that exact build. A
missing control acknowledgement, missing kill report, unmapped edge, completed
fixture, or differing replay state fails the gate.
An unarmed instrumented run must first complete all rounds with the expected
checksum, so the accepted crash prefixes belong to a valid concurrent program.

On a Linux/amd64 host with Go 1.24, QEMU, `cpio`, and the locked faultlab base
initramfs, run the complete gate with:

```sh
bash ./build-gate-images.sh /path/to/initramfs.cpio.gz /tmp/go-event-gate
python3 ./gate.py \
  --qemu /path/to/qemu-system-x86_64 \
  --kernel /path/to/bzImage-faultlab \
  --instrumented-initramfs /tmp/go-event-gate/go-event-coordinate.instrumented.cpio.gz \
  --stock-initramfs /tmp/go-event-gate/go-event-coordinate.stock.cpio.gz \
  --symbols /tmp/go-event-gate/symbols
```

The fixed gate matrix is ordinals 1, 8, and 32, each replayed in three fresh
guests. These are fixture coverage points, not workload configuration.
