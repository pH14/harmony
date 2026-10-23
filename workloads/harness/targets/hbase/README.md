# HBase fault target

This target runs ten independently supervised JVMs: three ZooKeeper servers,
one HDFS NameNode, three DataNodes, one HMaster and two RegionServers. The
Go register adapter checks normal HBase routing and synchronous WAL writes.

## Service inventory

| Service | Language/version | Build system | Runtime path | Symbols |
| --- | --- | --- | --- | --- |
| Three ZooKeeper servers | Java 21, ZooKeeper 3.9.4 | Pinned Apache jars | `/opt/zookeeper/lib` | Instrumented HotSpot under `/symbols/native` |
| HDFS NameNode and three DataNodes | Java 21, Hadoop 3.4.3 | Pinned Apache jars | `/opt/hadoop/share/hadoop` | Instrumented HotSpot under `/symbols/native` |
| HMaster and two RegionServers | Java 21, HBase master at `2c79c38bea97027fc841e2cfe074e0aef93e6e19` | Maven | `/opt/hbase/lib` | Instrumented HotSpot under `/symbols/native` |
| Harness client | Go, gohbase at `20216bdbc67df914b496844eb0a794186e7bc240` | Go + Harmony instrumentation | `/opt/harmony/hbase-harness` | `/symbols/hbase-go` |
| Network helper | Go, repository harness kit | Go + Harmony instrumentation | `/opt/harmony/harness-net` | `/symbols/go` |

The pinned HBase master revision is 4.0.0-alpha-1-SNAPSHOT, built for Java 17
or newer and run here on the instrumented Java 21 runtime. Native Hadoop, Netty and compression
libraries must be disabled or instrumented before image admission. Every
selected JDK module and application class uses the Java recipe's rewriting.

The topology uses one guest vCPU, 8 GiB guest RAM, and a declared 600-second
setup budget. JVMs use C1 (`TieredStopAtLevel=1`), Serial GC, one active
processor and a 32 MiB initial heap. Maximum heaps are 128 MiB for ZooKeeper,
256 MiB for NameNode, DataNodes and HMaster, and 512 MiB for RegionServers.
HDFS replication is three. Readiness must establish HMaster, two registered RegionServers, table
creation, and a committed write/read. A single NameNode and HMaster exclude
HA failover claims for those roles. Runtime commands are in `node.sh` and
`bundle`; validation commands are below.

## Build

On a qualified Linux KVM host, from the repository root:

```sh
bash workloads/harness/targets/hbase/build-image.sh
target/release/harmony prepare harmony-hbase:local
target/release/harmony debug run harmony-hbase:local --config workloads/harness/targets/hbase/harmony.toml --for 20m -- /opt/harmony/hbase-probe.sh
target/release/harmony search --config workloads/harness/targets/hbase/harmony.toml --for 30m
```

`HARMONY_HBASE_BUILD_CACHE` selects the persistent source-download cache. The
recipe verifies both source archives against pinned hashes. The HBase Maven
build needs a file-descriptor limit of 65,536; the build script supplies it.
It compiles upstream test sources but does not run the upstream test suite.
`HARMONY_HBASE_BUILD_CPUS` and `HARMONY_HBASE_BUILD_MEMORY` bound container
builds directly; Podman can place build containers outside a parent systemd
unit. The memory default is 24 GiB.

The Java module set starts with `java.se` plus `jdk.unsupported`,
`jdk.security.auth`, `jdk.management`, `jdk.naming.dns`, `jdk.crypto.ec` and
`jdk.zipfs`. All resolved modules are rewritten. The image records `jdeps`
output and the native resources removed from dependency jars under
`/opt/hbase`. Hadoop native libraries are not copied; bundled ELF/PE/Mach-O
resources and native-library resources are removed from the copied jars.
Netty native transports are disabled and table compression is `NONE`.
This qualifies the Java implementations, not native compression or transport.
The launcher and `jdeps` classpath include HBase 4's
`lib/client-facing-thirdparty` directory before the other libraries, matching
upstream's dependency layout and logging precedence. Optional attach/debugger,
SCTP and JDK HTTP-server tooling paths are not enabled or qualified.

## Workload contract

The register adapter uses HBase's strongly consistent Get, Put with
`SYNC_WAL`, and atomic CheckAndPut on one column per row. HBase uses the
supported filesystem WAL provider over three-copy HDFS; stream-capability
checks remain enabled. No local-filesystem substitute or skipped WAL is used.
The table has two initial regions split at `k2`. Readiness waits for two
RegionServers, commits and reads a separate readiness row in each region, and
checks cluster status for test-table ownership on both RegionServers;
those rows are outside the checker's `k0`–`k4` keyspace.

The pinned Go client has two reviewed patches. Mutation RPCs return after
the first attempt. Region discovery and read retries remain intact. Without
this boundary, a lost reply could cause a repeated Put to overwrite an
intervening writer or a repeated successful CAS to report failure. The harness
records mutation errors as unknown, and only an explicit negative CAS result
as a definite failure. Reads are definite on error.

Unrestricted Get and Scan requests omit the optional protobuf time range. An
empty time-range message means an empty interval to HBase 4's decoder; emitting
it would make metadata lookup incorrectly report missing tables. Explicit
non-default bounds retain their representation. The image build runs the
upstream RPC serialization tests and a regression that round-trips default,
lower-bounded, upper-bounded and fully bounded ranges through the wire format.

HBase regions are shards, not identical replicas on every RegionServer. The
adapter therefore does not claim pinned-node reads or equality across servers.
The register checker covers per-key linearizability and stale reads through
normal HBase routing. The bundle exposes process, event, clock and network
faults; it does not yet declare disks or claim power-loss and disk-corruption
coverage. The single NameNode and HMaster do not provide HA failover.

HMaster waits for NameNode's RPC port before launching its JVM. This startup
dependency check avoids a measured pre-readiness HMaster abort when the
NameNode is still loading. Setup adds the guest's hostname to `/etc/hosts`,
and the JVM launcher carries HBase's upstream Java module-access flags.
These are required in the isolated guest, which has no external DNS service.

`python3 workloads/harness/targets/hbase/check-native.py NATIVE_LOG` validates
a retained `native.py --no-faults` run: the final check must be decided, with
no check failures or process deaths, and the journal must contain successful
reads, writes and CAS operations. The check rejects clean-looking runs whose journal never acknowledges a CAS.

## Recovery and replay

`bash workloads/harness/targets/hbase/check-recovery.sh IMAGE EVIDENCE CONFIG`
runs the fixed RegionServer scenario and exact replay. `recovery.json` waits
ten guest seconds, kills bundle node 8 (`r1`) for one second, then waits 200
seconds; each action uses coverage quantum 4,096. The check requires no
violations, execution failures or watchdog cutoffs, all reachability
assertions including a post-fault read, and zero undecided keys or pending
faults at the final check.

Use `probe.sh` to measure cluster launch and readiness, guest available memory,
per-process RSS and thread counts, and a short workload followed by a decided
check. Keep measurement output outside the repository and report the image
revision, guest budget and observed results with the pull request. A longer
fault-search qualification must also satisfy every reachability assertion.
Replays use their exact recording binaries, kernel, initramfs and image;
measurements from different builds are not interchangeable.
