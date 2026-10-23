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

## Startup and shutdown checks

`measure.sh` accepts JVM flags and runs one server with an otherwise unchanged
JVM command. `measure-cluster.sh` measures the configured target JVM settings
for three or five servers. Both record guest uptime and memory/thread data,
then terminate their own test processes. These small heaps serve a bounded
test log; they are not production sizing recommendations. The selected C1
configuration retains compilation for sustained workload traffic. Callback
batching is unchanged.

`bash workloads/harness/targets/zookeeper/check-shutdown.sh IMAGE EVIDENCE CONFIG`
boots the raw-default JVM measurement twice with seeds 17 and 18 and replays
seed 17. It requires readiness and complete guest shutdown: a run that becomes
ready and then stalls fails. `CONFIG` is optional and selects the kernel and
initramfs under test. The pending-dentry scheduling fix belongs to the VM
kernel patch series; target JVM flags do not implement it.

Before searching, run the Java recipe's timer, two-boot determinism, park,
C2 and generated-loop fixtures. Run the native baseline without faults and
verify a decided check with no failures. A fault-search qualification must
satisfy every reachability assertion, including a successful read after a
fault. Preserve the exact recording binary pair, kernel, initramfs, image and
seed for every replay. Keep run evidence outside the repository and attach
measurements to the pull request.

## Related target

The [HBase target](../hbase/README.md) composes this ZooKeeper ensemble with
replicated HDFS, HMaster and two RegionServers.
