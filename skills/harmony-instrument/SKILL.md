---
name: harmony-instrument
description: Add Harmony observability to a workload through its supported package path while preserving behavior.
---

# Harmony instrumentation

Use this skill when wiring a workload’s source behavior into Harmony. Start by
identifying the target language, package boundary, guest entrypoint, and
existing host consumer. Pick the supported path that fits that boundary:

- A guest that can use the Rust `harmony-sdk` uses its `no_std`,
  allocation-free API over `hypercall_proto::Transport` and initializes one
  catalog with `Sdk::init(transport, catalog)`.
- A separate executable or language uses the fault-agent bundle’s `setup`,
  `node`, `hook`, and `ready` lines. A hook emits the current directives
  `@sometimes <id>`, `@reachable <id>`, or `@always <id> <0|1>` on stdout;
  ordinary lines remain console output. Validate the finished bundle with the
  fault-agent’s documented `--check-bundle` command.

Trace a signal all the way to the selected host consumer. Run a small valid
control and a well-formed failing control when available, and confirm the
expected event, state, or report arrives. Confirm that a checker which never
runs or emits no directive remains unevaluated rather than being treated as a
pass. Do not add or change a host decoder for ordinary integration when an
existing package consumer already defines the path; extend that boundary only
when the source contract and current interfaces require it.

For the SDK path, give every `Point` a stable name and a non-colliding
namespace/ID coordinate, keep local IDs within the 24-bit wire field, and keep
the one-frame catalog within the transport limit. Use assertion calls for
assertion events, state calls for raw register values, lifecycle calls for
setup or frame boundaries, and `coverage_yield` only with the cooperating
runtime ordering it requires. `entropy_fill` uses Harmony’s seeded entropy
service; do not introduce host time or a second random source.

Preserve application control flow, return values, and error behavior. Keep
instrumentation independent of host wall time, propagate SDK or agent errors,
and document what each signal proves and what it leaves unobserved. A symbol,
callback, compiler flag, or presence bit is not search feedback until a
packaged runtime emits data that the selected search actually consumes.
