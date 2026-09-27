# Your first reproducible run

Run a small container twice, inspect its recorded result, and compare the complete console output. You will learn the basic workflow without writing an application.

## Before you start

Complete [installation](../how-to/install.md). You need a working guest runtime, a supported hypervisor, Docker or Podman to acquire the image, and Python 3 to inspect the result. The example uses Docker; substitute `podman` for the acquisition commands if that is your working engine.

```sh
harmony preflight
mkdir harmony-first-run
cd harmony-first-run
docker pull busybox:musl
docker image save -o busybox.tar busybox:musl
```

Saving the image once gives both runs the same bytes, even if the registry tag changes later. Use an image for your host's architecture.

!!! note "A host classified as untested"
    If preflight's only remaining blocker is an `expected`/untested support-matrix cell, add `--allow-untested` to **both** `oci run` commands below. This opts into that host; it does not fix missing runtime files or unsupported workloads.

## Run the container

```sh
harmony oci run ./busybox.tar --seed 7 --timeout 60 \
  --out first -- /bin/echo hello
```

Look for `hello`, `container exited rc=0`, and an artifact directory. The command writes `first/serial.log` and `first/run.json`. The serial log includes the guest boot and shutdown, not just `hello`.

Read the application result:

```sh
python3 - <<'PYCODE'
import json
from pathlib import Path
run = json.loads(Path('first/run.json').read_text())
print('application exit:', run['container_rc'])
print('serial digest:', run['serial_sha256'])
assert run['container_rc'] == 0
PYCODE
```

The application exit should be `0`. Your digest is a record of this particular run, not a value to copy from this tutorial.

## Repeat the same input

```sh
harmony oci run ./busybox.tar --seed 7 --timeout 60 \
  --out second -- /bin/echo hello
cmp first/serial.log second/serial.log
```

`cmp` prints nothing and exits successfully when the logs match. If they differ, keep both directories and work through [troubleshooting](../how-to/troubleshooting.md); do not discard the mismatch as harmless.

## What you learned

You supplied an image, a command, and a seed; Harmony recorded the execution inputs and console digest. Holding those inputs fixed lets you check repeatability. Matching output in this experiment is evidence for this run, not a guarantee about every image or platform.

Next, [run your own OCI image](../how-to/oci.md), or learn to [search a small workload for failures](first-search.md).
