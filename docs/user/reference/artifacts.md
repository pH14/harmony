# Run artifacts

Keep the whole output directory together with the inputs. Reports identify many execution inputs by hash, but they do not contain the application image, kernel, or installed CLI.

## OCI execution

| File | Contents |
| --- | --- |
| `serial.log` | Complete guest serial output, including boot and shutdown |
| `run.json` | Completed execution metadata, hashes, terminal reason, and exit statuses |

On a guest timeout, `serial.log` contains partial output and no successful `run.json` is written. Always use a fresh directory so an old report cannot survive beside a new partial log.

Important `run.json` fields:

| Field | Interpretation |
| --- | --- |
| `image`, `seed`, `isa`, `os`, `guest_ram_mib` | Requested image name/path and execution settings |
| `cmdline` | Guest kernel command line, not the application's argv |
| `kernel_sha256`, `base_initramfs_sha256` | Runtime input hashes |
| `rootfs_segment_sha256`, `control_segment_sha256` | Prepared image and control input hashes |
| `execution_identity`, `execution_json_sha256` | Identity of the resolved execution specification |
| `steps`, `terminal` | Execution count and terminal reason recorded by the runner |
| `container_rc` | Application return status, only present after its process actually returns |
| `supervisor_failure_rc` | Supervisor preparation or spawn failure |
| `runtime_rc` | OCI runtime return status, including runtime launch failures |
| `startup_rc` | Earlier guest startup failure |
| `serial_sha256` | Digest of the full serial log |

An absent application status is JSON `null`, not zero. Save the original application command separately: the record has its execution identity but does not reproduce its full argv as a convenient command line.

## Fault campaigns and replay

| File | Use |
| --- | --- |
| `report.json` | Main campaign/replay summary and evidence |
| `stream.jsonl` | Campaign choices and outcomes |
| `campaign-summary.json` | Additional campaign summary |
| `progress.jsonl` | Progress records |
| `bug-N.json` | Recorded input for a discovered bug |
| `first-bug-input.json` | First bug input when one is available |

Search produces campaign files; replay primarily writes its `report.json`. Bug-specific files exist only when the run produced those records.

The main report records `package`, `mode`, `image_sha256`, `kernel_sha256`, `identity`, seed, RAM, workers, execution count, `bug_found`, `first_bug_execution`, `bugs`, and `replays`. Inspect these result distinctions:

- `bug_found` reflects confirmed bug evidence, not merely a suspicious search endpoint.
- A bug's `confirmed` and `replay` fields distinguish the search observation from its fresh confirmation.
- `violations` identifies failed property IDs. `sometimes` contains reached evidence.
- `watchdog_cutoffs` counts guest action runs ended by a host watchdog, including reconstruction attempts.
- `execution_failures` records execution failures; do not silently classify them as passing checks.

Each replay records `actions_applied`, `guest_horizons`, `settle_actions`, `settle_ticks`, `state_hash`, `violations`, `bug`, and optional completed `check` evidence. Settling actions are additional validation work for continuous checks, separate from the input prefix. Current state hashes use `state_hash_encoding: "engine_digest"`; do not compare them as though they were older reports' re-hashed digests.

## NES campaigns

The shared NES command writes `stream.jsonl`, `report.json`, `prepared.json`, and `checkpoint.json`. Preserve the ROM and matching QuickNES core or guest image alongside them. NES records and fault action files are different formats; they are not interchangeable with `search --package faults --replay`.
