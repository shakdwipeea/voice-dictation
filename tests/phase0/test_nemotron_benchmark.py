import importlib.util
import json
import sys
import tempfile
import unittest
import unittest.mock
import wave
from pathlib import Path


MODULE_PATH = (
    Path(__file__).parents[2] / "services" / "asr" / "nemotron_benchmark.py"
)
SPEC = importlib.util.spec_from_file_location("nemotron_benchmark", MODULE_PATH)
benchmark = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = benchmark
SPEC.loader.exec_module(benchmark)


class NemotronBenchmarkTest(unittest.TestCase):
    def test_parse_profiles_deduplicates_in_order(self):
        self.assertEqual(benchmark.parse_profiles(["160", "80", "160"]), ["160", "80"])

    def test_parse_profiles_rejects_unknown_profile(self):
        with self.assertRaisesRegex(ValueError, "Unsupported profile"):
            benchmark.parse_profiles(["40"])

    def test_module_available_handles_missing_parent_package(self):
        self.assertFalse(
            benchmark.module_available("sunoto_missing_package.nested_module")
        )

    def test_preflight_reports_missing_runtime_without_crashing(self):
        with unittest.mock.patch.object(benchmark, "module_available", return_value=False):
            with unittest.mock.patch.object(
                benchmark,
                "command_output",
                return_value={"available": False, "returncode": None},
            ):
                result = benchmark.build_preflight()
        self.assertFalse(result["ready"])
        self.assertIn("Python module is missing: torch", result["blockers"])

    def test_preflight_warns_about_mismatched_cuda_bindings(self):
        with unittest.mock.patch.object(benchmark, "module_available", return_value=True):
            with unittest.mock.patch.object(
                benchmark, "package_version", return_value="13.3.1"
            ):
                with unittest.mock.patch.object(
                    benchmark, "command_output", return_value={"returncode": 0}
                ):
                    torch = unittest.mock.MagicMock()
                    torch.__version__ = "2.7.1+cu128"
                    torch.version.cuda = "12.8"
                    torch.cuda.is_available.return_value = False
                    with unittest.mock.patch.dict(sys.modules, {"torch": torch}):
                        result = benchmark.build_preflight()

        self.assertIn("optimized CUDA graphs may fall back", result["warnings"][0])

    def test_inspect_wav_accepts_required_format(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "valid.wav"
            with wave.open(str(path), "wb") as wav:
                wav.setnchannels(1)
                wav.setsampwidth(2)
                wav.setframerate(16_000)
                wav.writeframes(b"\x00\x00" * 16_000)

            metadata = benchmark.inspect_wav(path)

            self.assertEqual(metadata.channels, 1)
            self.assertEqual(metadata.sample_rate_hz, 16_000)
            self.assertEqual(metadata.duration_seconds, 1.0)

    def test_inspect_wav_rejects_stereo(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "stereo.wav"
            with wave.open(str(path), "wb") as wav:
                wav.setnchannels(2)
                wav.setsampwidth(2)
                wav.setframerate(16_000)
                wav.writeframes(b"\x00\x00\x00\x00" * 100)

            with self.assertRaisesRegex(ValueError, "expected mono"):
                benchmark.inspect_wav(path)

    def test_create_manifest_omits_missing_reference(self):
        audio = benchmark.AudioMetadata("sample.wav", 1.0, 16_000, 1, 2, 16_000)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.jsonl"
            benchmark.create_manifest(path, [audio])

            record = json.loads(path.read_text(encoding="utf-8"))

        self.assertNotIn("text", record)

    def test_parse_streaming_metrics_extracts_transcription_and_timing(self):
        stdout = "\n".join(
            (
                '[NeMo I] Final streaming transcriptions: ["hello world"]',
                "[NeMo I] The whole streaming process took: 0.25s",
            )
        )

        self.assertEqual(
            benchmark.parse_streaming_metrics(stdout),
            {
                "transcriptions": ["hello world"],
                "streaming_process_seconds": 0.25,
            },
        )

    def test_model_argument_uses_pretrained_name_for_repository_id(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "infer.py"
            script.touch()
            manifest = Path(directory) / "manifest.jsonl"
            manifest.touch()
            with unittest.mock.patch.object(benchmark.subprocess, "run") as run:
                run.return_value.returncode = 0
                run.return_value.stdout = ""
                run.return_value.stderr = ""
                benchmark.run_profile(
                    script,
                    manifest,
                    Path(directory),
                    "160",
                    "python",
                    benchmark.MODEL_NAME,
                )
            self.assertIn(
                f"pretrained_name={benchmark.MODEL_NAME}", run.call_args.args[0]
            )


if __name__ == "__main__":
    unittest.main()
