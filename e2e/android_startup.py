"""Recover a transient ADB transport failure before any E2E UI action runs."""

import json
from pathlib import Path
import re
import subprocess
import sys
import time

# A dropped transport is reported either as an offline device or as a device
# that adb cannot find. Both mean the next Maestro command would fail before
# the flow had done anything.
_TRANSPORT_ERROR = re.compile(
    r"(device offline|device .*not found|no devices/emulators found)",
    re.IGNORECASE,
)


def _is_transport_error(message):
    return bool(_TRANSPORT_ERROR.search(message or ""))


def is_startup_disconnect(artifacts):
    reports = list(Path(artifacts).rglob("commands.json"))
    if not reports:
        return False
    try:
        entries = json.loads(max(reports, key=lambda path: path.stat().st_mtime).read_text())
        actions = [entry for entry in entries
                   if entry["metadata"]["status"] in ("COMPLETED", "FAILED")
                   and not set(entry["command"]) <= {
                       "defineVariablesCommand", "applyConfigurationCommand",
                   }]
        return (len(actions) == 1
                and set(actions[0]["command"]) == {"launchAppCommand"}
                and actions[0]["metadata"]["status"] == "FAILED"
                and _is_transport_error(actions[0]["metadata"]["error"]["message"]))
    except (OSError, ValueError, KeyError, TypeError):
        return False


def _adb(adb, *args, timeout=10):
    return subprocess.run([*adb, *args], capture_output=True, text=True, timeout=timeout)


def _is_ready(adb):
    try:
        boot = _adb(adb, "shell", "getprop", "sys.boot_completed")
        packages = _adb(adb, "shell", "pm", "path", "android")
    except (subprocess.SubprocessError, OSError):
        return False
    return (boot.returncode == 0 and boot.stdout.strip() == "1"
            and packages.returncode == 0 and packages.stdout.startswith("package:"))


def wait_for_device(adb):
    # `adb wait-for-device` can return while the adb server still holds a stale,
    # offline transport. Ask it to drop that transport first so it reconnects.
    try:
        _adb(adb, "reconnect", "offline", timeout=30)
    except (subprocess.SubprocessError, OSError):
        pass
    subprocess.run([*adb, "wait-for-device"], check=True, timeout=60)
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if _is_ready(adb):
            # A transport that just came back can drop again immediately.
            time.sleep(2)
            if _is_ready(adb):
                return
        time.sleep(1)
    subprocess.run([*adb, "devices"], check=False)
    raise TimeoutError("Android did not become ready after the startup ADB disconnect")


if __name__ == "__main__":
    if sys.argv[1] == "check":
        raise SystemExit(0 if is_startup_disconnect(sys.argv[2]) else 1)
    if sys.argv[1] == "wait":
        wait_for_device(sys.argv[2:])
