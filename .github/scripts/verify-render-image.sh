#!/usr/bin/env bash
set -euo pipefail

revision=${1:?Pass the full Git commit for the Render image}
if [[ ! "$revision" =~ ^[a-f0-9]{40}$ ]]; then
  echo "Pass the full lowercase Git commit for the Render image." >&2
  exit 2
fi

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
package_version=${2:-}
if [[ -z "$package_version" ]]; then
  package_version=$(python3 -c '
import sys
import tomllib

with open(sys.argv[1], "rb") as manifest:
    print(tomllib.load(manifest)["package"]["version"])
' "$root/crates/bin/tokencraft/Cargo.toml")
fi

docker build --file "$root/Dockerfile" --build-arg "RENDER_GIT_COMMIT=$revision" --tag tokencraft-render:ci "$root"
for binary in tokencraft api-gateway admin-cli; do
  docker run --rm --network none --entrypoint "$binary" tokencraft-render:ci --version \
    | diff -u <(printf '%s %s (%s)\n' "$binary" "$package_version" "$revision") -
done
