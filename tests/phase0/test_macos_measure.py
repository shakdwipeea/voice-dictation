import importlib.util
import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "services" / "asr" / "phase0_macos_measure.py"


def load_module():
    spec = importlib.util.spec_from_file_location("phase0_macos_measure", MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


class MacosMeasureHelperTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.measure = load_module()

    def test_word_error_rate_normalizes_case_and_punctuation(self):
        wer = self.measure.word_error_rate("Hello, how are you?", "hello how are you")
        self.assertEqual(wer, 0.0)

    def test_word_error_rate_handles_substitution(self):
        wer = self.measure.word_error_rate("hello there", "hello bro")
        self.assertEqual(wer, 0.5)

    def test_percentile_uses_sorted_values(self):
        self.assertEqual(self.measure.percentile([2.0, 1.0, 9.0], 0.5), 2.0)


if __name__ == "__main__":
    unittest.main()
