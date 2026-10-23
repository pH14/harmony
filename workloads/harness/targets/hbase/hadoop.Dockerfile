# SPDX-License-Identifier: AGPL-3.0-or-later
FROM docker.io/library/eclipse-temurin:21-jdk-jammy@sha256:6adefddd4a20bceef702cedb0b03952fcd7691f9c7ccffe27014992abc0b46bd
COPY downloads/hadoop-3.4.3.tar.gz /tmp/hadoop.tar.gz
RUN echo 'e25be7e57b4d3c5bfe83895844321a21d6cf7331266d524d8983c27cf484e576c3d79b3b60d590f8cddecf16229d0e232de2491b1b61b362ee7d67072c7290e1  /tmp/hadoop.tar.gz' | sha512sum -c - \
    && mkdir /opt/hadoop && tar -xzf /tmp/hadoop.tar.gz -C /opt/hadoop --strip-components=1
