# lost-update

Two writer processes increment one counter in shared memory with a plain read
followed by a write. A writer stopped between the read and the write misses
every increment the other writer makes in that time, and then overwrites them.

## Triple

| part | here |
|---|---|
| workload | `lost-update writer /data/counter ID` for IDs 0 and 1 over one `MAP_SHARED` file that `setup` creates |
| fault surface | an event park that holds a writer between `read_value` and `write_value` |
| oracle | the Always assertion `the counter holds every finished increment` |

Each writer counts its finished increments in its own slot. After every
increment it reads both slots, then the counter, and asserts that the counter is
at least their sum. A writer sleeps 1 ms between increments, so the guest's one
vCPU switches writers only at that system call. The read and the write have no
system call between them, so no scheduling decision and no signal-based fault
lands there. Only a park can, which makes this the smallest check that event
parks reach syscall-free user code.

## Variants and difficulty

Kernel command-line knobs, passed with `--knobs`:

| knob | effect |
|---|---|
| `lost_update.atomic=1` | the increment is one atomic add; this is the clean control and must never violate the assertion |
| `lost_update.noise=N` | each iteration also calls `N` distinct instrumented functions, 0 through 32, so a park has `N` more sites to land on outside the window |

The default is the racy increment with no extra sites.

## Running

Build the image from the repository root:

```sh
docker build --platform linux/arm64 \
  -f workloads/bugs/category/lost-update/image/Dockerfile \
  -t harmony-lost-update .
docker save -o lost-update.oci harmony-lost-update
```

Search it like any faults image:

```sh
harmony search --package faults lost-update.oci --backend consonance \
  --kernel Image --base-initramfs initramfs-oci.cpio.gz \
  --seed 1 --executions 100000 --ram-mib 256 --out run/
```
