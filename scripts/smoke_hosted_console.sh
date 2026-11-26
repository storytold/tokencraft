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

check_gateway_revision() {
  local origin=$1 target=$2
  if ! request --fail --output /dev/null --dump-header - -- "$origin/ready" |
    awk -v expected="$revision" '
      tolower($1) == "tokencraft-source-revision:" {
        sub(/\r$/, "", $2)
        found++
        if ($2 == expected) matched++
      }
      END { exit !(found == 1 && matched == 1) }
    '; then
    printf 'FAIL %s /ready must report the expected gateway source revision\n' "$target" >&2
    return 1
  fi
  printf 'PASS %s gateway source revision\n' "$target"
}

check_session_cache_policy() {
  local origin=$1 target=$2
  if ! request --output /dev/null --dump-header - -- "$origin/v1/auth/me" |
    awk '
      /^HTTP\// { denied = ($2 == 401); policies = 0; private = 0; no_store = 0 }
      tolower($1) == "cache-control:" {
        policies++
        line = tolower($0)
        sub(/^cache-control:/, "", line)
        gsub(/[ \t\r]/, "", line)
        count = split(line, directive, ",")
        for (i = 1; i <= count; i++) {
          if (directive[i] == "private") private = 1
          if (directive[i] == "no-store") no_store = 1
        }
      }
      END { exit !(denied && policies == 1 && private && no_store) }
    '; then
    printf 'FAIL %s /v1/auth/me must return a private, non-storable 401\n' "$target" >&2
    return 1
  fi
  printf 'PASS %s session cache policy\n' "$target"
}

result=0
check "$console" console ready 'report the gateway ready' jq -es 'length == 1 and .[0].ready == true' || result=1
check "$gateway" gateway ready 'report ready' jq -es 'length == 1 and .[0].ready == true' || result=1
check "$gateway" gateway openapi.json 'match the release OpenAPI contract' matches_contract || result=1
check "$console" console openapi.json 'serve the release OpenAPI contract' matches_contract || result=1
check "$console" console docs/openapi.json 'serve the release OpenAPI contract' matches_contract || result=1
check "$console" console catalog/artcraft-v1.json 'match the release ArtCraft catalog bytes' \
  cmp -s "$root/catalog/artcraft-v1.json" - || result=1
check_rate_limit_route "$gateway" gateway || result=1
check_rate_limit_route "$console" console || result=1
if [[ -n "$revision" ]]; then
  check_gateway_revision "$gateway" gateway || result=1
  check_gateway_revision "$console" console || result=1
  check_session_cache_policy "$gateway" gateway || result=1
  check_session_cache_policy "$console" console || result=1
  check "$console" console release.json 'report the expected console source revision' \
    jq -es --arg revision "$revision" 'length == 1 and .[0].source_revision == $revision' || result=1
fi
exit "$result"
