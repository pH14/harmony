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

Set `HARMONY_HBASE_RELEASE=3.0.0` for the checksum-pinned Apache 3.0.0
binary instead of the default source build. This produces
`harmony-hbase:3.0.0` with the same harness and JVM instrumentation. The
HBase 3.0.0 release-line witness can then be checked with:

```sh
bash workloads/harness/targets/hbase/check-clock-skew.sh harmony-hbase:3.0.0 \
  /tmp/hbase3-clock-skew workloads/harness/targets/hbase/harmony.toml \
  workloads/harness/targets/hbase/clock-skew-hbase-three.json
```

`HARMONY_HBASE_BUILD_CACHE` selects the persistent source-download cache. The
recipe verifies downloaded archives against pinned hashes. The default HBase
Maven build needs a file-descriptor limit of 65,536; the build script supplies
it. It compiles upstream test sources but does not run the upstream test suite.
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

`bash workloads/harness/targets/hbase/check-recovery.sh IMAGE EVIDENCE CONFIG [ACTIONS]`
runs a fixed scenario and exact replay. The default `recovery.json` waits
ten guest seconds, kills bundle node 8 (`r1`) for one second, then waits 200
seconds; each action uses coverage quantum 4,096.

Additional scenarios warm up for 30 guest seconds, apply a five-second fault,
and allow 200 seconds for recovery:

- `recovery-datanode.json` restarts the first DataNode (bundle node 4).
- `recovery-hmaster.json` restarts HMaster (bundle node 7).
- `recovery-namenode.json` restarts the single NameNode (bundle node 3).
- `recovery-partition.json` partitions the network and invokes the heal hook.

These are diagnostic scenarios, not a promise that each fault recovers. The
single NameNode scenario can trigger secondary JVM exits. Network hook
completion alone does not establish that connectivity or HBase has recovered.

`check-recovery.py REPORT ACTIONS` also validates retained reports. It requires
an acknowledged write before the fault, the requested action sequence, observed
restarts or completed hooks, all ten services alive without unexpected exits,
and a successful read in the final check. It rejects violations, missing
reachability assertions, execution failures, watchdog cutoffs, undecided keys,
and pending faults. A read recorded earlier in the timeline cannot substitute
for a successful final check.

Use `probe.sh` to measure cluster launch and readiness, guest available memory,
per-process RSS and thread counts, and a short workload followed by a decided
check. Keep measurement output outside the repository and report the image
revision, guest budget and observed results with the pull request. A longer
fault-search qualification must also satisfy every reachability assertion.
Replays use their exact recording binaries, kernel, initramfs and image;
measurements from different builds are not interchangeable.

## Clock-skew consistency witness

`bash workloads/harness/targets/hbase/check-clock-skew.sh IMAGE EVIDENCE CONFIG`
warms up the cluster, steps the first RegionServer's wall clock back one
second, and checks for an acknowledged Put followed by a stale Get. It treats
the consistency finding as the expected result and requires exact replay.
Pass `clock-skew-hbase-three.json` as a fourth argument when testing the Apache
HBase 3.0.0 binary image; its longer clock-fault window and coverage choices
reproduced the same violation under a one-second backward step. Both the run
and its exact replay had all ten services alive, no restarts, no pending
faults, and no undecided keys. Its guest journal records a successful CAS on
`k2` before the step, a later acknowledged Put after the step, and two
subsequent successful Gets returning the CAS value. These operations do not
overlap, and the journal uses a monotonic clock. A different timing and
coverage choice with the same one-second step completed without a violation,
so this scenario establishes a reachable failure rather than a universal
outcome. The diagnostic image used instrumented Java 21, while HBase 3.0.0's
[documented supported runtime](https://hbase.apache.org/docs/configuration/basic-prerequisites/)
is Java 17.

The pinned HBase revision and image `harmony-hbase:qualified-2c79c38`
reproduced the finding without a service restart: an acknowledged CAS on `k0`
completed, a later Put on `k0` was acknowledged, and two later successful
reads returned the CAS value. The checker orders these operations with its
monotonic journal clock, so the wall-clock step does not create a false order.

HBase's [version semantics](https://hbase.apache.org/docs/single-page/) assign
an implicit Put the RegionServer's current wall-clock time and make an
ordinary Get return the highest timestamped version. A backward clock step
can therefore make a later acknowledged write temporarily invisible to a
normal Get. The master's default [skew warning and rejection thresholds](https://hbase.apache.org/devapidocs/src-html/org/apache/hadoop/hbase/master/ServerManager.FlushedSequenceIdFlusher.html)
are 10 and 30 seconds, respectively; they do not prevent a one-second step.
Applications requiring read-after-write visibility should keep RegionServer
clocks synchronized and avoid backward steps. This is a conditional clock
fault, not evidence of stale reads under normal synchronized operation.
