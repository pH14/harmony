<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Go language recipe

The recipe builds with Go 1.24 and the Antithesis compiler wrapper and SDK at
v0.8.0, as the [etcd case](../../bugs/historical/etcd-3.5-inconsistency/image/README.md)
does. `CGO_ENABLED=1` is required because the SDK reaches
`/usr/lib/libvoidstar.so` through cgo. The SDK forwarding code is unchanged.

The fixture is
[`cmd/language-fixture`](../../../consonance/harmony-linux/linux/go-runtime-guest/cmd/language-fixture/main.go).

```sh
bash workloads/languages/build-image.sh go
bash workloads/languages/run-check.sh harmony-language-go:local evidence/go
```

## Local patches

- `antithesis-sdk-go-v0.8.0-linux-arm64.patch` (in the etcd case) fixes a build
  tag so the cgo handler builds on arm64. Ask before sending it upstream. Drop
  it once an upstream release includes the fix.
- `antithesis-go-toolexec-v0.8.0-stdlib.patch` adds an
  `ANTITHESIS_STDLIB_PACKAGES` list. The upstream wrapper
  [skips every standard-library package](https://github.com/antithesishq/antithesis-sdk-go/blob/v0.8.0/tools/antithesis-go-toolexec/policy.go).
  The patch adds the list to the compiler action cache key and always excludes
  the SDK and `runtime` packages.

Set `GOFLAGS=-trimpath` before building the patched wrapper as well as the
application. The wrapper's binary hash is part of the action cache key, so
without `-trimpath` two clean builds produce different binaries.

## Standard-library selection

`configure-stdlib.sh SDK_DIR APP_PREFIX OUT_DIR` prints the
`ANTITHESIS_STDLIB_PACKAGES` and `ANTITHESIS_INSTRUMENT` exports. It selects
`go list std` minus every `runtime` package and minus the standard-library
packages that `runtime` and the SDK's instrumentation package import.
Instrumenting those would create import cycles or callbacks into the callback
path. The selected and excluded lists are kept under `/symbols/stdlib`.

## Known limits

- The runtime, the garbage collector, SDK dependencies, assembly, and files the
  upstream rewriter declines (for example files with `//go:linkname`) have no
  callbacks. A loop there does not advance virtual time.
- A garbage collection with 1,048,576 live heap pointers stops the world for
  about 1 ms in the guest. Larger heaps stop it longer. Run
  `harmony debug run harmony-language-go:local --for 120s -- /opt/harmony/fixture gc`
  to measure it.
- Each callback goes through cgo. On the etcd reference the callback path used
  about half of the server's CPU (about 140 ns per callback, natively). The
  runtime still grants one-visit leases, because a larger lease of n hits would
  let a park land only every n visits to an edge.

cgo marks the goroutine as in a system call, so the runtime hands its P to
another M while a callback is held. The park check depends on this.

## Measuring callback cost on etcd

`measure.Dockerfile` and `measure-etcd.py` build an uninstrumented etcd from the
same source and toolchain, run the original three-member topology and writer,
and record callbacks, server CPU, acknowledged puts and CPU profiles. Run it on
an idle host.

```sh
runtime=$(bash workloads/languages/build-runtime.sh)
docker build -f workloads/bugs/historical/etcd-3.5-inconsistency/image/Dockerfile \
  --target instrumented-build -t harmony-go-etcd-build:local .
docker build -f workloads/bugs/historical/etcd-3.5-inconsistency/image/Dockerfile \
  --build-arg "HARMONY_RUNTIME_IMAGE=$runtime" -t harmony-etcd-language:local .
docker build -f workloads/languages/go/measure.Dockerfile -t harmony-go-etcd-measure:local .
mkdir -p evidence/go-cost
docker run --rm --mount "type=bind,source=$PWD/evidence/go-cost,target=/evidence" \
  harmony-go-etcd-measure:local
```
