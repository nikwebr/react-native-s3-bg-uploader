#!/usr/bin/env bash
set -euo pipefail
export MAESTRO_CLI_NO_ANALYTICS=1
export MAESTRO_CLI_ANALYSIS_NOTIFICATION_DISABLED=true
cd "$(dirname "$0")/.."
platform="${1:?Usage: bash e2e/run.sh ios|android|web}"
case "$platform" in ios|android|web) ;; *) echo "Unknown platform: $platform" >&2; exit 1 ;; esac
python="$PWD/e2e/.venv/bin/python"
maestro="${MAESTRO_BIN:-$PWD/e2e/.tools/maestro/bin/maestro}"
artifacts="$PWD/e2e/artifacts/$platform"
mkdir -p "$artifacts"
# Never attach tests to, or stop, somebody else's server.
"$python" - <<'PY'
import socket
for port in (8765, 9000, 8080):
    with socket.socket() as sock:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(("127.0.0.1", port))
PY
backend_pid=""
app_pid=""
cleanup() {
  local result=$?
  for pid in "$app_pid" "$backend_pid"; do
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid"
      wait "$pid" || true
    fi
  done
  trap - EXIT
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
"$python" -u e2e/server.py --artifacts "$artifacts" > "$artifacts/backend.log" 2>&1 &
backend_pid=$!
wait_for() {
  "$python" - "$1" <<'PY'
import sys, time
from urllib.request import urlopen
from urllib.error import URLError
deadline = time.monotonic() + 120
while True:
    try:
        with urlopen(sys.argv[1], timeout=2) as response:
            if response.status == 200:
                break
    except (URLError, TimeoutError):
        if time.monotonic() > deadline:
            raise SystemExit(f"Server did not become ready: {sys.argv[1]}")
    time.sleep(0.5)
PY
}
wait_for http://127.0.0.1:8765/health
fixture_path=""
app_id=com.s3bguploaderexample
maestro_args=("$maestro")
test_args=(test)
if [[ "$platform" == web ]]; then
  test_args+=(--headless)
  (
    exec "$python" -m http.server 8080 --bind 127.0.0.1 --directory example/web-build
  ) > "$artifacts/app.log" 2>&1 &
  app_pid=$!
  app_id=http://127.0.0.1:8080
  wait_for http://127.0.0.1:8080
else
  (
    cd example
    exec env E2E=1 node ../node_modules/react-native/cli.js start --reset-cache --port 8080 --no-interactive
  ) > "$artifacts/app.log" 2>&1 &
  app_pid=$!
  wait_for http://127.0.0.1:8080/status
  if [[ "$platform" == android ]]; then
    adb_args=(adb)
    if [[ -n "${E2E_DEVICE:-}" ]]; then
      adb_args+=(-s "$E2E_DEVICE")
      maestro_args+=(--device "$E2E_DEVICE")
    fi
    "${adb_args[@]}" install -r "${E2E_APP:-example/android/app/build/outputs/apk/debug/app-debug.apk}"
    "${adb_args[@]}" shell am force-stop "$app_id"
    # Only the dedicated example app's state is reset; no device-wide clear.
    "${adb_args[@]}" shell pm clear "$app_id"
    "${adb_args[@]}" reverse tcp:8081 tcp:8080
    "${adb_args[@]}" reverse tcp:8765 tcp:8765
    "${adb_args[@]}" shell run-as "$app_id" mkdir -p files shared_prefs
    # Emulator defaults use 10.0.2.2, which bypasses ADB reverse.
    "${adb_args[@]}" shell run-as "$app_id" sh -c "'cat > shared_prefs/${app_id}_preferences.xml'" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<map>
  <string name="debug_http_host">127.0.0.1:8081</string>
</map>
XML
    "${adb_args[@]}" shell run-as "$app_id" sh -c "'cat > files/fixture.bin'" < "$artifacts/fixture.bin"
    fixture_path="/data/user/0/$app_id/files/fixture.bin"
  else
    device="${E2E_DEVICE:-booted}"
    if [[ "$device" != booted ]]; then maestro_args+=(--device "$device"); fi
    xcrun simctl install "$device" "${E2E_APP:-example/ios/build/Build/Products/Debug-iphonesimulator/S3BgUploaderExample.app}"
    container="$(xcrun simctl get_app_container "$device" "$app_id" data)"
    mkdir -p "$container/Documents"
    cp "$artifacts/fixture.bin" "$container/Documents/fixture.bin"
    fixture_path="$container/Documents/fixture.bin"
  fi
fi
"${maestro_args[@]}" \
  "${test_args[@]}" \
  --format junit --output "$artifacts/junit.xml" --test-output-dir "$artifacts/maestro" \
  -e "APP_ID=$app_id" -e "FIXTURE_PATH=$fixture_path" \
  "e2e/maestro/$platform.yaml"
curl --fail --silent --show-error -X POST -H 'Content-Type: application/json' \
  -d '{}' http://127.0.0.1:8765/assert > "$artifacts/final-assertions.json"
