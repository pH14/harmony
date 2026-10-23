# SPDX-License-Identifier: AGPL-3.0-or-later
FROM docker.io/library/eclipse-temurin:21-jdk-jammy@sha256:6adefddd4a20bceef702cedb0b03952fcd7691f9c7ccffe27014992abc0b46bd
RUN mkdir -p /build/hbase-assembly/target
COPY downloads/hbase-3.0.0-bin.tar.gz /build/hbase-assembly/target/hbase-3.0.0-bin.tar.gz
RUN echo '8290bc3cece084d36f7f47aae726268c7332e4828c6e8599df1343b03348e93f91165fad03ac6107bfbf0d8535e72024fa5d19217890d2848d013d8418a584ed  /build/hbase-assembly/target/hbase-3.0.0-bin.tar.gz' | sha512sum -c -
