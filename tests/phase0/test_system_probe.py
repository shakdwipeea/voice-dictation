import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).parents[2] / "tools" / "phase0" / "system_probe.py"
SPEC = importlib.util.spec_from_file_location("system_probe", MODULE_PATH)
probe = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(probe)


class SystemProbeTest(unittest.TestCase):
    def test_run_captures_success(self):
        result = probe.run(["sh", "-c", "printf phase-zero"])
        self.assertEqual(result["returncode"], 0)
        self.assertEqual(result["stdout"], "phase-zero")

    def test_run_captures_missing_command(self):
        result = probe.run(["sunoto-command-that-does-not-exist"])
        self.assertIsNone(result["returncode"])
        self.assertIn("error", result)


if __name__ == "__main__":
    unittest.main()
