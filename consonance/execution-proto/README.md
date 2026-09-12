# Execution protocol

`execution-proto` defines the canonical JSON document mounted at
`/run/harmony/execution.json`. Version 1 carries one command, its complete
environment, working directory, resolved credentials, supplemental groups, and
an optional absolute bundle path. The same document drives both a plain command
and structured process supervision; a missing bundle means the command runs
once.

Decoding rejects unknown fields, unsupported versions, empty commands, NULs,
malformed or duplicate environment keys, and non-normalized paths. Encoding
validates the same rules and preserves a stable struct field order for execution
identity hashing.

```sh
cargo test -p execution-proto
cargo clippy -p execution-proto --all-targets -- -D warnings
```
