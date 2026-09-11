"""Cross-language wire validator contract tests."""

from __future__ import annotations

import copy
import json
import subprocess
import unittest
from decimal import Decimal
from pathlib import Path
from unittest import mock

from claw_os_sdk import ai, tools
from claw_os_sdk.generated import (
    WIRE_ENUM,
    WIRE_MAXIMUM,
    WIRE_MINIMUM,
    WIRE_REQUIRED,
    WIRE_TYPE,
    WIRE_UNKNOWN_FIELD,
    Operation,
    Operationeffect,
    WireDecodeError,
    WireDecimal,
    _validate_wire_schema,
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
        self.assertIn("WIRE_MAXIMUM", str(raised.exception))

    def test_huge_wire_decimal_integer_classification_is_lexical(self) -> None:
        huge_exponent = "999999999999999999999999"
        template = json.dumps(_valid_ai())

        zero = decode_wire_json(
            template.replace('"units": 3', f'"units": 0e{huge_exponent}')
        )
        validate_ai(zero)
        self.assertEqual(ai._parse_response(zero).usage.units, 0)

        nonzero = decode_wire_json(
            template.replace('"units": 3', f'"units": 1e{huge_exponent}')
        )
        with self.assertRaises(WireDecodeError) as raised:
            validate_ai(nonzero)
        self.assertEqual(raised.exception.code, WIRE_MAXIMUM)
        self.assertEqual(raised.exception.path, "$.usage.units")

    def test_wire_decimal_rejects_invalid_or_injectable_lexemes(self) -> None:
        for lexeme in ("NaN", "Infinity", "01", "0} , \"injected\": true, {"):
            with self.subTest(lexeme=lexeme):
                with self.assertRaises(ValueError):
                    WireDecimal(lexeme)

        forged = object.__new__(WireDecimal)
        object.__setattr__(forged, "lexeme", "NaN")
        with self.assertRaises(ValueError):
            encode_wire_json(forged)

    def test_long_integer_lexeme_stays_compact(self) -> None:
        lexeme = "1" * 5000
        value = decode_wire_json(lexeme)
        self.assertIsInstance(value, WireDecimal)
        self.assertEqual(value.lexeme, lexeme)
        self.assertEqual(encode_wire_json(value), lexeme)

    def test_stable_error_code_precedes_opaque_message(self) -> None:
        with self.assertRaises(ai.AiBudgetExceeded):
            ai._raise_for_error({"error": "opaque", "code": "budget_exceeded"})
        with self.assertRaises(ai.AiSafetyViolation):
            ai._raise_for_error({"error": "opaque", "code": "SaFeTy_ViOlAtIoN"})


class OperationEffectSchemaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        path = Path(__file__).resolve().parents[3] / "wire" / "v1" / "manifest.schema.json"
        cls.schema = json.loads(path.read_text(encoding="utf-8"))
        cls.operation = cls.schema["$defs"]["operation"]
        cls.effect = cls.schema["$defs"]["operationEffect"]

    def validate_effect(self, value: object) -> None:
        _validate_wire_schema(self.effect, self.schema, value, "OperationEffect", "$")

    def test_operation_effect_schema_declares_optional_bounded_guidance(self) -> None:
        self.assertNotIn("effects", self.operation["required"])
        effects = self.operation["properties"]["effects"]
        self.assertEqual(effects["type"], "array")
        self.assertEqual(effects["maxItems"], 16)
        self.assertEqual(effects["items"], {"$ref": "#/$defs/operationEffect"})
        self.assertNotIn("default", effects)
        self.assertEqual(self.effect["required"], ["kind", "label"])
        self.assertFalse(self.effect["additionalProperties"])
        properties = self.effect["properties"]
        self.assertEqual(set(properties), {"kind", "label", "target_arg", "recovery"})
        self.assertEqual(
            properties["kind"]["enum"],
            ["read", "create", "update", "delete", "external", "execute"],
        )
        self.assertEqual(
            properties["recovery"]["enum"],
            ["not_applicable", "reversible", "compensatable", "irreversible", "unknown"],
        )
        self.assertEqual(properties["recovery"]["default"], "unknown")
        self.assertEqual(properties["label"]["$ref"], "#/$defs/localizedText")
        self.assertIn("512 UTF-8 bytes", properties["label"]["description"])
        self.assertEqual(properties["target_arg"]["type"], "string")

    def test_operation_effect_structural_rules_accept_declared_kinds_and_recovery(self) -> None:
        for kind in self.effect["properties"]["kind"]["enum"]:
            self.validate_effect({"kind": kind, "label": {"en": "Declared effect"}})
            for recovery in self.effect["properties"]["recovery"]["enum"]:
                self.validate_effect(
                    {
                        "kind": kind,
                        "label": {"en": "Declared effect", "fr": "Effet declare"},
                        "target_arg": "paths",
                        "recovery": recovery,
                    }
                )
        for effects in ([], [{"kind": "read", "label": {"en": "Read"}}] * 16):
            _validate_wire_schema(
                self.operation["properties"]["effects"],
                self.schema,
                effects,
                "OperationEffects",
                "$",
            )

    def test_operation_effect_structural_rules_reject_invalid_or_authoritative_fields(self) -> None:
        label = {"en": "Declared effect"}
        cases = [
            ({}, WIRE_REQUIRED, "$.kind"),
            ({"kind": "read"}, WIRE_REQUIRED, "$.label"),
            ({"kind": "write", "label": label}, WIRE_ENUM, "$.kind"),
            ({"kind": False, "label": label}, WIRE_TYPE, "$.kind"),
            ({"kind": "read", "label": "Read"}, WIRE_TYPE, "$.label"),
            ({"kind": "read", "label": {"fr": "Lire"}}, WIRE_REQUIRED, "$.label.en"),
            ({"kind": "read", "label": {"en": "Read", "fr": 1}}, WIRE_TYPE, "$.label.fr"),
            ({"kind": "read", "label": label, "target_arg": []}, WIRE_TYPE, "$.target_arg"),
            ({"kind": "read", "label": label, "recovery": None}, WIRE_TYPE, "$.recovery"),
            ({"kind": "read", "label": label, "recovery": "automatic"}, WIRE_ENUM, "$.recovery"),
        ]
        for field in ("authority", "grant", "receipt", "file_diff", "inverse"):
            cases.append(
                ({"kind": "read", "label": label, field: True}, WIRE_UNKNOWN_FIELD, f"$.{field}")
            )
        for value, code, path in cases:
            with self.subTest(value=value), self.assertRaises(WireDecodeError) as raised:
                self.validate_effect(value)
            self.assertEqual(raised.exception.code, code)
            self.assertEqual(raised.exception.path, path)

    def test_operation_effect_codegen_preserves_omission_and_explicit_declarations(self) -> None:
        self.assertIn("effects", Operation.__optional_keys__)
        self.assertEqual(Operationeffect.__required_keys__, {"kind", "label"})
        self.assertEqual(Operationeffect.__optional_keys__, {"target_arg", "recovery"})
        minimal: Operation = {"label": {"en": "Inspect"}}
        self.assertNotIn("effects", json.loads(encode_wire_json(minimal)))
        effect: Operationeffect = {"kind": "read", "label": {"en": "Read requested paths"}}
        for effects in ([], [effect]):
            operation: Operation = {"label": {"en": "Inspect"}, "effects": effects}
            decoded = json.loads(encode_wire_json(operation))
            self.assertEqual(decoded, operation)
            if effects:
                self.assertNotIn("recovery", decoded["effects"][0])
                self.assertNotIn("target_arg", decoded["effects"][0])


if __name__ == "__main__":
    unittest.main()
