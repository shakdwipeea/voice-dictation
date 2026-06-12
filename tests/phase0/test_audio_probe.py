import importlib.util
import sys
import tempfile
import unittest
import wave
from pathlib import Path
from unittest import mock


MODULE_PATH = Path(__file__).parents[2] / "tools" / "phase0" / "audio_probe.py"
SPEC = importlib.util.spec_from_file_location("audio_probe", MODULE_PATH)
audio_probe = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = audio_probe
SPEC.loader.exec_module(audio_probe)


class FakeProcess:
    returncode = -2

    def send_signal(self, signal):
        return None

    def communicate(self, timeout):
        return b"", b""


class AudioProbeTest(unittest.TestCase):
    def test_capture_validates_generated_wav(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "probe.wav"

            def fake_popen(*args, **kwargs):
                with wave.open(str(path), "wb") as wav:
                    wav.setnchannels(1)
                    wav.setsampwidth(2)
                    wav.setframerate(16_000)
                    wav.writeframes(b"\x10\x00" * 160)
                return FakeProcess()

            with mock.patch.object(audio_probe.subprocess, "Popen", fake_popen):
                with mock.patch.object(audio_probe.time, "sleep"):
                    result = audio_probe.capture(path, 0.01, "@DEFAULT_SOURCE@")

            self.assertTrue(result["valid_for_nemotron"])
            self.assertTrue(result["non_silent"])
            self.assertEqual(result["device"], "@DEFAULT_SOURCE@")


if __name__ == "__main__":
    unittest.main()
