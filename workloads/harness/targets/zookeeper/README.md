# ZooKeeper fault target

This target runs Apache ZooKeeper 3.9.4 as three independent JVMs in network
namespaces, driven by `github.com/go-zookeeper/zk` v1.0.4 and the harness kit's
`log` family. The Apache binary distribution is pinned by SHA-512 in the
Dockerfile. Its classes are rewritten when loaded by the instrumented OpenJDK
21 runtime. The Go client, its non-standard-library dependencies, and the
network helper use the Go instrumentation pipeline and retain their symbols.

## Build and run

Run these commands from the repository root on a qualified Linux KVM host:

```sh
export HARMONY_CONTAINER_TOOL=podman
bash workloads/languages/build-image.sh java --build-arg \
  JAVA_MODULES=java.base,java.management,java.naming,java.sql,java.security.jgss,java.instrument,jdk.unsupported
podman build --format docker -f workloads/harness/targets/zookeeper/Dockerfile \
  -t harmony-zookeeper:local .
target/release/harmony prepare harmony-zookeeper:local
target/release/harmony search --config workloads/harness/targets/zookeeper/harmony.toml
```

The Java builder resolves and rewrites transitive modules. `jdeps` output from
the complete ZooKeeper dependency graph is retained at `/opt/zookeeper/modules.txt`.
That graph also lists `java.desktop`; the tested NIO server, console logger,
JMX initialization, and client paths do not load its optional desktop classes.
TLS, SASL, native Netty transports, and other optional server configurations
are not qualified by this target. Add and rewrite their modules and review
native libraries before enabling them.

The default bundle uses three servers. `--build-arg HARNESS_BUNDLE=bundle-single`
selects a standalone server. `measure-cluster.sh 5` measures five-server boot;
it is a sizing probe, not a five-server search bundle. The standalone image
command is useful for debugging; fault searches use `/etc/harmony/bundle`.

The target keeps ZooKeeper's standard 2-second tick, initialization and sync
limits, snapshot policy, and transaction-log behavior. It selects C1-only
compilation, Serial GC, one advertised processor, a 32 MiB initial heap, and a
128 MiB maximum heap for each JVM. These are JVM resource settings, not changes
to the replication protocol. `harmony.toml` declares 1 GiB guest RAM and a
300-second guest setup budget; other targets retain the 120-second default.
The measured three-server configuration also fits that default budget.

## Oracle and readiness

Each append creates an immutable persistent sequential znode under
`/harmony-log`. Reads commit a write to a separate fence znode on the same
session before listing and reading the immutable log entries in sequence order.
A stale server cannot satisfy that fence by serving only local state. Pinned
reads connect to exactly one named server and never fail over to another node.
Append failures have unknown outcome; read failures are definite because the
fence is outside the logical log. The checker does not mistake the fence for
an application append.

Readiness establishes a session and completes a committed fence write through
every configured server. Only readiness creates the two parent znodes. A
normal read never recreates missing application data. Operations close their
connection on deadline and reconnect on a subsequent call. The bundle exposes
kill, pause, restart, event, clock and network-hook faults. It does not declare
node disks and therefore does not claim power-loss or disk-corruption coverage.

## Measured startup

Measurements used seed 17 on pnap-3, a two-socket Xeon Gold 6336Y host with 24
physical cores, 125 GiB RAM, and Linux 6.8.0-139-generic. Every guest had one
vCPU. Guest seconds below run from the launch marker to a successful client
session and fence write, not merely a bound TCP port.

| Single-server settings | Guest seconds to ready | Completed run |
| --- | ---: | --- |
| JVM defaults | 79.71 | No: process termination stalled |
| `-XX:TieredStopAtLevel=1` | 10.88 | Yes |
| `-Xint` | 8.81 | Yes |
| Serial GC, one processor, two compiler threads | 15.47 | Yes |
| C1, Serial GC, one processor, 32–128 MiB heap | 9.94 | Yes |
| Selected settings plus an image-time CDS archive | 9.88 | Yes |

The selected configuration retains compilation for sustained workload traffic.
The CDS difference is too small to justify an archive in the default image.
The default-JVM shutdown stall is tracked in
[issue 881](https://github.com/pH14/harmony/issues/881); sampled guest PCs were
in `d_walk`, `select_collect2`, and `_raw_spin_lock`. This is not attributed to
ZooKeeper and does not establish that every default-JVM run will stall.

| Servers | Guest RAM | Guest seconds to ready | JVM RSS at ready | Guest available memory |
| --- | ---: | ---: | ---: | ---: |
| 3 | 1024 MiB | 36.19 | 79–82 MiB each | 482 MiB |
| 5 | 1536 MiB | 64.46 | 79–84 MiB each | 845 MiB |

Five servers fit this measured configuration; neither RAM nor the original
120-second guest budget prevented readiness. CPU work grows with the number
of JVMs on the single vCPU. These small heaps are for a bounded test log, not
production sizing or evidence of indefinite operation without heap pressure.

`measure.sh` accepts JVM flags and runs one server with an otherwise unchanged
JVM command. `measure-cluster.sh` measures the configured target JVM settings
for 3 or 5 servers. Both record guest uptime and memory/thread data, then kill
their own test processes. Keep each measurement's exact binary pair, image,
kernel, initramfs, and seed for replay.

## Validation

The initial native three-server, no-fault run completed 4,407 operations and
103 checks in 61.598 seconds, without check failures or unexpected node exits.
The expanded Java image passed the language recipe's two-boot determinism,
twenty ordered markers, park, C2, and dynamically generated loop fixtures.
A short four-worker fault search completed 14 executions with no confirmed
findings or infrastructure failures, but did not satisfy post-fault read
reachability. That short run is not a passing search qualification.

The subsequent seed-17 search with a requested 30-minute budget passed:
178 executions, no confirmed findings, no unmet reachability assertions,
no execution failures, and no watchdog cutoffs. It used ten workers on physical
cores 14–23, 1 GiB guest RAM per worker, and a 32 GiB systemd memory limit.
The report records 1,986 wall seconds including setup and finishing outstanding
work. All four reachability assertions were satisfied. This bounded run is
evidence of target viability, not proof that ZooKeeper has no bugs.

A separate fixed kill/recovery run on the final target image passed all four
reachability checks, including a successful read after the fault, with no
violations, execution failures or watchdog cutoffs. Its setup snapshot was at
39.374 guest seconds, including boot and ensemble readiness. A fresh replay
using the recording's exact binary and image verified the execution. The
action sequence waited 1,000 ticks, killed node zero for 100 ticks, then waited
6,000 ticks, with a coverage quantum of 4,096 and 10 ms ticks.

Evidence is retained on pnap-3 under
`/home/ubuntu/harmony-jvm-viability/evidence`, including the binary pairs,
admission records, startup profiles, language fixtures, search and recovery
artifacts. VM measurements used recovery base `d90a95102` plus the Go and JVM
changes. The delivered branch also includes recovery commit `f18110877`;
its supervisor tests and warnings-as-errors checks passed on pnap-3. That
upstream change handles disk remounts; this target declares no node disks.
The final rebuilt image separately passed admission and fixed recovery/replay;
it is not bit-identical to the image used for the timing panel and search.

## Startup profile

HotSpot performance counters from the selected configuration reported about
7.39 seconds in the class-loading timer, 7.49 seconds in class initialization
(4.10 seconds self time), and 1.22 seconds in verification (0.21 seconds self
time). These counters overlap and include elapsed virtual time; they are not
additive CPU phases. There were 2,234 initialized classes and 1,052 compiled
methods. C1 compilation counters totaled 1.78 seconds. Five young collections
took 0.082 seconds; there were no full collections. `/proc` thread accounting
in the timing panel attributed 6.40 CPU seconds to the main Java thread and
about 2.41 seconds to the two C1 compiler threads. The selected run accumulated
only about 0.07 seconds of additional guest idle time. Class startup and
compilation dominate this configuration; GC and sleeps do not.

With Serial GC but full tiered compilation, the compiler threads consumed
about 7.72 CPU seconds, including 5.70 seconds in C2. Interpreter-only startup
used 7.28 CPU seconds in the main thread and avoided compilation. The default
run accumulated much more guest idle time and then hit the separate shutdown
stall, so its 79.71-second result is not explained solely by compilation.

The Java recipe's diagnostic callback counter found that only about 5.2% of
coverage exchanges came directly from Java back-edge callbacks. The existing
countdown was retained; no instrumentation or virtual-time yields were removed.
Verbose class/JIT logging itself increased readiness time to 13.11 seconds,
so the timing panel and diagnostic run must not be treated as interchangeable.

## Path to HBase

The next implementation belongs on `codex/hbase-target`, cut from this branch.
An honest first distributed HBase test needs independently supervised HMaster
and RegionServer processes plus a shared durable filesystem. A standalone
HBase JVM or a local-filesystem smoke test can establish class loading and
startup, but cannot establish distributed storage fault behavior. In
particular, disabling HBase's hflush/hsync capability check changes the data-loss
contract; the [HBase guide](https://hbase.apache.org/book.html) documents that
local-filesystem limitation.

| Process | Smallest distributed probe | Replicated-storage probe | Initial heap trial |
| --- | ---: | ---: | ---: |
| ZooKeeper | 3 | 3 | 128 MiB each, measured here |
| HDFS NameNode | 1 | 1 | 256 MiB, unmeasured |
| HDFS DataNode | 1 | 3 | 256 MiB each, unmeasured |
| HMaster | 1 | 1 | 256 MiB, unmeasured |
| RegionServer | 2 | 2 | 512 MiB each, unmeasured |
| Total JVMs | 8 | 10 | 2,176 / 2,688 MiB heap total |

One DataNode gives genuine shared HDFS semantics but no storage replication;
use three DataNodes and replication factor three before claiming replicated
storage recovery. A single NameNode and HMaster also exclude HA failover
claims for those roles. The Go HBase client must use the same all-dependency
instrumentation as this target.

Start a qualification probe with 6 GiB guest RAM for eight JVMs, or 8 GiB for
ten. These are proposed test allocations, not measured HBase minima: they leave
space beyond heaps for metaspace, native code, thread stacks, the rootfs and
filesystem cache. Keep one guest vCPU; adding vCPUs is outside Consonance's
execution model. On this host, six 8-GiB workers would consume roughly 54 GiB
before additional cache allocation and leave substantial build headroom.

The measured ZooKeeper part takes 36.19 guest seconds. HDFS and HBase startup
have not been measured and cannot honestly be assigned ready times from
ZooKeeper alone. Start their probe with a declared 600-second setup budget,
record each service's readiness time, and revise that budget from measurements.
Readiness must include a live HMaster, two registered RegionServers, table
creation, and a committed write/read, not only listening RPC ports.

Select and pin a release with Java 21 support: the current
[HBase prerequisites](https://hbase.apache.org/docs/configuration/basic-prerequisites/)
list HBase 2.6.8+, 2.7, 3.0.1+, and 3.1 as supported combinations. Independently
check the bundled Hadoop/JDK combination. Run `jdeps` over the chosen HBase and
Hadoop jars and rewrite every selected JDK module. Review JNI-defined classes,
Hadoop native libraries, compression libraries, Netty native transports and
all other executable paths before admission; ZooKeeper's module set does not
qualify HBase's larger runtime. The ZooKeeper sizing evidence makes this next
probe plausible, but does not prove HBase fits or passes a fault search.

## Default-JVM shutdown regression

`bash workloads/harness/targets/zookeeper/check-shutdown.sh IMAGE EVIDENCE CONFIG`
boots the raw-default JVM measurement twice with seeds 17 and 18 and replays
seed 17. It requires successful readiness and complete guest shutdown, so a
run that reaches readiness and then stalls does not pass. `CONFIG` is optional;
use it to select the exact kernel and initramfs under test. The dcache fix is
in the common guest-kernel patches, not in ZooKeeper or its JVM flags.

The production dcache patch passed the original default-JVM measurement on
pnap-3 with seeds 17 and 18: readiness at 79.71 and 79.62 guest seconds,
respectively, followed by successful shutdown (exit zero). Seed 17 replayed
successfully. Evidence is in `dcache-final-17`, `dcache-final-18`, and
`dcache-final-replay` beneath the evidence directory above. This fixes the
observed shutdown starvation; it does not make default JVM startup as fast
as the selected C1/Serial-GC configuration. The x86 kernel was tested;
the arm64 conditional path was not executed on this host.
