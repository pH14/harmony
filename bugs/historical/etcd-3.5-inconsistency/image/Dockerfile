# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG GO_IMAGE=golang:1.24-bookworm@sha256:1a6d4452c65dea36aac2e2d606b01b4a029ec90cc1ae53890540ce6173ea77ac

FROM ${GO_IMAGE} AS instrumented-build

ARG ETCD_VERSION=3.5.2
ARG ETCD_SOURCE_SHA256=ecb4d2bc76e48ae504d9a00b50bcc906503c4d0324acf9dee79d97d16f7282ad
ARG ANTITHESIS_SDK_VERSION=0.8.0
ARG ETCD_SOURCE_BASE=https://github.com/etcd-io/etcd/archive/refs/tags

SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl gcc libc6-dev make \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
RUN curl --fail --silent --show-error --location --retry 3 \
       --output etcd-source.tar.gz \
       "${ETCD_SOURCE_BASE}/v${ETCD_VERSION}.tar.gz" \
    && echo "${ETCD_SOURCE_SHA256}  etcd-source.tar.gz" | sha256sum --check --strict - \
    && mkdir etcd \
    && tar -xf etcd-source.tar.gz -C etcd --strip-components=1 \
    && rm etcd-source.tar.gz

# This is the actual Antithesis source transformer. It creates customer/ (the
# rewritten source), symbols/ (the generated .sym.tsv), and notifier/ (the
# generated coverage callback module). The pinned source tree remains intact.
RUN go install "github.com/antithesishq/antithesis-sdk-go/tools/antithesis-go-instrumentor@v${ANTITHESIS_SDK_VERSION}" \
    && mkdir instrumented \
    && antithesis-go-instrumentor /build/etcd /build/instrumented

# etcd is a multi-module repository. Build the two binaries used by the image
# from the transformed modules; -mod=mod records the generated notifier and
# SDK dependency without hand-editing the upstream source.
RUN mkdir -p /out/etcd \
    && (cd /build/instrumented/customer/server \
        && CGO_ENABLED=1 GOOS=linux GOARCH=amd64 \
           go build -mod=mod -trimpath -buildvcs=false -o /out/etcd/etcd .) \
    && (cd /build/instrumented/customer/etcdctl \
        && CGO_ENABLED=1 GOOS=linux GOARCH=amd64 \
           go build -mod=mod -trimpath -buildvcs=false -o /out/etcd/etcdctl .) \
    && test -x /out/etcd/etcd \
    && test -x /out/etcd/etcdctl \
    && cp /build/instrumented/symbols/*.sym.tsv /out/

COPY consonance/harmony-linux/libvoidstar /build/libvoidstar
RUN make -C /build/libvoidstar BUILD_DIR=/build/libvoidstar-build all \
    && cp /build/libvoidstar-build/libvoidstar.so /out/

FROM ${DEBIAN_IMAGE}

ARG ETCD_VERSION=3.5.2

SHELL ["/bin/sh", "-euxc"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=instrumented-build /out/etcd/etcd /opt/etcd/etcd
COPY --from=instrumented-build /out/etcd/etcdctl /opt/etcd/etcdctl
COPY --from=instrumented-build /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=instrumented-build /out/*.sym.tsv /symbols/

RUN chmod 0755 /opt/etcd/etcd /opt/etcd/etcdctl /usr/lib/libvoidstar.so \
    && test -x /opt/etcd/etcd \
    && test -x /opt/etcd/etcdctl \
    && test -s /symbols/*.sym.tsv

LABEL org.opencontainers.image.title="harmony etcd consistent-index inconsistency workload"
LABEL org.opencontainers.image.version="${ETCD_VERSION}"
LABEL org.opencontainers.image.description="upstream etcd source built through the Antithesis Go instrumentation pipeline"

COPY bugs/historical/etcd-3.5-inconsistency/image/bundle /etc/harmony/bundle
COPY bugs/historical/etcd-3.5-inconsistency/image/setup.sh \
     bugs/historical/etcd-3.5-inconsistency/image/node.sh \
     bugs/historical/etcd-3.5-inconsistency/image/ready.sh \
     bugs/historical/etcd-3.5-inconsistency/image/hooks.sh /opt/harmony/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh \
        /opt/harmony/ready.sh /opt/harmony/hooks.sh

CMD ["/opt/harmony/node.sh"]
