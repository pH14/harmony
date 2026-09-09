<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Go runtime proof guest

This directory contains a deliberately small static `linux/amd64` Go workload
for the Harmony virtual-clock acceptance lane. The binary is suitable for
direct execution as `/init` (after the image builder has mounted any desired
kernel filesystems) or for `exec` from a tiny init wrapper. It has no libc, cgo,
Harmony SDK, or Antithesis dependency; the image builder only needs to install
the resulting executable and make it the PID 1 entrypoint.

The probe emits deterministic markers in this order:

```text
GO_RUNTIME_PROOF_BOOT
GO_RUNTIME_PROOF_GOROUTINES workers=3
GO_RUNTIME_PROOF_CHANNELS count=12 checksum=<fixed-hex>
GO_RUNTIME_PROOF_SLEEP done=1
GO_RUNTIME_PROOF_TIMER fired=1
GO_RUNTIME_OK count=12 checksum=<same-fixed-hex>
```

`BOOT` is printed after ordinary Go runtime startup, whose `schedinit` path
initializes the runtime tick clock with `cputicks` on amd64. The workload then
starts three goroutines, synchronizes them through an unbuffered jobs channel,
collects each fixed-content task exactly once, and checks a hard-coded FNV-style
checksum. Workers call `time.Sleep(1ms)` and the main goroutine waits for a
`time.NewTimer(2ms)`, without printing elapsed values. Each worker checks
`time.Since` after sleeping, and the timer is checked immediately after its
channel receive, so an early return emits `GO_RUNTIME_FAIL` instead of passing.
After `GO_RUNTIME_OK`, the process waits in a long `time.Sleep` loop so a
harness can stop or checkpoint a live PID 1 without triggering Go's
all-goroutines-asleep panic.

Any invalid task, duplicate/missing task, early sleep/timer, or checksum
mismatch emits a stable `GO_RUNTIME_FAIL reason=...` line and exits nonzero. A
timer that never wakes therefore fails the harness by timeout rather than being
mistaken for a successful marker.

## Build and local checks

The repository's pinned Linux image toolchain supplies the Go version used for
the acceptance artifact. The module's `go 1.22` line keeps this dependency-free
probe buildable with that toolchain; use the same toolchain for reproducible
artifacts.

Native compile/test on a developer host:

```sh
go version
go test ./...
go build -trimpath -buildvcs=false -o go-runtime-proof .
```

Cross-build the static guest executable from any host with Go installed:

```sh
CGO_ENABLED=0 GOOS=linux GOARCH=amd64 GOAMD64=v1 \
  go build -trimpath -buildvcs=false -ldflags='-buildid=' \
  -o go-runtime-proof-linux-amd64 .
file go-runtime-proof-linux-amd64
go tool objdump -s 'runtime\.cputicks' go-runtime-proof-linux-amd64 | \
  grep -E 'RDTSC|RDTSCP'
```

The final command is a static evidence check that the amd64 Go runtime contains
its counter-read implementation. Run the native binary under a short external
timeout when desired; it intentionally remains alive after `GO_RUNTIME_OK`.

This is a focused runtime and virtual-clock probe. It demonstrates ordinary Go
startup, scheduler/goroutine/channel activity, fixed-content computation,
`time.Sleep`, timers, and the amd64 `cputicks` path. It does not establish
correctness for arbitrary Go programs, the Go standard library as a whole, or
etcd.
