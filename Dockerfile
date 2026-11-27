FROM rust:1.96-trixie AS build
RUN apt-get update && apt-get install -y --no-install-recommends postgresql-17 libpq-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates crates
COPY sdk/rust sdk/rust
COPY examples examples
ENV CARGO_PROFILE_RELEASE_STRIP=symbols
ARG RENDER_GIT_COMMIT
RUN set -eu; \
    if [ -n "${RENDER_GIT_COMMIT:-}" ]; then \
        case "$RENDER_GIT_COMMIT" in \
            *[!0123456789abcdef]*) echo "RENDER_GIT_COMMIT must be a full lowercase Git SHA" >&2; exit 2 ;; \
        esac; \
        if [ "${#RENDER_GIT_COMMIT}" -ne 40 ]; then \
            echo "RENDER_GIT_COMMIT must be a full lowercase Git SHA" >&2; exit 2; \
        fi; \
        export TOKENCRAFT_SOURCE_REVISION="$RENDER_GIT_COMMIT"; \
    fi; \
    export PATH="/usr/lib/postgresql/17/bin:$PATH"; \
    pgdata="$(mktemp -d)"; \
    chown postgres:postgres "$pgdata"; \
    trap 'runuser -u postgres -- pg_ctl -D "$pgdata" -m fast -w stop || true; rm -rf "$pgdata"' EXIT; \
    runuser -u postgres -- initdb -D "$pgdata" --auth=trust --encoding=UTF8 --no-locale; \
    runuser -u postgres -- pg_ctl -D "$pgdata" -o "-h 127.0.0.1 -p 55439 -c unix_socket_directories=" -w start; \
    export DATABASE_URL=postgresql://postgres@127.0.0.1:55439/postgres SQLX_OFFLINE=false; \
    cargo build --locked --release -p admin_cli --bin admin-cli; \
    target/release/admin-cli migrate; \
    cargo build --locked --release --bin api-gateway --bin tokencraft

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3t64 libpq5 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 tokencraft
COPY --from=build /src/target/release/admin-cli /src/target/release/api-gateway /src/target/release/tokencraft /usr/local/bin/
COPY examples/connector/catalog.json examples/connector/bindings.json /opt/tokencraft/
COPY scripts/serve-render-demo.sh /opt/tokencraft/
USER tokencraft
CMD ["/bin/bash", "/opt/tokencraft/serve-render-demo.sh"]
