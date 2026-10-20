# SPDX-License-Identifier: AGPL-3.0-or-later
ARG JDK_IMAGE=docker.io/library/eclipse-temurin:21-jdk@sha256:23cc5734ff46e776f51ce66eb9ee08444fd47caabec0a95b7eb6e94be2d3696d
FROM ${JDK_IMAGE}

ENV DEBIAN_FRONTEND=noninteractive LEIN_ROOT=1
SHELL ["/bin/bash", "-euxo", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl git gnuplot-nox graphviz openssh-client tini \
    && rm -rf /var/lib/apt/lists/* \
    && ssh-keygen -q -t ed25519 -N '' -f /root/.ssh/id_ed25519

ARG LEIN_VERSION=2.11.2
ARG LEIN_SHA256=f83e3de252cb09d301f18f1912acffa807b54c1c9a96772ae63847a946032bd4
RUN curl --fail --silent --show-error --location --output /usr/local/bin/lein \
       "https://raw.githubusercontent.com/technomancy/leiningen/${LEIN_VERSION}/bin/lein" \
    && echo "${LEIN_SHA256}  /usr/local/bin/lein" | sha256sum --check --strict - \
    && chmod 0755 /usr/local/bin/lein \
    && lein version

ARG JEPSEN_NATS_COMMIT=4760f97f86350c5c9983478656dbcbcdade33817
RUN git clone https://github.com/jepsen-io/nats.git /jepsen/nats \
    && git -C /jepsen/nats checkout --detach "${JEPSEN_NATS_COMMIT}"

WORKDIR /jepsen/nats
COPY workloads/harness/jepsen/nats.patch /jepsen/nats.patch
RUN git apply /jepsen/nats.patch \
    && lein uberjar \
    && mv target/jepsen.nats-*-standalone.jar /jepsen/jepsen-nats.jar \
    && lein deps :tree > /jepsen/deps-tree.txt 2>&1

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["sleep", "infinity"]
