#!/usr/bin/env bash
set -euo pipefail
umask 077

if [[ -r /etc/secrets/tokencraft.env ]]; then
    set -a
    source /etc/secrets/tokencraft.env
    set +a
fi

: "${DATABASE_URL:?Set the Render Postgres external URL with sslmode=verify-full}"
: "${APP_BASE_URL:?Set the public HTTPS origin for the console}"
: "${SESSION_SECRET:?Set an independent 32-byte hex secret}"
: "${TOKENCRAFT_DEMO_PAYLOAD_KEY:?Set an independent 32-byte hex secret}"
: "${TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE:?Set an independent bearer token}"

export FOUNDATION_ENV=staging COOKIE_SECURE=true TOKENCRAFT_LISTEN_IP=0.0.0.0
export API_GATEWAY_PORT="${PORT:-10000}"
bundle_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
export TOKENCRAFT_CORE_CATALOG="$bundle_dir/catalog.json"
export TOKENCRAFT_CONNECTORS="$bundle_dir/bindings.json"

case "${TOKENCRAFT_RENDER_MIGRATE_ON_START:-false}" in
    true) admin-cli initialize-demo ;;
    false) ;;
    *) echo "Set TOKENCRAFT_RENDER_MIGRATE_ON_START to true or false" >&2; exit 2 ;;
esac

connector=
gateway=
cleanup() {
    local child
    for child in "$gateway" "$connector"; do
        if [[ -n "$child" ]]; then
            kill "$child" 2>/dev/null || true
        fi
    done
    wait || true
    rm -rf -- "$state"
}

state="$(mktemp -d)"
trap cleanup EXIT
trap 'exit 0' TERM INT
(
    # The synthetic connector only needs its own service token.
    while IFS= read -r name; do
        case "$name" in
            PATH|TOKENCRAFT_CONNECTOR_TOKEN_EXAMPLE) ;;
            *) export -n "$name" ;;
        esac
    done < <(compgen -e)
    exec tokencraft example-connector "$state"
) &
connector=$!
api-gateway &
gateway=$!
wait -n "$gateway" "$connector"
