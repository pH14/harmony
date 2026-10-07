# Troubleshooting

Start with the stage that failed. Preparation, execution readiness, application
findings, and investigation failures require different fixes.

| Symptom | Check next |
| --- | --- |
| Image build fails | Container builder output, build context, and language recipe. |
| Admission rejects an executable | The reported file and instruction, instrumentation attestation, and matching symbols. |
| Missing runtime or backend | `check` output, host access, artifact paths, and release availability. |
| Search produces no useful evidence | Execution failures, readiness, traffic, and assertion reachability. |
| Guest command is unavailable | Whether the image includes that shell or utility. |
| A saved result cannot be reconstructed | Original executable identity, artifact hashes, architecture, and UML host identity. |
| Saving fails because a name exists | Choose a new result name; preserve the earlier evidence. |

## No finding is not a correctness proof

First establish that the application ran and the intended checks were reached.
Then decide whether more search time is useful. Increasing the budget cannot
repair failed setup or an assertion that never observes the interesting state.

## Keep enough evidence to diagnose the failure

Keep the command, recipe, CLI revision, host information, and full saved result.
Preserve build and readiness output when failure happens before a saved search
exists. Distinguish a host timeout from a guest property violation.

Do not bypass instruction admission or edit saved artifact hashes to force an
experiment to load. Rebuild a matching prepared workload or keep the rejected
artifacts for diagnosis.
