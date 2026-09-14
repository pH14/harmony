# Process protocol

`process-proto` owns the generic process fault target and standing-window wire
contract. It has no workload or policy dependencies. The target starts with a
little-endian node id and uses the established action tags for pause, kill,
restart, hooks, and instruction parking. Standing responses use a little-endian
moment, count, and `(class, target, start, end)` windows.

Workload policy crates translate their semantic fault enums to
`ProcessAction`; the platform supervisor consumes the generic action and window
types. Invalid targets are ignored when filtering a standing response so an
unrelated or future decision class cannot stop process supervision.

```sh
cargo test -p process-proto
cargo clippy -p process-proto --all-targets -- -D warnings
```
