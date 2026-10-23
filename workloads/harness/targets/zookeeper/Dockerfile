# SPDX-License-Identifier: AGPL-3.0-or-later
ARG BOOT_JDK_IMAGE=docker.io/library/eclipse-temurin:21-jdk-jammy@sha256:6adefddd4a20bceef702cedb0b03952fcd7691f9c7ccffe27014992abc0b46bd
ARG JAVA_IMAGE=harmony-language-java:local
ARG GO_IMAGE=docker.io/library/golang:1.27-bookworm@sha256:5cf287a799e6b94384bad13d16b14904c531f51ba65792237e122ce42b392f61
FROM ${BOOT_JDK_IMAGE} AS distribution
ARG ZOOKEEPER_VERSION=3.9.4
ARG ZOOKEEPER_SHA512=36bffae6440ed0d71ed83a621b8c52c583860b414812197373237f0c148bd16e6b599977c90e5eb81c0fce6b82ef44aa782621535417cffc4c2a0a51a56f2cdf
RUN curl --fail --silent --show-error --location --retry 3 \
      "https://archive.apache.org/dist/zookeeper/zookeeper-${ZOOKEEPER_VERSION}/apache-zookeeper-${ZOOKEEPER_VERSION}-bin.tar.gz" -o /tmp/zk.tar.gz \
    && echo "${ZOOKEEPER_SHA512}  /tmp/zk.tar.gz" | sha512sum --check --strict \
    && mkdir /opt/zookeeper \
    && tar -xzf /tmp/zk.tar.gz -C /opt/zookeeper --strip-components=1 \
    && rm /tmp/zk.tar.gz \
    && jdeps --ignore-missing-deps --multi-release 21 --print-module-deps \
         --class-path '/opt/zookeeper/lib/*' /opt/zookeeper/lib/zookeeper-${ZOOKEEPER_VERSION}.jar > /opt/zookeeper/modules.txt
FROM ${GO_IMAGE} AS harness-build
ARG ANTITHESIS_SDK_VERSION=0.8.0
ENV CGO_ENABLED=1 GOFLAGS=-trimpath
RUN apt-get update && apt-get install --yes --no-install-recommends gcc libc6-dev patch \
    && rm -rf /var/lib/apt/lists/*
RUN go install "github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-toolexec@v${ANTITHESIS_SDK_VERSION}"
COPY workloads/bugs/historical/etcd-3.5-inconsistency/image/patches/antithesis-sdk-go-v0.8.0-linux-arm64.patch /build/arm64.patch
COPY workloads/languages/go/antithesis-go-toolexec-v0.8.0-stdlib.patch /build/stdlib.patch
COPY workloads/languages/go/configure-instrumentation.sh /build/configure-instrumentation.sh
RUN sdk_cache_dir="$(go env GOMODCACHE)/github.com/antithesishq/antithesis-sdk-go@v${ANTITHESIS_SDK_VERSION}" \
    && mkdir -p /build/sdk && cp -a "${sdk_cache_dir}/." /build/sdk/ && chmod -R u+w /build/sdk \
    && patch --batch --forward --fuzz=0 -d /build/sdk -p1 < /build/arm64.patch \
    && patch --batch --forward --fuzz=0 -d /build/sdk -p1 < /build/stdlib.patch \
    && (cd /build/sdk && gofmt -w tools/antithesis-go-toolexec/config.go tools/antithesis-go-toolexec/policy.go tools/antithesis-go-toolexec/harmony_std_test.go \
        && go test ./tools/antithesis-go-toolexec && go install ./tools/antithesis-go-toolexec)
COPY workloads/harness/kit /build/kit
COPY workloads/harness/targets/zookeeper/go.mod workloads/harness/targets/zookeeper/go.sum workloads/harness/targets/zookeeper/main.go /build/targets/zookeeper/
SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]
WORKDIR /build/targets/zookeeper
RUN go mod download && go vet -mod=readonly ./... && go test -mod=readonly ./... \
    && mkdir -p /out/symbols /out/stdlib/harness /out/stdlib/net \
    && bash /build/configure-instrumentation.sh /build/sdk /out/stdlib/harness . > /out/stdlib/harness/instrumentation.env \
    && source /out/stdlib/harness/instrumentation.env \
    && ANTITHESIS_SDK_MODULE_DIR=/build/sdk ANTITHESIS_SYMBOLS_DIR=/out/symbols ANTITHESIS_SYMBOL_PREFIX=zookeeper-harness \
       go build -toolexec=antithesis-go-toolexec -buildvcs=false -o /out/zookeeper-harness . \
    && cp /out/zookeeper-harness /out/symbols/zookeeper-harness \
    && sha256sum /out/zookeeper-harness | awk '{print $1 "  /opt/harmony/zookeeper-harness"}' > /out/attestation
WORKDIR /build/kit
RUN go test ./... \
    && bash /build/configure-instrumentation.sh /build/sdk /out/stdlib/net ./cmd/harness-net > /out/stdlib/net/instrumentation.env \
    && source /out/stdlib/net/instrumentation.env \
    && ANTITHESIS_SDK_MODULE_DIR=/build/sdk ANTITHESIS_SYMBOLS_DIR=/out/symbols ANTITHESIS_SYMBOL_PREFIX=harness-net \
       go build -toolexec=antithesis-go-toolexec -buildvcs=false -o /out/harness-net ./cmd/harness-net \
    && cp /out/harness-net /out/symbols/harness-net \
    && sha256sum /out/harness-net | awk '{print $1 "  /opt/harmony/harness-net"}' >> /out/attestation
FROM ${JAVA_IMAGE}
COPY --from=distribution /opt/zookeeper/lib /opt/zookeeper/lib
COPY --from=distribution /opt/zookeeper/LICENSE.txt /licenses/zookeeper.txt
COPY --from=distribution /opt/zookeeper/modules.txt /opt/zookeeper/modules.txt
COPY workloads/harness/targets/zookeeper/logback.xml /opt/zookeeper/conf/logback.xml
COPY workloads/harness/targets/zookeeper/standalone.sh /opt/harmony/zookeeper-standalone
CMD ["/bin/sh", "/opt/harmony/zookeeper-standalone"]
RUN apt-get update && apt-get install --yes --no-install-recommends iproute2 iptables \
    && rm -rf /var/lib/apt/lists/* \
    && dpkg --purge --force-depends libssl3 \
    && for dir in /usr/lib/*/xtables; do \
        ls "$dir" | tr A-Z a-z | sort | uniq -d | while read -r name; do \
            find "$dir" -maxdepth 1 -iname "$name" -delete; \
        done; \
    done
COPY --from=harness-build /out/zookeeper-harness /out/harness-net /opt/harmony/
COPY --from=harness-build /out/symbols /symbols/go
COPY --from=harness-build /out/stdlib /symbols/stdlib
COPY --from=harness-build /out/attestation /tmp/harness-attestation
RUN cat /tmp/harness-attestation >> /symbols/harmony-instrumented-events && rm /tmp/harness-attestation
COPY workloads/harness/targets/zookeeper/setup.sh workloads/harness/targets/zookeeper/node.sh workloads/harness/targets/zookeeper/measure.sh workloads/harness/targets/zookeeper/measure-cluster.sh /opt/harmony/
RUN chmod +x /opt/harmony/setup.sh /opt/harmony/node.sh /opt/harmony/measure.sh /opt/harmony/measure-cluster.sh
ARG HARNESS_BUNDLE=bundle
COPY workloads/harness/targets/zookeeper/${HARNESS_BUNDLE} /etc/harmony/bundle
