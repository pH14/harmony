# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
FROM --platform=linux/amd64 ${DEBIAN_IMAGE}

ARG ETCD_VERSION=3.5.2
ARG ETCD_SHA256=256cad725542d6fd463e81b8a19b86ead4cdfe113f7fb8a1eabc6c32c25d068b
ARG ETCD_BASE=https://github.com/etcd-io/etcd/releases/download

SHELL ["/bin/sh", "-euxc"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && curl --fail --silent --show-error --location --retry 3 \
         --output /tmp/etcd.tar.gz \
         "${ETCD_BASE}/v${ETCD_VERSION}/etcd-v${ETCD_VERSION}-linux-amd64.tar.gz" \
    && echo "${ETCD_SHA256}  /tmp/etcd.tar.gz" | sha256sum --check --strict - \
    && mkdir -p /opt/etcd \
    && tar -xf /tmp/etcd.tar.gz -C /opt/etcd --strip-components=1 \
    && rm /tmp/etcd.tar.gz \
    && test -x /opt/etcd/etcd \
    && test -x /opt/etcd/etcdctl

LABEL org.opencontainers.image.title="harmony etcd consistent-index inconsistency workload"
LABEL org.opencontainers.image.version="${ETCD_VERSION}"

COPY bundle /etc/harmony/bundle
COPY setup.sh node.sh ready.sh hooks.sh /opt/harmony/

RUN chmod 0755 /opt/harmony/setup.sh /opt/harmony/node.sh \
        /opt/harmony/ready.sh /opt/harmony/hooks.sh

CMD ["/opt/harmony/node.sh"]
