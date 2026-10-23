#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
printf '127.0.0.1 %s\n' "$(cat /proc/sys/kernel/hostname)" >> /etc/hosts
/opt/harmony/setup.sh 3
/opt/harmony/harness-net setup 10
mkdir -p /tmp/hbase /tmp/harness/journal
for index in $(seq 4 10); do
 dir=/tmp/hbase/n$index
 mkdir -p "$dir/conf" "$dir/data" "$dir/tmp"
 cat > "$dir/conf/core-site.xml" <<XML
<configuration>
<property><name>fs.defaultFS</name><value>hdfs://10.77.0.4:9000</value></property>
<property><name>hadoop.tmp.dir</name><value>$dir/tmp</value></property>
<property><name>io.native.lib.available</name><value>false</value></property>
</configuration>
XML
 cat > "$dir/conf/hdfs-site.xml" <<XML
<configuration>
<property><name>dfs.namenode.name.dir</name><value>file:///tmp/hbase/n4/data</value></property>
<property><name>dfs.datanode.data.dir</name><value>file://$dir/data</value></property>
<property><name>dfs.namenode.rpc-address</name><value>10.77.0.4:9000</value></property>
<property><name>dfs.namenode.http-address</name><value>10.77.0.4:9870</value></property>
<property><name>dfs.datanode.hostname</name><value>10.77.0.$index</value></property>
<property><name>dfs.datanode.address</name><value>0.0.0.0:9866</value></property>
<property><name>dfs.datanode.ipc.address</name><value>0.0.0.0:9867</value></property>
<property><name>dfs.datanode.http.address</name><value>0.0.0.0:9864</value></property>
<property><name>dfs.client.use.datanode.hostname</name><value>true</value></property>
<property><name>dfs.datanode.use.datanode.hostname</name><value>true</value></property>
<property><name>dfs.replication</name><value>3</value></property>
<property><name>dfs.namenode.datanode.registration.ip-hostname-check</name><value>false</value></property>
<property><name>dfs.permissions.enabled</name><value>false</value></property>
</configuration>
XML
 cat > "$dir/conf/hbase-site.xml" <<XML
<configuration>
<property><name>hbase.cluster.distributed</name><value>true</value></property>
<property><name>hbase.rootdir</name><value>hdfs://10.77.0.4:9000/hbase</value></property>
<property><name>hbase.zookeeper.quorum</name><value>10.77.0.1,10.77.0.2,10.77.0.3</value></property>
<property><name>hbase.master.hostname</name><value>10.77.0.8</value></property>
<property><name>hbase.regionserver.hostname</name><value>10.77.0.$index</value></property>
<property><name>hbase.master.info.port</name><value>-1</value></property>
<property><name>hbase.regionserver.info.port</name><value>-1</value></property>
<property><name>hbase.tmp.dir</name><value>$dir/tmp</value></property>
<property><name>hbase.wal.provider</name><value>filesystem</value></property>
<property><name>hbase.regionserver.hlog.enabled</name><value>true</value></property>
<property><name>hbase.unsafe.stream.capability.enforce</name><value>true</value></property>
<property><name>hbase.netty.nativetransport</name><value>false</value></property>
</configuration>
XML
 cp /opt/hbase/conf/log4j2.properties "$dir/conf/"
done
/opt/harmony/hbase-node.sh 4 namenode -format -force -nonInteractive
printf 'HBASE_FORMATTED '; cat /proc/uptime
