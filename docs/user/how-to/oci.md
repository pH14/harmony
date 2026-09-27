# Run your OCI image

Use this guide when your application already has a Linux container image and you want one recorded execution. Complete [installation](install.md) and check the [image restrictions](../reference/compatibility.md#workload-restrictions) first.

## Choose an input

Harmony accepts a registry or local engine image name, a Docker image archive, or an OCI layout directory:

```sh
harmony oci run alpine:3 --out hello -- /bin/echo hello
harmony oci run ./app.tar --out app-run
harmony oci run ./app-layout --out layout-run
```

For names, Harmony checks Docker before Podman, uses an existing local image when present, and otherwise attempts a pull. If Docker is installed but unusable, fix it or export your image with the working engine and pass the archive directly.

For reproducible experiments, save the image once and retain the archive:

```sh
docker image save -o app.tar my-app:test
```

Build or export a Linux image for your host's architecture. Images remain subject to the [OCI contract](../reference/oci.md); acceptance of the file format does not certify determinism.

## Select the command and limits

Without `--`, Harmony combines the image's `ENTRYPOINT` and `CMD`. Arguments after `--` replace that whole command; they are not appended to the image entrypoint.

```sh
harmony oci run ./app.tar --seed 42 --ram-mib 1024 \
  --timeout 300 --out experiment-42 -- /app/my-service --self-test
```

Use the image's own scripts for pipelines or shell expansion:

```sh
harmony oci run ./app.tar --out scripted -- /bin/sh /app/test.sh
```

Bake environment variables, working directory, files, and the application user into the image. The CLI has no Docker-style `-e`, `-v`, `-p`, or `--user` flags. There is no host port publishing or external-network configuration.

`--timeout` limits host time during guest execution; image acquisition and preparation occur before that budget starts. Add `--console` to stream the full boot log. Use a fresh `--out` directory for every run so stale files cannot be mistaken for current evidence.

## Check the result

A successful CLI exit means it observed application exit code zero. For other results, inspect `run.json` and `serial.log`: a failed runtime startup and an application that exits unsuccessfully are different events.

On a guest timeout, Harmony saves the partial `serial.log` and returns an error; it does not write a successful `run.json`. See [artifact fields](../reference/artifacts.md) for details.

To repeat the experiment, preserve the image, CLI version/commit, guest runtime, command, RAM, and seed. See [replay](replay.md) for a checklist and comparison commands.
