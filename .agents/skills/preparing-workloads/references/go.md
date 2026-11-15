# Go

The recipe is `workloads/languages/go/Dockerfile`. The fixture is `consonance/harmony-linux/linux/go-runtime-guest/cmd/language-fixture`. The etcd case at `workloads/bugs/historical/etcd-3.5-inconsistency/image/Dockerfile` is the reference service. `workloads/languages/go/README.md` has the measurement commands.

Go produces native code at build time. Keep the unstripped executable, its `*.sym.tsv`, and the SHA-256 and path attestation under `/symbols`.

## Build

Pin `antithesis-go-toolexec` and the SDK to the same release. Build with `CGO_ENABLED=1` and `go build -toolexec=antithesis-go-toolexec`. The SDK calls `/usr/lib/libvoidstar.so` through cgo.

The etcd case carries an arm64 build-tag patch for the SDK. Ask before sending it upstream. Drop it once an upstream release includes the fix.

Run `configure-instrumentation.sh SDK_DIR OUT_DIR [PACKAGE...]` in the build directory with the packages `go build` builds, save its exports to a file, and source that file before building. Do not pass a package list of your own. The script instruments every module in the build graph outside the standard library, because one uninstrumented loop that waits for another goroutine stops the single-vCPU guest. It excludes the SDK and the modules its instrumentation package imports. The upstream wrapper skips all standard-library packages; the local wrapper patch accepts an explicit list, and the script selects every standard-library package except `runtime` and the packages that `runtime` and the SDK import. Instrumenting those creates import cycles or callbacks inside the callback path. Keep the selected and excluded lists in the image.

## Known limits

- The runtime, the garbage collector, SDK dependencies, assembly, and files the wrapper declines have no callbacks.
- A garbage collection with about one million live pointers stops the world for about 1 ms. Larger heaps stop it longer.
- Each callback crosses cgo. On etcd the callback path used about half of the server's CPU. Measure it again on any service where it dominates.

cgo marks the goroutine as in a system call, so the scheduler runs other goroutines while one callback is held. The park check depends on this.

## Acceptance

```sh
bash workloads/languages/build-image.sh go
bash workloads/languages/run-check.sh harmony-language-go:local evidence/go
```

The etcd search must fire park and kill events and reach its comparison oracle in replays. The etcd runtime image copies only PEM certificate files from the build stage. This keeps the OpenSSL libraries and their RDRAND sites out of the image.
