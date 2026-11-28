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
  local accept=$1
  shift
  curl -q --silent --show-error --connect-timeout 5 --max-time 45 \
    --proto '=http,https' --compressed --header "Accept: $accept" "$@"
}

check() {
  local origin=$1 target=$2 route=$3 expected=$4 stamp=$5
  shift 5
  if ! request application/json --fail --output "$probe_body" --dump-header "$probe_headers" -- "$origin/$route" ||
    ! "$@" <"$probe_body" >/dev/null; then
    printf 'FAIL %s /%s must %s\n' "$target" "$route" "$expected" >&2
    return 1
  fi
  if [[ -n "$stamp" ]] && ! check_headers 200 "$stamp" "" <"$probe_headers"; then
    printf 'FAIL %s /%s must report the expected gateway source revision\n' "$target" "$route" >&2
    return 1
  fi
  printf 'PASS %s /%s\n' "$target" "$route"
}

matches_contract() {
  jq -es 'length == 2 and .[0] == .[1]' "$root/contracts/openapi.json" -
}

probe_work=$(mktemp -d "${TMPDIR:-/tmp}/tokencraft-hosted-smoke.XXXXXXXX")
trap 'rm -rf -- "$probe_work"' EXIT
probe_body="$probe_work/body"
probe_headers="$probe_work/headers"

check_rate_limit_route() {
  local origin=$1 target=$2 status
  local path='/v1/rate-limits?account_id=hosted-smoke'
  if ! status=$(request application/json --output "$probe_body" --dump-header "$probe_headers" --write-out '%{http_code}' -- "$origin$path") ||
    [[ "$status" != 401 ]] ||
    ! jq -es 'length == 1 and .[0].error.code == "unauthorized"' "$probe_body" >/dev/null; then
    printf 'FAIL %s /v1/rate-limits expected a gateway 401, got HTTP %s\n' \
      "$target" "${status:-unavailable}" >&2
    return 1
  fi
  if [[ -n "$revision" ]] && ! check_headers 401 "$revision" private <"$probe_headers"; then
    printf 'FAIL %s /v1/rate-limits must return a fresh, private denial from the expected source revision\n' "$target" >&2
    return 1
  fi
  printf 'PASS %s /v1/rate-limits\n' "$target"
}

check_headers() {
  awk -v expected_status="$1" -v expected_revision="$2" -v expected_policy="$3" -v expected_type="${4:-}" '
    function header_value(line) {
      sub(/^[^:]+:[ \t]*/, "", line)
      sub(/[ \t\r]+$/, "", line)
      return line
    }
    /^HTTP\// {
      status = ($2 + 0 == expected_status)
      policies = private = no_store = stamps = matching_stamp = ages = fresh_age = 0
      content_types = matching_type = 0
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
    tolower($0) ~ /^content-type:/ {
      content_types++
      value = tolower(header_value($0))
      sub(/[ \t]*;.*/, "", value)
      matching_type = (value == expected_type)
    }
    END {
      valid = status && (expected_revision == "" || (stamps == 1 && matching_stamp))
      if (expected_policy != "") {
        valid = valid && policies == 1 && no_store && (ages == 0 || (ages == 1 && fresh_age))
      }
      if (expected_policy == "private") valid = valid && private
      if (expected_type != "") valid = valid && content_types == 1 && matching_type
      exit !valid
    }
  '
}

check_release_ready() {
  local origin=$1 target=$2
  if ! request application/json --fail --output "$probe_body" --dump-header - -- "$origin/ready" |
    check_headers 200 "$revision" no-store ||
    ! jq -es 'length == 1 and .[0].ready == true' "$probe_body" >/dev/null; then
    printf 'FAIL %s /ready must return fresh, non-storable readiness for the expected source revision\n' "$target" >&2
    return 1
  fi
  printf 'PASS %s /ready and gateway source revision\n' "$target"
}

check_session_cache_policy() {
  local origin=$1 target=$2
  if ! request application/json --output /dev/null --dump-header - -- "$origin/v1/auth/me" |
    check_headers 401 "$revision" private; then
    printf 'FAIL %s /v1/auth/me must return a fresh, private, non-storable 401 from the expected source revision\n' "$target" >&2
    return 1
  fi
  printf 'PASS %s session cache policy\n' "$target"
}

check_console_release() {
  local expected_digest observed_digest
  if ! request application/json --fail --output "$probe_body" --dump-header "$probe_headers" -- "$console/release.json" ||
    ! check_headers 200 "" no-store application/json <"$probe_headers" ||
    ! expected_digest=$(jq -ers --arg revision "$revision" '
      select(length == 1) | .[0]
      | select(.source_revision == $revision and .launcher_api != true)
      | .index_sha256 | strings | select(length == 64 and test("^[a-f0-9]+$"))
    ' <"$probe_body" 2>/dev/null); then
    printf 'FAIL console /release.json must return a fresh hosted manifest for the expected source revision\n' >&2
    return 1
  fi
  printf 'PASS console /release.json\n'

  if ! request text/html --fail --output "$probe_body" --dump-header "$probe_headers" -- "$console/" ||
    ! check_headers 200 "" "" text/html <"$probe_headers" ||
    ! observed_digest=$(openssl dgst -sha256 -r <"$probe_body") ||
    [[ "${observed_digest%% *}" != "$expected_digest" ]]; then
    printf 'FAIL console / must match the release HTML in /release.json\n' >&2
    return 1
  fi
  printf 'PASS console / and release HTML digest\n'
}

result=0
if [[ -n "$revision" ]]; then
  check_release_ready "$console" console || result=1
  check_release_ready "$gateway" gateway || result=1
else
  check "$console" console ready 'report the gateway ready' "" jq -es 'length == 1 and .[0].ready == true' || result=1
  check "$gateway" gateway ready 'report ready' "" jq -es 'length == 1 and .[0].ready == true' || result=1
fi
check "$gateway" gateway openapi.json 'match the release OpenAPI contract' "$revision" matches_contract || result=1
check "$console" console openapi.json 'serve the release OpenAPI contract' "" matches_contract || result=1
check "$console" console docs/openapi.json 'serve the release OpenAPI contract' "" matches_contract || result=1
check "$gateway" gateway v1/schemas/primitives 'match the release connector primitive registry' "$revision" \
  cmp -s "$primitives" - || result=1
check "$console" console v1/schemas/primitives 'proxy the release connector primitive registry' "$revision" \
  cmp -s "$primitives" - || result=1
check "$console" console catalog/artcraft-v1.json 'match the release ArtCraft catalog bytes' "" \
  cmp -s "$root/catalog/artcraft-v1.json" - || result=1
check_rate_limit_route "$gateway" gateway || result=1
check_rate_limit_route "$console" console || result=1
if [[ -n "$revision" ]]; then
  check_session_cache_policy "$gateway" gateway || result=1
  check_session_cache_policy "$console" console || result=1
  check_console_release || result=1
fi
exit "$result"
