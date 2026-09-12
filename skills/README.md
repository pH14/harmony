<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Harmony developer skills

Four skills carry an application from its documented guarantees to a Harmony
integration, a bounded experiment, and a report backed by evidence:

| Skill | Covers |
| --- | --- |
| [`harmony-properties`](harmony-properties/SKILL.md) | Turning a feature contract into safety, progress, and reachability claims with independent oracles |
| [`harmony-instrument`](harmony-instrument/SKILL.md) | Getting those claims into a workload bundle as checks with stable ids, and confirming the reports arrive |
| [`harmony-build`](harmony-build/SKILL.md) | A pinned, reproducible image, the runtime and symbols it needs, and an honest inventory of what is instrumented |
| [`harmony-run`](harmony-run/SKILL.md) | Designing a bounded campaign, investigating a finding through the CLI, and reporting what the evidence establishes |

Each states what Harmony supports today. Where a capability is absent, the
skill says so rather than describing an intended one; an agent that follows a
skill into an unsupported path produces flags without observations.

They are installed for an evaluated attempt by
[the evaluation runner](../benchmarks/skills/README.md), which copies this
directory into the attempt workspace's `.claude/skills`. A human reads them
here.
