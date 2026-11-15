"""Recover a transient ADB disconnect before any E2E UI action runs."""

import json
from pathlib import Path
import subprocess
import sys
import time


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
                and "device offline" in actions[0]["metadata"]["error"]["message"])
    except (OSError, ValueError, KeyError, TypeError):
        return False


def wait_for_device(adb):
    deadline = time.monotonic() + 60
    subprocess.run([*adb, "wait-for-device"], check=True, timeout=60)
    while time.monotonic() < deadline:
        try:
            boot = subprocess.run([*adb, "shell", "getprop", "sys.boot_completed"],
                                  capture_output=True, text=True, check=True, timeout=5)
            packages = subprocess.run([*adb, "shell", "pm", "path", "android"],
                                      capture_output=True, text=True, check=True, timeout=5)
            if boot.stdout.strip() == "1" and packages.stdout.startswith("package:"):
                return
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
            pass
        time.sleep(1)
    raise TimeoutError("Android did not become ready after the startup ADB disconnect")


if __name__ == "__main__":
    if sys.argv[1] == "check":
        raise SystemExit(0 if is_startup_disconnect(sys.argv[2]) else 1)
    if sys.argv[1] == "wait":
        wait_for_device(sys.argv[2:])
