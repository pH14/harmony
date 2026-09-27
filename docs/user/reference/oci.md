# OCI image contract

## Inputs

| Input | Requirement |
| --- | --- |
| Image name, such as `alpine:3` | Accessible through Docker or Podman; a local image is preferred before attempting a pull |
| Docker image archive | Produced by `docker image save` or an equivalent compatible export |
| OCI layout directory | Contains the OCI metadata and image blobs for the intended platform |

An existing filesystem path is treated as an archive or layout. A rootfs tar file from `docker export` is not a Docker image archive: it lacks the image configuration. Host `tar` is used during staging.

## Application launch

| Image property | Behavior |
| --- | --- |
| `ENTRYPOINT` + `CMD` | Combined when no explicit command is provided |
| Explicit arguments after `--` | Replace the full image command |
| `ENV` | Passed to the workload; names must be valid shell-style variable names |
| `WORKDIR` | Used as the application's working directory |
| `USER` | Resolved from the image's account files; missing names are errors |
| `LD_BIND_NOW` | Forced to `1` by Harmony |

Supported user forms are `uid`, `uid:gid`, `user`, `user:group`, `uid:group`, and `user:gid`. An omitted user means root. An unknown numeric UID stays that UID and uses GID 0 when no group is specified; it does not become UID 0. An explicit group suppresses supplementary groups. With no explicit group, known account memberships supply them.

A platform supervisor starts the application and records whether it actually returned. Startup failures are separate from application exits.

## Files, processes, and networking

The image root is writable for the execution. Changes inside the guest are not exported as a new image or a host volume by `oci run`. Retain important diagnostic information in application output.

The runtime supplies process, IPC, mount, UTS, cgroup, and network namespaces, a delegated cgroup v2 subtree, and the `/dev/harmony` SDK device. The CLI exposes no host-volume or external-network configuration. Services in one fault bundle must communicate within that guest; do not expect a database on your host to be reachable.

Image files cannot replace platform-owned execution configuration or mounts. `oci run` launches one command; merely adding `/etc/harmony/bundle` to an image does not select fault supervision. Use `search --package faults` for a [structured bundle](bundles.md).

## Repeatability

Pin and retain the image bytes. A registry tag is a convenient acquisition name, not an immutable replay input. The recorded rootfs and execution hashes let you detect changed preparation inputs. See [artifacts](artifacts.md) and [workload restrictions](compatibility.md#workload-restrictions).
