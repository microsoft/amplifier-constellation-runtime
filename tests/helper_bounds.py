"""Actual local helper commands; no agent home, provider or model connection."""
import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("RUNTIME_BINARY", ROOT / "target/debug/agent-runtime"))
if os.name == "nt" and not BIN.suffix:
    BIN = BIN.with_suffix(".exe")


class HelperBounds(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)

    def call(self, command, value, *, success=True, watch=False):
        path = self.folder / "input.json"
        path.write_text(json.dumps(value))
        args = [str(BIN), command]
        if watch:
            args += ["--store", str(self.folder / "watches.json")]
        result = subprocess.run(args + [str(path)], capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode == 0, success, result.stderr)
        return json.loads(result.stdout) if success else result.stderr

    def test_static_discovery_needs_no_home(self):
        result = subprocess.run([str(BIN), "helper-capabilities"], capture_output=True, text=True, timeout=10, check=True)
        capability = json.loads(result.stdout)
        self.assertEqual(capability["schema_version"], 1)
        for helper in ["watch", "continuation"]:
            self.assertEqual(capability["helpers"][helper]["version"], 2)
            self.assertEqual(capability["helpers"][helper]["command"], helper + "-v2")
            self.assertEqual(capability["helpers"][helper]["bound_kinds"], ["finite", "no_configured_bound"])

    def test_continuation_versions_do_not_guess_missing_limits(self):
        value = json.loads((ROOT / "examples/helpers/continuation-v2.json").read_text())
        self.assertEqual(self.call("continuation-v2", value)["action"], "continue")
        self.call("continuation", value, success=False)
        for field in ["remaining_ms", "remaining_turns"]:
            for invalid in [None, 0, {}, {"kind": "unknown"}]:
                changed = copy.deepcopy(value)
                changed[field] = invalid
                self.call("continuation-v2", changed, success=False)
            changed = copy.deepcopy(value)
            del changed[field]
            self.call("continuation-v2", changed, success=False)
        value["remaining_ms"] = 200000
        value["remaining_turns"] = 2
        self.assertEqual(self.call("continuation", value)["policy"], "bounded-continuation/1")
        self.call("continuation-v2", value, success=False)

    def test_watch_roundtrip_keeps_store_and_legacy_views(self):
        value = json.loads((ROOT / "examples/helpers/watch-v2.json").read_text())
        created = self.call("watch-v2", value, watch=True)["watch"]
        self.assertEqual(created["expires_ms"] - created["created_ms"], 300000)
        store = self.folder / "watches.json"
        before = store.read_bytes()
        for invalid in [None, 123, {}, {"kind": "no_configured_bound", "value": 1}]:
            changed = copy.deepcopy(value)
            changed["task"]["deadline_ms"] = invalid
            self.call("watch-v2", changed, watch=True, success=False)
            self.assertEqual(store.read_bytes(), before)
        value["action"] = {"operation": "list"}
        self.assertEqual(self.call("watch-v2", value, watch=True)["watches"][0], created)
        value["task"]["deadline_ms"] = created["expires_ms"] + 60000
        value["task"]["remaining_turns"] = 4
        legacy = self.call("watch", value, watch=True)
        self.assertEqual(legacy["watches"][0], created)
        self.assertEqual(legacy["calls_this_hour"], 0)


if __name__ == "__main__":
    unittest.main()
