# Saved evidence

Names identify saved searches and branches beneath `.harmony/runs`. Inspection
commands also accept a saved directory. A new result needs a fresh name or
output directory; existing evidence is not overwritten.

Use `list` to discover saved objects and `show` to examine them. Their JSON
output is available for automation. The command reference is generated from the
current executable and lists the supported selectors.

## What is saved

The manifest records the resolved workload and runner, execution identities,
artifact hashes, and parent relationship. Package-owned records describe the
actual actions and outcomes. A search also retains its exploration evidence
and available whole-search checkpoints.

A branch can retain a saved guest state. Interactive branches additionally
retain terminal evidence. A transcript helps explain an investigation; the
saved state is what allows search to continue after guest changes.

Console evidence is a bounded tail. Adjacent observations may include overlapping
output. Use it alongside assertions and the timeline rather than treating it as
a complete application log archive.

Removing a saved directory removes its evidence. Preserve the full directory
when keeping a result; copying only the report is not sufficient for future
investigation.

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | Operation completed successfully. |
| 1 | Application search found a violation or an unmet reachability condition, or an application command failed. |
| 2 | Invalid input, infrastructure failure, or rejected reproduction. |

A tutorial search that discovers its intended bug exits 1. Automation must
check both the expected status and the finding evidence; accepting any nonzero
status would also accept a broken installation.
