#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
index=$1
service=$2
shift 2
conf=/tmp/hbase/n$index/conf
heap=256m
case "$service" in
 namenode) main=org.apache.hadoop.hdfs.server.namenode.NameNode ;;
 datanode) main=org.apache.hadoop.hdfs.server.datanode.DataNode ;;
 master) /opt/harmony/hbase-harness wait-namenode; main=org.apache.hadoop.hbase.master.HMaster; set -- start "$@" ;;
 regionserver) main=org.apache.hadoop.hbase.regionserver.HRegionServer; heap=512m; set -- start "$@" ;;
 *) exit 2 ;;
esac
export HADOOP_HOME=/opt/hadoop HADOOP_CONF_DIR=$conf HBASE_CONF_DIR=$conf
exec /opt/harmony/harness-net exec "$index" -- /opt/java/bin/java \
 -XX:TieredStopAtLevel=1 -XX:+UseSerialGC -XX:ActiveProcessorCount=1 -Xms32m -Xmx$heap \
 -Djava.security.manager=allow -Dio.netty.transport.noNative=true -Dio.netty.noNative=true \
 -Dorg.apache.hbase.thirdparty.io.netty.transport.noNative=true \
 --add-opens=java.base/java.lang=ALL-UNNAMED --add-opens=java.base/java.nio=ALL-UNNAMED \
 --add-opens=java.base/sun.nio.ch=ALL-UNNAMED \
 --add-opens=java.base/java.io=ALL-UNNAMED --add-opens=java.base/jdk.internal.ref=ALL-UNNAMED \
 --add-opens=java.base/java.lang.reflect=ALL-UNNAMED --add-opens=java.base/java.util=ALL-UNNAMED \
 --add-opens=java.base/java.util.concurrent=ALL-UNNAMED \
 --add-exports=java.base/jdk.internal.misc=ALL-UNNAMED \
 --add-exports=java.security.jgss/sun.security.krb5=ALL-UNNAMED \
 --add-exports=java.base/sun.net.dns=ALL-UNNAMED --add-exports=java.base/sun.net.util=ALL-UNNAMED \
 -Dorg.apache.hbase.thirdparty.io.netty.tryReflectionSetAccessible=true \
 -Dlog4j.configurationFile=$conf/log4j2.properties \
 -Dhbase.log.dir=/tmp/hbase/n$index -Dhbase.log.file=server.log \
 -Dhadoop.log.dir=/tmp/hbase/n$index -Dhadoop.log.file=hadoop.log \
 -cp "$conf:/opt/hbase/lib/client-facing-thirdparty/*:/opt/hbase/lib/*:/opt/hbase/lib/hadoop-3/*:/opt/hadoop/share/hadoop/common/*:/opt/hadoop/share/hadoop/common/lib/*:/opt/hadoop/share/hadoop/hdfs/*:/opt/hadoop/share/hadoop/hdfs/lib/*" \
 "$main" "$@" >> /tmp/hbase/n$index/console.log 2>&1
