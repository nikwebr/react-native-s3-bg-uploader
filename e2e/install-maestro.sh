#!/usr/bin/env bash
set -euo pipefail
export MAESTRO_CLI_NO_ANALYTICS=1
export MAESTRO_CLI_ANALYSIS_NOTIFICATION_DISABLED=true
cd "$(dirname "$0")/.."
version=cli-2.12.0
directory="$PWD/e2e/.tools"
mkdir -p "$directory"
curl --fail --location --silent --show-error \
  "https://github.com/mobile-dev-inc/Maestro/releases/download/$version/maestro.zip" \
  -o "$directory/maestro.zip"
curl --fail --location --silent --show-error \
  "https://github.com/mobile-dev-inc/Maestro/releases/download/$version/checksums_sha256.txt" \
  -o "$directory/checksums_sha256.txt"
(
  cd "$directory"
  grep 'maestro.zip$' checksums_sha256.txt | shasum -a 256 -c -
  unzip -q -o maestro.zip
)
"$directory/maestro/bin/maestro" --version
if [[ -n "${GITHUB_PATH:-}" ]]; then
  echo "$directory/maestro/bin" >> "$GITHUB_PATH"
fi
