#!/usr/bin/env bash
set -euo pipefail

: "${RUNNER_TEMP:?GitHub Actions must provide RUNNER_TEMP}"
: "${GITHUB_PATH:?GitHub Actions must provide GITHUB_PATH}"

key=/usr/share/keyrings/pgdg.asc
curl --fail --silent --show-error --retry 3 --max-time 30 \
  https://www.postgresql.org/media/keys/ACCC4CF8.asc --output "$RUNNER_TEMP/pgdg.asc"
fingerprints=$(gpg --batch --show-keys --with-colons "$RUNNER_TEMP/pgdg.asc" |
  awk -F: '$1 == "pub" { primary = 1 } $1 == "fpr" && primary { print $10; primary = 0 }')
if [[ "$fingerprints" != B97B0AFCAA1A47F044F244A07FCC7D46ACCC4CF8 ]]; then
  echo 'PostgreSQL APT signing key fingerprint did not match' >&2
  exit 1
fi
sudo install -m 0644 "$RUNNER_TEMP/pgdg.asc" "$key"
printf 'deb [signed-by=%s] https://apt.postgresql.org/pub/repos/apt noble-pgdg main\n' "$key" |
  sudo tee /etc/apt/sources.list.d/pgdg.list >/dev/null
sudo apt-get update
sudo apt-get install -y postgresql-17 libpq-dev jq
printf '%s\n' /usr/lib/postgresql/17/bin >> "$GITHUB_PATH"
