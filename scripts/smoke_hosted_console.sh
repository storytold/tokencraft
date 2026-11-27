#!/usr/bin/env bash
set -euo pipefail

if (( $# < 2 || $# > 3 )); then
  echo "Usage: bash scripts/smoke_hosted_console.sh CONSOLE_ORIGIN GATEWAY_ORIGIN [SOURCE_REVISION]" >&2
  exit 2
fi
console=${1%/}
gateway=${2%/}
revision=${3:-}
if (( $# == 3 )) && [[ ! "$revision" =~ ^[0-9a-f]{40}$ ]]; then
  echo "Use a full lowercase Git SHA for SOURCE_REVISION" >&2
  exit 2
fi
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
primitives="$root/contracts/connector-primitives.json"
if [[ ! -f "$primitives" ]]; then
  primitives="$root/crates/core/provider-mapping/schemas/v1.json"
fi

request() {
  curl -q --silent --show-error --connect-timeout 5 --max-time 45 \
    --proto '=http,https' --header 'Accept: application/json' "$@"
}

check() {
  local origin=$1 target=$2 route=$3 expected=$4
  shift 4
  if ! request --fail -- "$origin/$route" | "$@" >/dev/null; then
    printf 'FAIL %s /%s must %s\n' "$target" "$route" "$expected" >&2
    return 1
  fi
  printf 'PASS %s /%s\n' "$target" "$route"
}

matches_contract() {
  jq -es 'length == 2 and .[0] == .[1]' "$root/contracts/openapi.json" -
}

probe_body=$(mktemp)
trap 'rm -f -- "$probe_body"' EXIT

check_rate_limit_route() {
  local origin=$1 target=$2 status
  local path='/v1/rate-limits?account_id=hosted-smoke'
  if ! status=$(request --output "$probe_body" --write-out '%{http_code}' -- "$origin$path") ||
    [[ "$status" != 401 ]] ||
    ! jq -es 'length == 1 and .[0].error.code == "unauthorized"' "$probe_body" >/dev/null; then
    printf 'FAIL %s /v1/rate-limits expected a gateway 401, got HTTP %s\n' \
      "$target" "${status:-unavailable}" >&2
    return 1
  fi
  printf 'PASS %s /v1/rate-limits\n' "$target"
}

check_no_store_headers() {
  awk -v expected_status="$1" -v expected_revision="$2" '
    function header_value(line) {
      sub(/^[^:]+:[ \t]*/, "", line)
      sub(/[ \t\r]+$/, "", line)
      return line
    }
    /^HTTP\// {
      status = ($2 + 0 == expected_status)
      policies = private = no_store = stamps = matching_stamp = ages = fresh_age = 0
    }
    tolower($0) ~ /^cache-control:/ {
      policies++
      count = split(tolower(header_value($0)), directives, ",")
      for (i = 1; i <= count; i++) {
        gsub(/^[ \t]+|[ \t]+$/, "", directives[i])
        if (directives[i] == "private") private = 1
        if (directives[i] == "no-store") no_store = 1
      }
    }
    tolower($0) ~ /^tokencraft-source-revision:/ {
      stamps++
      matching_stamp = ("sha:" header_value($0) == "sha:" expected_revision)
    }
    tolower($0) ~ /^age:/ {
      ages++
      fresh_age = (header_value($0) ~ /^0+$/)
    }
    END {
      valid = status && policies == 1 && no_store
      if (expected_revision == "") {
        valid = valid && private
      } else {
        valid = valid && stamps == 1 && matching_stamp && (ages == 0 || (ages == 1 && fresh_age))
      }
      exit !valid
    }
  '
}

check_release_ready() {
  local origin=$1 target=$2
  if ! request --fail --output "$probe_body" --dump-header - -- "$origin/ready" |
    check_no_store_headers 200 "$revision" ||
    ! jq -es 'length == 1 and .[0].ready == true' "$probe_body" >/dev/null; then
    printf 'FAIL %s /ready must return fresh, non-storable readiness for the expected source revision\n' "$target" >&2
    return 1
  fi
  printf 'PASS %s /ready and gateway source revision\n' "$target"
}

check_session_cache_policy() {
  local origin=$1 target=$2
  if ! request --output /dev/null --dump-header - -- "$origin/v1/auth/me" |
    check_no_store_headers 401 ""; then
    printf 'FAIL %s /v1/auth/me must return a private, non-storable 401\n' "$target" >&2
    return 1
  fi
  printf 'PASS %s session cache policy\n' "$target"
}

result=0
if [[ -n "$revision" ]]; then
  check_release_ready "$console" console || result=1
  check_release_ready "$gateway" gateway || result=1
else
  check "$console" console ready 'report the gateway ready' jq -es 'length == 1 and .[0].ready == true' || result=1
  check "$gateway" gateway ready 'report ready' jq -es 'length == 1 and .[0].ready == true' || result=1
fi
check "$gateway" gateway openapi.json 'match the release OpenAPI contract' matches_contract || result=1
check "$console" console openapi.json 'serve the release OpenAPI contract' matches_contract || result=1
check "$console" console docs/openapi.json 'serve the release OpenAPI contract' matches_contract || result=1
check "$gateway" gateway v1/schemas/primitives 'match the release connector primitive registry' \
  cmp -s "$primitives" - || result=1
check "$console" console v1/schemas/primitives 'proxy the release connector primitive registry' \
  cmp -s "$primitives" - || result=1
check "$console" console catalog/artcraft-v1.json 'match the release ArtCraft catalog bytes' \
  cmp -s "$root/catalog/artcraft-v1.json" - || result=1
check_rate_limit_route "$gateway" gateway || result=1
check_rate_limit_route "$console" console || result=1
if [[ -n "$revision" ]]; then
  check_session_cache_policy "$gateway" gateway || result=1
  check_session_cache_policy "$console" console || result=1
  check "$console" console release.json 'report the expected console source revision' \
    jq -es --arg revision "$revision" 'length == 1 and .[0].source_revision == $revision' || result=1
fi
exit "$result"
