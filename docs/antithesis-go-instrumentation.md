# Antithesis Go instrumentation integration

Harmony uses the public [Antithesis Go SDK](https://github.com/antithesishq/antithesis-sdk-go) and
its documented [Go instrumentor](https://antithesis.com/docs/reference/sdk/go/instrumentor/) for
historical Go workload images. The Docker build installs the pinned
`github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-instrumentor` release, transforms
the pinned upstream source before compilation, and retains the generated `.sym.tsv` table beside
the resulting executable. A release archive or stock binary is not an equivalent artifact.

The SDK has two relevant pieces:

- `antithesis-go-instrumentor` rewrites Go source or ASTs and inserts a callback at every basic
  block, while emitting the symbol metadata used by the runtime.
- The generated notifier loads `/usr/lib/libvoidstar.so`, Harmony's clean-room implementation of
  the public runtime ABI. `notify_coverage` is an instrumented application event, not a host
  instruction counter.

The generic process fault `ProcEventKill { ordinal }` sends a positive ordinal to an inherited Unix
socket. `libvoidstar.so` consumes the arm in a blocking runtime thread and kills its own process
group synchronously after that many future callbacks. Zero disarms it. This path has no ELF
scanning, address discovery, debug registers, timers, or supervisor polling. The ordinal is
serialized in the fault reproducer, so a retained action prefix and replay use the same event
coordinate. Dissonance treats that field as part of the ordinary action alphabet: suffix mutation
chooses new ordinals, archive retention keeps useful prefixes, and replay reuses the exact ordinal.

The [upstream SDK license](https://github.com/antithesishq/antithesis-sdk-go/blob/main/LICENSE)
is MIT, including the instrumentor. Harmony downloads the tool during the image build rather than
redistributing its source; the exact version and source hashes are part of the historical image
contract.
