"""Cross-language wire validator contract tests."""

from __future__ import annotations

import copy
import unittest

from claw_os_sdk import ai
from claw_os_sdk.generated import (
    WIRE_ENUM,
    WIRE_MINIMUM,
    WIRE_REQUIRED,
    WIRE_TYPE,
    WIRE_UNKNOWN_FIELD,
    WireDecodeError,
    validate_ai,
    validate_tool,
    validate_tool_catalog,
)


def _valid_ai() -> dict:
    return {
        "text": "hello",
        "model": "m",
        "provider": "p",
        "verb": "ai.chat",
        "usage": {"input_tokens": 1, "output_tokens": 2, "units": 3},
        "budget": {"period": "2026-08", "units_used": 3, "units_cap": 100},
        "review": {"safety": "strict", "prompt_redacted": False},
        "tool_calls": [{"id": "c1", "name": "echo", "input": {"value": "ok"}}],
    }


class WireValidationTests(unittest.TestCase):
    def test_ai_validator_enforces_shared_contract(self) -> None:
        cases = []

        payload = _valid_ai()
        del payload["text"]
        cases.append((payload, WIRE_REQUIRED, "$.text"))

        payload = _valid_ai()
        payload["usage"]["input_tokens"] = "1"
        cases.append((payload, WIRE_TYPE, "$.usage.input_tokens"))

        payload = _valid_ai()
        payload["usage"]["units"] = -1
        cases.append((payload, WIRE_MINIMUM, "$.usage.units"))

        payload = _valid_ai()
        payload["verb"] = "ai.unknown"
        cases.append((payload, WIRE_ENUM, "$.verb"))

        payload = _valid_ai()
        payload["usage"]["extra"] = True
        cases.append((payload, WIRE_UNKNOWN_FIELD, "$.usage.extra"))

        payload = _valid_ai()
        del payload["tool_calls"][0]["name"]
        cases.append((payload, WIRE_REQUIRED, "$.tool_calls[0].name"))

        payload = _valid_ai()
        payload["tool_calls"][0]["input"] = "scalar"
        cases.append((payload, WIRE_TYPE, "$.tool_calls[0].input"))

        for payload, code, path in cases:
            with self.subTest(code=code, path=path):
                with self.assertRaises(WireDecodeError) as raised:
                    validate_ai(payload)
                self.assertEqual(raised.exception.code, code)
                self.assertEqual(raised.exception.path, path)

    def test_adapter_preserves_valid_structured_tool_input(self) -> None:
        response = ai._parse_response(copy.deepcopy(_valid_ai()))
        self.assertEqual(response.tool_calls[0].input, {"value": "ok"})

    def test_adapter_rejects_entire_response_for_malformed_tool_call(self) -> None:
        payload = _valid_ai()
        del payload["tool_calls"][0]["name"]
        with self.assertRaises(ai.AiUnavailable) as raised:
            ai._parse_response(payload)
        self.assertIn("WIRE_REQUIRED", str(raised.exception))
        self.assertIn("$.tool_calls[0].name", str(raised.exception))

    def test_structured_items_are_not_skipped(self) -> None:
        with self.assertRaises(WireDecodeError) as raised:
            validate_tool_catalog(
                {
                    "tools": [
                        {
                            "name": "echo",
                            "summary": "Echo",
                            "verb": "ipc.invoke",
                            "stability": "stable",
                            "args_schema": {},
                            "returns_schema": {},
                        },
                        7,
                    ]
                }
            )
        self.assertEqual(raised.exception.code, WIRE_TYPE)
        self.assertEqual(raised.exception.path, "$.tools[1]")
        validate_tool(
            {"tool": "echo", "app_id": "app", "status": "ok", "result": None}
        )


if __name__ == "__main__":
    unittest.main()
