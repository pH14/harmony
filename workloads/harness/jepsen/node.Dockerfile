# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=docker.io/library/debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
FROM ${DEBIAN_IMAGE}

ARG TARGETARCH
ENV DEBIAN_FRONTEND=noninteractive
SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN printf 'Acquire::Check-Date "false";\nAcquire::Check-Valid-Until "false";\n' >/etc/apt/apt.conf.d/99-clock-skew \
    && apt-get update \
    && apt-get install --yes --no-install-recommends \
       apt-transport-https bzip2 build-essential ca-certificates cmake curl dirmngr dnsutils faketime fuse3 g++ \
       git iproute2 iptables iputils-ping libfuse3-3 libfuse3-dev libzip4 logrotate man-db netcat-openbsd \
       ntpdate openssh-server procps psmisc rsyslog sudo tar tcpdump tini unzip vim wget

RUN git clone https://github.com/dsrhaslab/lazyfs.git /opt/jepsen/lazyfs \
    && cd /opt/jepsen/lazyfs \
    && git checkout 0.3.0 \
    && (cd libs/libpcache && ./build.sh) \
    && (cd lazyfs && ./build.sh)

RUN declare -A sums=( \
        [2.10.20-amd64]=979c6e633fb03987771b8f7baf99041b574638486ead35acdb868f6a7187a164 \
        [2.10.20-arm64]=f12b65d736dcd6c58d1cd39a2378a1c72041b3b2ba158ab9665cac137c4c7be4 \
        [2.10.23-amd64]=a79fd5a82ff9fc02f0a9619b121bbdf28f02002163170abef31c81c19a1d3014 \
        [2.10.23-arm64]=a267a45cb12deb5bbe5c849ae26b2fc020a86109d62fe94538f6b6550cad620a \
        [2.12.1-amd64]=a104163114e292f90233afd8cf2bd745d2f04d53948db987ad018735449a2232 \
        [2.12.1-arm64]=1e31b395fbe6443d54a70cedb3f66d6871525a7c9a9862ebdfeb55df7158ecc7 \
    ) \
    && for version in 2.10.20 2.10.23 2.12.1; do \
        name="nats-server-v${version}-linux-${TARGETARCH}"; \
        curl --fail --silent --show-error --location --retry 3 --output /tmp/nats.tar.gz \
            "https://github.com/nats-io/nats-server/releases/download/v${version}/${name}.tar.gz"; \
        echo "${sums[${version}-${TARGETARCH}]}  /tmp/nats.tar.gz" | sha256sum --check --strict -; \
        mkdir -p "/usr/local/lib/nats-bin/${version}"; \
        tar -xzf /tmp/nats.tar.gz -C "/usr/local/lib/nats-bin/${version}" --strip-components=1 "${name}/nats-server"; \
        rm /tmp/nats.tar.gz; \
    done

RUN declare -A sums=( \
        [amd64]=3b5f0e95de3937c70816bceee3f0c3d3678fb93c500f47b8c66388119e2ef774 \
        [arm64]=92c4fa9234d6ef00c062196f54d1ac8056f66b58d90aa968558ddcf99dee22f5 \
    ) \
    && curl --fail --silent --show-error --location --retry 3 --output /tmp/nats.deb \
       "https://github.com/nats-io/natscli/releases/download/v0.2.2/nats-0.2.2-${TARGETARCH}.deb" \
    && echo "${sums[${TARGETARCH}]}  /tmp/nats.deb" | sha256sum --check --strict - \
    && dpkg -i /tmp/nats.deb \
    && rm /tmp/nats.deb

RUN echo 'root:root' | chpasswd \
    && sed -i 's/^#\?PermitRootLogin .*/PermitRootLogin yes/' /etc/ssh/sshd_config \
    && mkdir -p /run/sshd

EXPOSE 22
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/sbin/sshd", "-D", "-e"]
