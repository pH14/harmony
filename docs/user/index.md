---
hide:
  - toc
---
<div class="hero" markdown>
<span class="eyebrow">Harmony documentation</span>
# Make the failure repeatable.

Run Linux workloads in a controlled environment. Explore failures caused by process crashes, pauses, and restarts. Keep the inputs that let you reproduce what happened.

[Install Harmony](how-to/install.md){ .md-button .md-button--primary }
[Run your first workload](tutorials/first-run.md){ .md-button }
</div>

Harmony is an experimental testing tool for people building databases, services, and other stateful systems. You bring a Linux container image and a way to check whether your application is behaving correctly.

!!! note "Before you begin"
    These docs describe the current source version. Installation currently requires building the CLI and its Linux guest runtime; there are no published release downloads. Check [system compatibility](reference/compatibility.md) before starting a build.

<div class="doc-cards" markdown>
<div class="doc-card" markdown>
## Learn by doing
[Tutorials](tutorials/index.md) walk you through a reproducible container run and a small fault-search experiment, with expected results at each step.
</div>
<div class="doc-card" markdown>
## Get a task done
[How-to guides](how-to/index.md) cover installation, your own OCI images, workload preparation, SDK integration, and failure replay.
</div>
<div class="doc-card" markdown>
## Look up the details
[Reference](reference/index.md) defines supported systems, command options, image requirements, bundle syntax, SDK calls, and output files.
</div>
<div class="doc-card" markdown>
## Understand the behavior
[Explanation](explanation/index.md) describes what determinism means, how fault search works, and what makes an assertion useful.
</div>
</div>

## Choose your starting point

| Your goal | Start here |
| --- | --- |
| Run a container and repeat its output | [Your first reproducible run](tutorials/first-run.md) |
| Test a service through crashes and restarts | [Prepare a fault workload](how-to/fault-workload.md) |
| Reproduce an already recorded failure | [Replay a failure](how-to/replay.md) |
| Report application correctness to Harmony | [Add SDK assertions](how-to/sdk.md) |
| Explore a supported NES game | [Search an NES workload](how-to/nes.md) |

Harmony is licensed under [AGPL-3.0-or-later](https://github.com/pH14/harmony/blob/main/LICENSE).
