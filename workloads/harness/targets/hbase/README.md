# HBase fault target

This target runs ten independently supervised JVMs: three ZooKeeper servers,
one HDFS NameNode, three DataNodes, one HMaster and two RegionServers. The
Go register adapter checks normal HBase routing and synchronous WAL writes.

## Service inventory

| Service | Language/version | Build system | Runtime path | Symbols |
| --- | --- | --- | --- | --- |
| Three ZooKeeper servers | Java 21, ZooKeeper 3.9.4 | Pinned Apache jars | `/opt/zookeeper/lib` | Instrumented HotSpot under `/symbols/native` |
| HDFS NameNode and three DataNodes | Java 21, Hadoop 3.4.3 | Pinned Apache jars | `/opt/hadoop/share/hadoop` | Instrumented HotSpot under `/symbols/native` |
| HMaster and two RegionServers | Java 21, HBase branch-2.6 at `faa1c454c7bc9a6c29749b49218196ad4b7542d0` | Maven | `/opt/hbase/lib` | Instrumented HotSpot under `/symbols/native` |
| Harness client | Go, gohbase at `20216bdbc67df914b496844eb0a794186e7bc240` | Go + Harmony instrumentation | `/opt/harmony/hbase-harness` | `/symbols/hbase-go` |
| Network helper | Go, repository harness kit | Go + Harmony instrumentation | `/opt/harmony/harness-net` | `/symbols/go` |

The HBase source revision includes the Java 21 fixes targeted for 2.6.8;
2.6.8 is not yet a published release. Native Hadoop, Netty and compression
libraries must be disabled or instrumented before image admission. Every
selected JDK module and application class uses the Java recipe's rewriting.

The topology uses one guest vCPU, 8 GiB guest RAM, and a declared 600-second
setup budget. JVMs use C1 (`TieredStopAtLevel=1`), Serial GC, one active
processor and a 32 MiB initial heap. Maximum heaps are 128 MiB for ZooKeeper,
256 MiB for NameNode, DataNodes and HMaster, and 512 MiB for RegionServers.
HDFS replication is three. Readiness must establish HMaster, two registered RegionServers, table
creation, and a committed write/read. A single NameNode and HMaster exclude
HA failover claims for those roles. Runtime commands are in `node.sh` and
`bundle`; measured results are below.

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

## Workload contract

The register adapter uses HBase's strongly consistent Get, Put with
`SYNC_WAL`, and atomic CheckAndPut on one column per row. HBase uses the
supported filesystem WAL provider over three-copy HDFS; stream-capability
checks remain enabled. No local-filesystem substitute or skipped WAL is used.
The table has two initial regions split at `k2`. Readiness waits for two
RegionServers, commits and reads a separate readiness row in each region, and
checks cluster status for test-table ownership on both RegionServers;
those rows are outside the checker's `k0`–`k4` keyspace.

The pinned Go client has a small reviewed patch: mutation RPCs return after
the first attempt. Region discovery and read retries remain intact. Without
this boundary, a lost reply could cause a repeated Put to overwrite an
intervening writer or a repeated successful CAS to report failure. The harness
records mutation errors as unknown, and only an explicit negative CAS result
as a definite failure. Reads are definite on error.

HBase regions are shards, not identical replicas on every RegionServer. The
adapter therefore does not claim pinned-node reads or equality across servers.
The register checker covers per-key linearizability and stale reads through
normal HBase routing. The bundle exposes process, event, clock and network
faults; it does not yet declare disks or claim power-loss and disk-corruption
coverage. The single NameNode and HMaster do not provide HA failover.

HMaster waits for NameNode's RPC port before launching its JVM. This startup
dependency gate avoids a measured pre-readiness HMaster abort when the
NameNode is still loading. Setup adds the guest's hostname to `/etc/hosts`,
and the JVM launcher carries HBase's upstream Java module-access flags.
These are required in the isolated guest, which has no external DNS service.

## Measurements and validation

On pnap-3, seed 17, one guest vCPU and 8 GiB RAM, the first successful guest
probe formatted HDFS by uptime 28.90 seconds, launched the cluster at 28.91,
and reached full readiness at 451.30: 422.39 guest seconds from launch.
It completed twenty workload operations plus a check read with no violations,
then shut down successfully. At readiness the guest had 5,666,308 KiB available
memory. JVM RSS was about 83–86 MiB for ZooKeeper, 205 MiB for NameNode,
181–184 MiB per DataNode, 223 MiB for HMaster, and 199–203 MiB per RegionServer.
The total thread count was 684. This is a small bounded workload, not a
production memory-sizing recommendation.

The expanded Java runtime passed two-boot determinism, twenty ordered timer
markers, park progress, two C2 boots and the generated method-handle loop on
the patched kernel. The image passed admission with instrumentation symbols
and no scan errors. A focused Go-client regression verifies that a mutation's
first retryable result is returned without another RPC attempt. The native
resource-removal test verifies class bytes survive while executable resources
are removed. The final source-build recipe produced the expected distribution
archive. With CI's Go 1.26.3 and `GOTOOLCHAIN=local`, harness-kit vet/tests and
vet/build for every target module passed on pnap-3.

An initial native baseline completed 8,959 operations and 222 checks in
61.834 seconds with no checker failures, but one HMaster startup restart;
that observation prompted the NameNode dependency gate. The first guest
attempt failed on unresolved hostname lookup, before the configuration fix.
These earlier images are not interchangeable with the final image for replay.

Evidence and exact recording artifacts remain on pnap-3 under
`/home/ubuntu/harmony-jvm-viability/evidence`: `hbase-probe2`, the subsequent
`hbase-probe3`, `hbase-language-check`, native runs, and the recovery/search
artifacts. Guest executions use the audited dcache-patched kernel and the
retained paired CLI/host binaries. The final dependency-gated image reached
readiness at guest uptime 463.03,
434.12 seconds after cluster launch, with 5,675,904 KiB available and 686 JVM
threads. Its twenty-operation guest workload and check passed, and all JVMs
terminated successfully. The final native baseline completed 9,002 operations
and 247 checks in 61.214 seconds with no check failures or process restarts.
The requested 30-minute search uses three workers on cores 16–19 within a
64 GiB unit; automatic sizing reserves about 27 GiB for snapshot/worker stores.
The requested 30-minute search completed 94 executions with zero confirmed
findings, unmet reachability assertions, hung jobs, watchdog cutoffs or execution
failures. Including setup and completion, it took 2,129 wall seconds. The fixed
recovery scenario passed exact replay using its recorded binary, kernel and image.

`bash workloads/harness/targets/hbase/check-recovery.sh IMAGE EVIDENCE CONFIG`
runs the fixed RegionServer scenario and exact replay. `recovery.json` waits
10 guest seconds, kills bundle node 8 (`r1`) for one second, then waits 200
seconds; each action uses coverage quantum 4,096. The initial qualification
completed in 303 wall seconds with no violations, execution failures or
watchdog cutoffs. All three reachability assertions passed, including a
post-fault read. The final check had zero undecided keys and pending faults.
