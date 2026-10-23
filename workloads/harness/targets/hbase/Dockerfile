# SPDX-License-Identifier: AGPL-3.0-or-later
ARG HADOOP_IMAGE=harmony-hadoop-build:local
ARG HBASE_BUILD_IMAGE=harmony-hbase-build:local
ARG ZOOKEEPER_IMAGE=harmony-zookeeper:hbase
ARG GO_IMAGE=docker.io/library/golang:1.27-bookworm@sha256:5cf287a799e6b94384bad13d16b14904c531f51ba65792237e122ce42b392f61
FROM ${HADOOP_IMAGE} AS hadoop
FROM ${HBASE_BUILD_IMAGE} AS distribution
ARG HBASE_VERSION=4.0.0-alpha-1-SNAPSHOT
RUN mkdir /opt/hbase && tar -xzf /build/hbase-assembly/target/hbase-${HBASE_VERSION}-bin.tar.gz -C /opt/hbase --strip-components=1
COPY --from=hadoop /opt/hadoop/share/hadoop/common /opt/hadoop/share/hadoop/common
COPY --from=hadoop /opt/hadoop/share/hadoop/hdfs /opt/hadoop/share/hadoop/hdfs
RUN apt-get update && apt-get install -y --no-install-recommends python3 && rm -rf /var/lib/apt/lists/*
COPY workloads/harness/targets/hbase/strip-native.py /tmp/strip-native.py
RUN python3 /tmp/strip-native.py /opt/hbase /opt/hadoop > /opt/native-libraries-removed.txt \
    && jdeps --ignore-missing-deps --multi-release 21 --print-module-deps \
        --class-path '/opt/hbase/lib/client-facing-thirdparty/*:/opt/hbase/lib/*:/opt/hadoop/share/hadoop/common/*:/opt/hadoop/share/hadoop/common/lib/*:/opt/hadoop/share/hadoop/hdfs/*:/opt/hadoop/share/hadoop/hdfs/lib/*' \
        /opt/hbase/lib/hbase-server-${HBASE_VERSION}.jar > /opt/modules.txt
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
COPY workloads/harness/targets/hbase/go.mod workloads/harness/targets/hbase/go.sum workloads/harness/targets/hbase/main.go /build/targets/hbase/
SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]
WORKDIR /build/targets/hbase
COPY workloads/harness/targets/hbase/gohbase-single-mutation.patch /build/gohbase-single-mutation.patch
COPY workloads/harness/targets/hbase/gohbase-time-range.patch /build/gohbase-time-range.patch
RUN go mod download \
    && client_dir="$(go list -m -f '{{.Dir}}' github.com/tsuna/gohbase)" \
    && cp -a "$client_dir" /build/gohbase && chmod -R u+w /build/gohbase \
    && patch --batch --forward --fuzz=0 -d /build/gohbase -p1 < /build/gohbase-single-mutation.patch \
    && patch --batch --forward --fuzz=0 -d /build/gohbase -p1 < /build/gohbase-time-range.patch \
    && go mod edit -replace github.com/tsuna/gohbase=/build/gohbase
COPY workloads/harness/targets/hbase/gohbase-single-mutation_test.go.in /build/gohbase/harmony_single_mutation_test.go
COPY workloads/harness/targets/hbase/gohbase-time-range_test.go.in /build/gohbase/hrpc/harmony_time_range_test.go
RUN (cd /build/gohbase && go test -run '^TestHarmonyMutationOneAttempt$' . && go test ./hrpc)
RUN go mod download && go vet -mod=readonly ./... && go test -mod=readonly ./... \
    && mkdir -p /out/symbols /out/stdlib/harness /out/stdlib/net \
    && bash /build/configure-instrumentation.sh /build/sdk /out/stdlib/harness . > /out/stdlib/harness/instrumentation.env \
    && source /out/stdlib/harness/instrumentation.env \
    && ANTITHESIS_SDK_MODULE_DIR=/build/sdk ANTITHESIS_SYMBOLS_DIR=/out/symbols ANTITHESIS_SYMBOL_PREFIX=hbase-harness \
       go build -toolexec=antithesis-go-toolexec -buildvcs=false -o /out/hbase-harness . \
    && cp /out/hbase-harness /out/symbols/hbase-harness \
    && sha256sum /out/hbase-harness | awk '{print $1 "  /opt/harmony/hbase-harness"}' > /out/attestation
FROM ${ZOOKEEPER_IMAGE}
COPY --from=distribution /opt/hbase/lib /opt/hbase/lib
COPY --from=distribution /opt/hbase/conf /opt/hbase/conf
COPY --from=distribution /opt/hadoop /opt/hadoop
COPY --from=distribution /opt/native-libraries-removed.txt /opt/modules.txt /opt/hbase/
COPY --from=distribution /opt/hbase/LICENSE.txt /licenses/hbase.txt
COPY --from=harness-build /out/hbase-harness /opt/harmony/
COPY --from=harness-build /out/symbols /symbols/hbase-go
COPY --from=harness-build /out/stdlib /symbols/hbase-stdlib
COPY --from=harness-build /out/attestation /tmp/hbase-attestation
RUN cat /tmp/hbase-attestation >> /symbols/harmony-instrumented-events && rm /tmp/hbase-attestation
COPY workloads/harness/targets/hbase/setup.sh /opt/harmony/hbase-setup.sh
COPY workloads/harness/targets/hbase/node.sh /opt/harmony/hbase-node.sh
COPY workloads/harness/targets/hbase/bundle /etc/harmony/bundle
COPY workloads/harness/targets/hbase/probe.sh /opt/harmony/hbase-probe.sh
CMD ["/opt/harmony/hbase-probe.sh"]
RUN chmod +x /opt/harmony/hbase-setup.sh /opt/harmony/hbase-node.sh /opt/harmony/hbase-probe.sh
