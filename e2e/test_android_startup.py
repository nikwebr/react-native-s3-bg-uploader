"""Only a transport disconnect before the first UI action may retry Maestro."""

import json
from pathlib import Path
import tempfile
import unittest

from android_startup import is_startup_disconnect


def entry(command, status="COMPLETED", error=None):
    metadata = {"status": status}
    if error is not None:
        metadata["error"] = {"message": error}
    return {"command": {command: {}}, "metadata": metadata}


class AndroidStartupTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.report = Path(self.directory.name) / "commands.json"

    def check(self, entries):
        self.report.write_text(json.dumps(entries))
        return is_startup_disconnect(self.directory.name)

    def test_initial_launch_disconnect_can_retry(self):
        for message in (
            "Command failed (host:transport:emulator-5554): device offline",
            "Command failed (host:transport:emulator-5554): device 'emulator-5554' not found",
            "Command failed (host:transport:emulator-5554): no devices/emulators found",
        ):
            with self.subTest(message=message):
                self.assertTrue(self.check([
                    entry("defineVariablesCommand"),
                    entry("applyConfigurationCommand"),
                    entry("launchAppCommand", "FAILED", message),
                ]))

    def test_assertion_and_later_disconnects_cannot_retry(self):
        for failed_command in ("assertConditionCommand", "tapOnElementCommand", "launchAppCommand"):
            with self.subTest(command=failed_command):
                self.assertFalse(self.check([
                    entry("launchAppCommand"),
                    entry(failed_command, "FAILED", "device offline"),
                ]))
        self.assertFalse(self.check([
            entry("tapOnElementCommand"),
            entry("launchAppCommand", "FAILED", "device offline"),
        ]))

    def test_other_launch_failures_and_missing_evidence_cannot_retry(self):
        self.assertFalse(is_startup_disconnect(self.directory.name))
        self.assertFalse(self.check([entry("launchAppCommand", "FAILED", "app crashed")]))
        self.assertFalse(self.check([entry("launchAppCommand", "FAILED",
                                            "Package com.example not found")]))
        self.assertFalse(self.check([entry("launchAppCommand", "COMPLETED", "device offline")]))
        self.assertFalse(self.check([{}]))
        self.report.write_text("truncated JSON")
        self.assertFalse(is_startup_disconnect(self.directory.name))
