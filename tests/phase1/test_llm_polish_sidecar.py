"""Protocol smoke tests for the LLM polish sidecar, no model load required."""

from __future__ import annotations

import io
import json
import os
import sys
import threading
import time
import types
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "services/polish"))

llama_cpp = types.ModuleType("llama_cpp")
llama_cpp.Llama = object
llama_cpp.LlamaRAMCache = object
sys.modules.setdefault("llama_cpp", llama_cpp)

import llm_polish_sidecar  # noqa: E402


class FakeLlama:
    def __init__(self, response_text: str | list[str] | None = None) -> None:
        self.calls: list[dict[str, object]] = []
        if isinstance(response_text, list):
            self.responses = response_text
        elif response_text is None:
            self.responses = None
        else:
            self.responses = [response_text]

    def create_chat_completion(self, **kwargs):
        self.calls.append(kwargs)
        transcript = kwargs["messages"][-1]["content"].splitlines()[-1]
        if self.responses is None:
            response_text = transcript
        else:
            response_text = self.responses[min(len(self.calls) - 1, len(self.responses) - 1)]
        return {
            "choices": [
                {
                    "message": {"content": response_text},
                    "finish_reason": "stop",
                }
            ],
            "usage": {
                "prompt_tokens": 32,
                "completion_tokens": 7,
                "total_tokens": 39,
            },
        }


class LlmPolishWarmupTests(unittest.TestCase):
    def test_llama_runtime_config_defaults_and_env_overrides(self):
        with patch.dict(os.environ, {}, clear=True):
            self.assertEqual(
                llm_polish_sidecar.llama_runtime_config(),
                {
                    "n_gpu_layers": -1,
                    "n_ctx": 2048,
                    "n_batch": 512,
                    "n_ubatch": 512,
                    "n_threads": 8,
                    "flash_attn": True,
                    "seed": 42,
                },
            )

        with patch.dict(
            os.environ,
            {
                "SUNOTO_LLM_POLISH_GPU_LAYERS": "0",
                "SUNOTO_LLM_POLISH_CTX": "1024",
                "SUNOTO_LLM_POLISH_BATCH": "128",
                "SUNOTO_LLM_POLISH_UBATCH": "64",
                "SUNOTO_LLM_POLISH_THREADS": "4",
                "SUNOTO_LLM_POLISH_FLASH_ATTN": "0",
                "SUNOTO_LLM_POLISH_SEED": "7",
            },
            clear=True,
        ):
            self.assertEqual(
                llm_polish_sidecar.llama_runtime_config(),
                {
                    "n_gpu_layers": 0,
                    "n_ctx": 1024,
                    "n_batch": 128,
                    "n_ubatch": 64,
                    "n_threads": 4,
                    "flash_attn": False,
                    "seed": 7,
                },
            )

    def test_warmup_uses_production_completion_path(self):
        llm = FakeLlama()
        texts = [
            "Hey, how are you doing?",
            "Her email is jane, no, janet dot smith at example dot com.",
        ]
        stdout = io.StringIO()

        with (
            patch.dict(
                os.environ,
                {
                    "SUNOTO_LLM_POLISH_MODE": "one_pass_minimal",
                    "SUNOTO_LLM_POLISH_OUTPUT_MODE": "minimal",
                },
            ),
            redirect_stdout(stdout),
            redirect_stderr(io.StringIO()),
        ):
            llm_polish_sidecar.warmup(llm, texts)

        event = json.loads(stdout.getvalue())
        self.assertEqual(event["type"], "warmed")
        self.assertEqual([request["text"] for request in event["requests"]], texts)
        self.assertEqual(len(llm.calls), 2)
        for text, call in zip(texts, llm.calls):
            self.assertEqual(call["max_tokens"], llm_polish_sidecar.dynamic_tokens(text))
            self.assertIn("Clean this transcript:", call["messages"][-1]["content"])
        first = event["requests"][0]
        self.assertEqual(first["finish_reason"], "stop")
        self.assertEqual(first["completion_tokens"], 7)
        self.assertIn("cache_hit", first)
        self.assertGreaterEqual(first["raw_chars"], first["cleaned_chars"])

    def test_minimal_unchanged_output_returns_original_transcript(self):
        with patch.dict(
            os.environ,
            {
                "SUNOTO_LLM_POLISH_MODE": "one_pass_minimal",
                "SUNOTO_LLM_POLISH_OUTPUT_MODE": "minimal",
            },
        ):
            llm = FakeLlama("UNCHANGED")
            stdout = io.StringIO()

            with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                llm_polish_sidecar.polish(llm, 7, "The responses are currently really slow.")

            event = json.loads(stdout.getvalue())
            self.assertEqual(event["type"], "polished")
            self.assertEqual(event["session_id"], 7)
            self.assertEqual(event["text"], "The responses are currently really slow.")
            self.assertEqual(event["raw_output"], "UNCHANGED")
            self.assertEqual(event["output_mode"], "minimal")

    def test_minimal_edited_output_strips_control_prefix(self):
        with patch.dict(
            os.environ,
            {
                "SUNOTO_LLM_POLISH_MODE": "one_pass_minimal",
                "SUNOTO_LLM_POLISH_OUTPUT_MODE": "minimal",
            },
        ):
            llm = FakeLlama("EDITED: Check the logs and tell me how the current model is doing.")
            stdout = io.StringIO()

            with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                llm_polish_sidecar.polish(
                    llm,
                    8,
                    "Check the logs and tell me how our how is the current model doing.",
                )

            event = json.loads(stdout.getvalue())
            self.assertEqual(
                event["text"], "Check the logs and tell me how the current model is doing."
            )
            self.assertEqual(event["output_mode"], "minimal")

    def test_two_step_decision_unchanged_returns_original_transcript(self):
        with patch.dict(os.environ, {"SUNOTO_LLM_POLISH_MODE": "two_step"}):
            llm = FakeLlama("OK")
            stdout = io.StringIO()

            with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                llm_polish_sidecar.polish(llm, 9, "The responses are currently really slow.")

            event = json.loads(stdout.getvalue())
            self.assertEqual(event["text"], "The responses are currently really slow.")
            self.assertEqual(event["output_mode"], "two_step")
            self.assertEqual(event["decision_label"], "UNCHANGED")
            self.assertFalse(event["rewrite_called"])
            self.assertEqual(event["raw_output"], "UNCHANGED")
            self.assertEqual(len(llm.calls), 1)
            self.assertEqual(event["decision"]["completion_tokens"], 7)

    def test_two_step_decision_edit_runs_rewrite(self):
        with patch.dict(os.environ, {"SUNOTO_LLM_POLISH_MODE": "two_step"}):
            llm = FakeLlama(["EDIT", "Please send this to Priya tomorrow."])
            stdout = io.StringIO()

            with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                llm_polish_sidecar.polish(
                    llm,
                    10,
                    "Please send this to Rahul, um, please send this to Priya tomorrow.",
                )

            event = json.loads(stdout.getvalue())
            self.assertEqual(event["text"], "Please send this to Priya tomorrow.")
            self.assertEqual(event["decision_label"], "EDIT")
            self.assertTrue(event["rewrite_called"])
            self.assertFalse(event["decision_malformed"])
            self.assertFalse(event["validation_rejected"])
            self.assertEqual(event["rewrite"]["raw_output"], "Please send this to Priya tomorrow.")
            self.assertEqual(len(llm.calls), 2)

    def test_two_step_malformed_decision_falls_back_to_rewrite(self):
        with patch.dict(os.environ, {"SUNOTO_LLM_POLISH_MODE": "two_step"}):
            llm = FakeLlama(["MAYBE", "Please send this to Priya tomorrow."])
            stdout = io.StringIO()

            with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                llm_polish_sidecar.polish(
                    llm,
                    11,
                    "Please send this to Rahul, um, please send this to Priya tomorrow.",
                )

            event = json.loads(stdout.getvalue())
            self.assertIsNone(event["decision_label"])
            self.assertTrue(event["decision_malformed"])
            self.assertTrue(event["rewrite_called"])
            self.assertEqual(event["text"], "Please send this to Priya tomorrow.")
            self.assertEqual(len(llm.calls), 2)

    def test_two_step_validation_rejection_returns_original_transcript(self):
        with patch.dict(os.environ, {"SUNOTO_LLM_POLISH_MODE": "two_step"}):
            transcript = "Her email is jane dot smith at example dot com."
            llm = FakeLlama(["EDIT", "Her email is jane.smith@example.com."])
            stdout = io.StringIO()

            with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                llm_polish_sidecar.polish(llm, 12, transcript)

            event = json.loads(stdout.getvalue())
            self.assertEqual(event["text"], transcript)
            self.assertTrue(event["rewrite_called"])
            self.assertTrue(event["validation_rejected"])
            self.assertEqual(event["hard_unsafe"], [])
            self.assertIn("formatted_target", event["rewrite"]["hard_unsafe"])

    def test_constrained_one_call_ok_returns_original_transcript(self):
        with patch.dict(os.environ, {"SUNOTO_LLM_POLISH_MODE": "constrained_one_call"}):
            transcript = "The responses are currently really slow."
            llm = FakeLlama("OK")
            stdout = io.StringIO()

            with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                llm_polish_sidecar.polish(llm, 13, transcript)

            event = json.loads(stdout.getvalue())
            self.assertEqual(event["text"], transcript)
            self.assertEqual(event["output_mode"], "constrained_one_call")
            self.assertEqual(event["decision_label"], "UNCHANGED")
            self.assertFalse(event["rewrite_called"])
            self.assertFalse(event["decision_malformed"])
            self.assertEqual(len(llm.calls), 1)

    def test_constrained_one_call_edit_strips_control_prefix(self):
        with patch.dict(os.environ, {"SUNOTO_LLM_POLISH_MODE": "constrained_one_call"}):
            llm = FakeLlama("EDIT: Please open the dashboard.")
            stdout = io.StringIO()

            with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                llm_polish_sidecar.polish(
                    llm,
                    14,
                    "Please open settings, no wait, open the dashboard.",
                )

            event = json.loads(stdout.getvalue())
            self.assertEqual(event["text"], "Please open the dashboard.")
            self.assertEqual(event["decision_label"], "EDIT")
            self.assertTrue(event["rewrite_called"])
            self.assertFalse(event["decision_malformed"])
            self.assertEqual(len(llm.calls), 1)

    def test_constrained_one_call_identical_edit_is_unchanged(self):
        with patch.dict(os.environ, {"SUNOTO_LLM_POLISH_MODE": "constrained_one_call"}):
            transcript = "The dashboard is ready and the response is fast."
            llm = FakeLlama(f"EDIT: {transcript}")
            stdout = io.StringIO()

            with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                llm_polish_sidecar.polish(llm, 15, transcript)

            event = json.loads(stdout.getvalue())
            self.assertEqual(event["text"], transcript)
            self.assertEqual(event["decision_label"], "UNCHANGED")
            self.assertFalse(event["rewrite_called"])
            self.assertFalse(event["validation_rejected"])

    def test_constrained_one_call_content_drop_is_rejected(self):
        with patch.dict(os.environ, {"SUNOTO_LLM_POLISH_MODE": "constrained_one_call"}):
            transcript = "The dashboard is ready and the response is fast."
            llm = FakeLlama("EDIT: The response is fast.")
            stdout = io.StringIO()

            with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                llm_polish_sidecar.polish(llm, 16, transcript)

            event = json.loads(stdout.getvalue())
            self.assertEqual(event["text"], transcript)
            self.assertEqual(event["decision_label"], "EDIT")
            self.assertTrue(event["rewrite_called"])
            self.assertTrue(event["validation_rejected"])
            self.assertEqual(event["hard_unsafe"], [])

    def test_constrained_one_call_meaningful_marker_drop_is_rejected(self):
        with patch.dict(os.environ, {"SUNOTO_LLM_POLISH_MODE": "constrained_one_call"}):
            transcript = "Wait, are you sure?"
            llm = FakeLlama("EDIT: Are you sure?")
            stdout = io.StringIO()

            with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                llm_polish_sidecar.polish(llm, 17, transcript)

            event = json.loads(stdout.getvalue())
            self.assertEqual(event["text"], transcript)
            self.assertEqual(event["decision_label"], "EDIT")
            self.assertTrue(event["rewrite_called"])
            self.assertTrue(event["validation_rejected"])

    def test_constrained_one_call_malformed_output_is_reported(self):
        with patch.dict(os.environ, {"SUNOTO_LLM_POLISH_MODE": "constrained_one_call"}):
            llm = FakeLlama("Maybe this is clean.")
            stdout = io.StringIO()

            with redirect_stdout(stdout), redirect_stderr(io.StringIO()):
                llm_polish_sidecar.polish(llm, 18, "Maybe this is clean.")

            event = json.loads(stdout.getvalue())
            self.assertEqual(event["decision_label"], "MALFORMED")
            self.assertTrue(event["decision_malformed"])

    def test_constrained_prompt_and_few_shot_can_be_overridden_for_eval(self):
        env = {
            "SUNOTO_LLM_POLISH_CONSTRAINED_SYSTEM_PROMPT": "Return OK unless a real repair is needed.",
            "SUNOTO_LLM_POLISH_CONSTRAINED_SYSTEM_PROMPT_FILE": "",
            "SUNOTO_LLM_POLISH_CONSTRAINED_FEW_SHOT_JSON": json.dumps(
                [
                    ["Digits stay literal.", "OK"],
                    {
                        "input": "Use the old one, no wait, use the new one.",
                        "output": "EDIT: Use the new one.",
                    },
                ]
            ),
            "SUNOTO_LLM_POLISH_CONSTRAINED_FEW_SHOT_FILE": "",
        }

        with patch.dict(os.environ, env):
            messages = llm_polish_sidecar.constrained_messages_for("Final transcript.")

        self.assertEqual(messages[0]["content"], env["SUNOTO_LLM_POLISH_CONSTRAINED_SYSTEM_PROMPT"])
        self.assertEqual(messages[1]["content"], "Clean this transcript:\nDigits stay literal.")
        self.assertEqual(messages[2]["content"], "OK")
        self.assertEqual(
            messages[3]["content"],
            "Clean this transcript:\nUse the old one, no wait, use the new one.",
        )
        self.assertEqual(messages[4]["content"], "EDIT: Use the new one.")
        self.assertEqual(messages[-1]["content"], "Clean this transcript:\nFinal transcript.")

    def test_constrained_prompt_recognizes_disfluencies_as_a_class(self):
        """The constrained authority is merge-only of DISFLUENT speech, and must
        NOT gate on an enumerated cue-word list (too brittle; misses no-cue
        repetitions, false starts, and restarts).

        Guards: no affirmative filler-removal duty, grammar/tense/punctuation
        are off-limits, and the few-shot demonstrates a no-cue disfluency
        (so merging is driven by structure, not by cue words) plus the
        keeping-a-pronoun unequal-reword example.
        """
        import llm_polish_once

        prompt = llm_polish_once.CONSTRAINED_SYSTEM_PROMPT.lower()
        # authority is the class, not a cue-keyword gate.
        self.assertIn("structure", prompt)
        self.assertIn("not by whether a cue word is", prompt)
        # 'remove fillers' may only appear in the prohibition, never as a duty.
        self.assertIn("do not remove fillers", prompt)
        self.assertEqual(prompt.replace("do not remove fillers", "").count("remove filler"), 0)
        self.assertIn("grammar", prompt)
        self.assertIn("tense", prompt)
        self.assertIn("punctuation", prompt)
        # discourse 'Actually,' at start is NOT a correction.
        self.assertIn("emphasis", prompt)

        few_shot = llm_polish_once.CONSTRAINED_REPAIR_FEW_SHOT
        inputs = {entry[0] for entry in few_shot}
        # discourse Actually at start -> OK
        self.assertTrue(
            any(text.lower().startswith("actually,") for text in inputs),
            "few-shot must include a leading-'Actually,' discourse example",
        )
        # tense/grammar error NOT fixed -> OK
        self.assertTrue(
            any("he send" in text.lower() for text in inputs),
            "few-shot must include a grammar/tense-not-ours OK example",
        )
        # a no-cue disfluency (pure repetition) is recognized -> EDIT
        self.assertTrue(
            any("the the" in text.lower() for text in inputs),
            "few-shot must include a no-cue repetition EDIT example (structure, not cue)",
        )
        # unequal reword preserving a pre-retraction pronoun -> EDIT
        meet_example = next(
            (entry for entry in few_shot if "meet her at the cafe" in entry[0].lower()),
            None,
        )
        self.assertIsNotNone(meet_example, "few-shot must include the unequal-reword example")
        output = meet_example[1]
        self.assertTrue(output.startswith("EDIT:"))
        self.assertIn("meet her at the library", output.lower())
        self.assertNotIn("cafe", output.lower())


class KeepaliveTests(unittest.TestCase):
    def setUp(self):
        os.environ.pop("SUNOTO_LLM_POLISH_KEEPALIVE_S", None)
        llm_polish_sidecar._keepalive_counter = 0

    def test_keepalive_interval_defaults_to_1s(self):
        self.assertAlmostEqual(llm_polish_sidecar.keepalive_interval_s(), 1.0)

    def test_keepalive_interval_env_override(self):
        os.environ["SUNOTO_LLM_POLISH_KEEPALIVE_S"] = "2.5"
        self.assertAlmostEqual(llm_polish_sidecar.keepalive_interval_s(), 2.5)
        os.environ["SUNOTO_LLM_POLISH_KEEPALIVE_S"] = "0"
        self.assertAlmostEqual(llm_polish_sidecar.keepalive_interval_s(), 0.0)

    def test_keepalive_interval_bad_value_falls_back(self):
        os.environ["SUNOTO_LLM_POLISH_KEEPALIVE_S"] = "garbage"
        self.assertAlmostEqual(llm_polish_sidecar.keepalive_interval_s(), 1.0)

    def test_keepalive_skipped_until_first_request(self):
        """keepalive() must not call the LLM before warmup/polish has run."""
        llm_polish_sidecar._keepalive_ready = False
        llm = FakeLlama("OK")
        llm_polish_sidecar.keepalive(llm)
        self.assertEqual(len(llm.calls), 0)

    def test_keepalive_runs_after_warmup_and_does_not_emit_stdout(self):
        """After warmup, keepalive fires the LLM but never emits on stdout."""
        llm_polish_sidecar._keepalive_text = "Hey, how are you doing?"
        llm_polish_sidecar._keepalive_ready = True
        llm = FakeLlama("OK")
        captured = io.StringIO()
        with redirect_stdout(captured):
            llm_polish_sidecar.keepalive(llm)
        self.assertEqual(len(llm.calls), 1)
        # Protocol stdout MUST stay clean (no polished/keepalive event).
        self.assertEqual(captured.getvalue(), "")
        llm_polish_sidecar._keepalive_ready = False
        llm_polish_sidecar._keepalive_text = None

    def test_keepalive_suffix_is_monotonic_and_never_repeats(self):
        """Each keepalive must append a fresh counter so the prompt is never a
        full cache hit (which downclocks Metal and makes decode go cold). Two
        consecutive keepalives must pass distinct transcripts to the LLM."""
        llm_polish_sidecar._keepalive_text = "Hey, how are you doing?"
        llm_polish_sidecar._keepalive_ready = True
        llm = FakeLlama("OK")
        llm_polish_sidecar.keepalive(llm)
        llm_polish_sidecar.keepalive(llm)
        self.assertEqual(len(llm.calls), 2)
        first_prompt = llm.calls[0]["messages"][-1]["content"]
        second_prompt = llm.calls[1]["messages"][-1]["content"]
        self.assertIn("1.", first_prompt)
        self.assertIn("2.", second_prompt)
        self.assertNotEqual(first_prompt, second_prompt)
        llm_polish_sidecar._keepalive_ready = False
        llm_polish_sidecar._keepalive_text = None

    def test_keepalive_loop_skips_when_lock_held(self):
        """keepalive_loop must SKIP a ping (not block) when the main thread
        holds `_llm_lock` (a real polish is in flight). It must never wait on
        the lock — skipping is correct because the real call keeps the GPU
        warm. We hold the lock for a few keepalive cycles and assert NO llama
        call runs during that window."""
        llm_polish_sidecar._keepalive_ready = True
        llm_polish_sidecar._keepalive_text = "Hey, how are you doing?"
        llm_polish_sidecar._keepalive_counter = 0
        llm = FakeLlama("OK")
        llm_polish_sidecar._keepalive_stop.clear()
        # Hold the lock as a real polish would; fire the loop with a tiny
        # interval so multiple cycles elapse while locked.
        with llm_polish_sidecar._llm_lock:
            t = threading.Thread(
                target=llm_polish_sidecar.keepalive_loop,
                args=(llm, 0.02),
                daemon=True,
            )
            t.start()
            time.sleep(0.15)  # ~7 cycles elapse, all must be skipped
            self.assertEqual(len(llm.calls), 0)
        # Now that the lock is released, the loop should run a ping promptly.
        time.sleep(0.08)
        llm_polish_sidecar._keepalive_stop.set()
        t.join(timeout=2.0)
        self.assertGreaterEqual(len(llm.calls), 1)
        llm_polish_sidecar._keepalive_ready = False
        llm_polish_sidecar._keepalive_text = None
        llm_polish_sidecar._keepalive_stop.clear()

    def test_keepalive_loop_exits_on_stop(self):
        """Setting _keepalive_stop must wake the loop and let the thread exit
        promptly even mid-sleep."""
        llm_polish_sidecar._keepalive_ready = True
        llm = FakeLlama("OK")
        llm_polish_sidecar._keepalive_stop.clear()
        t = threading.Thread(
            target=llm_polish_sidecar.keepalive_loop,
            args=(llm, 30.0),  # long sleep; stop must preempt it
            daemon=True,
        )
        t.start()
        time.sleep(0.05)  # let it enter the sleep
        llm_polish_sidecar._keepalive_stop.set()
        t.join(timeout=2.0)
        self.assertFalse(t.is_alive())
        llm_polish_sidecar._keepalive_ready = False
        llm_polish_sidecar._keepalive_stop.clear()


if __name__ == "__main__":
    unittest.main()
