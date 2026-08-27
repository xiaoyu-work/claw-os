"""Cross-language wire validator contract tests."""

from __future__ import annotations

import copy
import json
import subprocess
import unittest
from decimal import Decimal
from unittest import mock

from claw_os_sdk import ai, tools
from claw_os_sdk.generated import (
    WIRE_ENUM,
    WIRE_MAXIMUM,
    WIRE_MINIMUM,
    WIRE_REQUIRED,
    WIRE_TYPE,
    WIRE_UNKNOWN_FIELD,
    WireDecodeError,
    WireDecimal,
    decode_wire_json,
    encode_wire_json,
    validate_ai,
    validate_budget_show,
    validate_tool,
    validate_tool_catalog,
    wire_integer_to_int,
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
        for payload, code, path in cases:
            with self.subTest(code=code, path=path):
                with self.assertRaises(WireDecodeError) as raised:
                    validate_ai(payload)
                self.assertEqual(raised.exception.code, code)
                self.assertEqual(raised.exception.path, path)

    def test_integer_semantics_and_adapter_conversion(self) -> None:
        template = json.dumps(_valid_ai())
        accepted = {
            "1.0": 1,
            "1e0": 1,
            "1.5e1": 15,
            "9007199254740992": 9007199254740992,
            "18446744073709551615": 18446744073709551615,
        }
        for literal, expected in accepted.items():
            payload = decode_wire_json(
                template.replace('"units": 3', f'"units": {literal}')
            )
            validate_ai(payload)
            self.assertEqual(wire_integer_to_int(payload["usage"]["units"]), expected)
            self.assertEqual(ai._parse_response(payload).usage.units, expected)

        for literal in ("1.5", "15e-1", "1e-400", "9007199254740990.5"):
            payload = decode_wire_json(
                template.replace('"units": 3', f'"units": {literal}')
            )
            with self.assertRaises(WireDecodeError) as raised:
                validate_ai(payload)
            self.assertEqual(raised.exception.code, WIRE_TYPE)
            self.assertEqual(raised.exception.path, "$.usage.units")

        oversized = decode_wire_json(
            template.replace('"units": 3', '"units": 18446744073709551616')
        )
        with self.assertRaises(WireDecodeError) as raised:
            validate_ai(oversized)
        self.assertEqual(raised.exception.code, WIRE_MAXIMUM)

        fractional_above = decode_wire_json(
            template.replace('"units": 3', '"units": 18446744073709551615.5')
        )
        with self.assertRaises(WireDecodeError) as raised:
            validate_ai(fractional_above)
        self.assertEqual(raised.exception.code, WIRE_TYPE)
    def test_adapter_preserves_all_valid_v1_tool_inputs(self) -> None:
        for tool_input in ("scalar", [1, True], None):
            payload = copy.deepcopy(_valid_ai())
            payload["tool_calls"][0]["input"] = tool_input
            response = ai._parse_response(payload)
            self.assertEqual(response.tool_calls[0].input, tool_input)

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

    def test_root_type_and_budget_show_contract(self) -> None:
        for validator in (validate_ai, validate_tool, validate_tool_catalog):
            with self.subTest(validator=validator.__name__):
                with self.assertRaises(WireDecodeError) as raised:
                    validator(None)
                self.assertEqual(raised.exception.code, WIRE_TYPE)
                self.assertEqual(raised.exception.path, "$")

        validate_budget_show(
            {"app": "notes", "period": "2026-08", "units_used": 7}
        )
        with mock.patch.object(ai, "_cos_binary", return_value="cos"), mock.patch.object(
            subprocess,
            "run",
            return_value=subprocess.CompletedProcess(
                ["cos"], 0, '{"app":"notes","period":"2026-08","units_used":7}', ""
            ),
        ):
            budget = ai.budget("notes")
        self.assertEqual(budget.period, "2026-08")
        self.assertEqual(budget.units_used, 7)
        self.assertEqual(budget.units_cap, 0)

    def test_adapter_rejects_scalar_root_with_stable_error(self) -> None:
        with self.assertRaises(ai.AiUnavailable) as raised:
            ai._parse_response(None)
        self.assertIn("WIRE_TYPE", str(raised.exception))
        self.assertIn(" at $:", str(raised.exception))

        with mock.patch.object(tools, "_cos_binary", return_value="cos"), mock.patch.object(
            tools,
            "_run_with_timeout",
            return_value=subprocess.CompletedProcess(["cos"], 0, "null", ""),
        ):
            with self.assertRaises(tools.ToolUnavailable) as tool_error:
                tools.call("echo", app_id="notes")
        self.assertIn("WIRE_TYPE", str(tool_error.exception))
        self.assertIn(" at $:", str(tool_error.exception))

    def test_proposed_input_round_trips_directly_through_tools_call(self) -> None:
        lexeme = "0.12345678901234567890"
        payload = decode_wire_json(
            json.dumps(_valid_ai()).replace(
                '"input": {"value": "ok"}',
                f'"input": {lexeme}',
            )
        )
        response = ai._parse_response(payload)
        self.assertEqual(response.tool_calls[0].input, Decimal(lexeme))

        completed = subprocess.CompletedProcess(
            ["cos"],
            0,
            '{"tool":"echo","app_id":"notes","status":"ok","result":null}',
            "",
        )
        with mock.patch.object(tools, "_cos_binary", return_value="cos"), mock.patch.object(
            tools, "_run_with_timeout", return_value=completed
        ) as run:
            tools.call(
                "echo",
                response.tool_calls[0].input,
                app_id="notes",
            )
        command = run.call_args.args[0]
        self.assertEqual(command[command.index("--args") + 1], lexeme)

    def test_public_lossless_values_and_catalog_schemas_encode_exactly(self) -> None:
        lexeme = "0.12345678901234567890"
        tool_completed = subprocess.CompletedProcess(
            ["cos"],
            0,
            f'{{"tool":"echo","app_id":"notes","status":"ok","result":{lexeme}}}',
            "",
        )
        with mock.patch.object(tools, "_cos_binary", return_value="cos"), mock.patch.object(
            tools, "_run_with_timeout", return_value=tool_completed
        ):
            result = tools.call("echo", {}, app_id="notes")
        self.assertEqual(result.value, Decimal(lexeme))
        self.assertEqual(encode_wire_json(result.value), lexeme)

        catalog_completed = subprocess.CompletedProcess(
            ["cos"],
            0,
            (
                '{"tools":[{"name":"echo","summary":"Echo","verb":"ipc.invoke",'
                '"stability":"stable","args_schema":{"minimum":'
                + lexeme
                + '},"returns_schema":{"maximum":'
                + lexeme
                + "}}]}"
            ),
            "",
        )
        with mock.patch.object(tools, "_cos_binary", return_value="cos"), mock.patch.object(
            tools, "_run_with_timeout", return_value=catalog_completed
        ):
            entry = tools.catalog()[0]
        self.assertEqual(encode_wire_json(entry.args_schema), f'{{"minimum":{lexeme}}}')
        self.assertEqual(encode_wire_json(entry.returns_schema), f'{{"maximum":{lexeme}}}')

    def test_tool_call_preserves_null_scalar_and_array_arguments(self) -> None:
        completed = subprocess.CompletedProcess(
            ["cos"],
            0,
            '{"tool":"echo","app_id":"notes","status":"ok","result":null}',
            "",
        )
        for args in (None, "scalar", [1, True]):
            with self.subTest(args=args), mock.patch.object(
                tools, "_cos_binary", return_value="cos"
            ), mock.patch.object(
                tools, "_run_with_timeout", return_value=completed
            ) as run:
                tools.call("echo", args, app_id="notes")
            command = run.call_args.args[0]
            self.assertEqual(command[command.index("--args") + 1], encode_wire_json(args))

    def test_compact_huge_exponent_round_trip_and_typed_failure(self) -> None:
        lexeme = "1e999999999999999999999999"
        value = decode_wire_json(lexeme)
        self.assertIsInstance(value, WireDecimal)
        self.assertEqual(value.lexeme, lexeme)
        self.assertEqual(encode_wire_json(value), lexeme)

        payload_text = json.dumps(_valid_ai()).replace(
            '"input": {"value": "ok"}',
            f'"input": {lexeme}',
        )
        response = ai._parse_response(decode_wire_json(payload_text))
        self.assertIsInstance(response.tool_calls[0].input, WireDecimal)
        self.assertEqual(encode_wire_json(response.tool_calls[0].input), lexeme)

        invalid_units = json.dumps(_valid_ai()).replace(
            '"units": 3',
            f'"units": {lexeme}',
        )
        with self.assertRaises(ai.AiUnavailable) as raised:
            ai._parse_response(decode_wire_json(invalid_units))
        self.assertIn("WIRE_TYPE", str(raised.exception))


if __name__ == "__main__":
    unittest.main()
