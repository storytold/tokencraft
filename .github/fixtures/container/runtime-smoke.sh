#!/usr/bin/env bash
set -euo pipefail
umask 077

api=${1:-http://localhost:4000}
worker=${2:-http://localhost:4001}
api_peer=${3:-http://localhost:4002}
: "${TOKENCRAFT_RUNTIME_APP_PASSWORD:?Set the fixture runtime password}"
: "${TOKENCRAFT_CAPACITY_TOKEN:?Set the fixture capacity token}"
scratch=$(mktemp -d)
cookie=$scratch/cookie
trap 'rm -rf -- "$scratch"' EXIT

http() {
    curl --silent --show-error --noproxy '*' --connect-timeout 2 --max-time 8 "$@"
}

ready() {
    local origin="$1" attempt
    for ((attempt=0; attempt<60; attempt++)); do
        if http --fail "$origin/ready" 2>/dev/null | jq -e '.ready == true' >/dev/null; then
            return
        fi
        sleep 0.5
    done
    echo "Readiness did not pass on $origin" >&2
    return 1
}

ready "$api"
ready "$worker"
ready "$api_peer"
process_id() {
    local origin=$1 role=$2 worker_status=$3
    http --fail -H "Authorization: Bearer $TOKENCRAFT_CAPACITY_TOKEN" "$origin/internal/capacity" |
        jq -er --arg role "$role" --arg worker_status "$worker_status" '
            select(.role == $role and .ready and .checks.migrations == "ready" and .checks.worker == $worker_status)
            | .instance_id | select(type == "string" and test("^[0-9a-f]{32}$"))'
}
api_id=$(process_id "$api" api external)
peer_id=$(process_id "$api_peer" api external)
worker_id=$(process_id "$worker" worker ready)
test "$api_id" != "$peer_id"
test "$api_id" != "$worker_id"
test "$peer_id" != "$worker_id"
echo "Two API replicas and a separate worker reported distinct ready process identities."

http --fail "$api/v1/catalog/models" |
    jq -e '[.models[].id] == ["runtime_image"]' >/dev/null
test "$(http --output /dev/null --write-out '%{http_code}' "$worker/v1/catalog/models")" = 404

signup=$(jq -nc --arg email "runtime-$(openssl rand -hex 8)@example.test" \
    --arg password "$(openssl rand -hex 24)" \
    '{email:$email,password:$password,display_name:"Gateway image smoke"}')
http --fail --cookie-jar "$cookie" -H 'Content-Type: application/json' \
    -d "$signup" "$api/v1/auth/signup" >/dev/null
account=$(http --fail --cookie "$cookie" "$api/v1/accounts" | jq -er '.accounts[0].account_id')

project=$(http --fail --cookie "$cookie" -H 'Content-Type: application/json' \
    -d '{"name":"Gateway image"}' "$api/v1/projects?account_id=$account" | jq -er '.id')
token=$(http --fail --cookie "$cookie" -H 'Content-Type: application/json' \
    -d '{"name":"Runtime probe","client_ref":"container_runtime","scopes":["requests:submit","requests:read","usage:read"]}' \
    "$api/v1/projects/$project/service-principals?account_id=$account" | jq -er '.token')

# Give this fixture account one credit and postpone refill so timing cannot affect the check.
PGPASSWORD="$TOKENCRAFT_RUNTIME_APP_PASSWORD" psql -Xqw -h 127.0.0.1 \
    -p "${TOKENCRAFT_RUNTIME_DB_PORT:-5432}" -U tokencraft_runtime -d tokencraft \
    -v ON_ERROR_STOP=1 -v account="$account" <<'SQL'
INSERT INTO gateway_submission_limits (account_id, credit, refilled_at)
VALUES (:'account', 60000000, clock_timestamp() + interval '5 minutes')
ON CONFLICT (account_id) DO UPDATE SET credit = excluded.credit, refilled_at = excluded.refilled_at;
SQL

key=$(openssl rand -hex 16)
input='{"model":"runtime_image","workload":{"kind":"image","count":1,"aspect_ratio":"4:3"}}'
quote=$(http --fail -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
    -d "$input" "$api/v1/quotes/scoped")
payload=$(jq -ec 'select(.id != null and .input != null) | {quote_id:.id,input}' <<< "$quote")
submit() {
    local origin=$1 idempotency_key=$2
    shift 2
    http "$@" -H "Authorization: Bearer $token" -H "Idempotency-Key: $idempotency_key" \
        -H 'Content-Type: application/json' -d "$payload" "$origin/v1/requests"
}

request_id=$(submit "$api" "$key" --fail | jq -er '.id | select(test("^req_[A-Za-z0-9]+$"))')
test "$(submit "$api_peer" "$key" --fail | jq -er '.id')" = "$request_id"
status=$(submit "$api_peer" "$(openssl rand -hex 16)" \
    --dump-header "$scratch/headers" --output "$scratch/rejection" --write-out '%{http_code}')
test "$status" = 429
jq -e '.error.code == "submission_rate_limit"' "$scratch/rejection" >/dev/null
test "$(awk 'tolower($1) == "retry-after:" { sub(/\r$/, "", $2); print $2 }' "$scratch/headers")" = 1
echo "The second API replayed the receipt and enforced the shared organization quota."

state=
for ((attempt=0; attempt<60; attempt++)); do
    state=$(http --fail -H "Authorization: Bearer $token" "$api_peer/v1/requests/$request_id" | jq -er '.status')
    case "$state" in
        succeeded) break ;;
        failed|canceled|awaiting_reconciliation) break ;;
    esac
    sleep 0.5
done
if [[ "$state" != succeeded ]]; then
    echo "Gateway image request ended as $state" >&2
    exit 1
fi
http --fail -H "Authorization: Bearer $token" "$api_peer/v1/usage" |
    jq -e '.totals | .requests == 1 and .succeeded == 1 and .completed_outputs == 1
        and .estimated_usd_nanos == "1250000" and .mock_reported_usd_nanos == "1250000"' >/dev/null
echo "The API and separate worker completed one request and recorded the expected usage."
