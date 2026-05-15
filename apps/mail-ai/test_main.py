"""Tests for the mail-ai Python app.

These tests focus on argument parsing, prompt assembly, JSON repair
and response shaping. The actual ``ai.chat`` call is monkey-patched so
no model is invoked.
"""

from __future__ import annotations

import json
import os
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, os.path.dirname(__file__))

# Add the SDK src dir to sys.path so `from claw_os_sdk import …` works
# inside main.
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(__file__),
        os.pardir,
        os.pardir,
        "claw-os-sdk",
        "python",
        "src",
    ),
)

import main  # noqa: E402
from claw_os_sdk import ai  # noqa: E402


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _fake_response(text: str) -> ai.AiResponse:
    return ai.AiResponse(
        text=text,
        model="fake-model",
        provider="fake",
        usage=ai.Usage(input_tokens=10, output_tokens=20, units=30),
        budget=ai.Budget(period="2024-01", units_used=100, units_cap=500_000),
        review=ai.Review(safety="strict", prompt_redacted=False),
    )


class _FakePolicy:
    def require(self, *a, **kw):
        return None


# ---------------------------------------------------------------------------
# Helpers (pure, no AI)
# ---------------------------------------------------------------------------

class TestStripQuoted(unittest.TestCase):
    def test_drops_outlook_divider(self):
        text = "hello\n_____\nFrom: bob@example.com\nblah"
        self.assertEqual(main._strip_quoted(text), "hello")

    def test_drops_signature(self):
        text = "Body line 1\nBody line 2\n-- \nBob\nCEO"
        self.assertEqual(main._strip_quoted(text), "Body line 1\nBody line 2")

    def test_keeps_when_no_marker(self):
        text = "Just a normal email body."
        self.assertEqual(main._strip_quoted(text), text)

    def test_empty(self):
        self.assertEqual(main._strip_quoted(""), "")
        self.assertEqual(main._strip_quoted(None), "")


class TestTruncate(unittest.TestCase):
    def test_keeps_short(self):
        self.assertEqual(main._truncate("hi", 100), "hi")

    def test_trims_long(self):
        s = "a" * 5000
        out = main._truncate(s, 200)
        self.assertLess(len(out), 5000)
        self.assertIn("truncated", out)


class TestSafeLoads(unittest.TestCase):
    def test_plain_json(self):
        self.assertEqual(main._safe_loads('{"a": 1}'), {"a": 1})

    def test_fenced_json(self):
        text = '```json\n{"a": 1, "b": [2, 3]}\n```'
        self.assertEqual(main._safe_loads(text), {"a": 1, "b": [2, 3]})

    def test_prose_around_json(self):
        text = 'Sure! Here is the result:\n{"x": "y"}\nLet me know.'
        self.assertEqual(main._safe_loads(text), {"x": "y"})

    def test_not_json(self):
        self.assertIsNone(main._safe_loads("nope, plain text"))
        self.assertIsNone(main._safe_loads(""))


# ---------------------------------------------------------------------------
# summarize
# ---------------------------------------------------------------------------

class TestSummarize(unittest.TestCase):
    def test_missing_body_fails(self):
        result = main.cmd_summarize([])
        self.assertIn("error", result)

    @patch.object(main, "policy", _FakePolicy())
    @patch.object(main.ai, "chat")
    def test_parses_structured_response(self, mock_chat):
        mock_chat.return_value = _fake_response(json.dumps({
            "summary": "Q3 plan review",
            "key_points": ["Two new hires", "Budget +10%"],
            "action_items": ["Review by Friday"],
            "sentiment": "neutral",
        }))
        result = main.cmd_summarize(["--body", "Hello, please review the Q3 plan.", "--subject", "Q3"])
        self.assertEqual(result["summary"], "Q3 plan review")
        self.assertEqual(len(result["key_points"]), 2)
        self.assertEqual(result["sentiment"], "neutral")
        self.assertEqual(result["provider"], "fake")

    @patch.object(main, "policy", _FakePolicy())
    @patch.object(main.ai, "chat")
    def test_handles_non_json_response(self, mock_chat):
        mock_chat.return_value = _fake_response("just some plain text")
        result = main.cmd_summarize(["--body", "Hello world"])
        self.assertEqual(result["summary"], "")
        self.assertIn("raw", result)
        self.assertTrue(result["raw"])

    @patch.object(main, "policy", _FakePolicy())
    @patch.object(main.ai, "chat")
    def test_propagates_budget_error(self, mock_chat):
        mock_chat.side_effect = ai.AiBudgetExceeded({"error": "over"})
        result = main.cmd_summarize(["--body", "Hi"])
        self.assertIn("budget", result["error"].lower())


# ---------------------------------------------------------------------------
# smart_reply
# ---------------------------------------------------------------------------

class TestSmartReply(unittest.TestCase):
    @patch.object(main, "policy", _FakePolicy())
    @patch.object(main.ai, "chat")
    def test_three_suggestions(self, mock_chat):
        mock_chat.return_value = _fake_response(json.dumps({
            "formal": "Thank you for your message. I will look into this.",
            "casual": "Got it — taking a look now!",
            "short": "On it.",
        }))
        result = main.cmd_smart_reply([
            "--thread", "From: alex\nCan you take a look?",
            "--from", "alex@example.com",
        ])
        self.assertIn("formal", result["suggestions"])
        self.assertIn("casual", result["suggestions"])
        self.assertIn("short", result["suggestions"])

    def test_missing_thread_fails(self):
        self.assertIn("error", main.cmd_smart_reply([]))


# ---------------------------------------------------------------------------
# smart_compose
# ---------------------------------------------------------------------------

class TestSmartCompose(unittest.TestCase):
    @patch.object(main, "policy", _FakePolicy())
    @patch.object(main.ai, "chat")
    def test_returns_body(self, mock_chat):
        mock_chat.return_value = _fake_response(json.dumps({
            "body": "Hi Alex,\n\nCould you confirm the deadline?\n\nThanks.",
            "subject": "Quick question on deadline",
        }))
        result = main.cmd_smart_compose([
            "--intent", "ask Alex about the deadline",
            "--to", "alex@example.com",
            "--style", "formal",
        ])
        self.assertIn("Could you confirm", result["body"])
        self.assertEqual(result["style"], "formal")

    def test_missing_intent_fails(self):
        # argparse required raises SystemExit -> our handler turns it into error
        result = main.cmd_smart_compose([])
        self.assertIn("error", result)


# ---------------------------------------------------------------------------
# translate
# ---------------------------------------------------------------------------

class TestTranslate(unittest.TestCase):
    @patch.object(main, "policy", _FakePolicy())
    @patch.object(main.ai, "chat")
    def test_round_trip(self, mock_chat):
        mock_chat.return_value = _fake_response("Hello")
        result = main.cmd_translate(["--text", "Bonjour", "--target", "English"])
        self.assertEqual(result["translation"], "Hello")
        self.assertEqual(result["target"], "English")

    def test_empty_text_fails(self):
        result = main.cmd_translate(["--text", "   ", "--target", "fr"])
        self.assertIn("error", result)


# ---------------------------------------------------------------------------
# triage
# ---------------------------------------------------------------------------

class TestTriage(unittest.TestCase):
    @patch.object(main, "policy", _FakePolicy())
    @patch.object(main.ai, "chat")
    def test_clamps_unknown_category(self, mock_chat):
        mock_chat.return_value = _fake_response(json.dumps({
            "category": "MADE_UP_CATEGORY",
            "tags": ["INVOICE", "STRIPE"],
            "priority": "screaming",
            "reason": "A receipt.",
        }))
        result = main.cmd_triage([
            "--from", "noreply@stripe.com",
            "--subject", "Your receipt",
        ])
        self.assertEqual(result["category"], "other")
        self.assertEqual(result["priority"], "normal")
        self.assertEqual(result["tags"], ["invoice", "stripe"])

    @patch.object(main, "policy", _FakePolicy())
    @patch.object(main.ai, "chat")
    def test_accepts_valid_category(self, mock_chat):
        mock_chat.return_value = _fake_response(json.dumps({
            "category": "receipt",
            "tags": ["stripe"],
            "priority": "low",
            "reason": "A receipt.",
        }))
        result = main.cmd_triage([
            "--from", "noreply@stripe.com",
            "--subject", "Your receipt",
        ])
        self.assertEqual(result["category"], "receipt")
        self.assertEqual(result["priority"], "low")

    def test_needs_something(self):
        result = main.cmd_triage([])
        self.assertIn("error", result)


# ---------------------------------------------------------------------------
# chat
# ---------------------------------------------------------------------------

class TestChat(unittest.TestCase):
    @patch.object(main, "policy", _FakePolicy())
    @patch.object(main.ai, "chat")
    def test_extracts_citations(self, mock_chat):
        mock_chat.return_value = _fake_response("Standup is Monday [2]. See also [1].")
        ctx = json.dumps([
            {"from": "a", "subject": "S", "snippet": "hi", "date": "Mon"},
            {"from": "b", "subject": "Standup", "snippet": "10am", "date": "Tue"},
        ])
        result = main.cmd_chat([
            "--question", "When is standup?",
            "--context-json", ctx,
        ])
        self.assertEqual(result["citations"], [1, 2])

    @patch.object(main, "policy", _FakePolicy())
    @patch.object(main.ai, "chat")
    def test_ignores_out_of_range_citations(self, mock_chat):
        mock_chat.return_value = _fake_response("Answer [99] something.")
        result = main.cmd_chat([
            "--question", "Q",
            "--context-json", "[]",
        ])
        self.assertEqual(result["citations"], [])

    def test_bad_context_json_fails(self):
        result = main.cmd_chat([
            "--question", "Q",
            "--context-json", "not-json",
        ])
        self.assertIn("error", result)

    def test_context_not_array_fails(self):
        result = main.cmd_chat([
            "--question", "Q",
            "--context-json", '{"oops": 1}',
        ])
        self.assertIn("error", result)


# ---------------------------------------------------------------------------
# Schema + entry point
# ---------------------------------------------------------------------------

class TestEntryPoint(unittest.TestCase):
    def test_schema_lists_all_ops(self):
        schema = main.run("__schema__", [])
        for op in ("summarize", "smart_reply", "smart_compose", "translate", "triage", "chat"):
            self.assertIn(op, schema)

    def test_unknown_command(self):
        result = main.run("nope", [])
        self.assertIn("error", result)


if __name__ == "__main__":
    unittest.main()
