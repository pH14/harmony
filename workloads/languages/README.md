<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Instrumented language images

Each language has one image recipe and one fixture with the same behaviour. One
thread spins with no system calls. Another thread sleeps 10 ms, prints a marker,
and repeats until it has printed twenty. The sleeping thread wakes only because
coverage callbacks in the spinning thread exit to the VM and advance virtual
time.

Every target image meets three conditions:

| Condition | How it is checked |
| --- | --- |
| Hidden instructions appear only at reviewed sites | Admission scans every ELF file in the image ([`reviewed/`](reviewed/README.md)) |
| No runtime code generation | Recipes turn off code generators; fixtures check `/proc/self/maps` |
| Every loop reaches a libvoidstar callback | Clang, GCC, Rust or Go coverage instrumentation |

## Layout

| Path | Contents |
| --- | --- |
| `c/`, `rust/`, `go/`, `python/`, `java/` | One `Dockerfile` per language, with a `language-base` target |
| `runtime/` | Shared build of libvoidstar, the fault runtime, the park launcher and the GCC fixture |
| `compose.Dockerfile` | Copies the runtime into a language base image |
| `vendor/` | The unmodified Antithesis C forwarding header |
| `reviewed/` | Reviewed instruction sites per executable digest |

Each `language-base` image holds the language toolchain output, the fixture,
unstripped binaries under `/symbols`, and `/symbols/harmony-instrumented-events`
with the SHA-256 and installed path of every instrumented file. The runtime is
copied in last, so a change to libvoidstar never rebuilds a language layer.
`image-key.py` hashes only a layer's own recipe inputs, which makes it the cache
key for that layer.

Precompiled libc, libffi and the Rust standard library stay uninstrumented.
Their loops are bounded by input size.

## Running the checks

```sh
bash workloads/languages/build-image.sh c
bash workloads/languages/run-check.sh harmony-language-c:local evidence/c
```

`run-check.sh` runs `harmony preflight --image`, boots the fixture twice with a
fixed seed, and requires twenty ordered markers and identical serial logs and
run records. It then runs the fixture under the park launcher, which holds one
thread at a coverage site and requires another thread to keep making callbacks.
The C check also runs the fixture as two processes and the GCC trace-pc build.

Set `HARMONY_BINARY` and `HARMONY_GUEST_DIR` to use another CLI or guest build.
Guest RAM defaults to 1024 MiB because the initramfs holds the rootfs and
unstripped symbols; set `HARMONY_LANGUAGE_RAM_MIB` to change it. The seed
defaults to 17. The macOS OCI backend requires `HARMONY_LANGUAGE_SEED=0`.

## CI

`Checks / Harmony Workloads / Languages` restores or builds the guest in
`Language Guest Runtime`. Each `Language Image — <Language>` job restores or
builds one language layer and composes the current runtime onto it. Each
`Check — <Language>` job runs `run-check.sh` on that image. The weekly schedule and the `rebuild_images` dispatch input
rebuild every layer without the cache.

The [preparing-workloads skill](../../.agents/skills/preparing-workloads/SKILL.md)
has one reference per language, written from these recipes.
