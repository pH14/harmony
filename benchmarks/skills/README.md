<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Skill evaluation infrastructure

These modules prepare paired evaluation materials and bound tool execution.
They do not select a model, invoke a provider, or infer skill effectiveness.

`materials.freeze_pair` takes explicit common and treatment input mappings,
a shared task, and shared settings. It writes independent `docs` and `skills`
arms; only the latter receives `.agent/skills`. The manifest binds file bytes,
paths, executable modes, directories, prompt, and settings. Keep its SHA-256
outside the agent environment and check it with `materials.verify_pair` before
use. Read-only file modes protect artifacts from accidental edits; execution
isolation belongs to the sandbox.

`Sandbox` uses a controller-selected immutable local Linux Docker image. The
image must contain `/usr/bin/python3`; the host must expose the local Docker
endpoint at `/var/run/docker.sock`. The controller must audit the image's
contents and freeze its identity with the
cohort. Agent files, inherited configuration, provider credentials, private
case data, and grader code must not be baked into it. The sandbox stages only
the selected arm and dispatches explicit command argument arrays with resource
limits and a fresh environment. Docker remains a controller capability.

`build.build_frozen` compiles a controller-selected frozen submission in a
fresh sandbox, then collects an explicit artifact allowlist through a bounded
reader. Compiler commands and collected files never execute on the host. Its
receipt records supplied source identity, tool image, command, copied artifact
bytes and hashes, and bounded compiler output. These identities do not prove
that arbitrary build scripts honestly compiled the submitted source; independent
semantic checks must establish that relationship.

`python3 -B -m benchmarks.skills.qualify_build --image sha256:...` qualifies this
path using an actual compiler in an immutable Linux image. The CI build job
retains that image and exercises benign source changes, compiler failures, and
rejected artifact types and sizes. This is compilation and collection evidence;
it does not qualify guest boot or a semantic grader.

Portable checks run from the repository root:

```sh
python3 -B -m unittest benchmarks/skills/test_materials.py benchmarks/skills/test_sandbox.py benchmarks/skills/test_build.py benchmarks/skills/test_guest_image.py benchmarks/skills/test_guest_evidence.py benchmarks/skills/test_guest_files.py benchmarks/skills/test_guest_limits.py benchmarks/skills/test_qualify_guest.py
```

The [qualification workflow](../../.github/workflows/skill-evaluator.yml) records
the exact code and image identities and retains the tool image with its results.

Portable tests exercise material handling, process helpers, and artifact parsing.
Actual Docker canaries separately qualify the Linux execution and compilation
boundaries. Guest boot, meaningful checker controls, general built-source
semantics, and private held-out grading require further trusted services outside
the agent sandbox; passing these infrastructure checks does not qualify them.

`guest_image.package_artifacts` writes a deterministic Docker-save archive from
copied build artifacts and a controller-owned bundle. `qualify_guest` uses fixed
benign compiled fixtures to test real KVM boot and SDK delivery through the
shipping CLI. It retains inputs, binaries, invocation, report, and complete event
sidecar. Replay summaries and sidecars must agree on the endpoint timestamp;
legacy summaries without a timestamp cannot qualify this gate. The controller trusts and pins the shipping CLI and prepare-only helper;
compiled artifacts execute only inside the guest. The helper prepares the supplied
archive through the production image assembler, and its digest must equal the
report's prepared-image digest. Archive, actions, tool inputs, and frozen sources
are rechecked after execution. Read-only modes prevent accidental edits; these
checks do not make a malicious host CLI trustworthy.

The guest qualification output parent must be a dedicated Linux `tmpfs` mount,
with `nodev,nosuid,noexec`, at most 512 MiB and 16,384 inodes. CI mounts it before
execution and unmounts it after retaining artifacts. Set `TMPDIR` to an existing
directory in that mount before starting Python; the qualifier verifies this
before building. Host build staging and private Docker configuration, as well as
CLI output, home, caches and temporary files, stay within this bound. Evidence JSON reads use an anchored
directory descriptor, reject symlinks and nonregular files, and cap actual bytes.
The silent fixture must be rejected as missing telemetry; a delivered
assertion is not evidence of a meaningful application checker.

Run the qualification workflow manually with `guest_artifact_run_id` naming a
trusted run containing `guest-images-<run-id>`. The selected kernel/base hashes
are recorded; an empty input skips KVM execution. Portable packaging and evidence
checks still run on pull requests. Guest images must match the fault-agent kernel
interface described in the guest build documentation.
