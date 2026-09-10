# Antithesis Go instrumentation integration

Harmony uses the public [Antithesis Go SDK](https://github.com/antithesishq/antithesis-sdk-go) and
its documented [Go instrumentor](https://antithesis.com/docs/reference/sdk/go/instrumentation/) for
historical Go workload images. The Docker build installs the pinned
`github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec` release, passes it to
`go build -toolexec`, and retains the generated `.sym.tsv` tables beside the executable. A release
archive or stock binary is not an equivalent artifact.

The SDK has two relevant pieces:

- `antithesis-go-toolexec` wraps the compiler, transforms its Go inputs, inserts a callback at every
  basic block, and emits symbol metadata automatically from each pinned build.
- The injected SDK instrumentation module loads `/usr/lib/libvoidstar.so`, Harmony's clean-room
  implementation of the public runtime ABI. Each `notify_coverage` call is an instrumented
  application event.

The generic process fault `ProcEventKill { ordinal }` sends a positive ordinal to an inherited Unix
socket. `libvoidstar.so` consumes the arm in a runtime thread and kills its own process group
synchronously after that many future callbacks. Zero disarms it. The ordinal is serialized in the
fault reproducer, so a retained action prefix and replay use the same event
coordinate. The image build verifies the instrumented node marker in the executable and writes a
hash attestation beside the generated symbol tables. Dissonance admits the action automatically
only when staging finds that attestation, the runtime bridge, and nonempty `.sym.tsv` metadata, and
records the capability in the replay vocabulary; an arm that cannot reach the instrumented runtime
fails the execution loudly. Initial draws
choose uniformly among binary scales, which covers finite callback prefixes without importing a
workload bound. A fired coordinate changes the unexpected-death archive state, retained prefixes
become anchors for dyadic refinement, and replay reuses the exact serialized ordinal.

The [upstream SDK license](https://github.com/antithesishq/antithesis-sdk-go/blob/main/LICENSE) is
MIT and covers the instrumentor source. The public documentation describes the same toolexec build
integration used here. Harmony downloads the pinned release during the image build rather than
redistributing its source; the exact version and source hashes are part of the historical image
contract.
