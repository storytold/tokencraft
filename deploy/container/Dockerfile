FROM rust:1.96.1-slim-trixie AS rust-toolchain

FROM postgres:17-trixie AS postgres-runtime
ENV PATH="/usr/lib/postgresql/17/bin:${PATH}"

FROM postgres-runtime AS build
ENV RUSTUP_HOME=/usr/local/rustup \
    RUSTUP_TOOLCHAIN=1.96.1 \
    CARGO_HOME=/tmp/tokencraft-cargo \
    CARGO_TARGET_DIR=/tmp/tokencraft-target
COPY --from=rust-toolchain /usr/local/rustup/ /usr/local/rustup/
COPY --from=rust-toolchain /usr/local/cargo/bin/ /usr/local/bin/
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl gcc jq libc6-dev openssl python3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY --chown=postgres:postgres . .
USER postgres
ARG SOURCE_REVISION
RUN python3 scripts/native_package.py --output /tmp/packages ${SOURCE_REVISION:+--source-revision="$SOURCE_REVISION"} \
    && mkdir /tmp/tokencraft-package \
    && tar -xzf /tmp/packages/tokencraft-*.tar.gz -C /tmp/tokencraft-package --strip-components=1

FROM debian:trixie-slim AS gateway
LABEL org.opencontainers.image.title="TokenCraft gateway" \
      org.opencontainers.image.licenses="AGPL-3.0-only"
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /opt/tokencraft
COPY --from=build /tmp/tokencraft-package/LICENSE /tmp/tokencraft-package/SECURITY.md /tmp/tokencraft-package/VERSION.json ./
COPY --from=build /tmp/tokencraft-package/catalog/ ./catalog/
COPY --from=build /tmp/tokencraft-package/contracts/ ./contracts/
COPY --from=build /tmp/tokencraft-package/deploy/postgres/ ./deploy/postgres/
COPY --from=build /tmp/tokencraft-package/bin/api-gateway /tmp/tokencraft-package/bin/admin-cli /usr/local/bin/
USER 65532:65532
EXPOSE 4000
ENTRYPOINT ["api-gateway"]

FROM postgres-runtime AS demo
LABEL org.opencontainers.image.title="TokenCraft local demo" \
      org.opencontainers.image.licenses="AGPL-3.0-only"
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl jq openssl socat \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -o postgres -g postgres -m 0700 /var/lib/postgresql/data/tokencraft
WORKDIR /opt/tokencraft
COPY --from=build /tmp/tokencraft-package/ ./
ENV PATH="/opt/tokencraft/bin:${PATH}"
COPY --chmod=0755 deploy/container/run-demo.sh /usr/local/bin/tokencraft-container-demo
USER postgres
EXPOSE 14000
STOPSIGNAL SIGTERM
ENTRYPOINT ["tokencraft-container-demo"]
