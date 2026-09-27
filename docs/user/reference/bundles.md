# Workload bundles

`harmony search --package faults` reads `/etc/harmony/bundle` from the OCI image. A bundle is a UTF-8 text file with one declaration per line.

## Declarations

| Syntax | Cardinality | Behavior |
| --- | --- | --- |
| `node NAME ARGV...` | 1–64 | A supervised workload process; names must be unique |
| `hook ID ARGV...` | Zero or more | An operation the campaign may launch; unique unsigned 32-bit ID |
| `setup ARGV...` | Zero or one | Runs before nodes start |
| `ready ARGV...` | Zero or one | Initial readiness and recovery probe |
| `workload ARGV...` | Zero or one | Long-lived traffic driver, started after initial readiness |
| `check ARGV...` | Zero or one | Repeated short-lived oracle, started after initial readiness |

Blank lines and lines beginning with `#` are ignored. Commands are argument vectors, not implicit shell scripts. The tokenizer groups words with double quotes and recognizes escaped double quotes and backslashes inside them. Single quotes are literal characters, not quoting syntax. It does not perform environment-variable expansion, pipelines, globbing, or redirection. Put complex logic in an image-owned script and invoke `/bin/sh /app/script.sh`.

Node order determines the node indexes used by recorded process actions. Keep the bundle unchanged for replay. A bundle with no nodes, duplicate names or hook IDs, an empty command, unknown declaration, or unterminated quote is rejected.

All children run with the resolved OCI image user and groups. A bundle does not assign a different user to each node.

## Checker directives

The supervisor interprets these lines from hook and continuous-check output:

| Line | Meaning |
| --- | --- |
| `@always ID 1` | The observed condition is true; no violation is emitted |
| `@always ID 0` | Report a violation of property `ID` |
| `@sometimes ID` | Report a hit for property `ID` |
| `@reachable ID` | Report reaching point `ID` |

Write each directive as a complete line to standard output. Ordinary output is ignored by the directive parser. Malformed `@` lines are logged and ignored; they must not be counted as evidence that an assertion ran. Keep lines below 64 KiB.

Use IDs `0`–`47` for reached points that must contribute to completed-check and sometimes bitmaps. The SDK's general local-ID space is larger, but these supervisor summaries are deliberately narrower. Keep IDs stable and avoid collisions with other assertions in your workload.

A check needs a successful exit **and** at least one supported `@reachable` or `@sometimes` point to replace the completed-check evidence. A true `@always` by itself does not establish that evidence. Exit code 42 from a hook/check is treated as the supervisor's hook assertion failure; prefer explicit directives to identify your own property. Other nonzero statuses are not a substitute for a property-specific assertion.

## Recovery evidence

A node restart begins a new disturbance generation. Checks receive their starting generation in `HARMONY_DISTURBANCE_GENERATION`. Replay records which generation a completed check started and ended in, its reached points, and outstanding process faults.

For a property about recovery, require fresh completed-check evidence in the final generation with no outstanding fault. A hit from before a crash cannot establish recovery after it. See [assertions](../explanation/assertions.md).
