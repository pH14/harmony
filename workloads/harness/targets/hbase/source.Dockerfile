# SPDX-License-Identifier: AGPL-3.0-or-later
FROM docker.io/library/eclipse-temurin:21-jdk-jammy@sha256:6adefddd4a20bceef702cedb0b03952fcd7691f9c7ccffe27014992abc0b46bd
RUN apt-get update && apt-get install -y --no-install-recommends maven git && rm -rf /var/lib/apt/lists/*
COPY downloads/hbase-source.tar.gz /tmp/hbase.tar.gz
RUN echo 'a3232735eb6b2b35d1a4c03b488a7ab8bb70a09ac8a2ee12dca68286a3e58a3e  /tmp/hbase.tar.gz' | sha256sum -c - && mkdir /build && tar -xzf /tmp/hbase.tar.gz -C /build --strip-components=1
WORKDIR /build
ENV MAVEN_OPTS=-Xmx4g
RUN mvn -B -T 8 -DskipTests -Dcheckstyle.skip -Dspotbugs.skip -Drat.skip=true -Dhadoop.profile=3.0 -Dhadoop-three.version=3.4.3 -Prelease,hadoop-3.0 package assembly:single
