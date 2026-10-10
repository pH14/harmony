# Troubleshooting

Locate the stage where the problem occurs before changing the application or
increasing the search budget. These are useful places to start:

| Problem | What to check |
| --- | --- |
| The image won’t build | Read the container builder’s output and check the build context and language recipe. |
| An executable is rejected during admission | Check the reported file and instruction, its instrumentation attestation, and matching symbols. |
| A runtime or backend is missing | Read the `harmony check` output and check host access, artifact paths, and release availability. |
| The search produces no useful evidence | Look for execution errors and check readiness, traffic, and whether assertions were reached. |
| A guest command isn’t available | Check that the application image includes the required shell or utility. |
| A saved result won’t reconstruct | Check the original executable, artifact hashes, architecture, and, for UML, host identity. |
| The result name is already in use | Choose a new name to keep the earlier evidence. |

## When a search finds nothing

Check that the application ran and reached the assertions you intended to test.
If setup failed or the checks never observed the relevant behavior, more search
time won’t resolve the problem. Once those are working, you can decide whether
a longer search would be useful. Completing a search without a finding does not
prove the application is correct.

## Save what you’ll need to diagnose the problem

Keep the command, recipe, CLI revision, host information, and full saved result.
If the failure happened before a search could be saved, keep the build and
readiness output too. Include whether the run ended because of a host timeout
or a guest assertion failure.

If admission or artifact validation rejects a workload, keep the rejected
artifacts for diagnosis or rebuild a matching prepared workload. Bypassing
admission or editing saved hashes would remove the checks needed to trust the
reconstructed execution.
