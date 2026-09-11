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
python3 -B -m unittest benchmarks/skills/test_materials.py benchmarks/skills/test_sandbox.py benchmarks/skills/test_build.py
```

The [qualification workflow](../../.github/workflows/skill-evaluator.yml) records
the exact code and image identities and retains the tool image with its results.

Portable tests exercise material handling, process helpers, and artifact parsing.
Actual Docker canaries separately qualify the Linux execution and compilation
boundaries. Guest boot, meaningful checker controls, general built-source
semantics, and private held-out grading require further trusted services outside
the agent sandbox; passing these infrastructure checks does not qualify them.
