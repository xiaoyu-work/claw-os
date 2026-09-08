"""Shared Mail business behavior, validation, authority and manifest contract."""

from __future__ import annotations

import inspect
import io
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
from unittest.mock import Mock

import pytest
from test_support import load_local_module


HERE = Path(__file__).parent
main = load_local_module(HERE / "main.py", "claw_test_mail_ai_main")
MANIFEST = json.loads((HERE / "app.json").read_text())
CASES = [
    ("summarize", {"body": "Please review by Friday.", "subject": "Q3", "sender": "alex"},
     '{"summary":"Review Q3","key_points":["Plan"],"action_items":["Review"],"sentiment":"urgent"}', 3000),
    ("smart_reply", {"thread": "Can you review?", "sender": "alex", "intent": "accept"},
     '{"formal":"Certainly.","casual":"Sure!","short":"Yes."}', 4000),
    ("smart_compose", {"intent": "ask for more time", "recipient": "alex", "style": "short"},
     '{"body":"Could we extend?","subject":"Deadline"}', 4000),
    ("translate", {"text": "Bonjour", "target": "English"}, "Hello", 4000),
    ("triage", {"sender": "alex", "subject": "Deadline", "has_attachments": True},
     '{"category":"work","tags":["DEADLINE"],"priority":"high","reason":"Due today."}', 1000),
    ("chat", {"question": "When?", "context_json": '[{"sender":"alex","subject":"Review","date":"Friday","snippet":"Due"}]'},
     "Friday [1]. Not [99].", 3000),
]


def response(text):
    return main.ai.AiResponse(
        text=text, model="fake-model", provider="fake",
        usage=main.ai.Usage(input_tokens=10, output_tokens=20, units=30),
        budget=main.ai.Budget(period="2024-01", units_used=100, units_cap=500_000),
        review=main.ai.Review(safety="strict", prompt_redacted=False),
    )


@pytest.fixture
def effects(monkeypatch):
    require, chat, remember = Mock(), Mock(), Mock()
    monkeypatch.setattr(main.policy, "require", require)
    monkeypatch.setattr(main.ai, "chat", chat)
    monkeypatch.setattr(main.memory, "remember", remember)
    return require, chat, remember


@pytest.mark.parametrize("verb,args,text,units", CASES)
def test_business_response_and_exact_authority(verb, args, text, units, effects):
    require, model, remember = effects
    model.return_value = response(text)
    result = main.HANDLERS[verb](**args)
    require.assert_called_once_with("ai.chat.untrusted", wild=True)
    assert model.call_args.kwargs["origin"] == "external-content"
    assert model.call_args.kwargs["max_units"] == units
    assert result["model"] == "fake-model"
    assert result["budget"]["units_cap"] == 500_000
    assert result["usage"]["units"] == 30
    assert result["review"] == {"safety": "strict", "prompt_redacted": False}
    if verb in ("summarize", "triage"):
        remember.assert_called_once()
        assert remember.call_args.kwargs["source"] == "mail-ai"
    else:
        remember.assert_not_called()
    expected = {
        "summarize": ("summary", "Review Q3"),
        "smart_reply": ("suggestions", {"formal": "Certainly.", "casual": "Sure!", "short": "Yes."}),
        "smart_compose": ("body", "Could we extend?"),
        "translate": ("translation", "Hello"),
        "triage": ("tags", ["deadline"]),
        "chat": ("citations", [1]),
    }
    key, value = expected[verb]
    assert result[key] == value


def invalid_cases():
    valid = {name: args for name, args, _, _ in CASES}
    for tool in MANIFEST["mcp"]["tools"]:
        verb = tool["name"].split(".")[1]
        yield verb, {**valid[verb], "unknown": "value"}
        for arg in tool["args"]:
            wrong = [None, 1, [], {}]
            wrong += [True, False] if arg["kind"] == "text" else ["true", "false", ""]
            for value in wrong:
                yield verb, {**valid[verb], arg["name"]: value}
            if arg["required"]:
                yield verb, {key: value for key, value in valid[verb].items() if key != arg["name"]}
                yield verb, {**valid[verb], arg["name"]: " \n "}
    for verb, args in [
        ("summarize", {"body": "Hi", "from": "alex"}),
        ("smart_reply", {"thread": "Hi", "my-intent": "decline"}),
        ("smart_compose", {"intent": "Hi", "to": "alex"}),
        ("triage", {"subject": "Hi", "has-attachments": True}),
        ("chat", {"question": "Hi", "context-json": "[]"}),
        ("smart_compose", {"intent": "Hi", "style": "unknown"}),
        ("triage", {}),
        ("triage", {"subject": " ", "sender": "\n", "snippet": "  "}),
        ("chat", {"question": "Hi", "lang": ""}),
    ]:
        yield verb, args
    for context in ("not-json", "{}", "null", "[1]", "[null]", '[{"snippet":false}]',
                    '[{"sender":1}]', '[{"from":"old spelling"}]', '[{"unknown":"x"}]',
                    '[{"snippet":NaN}]', '[{"snippet":Infinity}]',
                    json.dumps([{}] * 20 + [{"snippet": False}])):
        yield "chat", {"question": "Hi", "context_json": context}


@pytest.mark.parametrize("verb,args", list(invalid_cases()))
def test_invalid_input_has_no_effects(verb, args, effects):
    handler = main.HANDLERS[verb]
    try:
        inspect.signature(handler).bind(**args)
    except TypeError:
        with pytest.raises(TypeError):
            handler(**args)
    else:
        assert "error" in handler(**args)
    for effect in effects:
        effect.assert_not_called()


@pytest.mark.parametrize("error,expected", [
    (main.ai.AiBudgetExceeded({"error": "over"}), {"error": "AI budget exceeded for this app", "detail": {"error": "over"}}),
    (main.ai.AiSafetyViolation({"error": "unsafe"}), {"error": "safety violation", "detail": {"error": "unsafe"}}),
    (main.ai.AiDenied({"error": "no consent"}), {"error": "AI call denied", "detail": {"error": "no consent"}}),
    (main.ai.AiUnavailable("offline"), {"error": "AI unavailable: offline"}),
    (main.ai.AiError("failed"), {"error": "failed"}),
])
def test_ai_errors_remain_structured(error, expected, effects):
    effects[1].side_effect = error
    assert main.summarize(body="Hi") == expected
    effects[2].assert_not_called()


@pytest.mark.parametrize("error,expected", [
    (main.policy.PermissionDenied({"summary": "not granted", "verb": "ai.chat.untrusted"}),
     {"error": "not granted", "denial": {"summary": "not granted", "verb": "ai.chat.untrusted"}}),
    (main.policy.PolicyUnavailable("offline"), {"error": "capability check failed: offline"}),
])
def test_policy_errors_remain_structured(error, expected, effects):
    effects[0].side_effect = error
    assert main.translate(text="Hi", target="French") == expected
    effects[1].assert_not_called()
    effects[2].assert_not_called()


def test_memory_failure_is_only_narrow_optional_error(effects):
    effects[1].return_value = response('{"summary":"A summary"}')
    effects[2].side_effect = main.memory.MemoryError("unavailable")
    assert main.summarize(body="Hi")["summary"] == "A summary"
    effects[2].side_effect = RuntimeError("unexpected")
    with pytest.raises(RuntimeError, match="unexpected"):
        main.summarize(body="Hi")


@pytest.mark.parametrize("category,priority,remembered", [
    ("other", "normal", False), ("newsletter", "low", False),
    ("work", "normal", True), ("receipt", "low", True), ("other", "high", True),
])
def test_preserves_notable_triage_memory(category, priority, remembered, effects):
    effects[1].return_value = response(json.dumps({"category": category, "priority": priority}))
    main.triage(subject="A message")
    assert effects[2].called is remembered


def test_existing_response_repair_and_clamping(effects):
    for text in ('{"a":1}', '```json\n{"a":1}\n```', 'Here: {"a":1}.'):
        assert main._safe_loads(text) == {"a": 1}
    assert main._safe_loads("not json") is None
    effects[1].return_value = response("plain model text")
    assert main.summarize(body="Hi")["raw"] == "plain model text"
    effects[2].assert_not_called()
    assert main.smart_compose(intent="Hi")["body"] == "plain model text"
    effects[1].return_value = response('{"category":"invalid","priority":"invalid","tags":["INVOICE"]}')
    result = main.triage(subject="Hi")
    assert (result["category"], result["priority"], result["tags"]) == ("other", "normal", ["invoice"])


def test_prompt_limits_and_quoted_text(effects):
    effects[1].return_value = response("{}")
    assert main._strip_quoted("Hello\n_____\nOld") == "Hello"
    assert main._strip_quoted("Hello\n-- \nSignature") == "Hello"
    assert main._strip_quoted("") == ""
    main.summarize(body="Hello\nOn Monday old")
    assert "--- email body ---\nHello\n--- end ---" in effects[1].call_args.kwargs["prompt"]
    for verb, args, limit in [
        ("summarize", {"body": "x" * 50_000}, main.MAX_BODY_CHARS),
        ("smart_reply", {"thread": "x" * 50_000}, main.MAX_THREAD_CHARS),
        ("smart_compose", {"intent": "reply", "draft": "x" * 50_000}, main.MAX_DRAFT_CHARS),
        ("translate", {"text": "x" * 50_000, "target": "fr"}, main.MAX_BODY_CHARS),
    ]:
        main.HANDLERS[verb](**args)
        prompt = effects[1].call_args.kwargs["prompt"]
        assert "truncated" in prompt
        assert len(prompt) < limit + 500
    main.triage(snippet="x" * 5000)
    assert effects[1].call_args.kwargs["prompt"].count("x") == 1000
    context = [{"snippet": "x" * 1000}] * 25
    main.chat(question="Q", context_json=json.dumps(context))
    prompt = effects[1].call_args.kwargs["prompt"]
    assert "(20 messages)" in prompt
    assert "[21]" not in prompt
    snippets = [line.strip() for line in prompt.splitlines() if line.startswith("     ")]
    assert snippets == ["x" * 400] * 20


def test_manifest_is_single_schema_and_headless():
    assert MANIFEST["id"] == "mail-ai"
    assert "operations" not in MANIFEST
    assert "thunderbird" not in MANIFEST.get("dependencies", {}).get("binaries", [])
    assert MANIFEST["ai"] == {
        "budget": {"monthly_units": 500000}, "safety": "strict", "origins": ["external-content"],
    }
    assert len(MANIFEST["mcp"]["tools"]) == len(main.HANDLERS) == 6
    for tool in MANIFEST["mcp"]["tools"]:
        name = tool["name"].split(".")[1]
        parameters = inspect.signature(main.HANDLERS[name]).parameters
        assert set(parameters) == {arg["name"] for arg in tool["args"]}
        for arg in tool["args"]:
            parameter = parameters[arg["name"]]
            assert parameter.kind is inspect.Parameter.KEYWORD_ONLY
            assert arg["binding"] == "flag"
            assert arg["required"] is (parameter.default is inspect.Parameter.empty)
            if not arg["required"]:
                assert arg["default"] == parameter.default
            assert parameter.annotation == ("bool" if arg["kind"] == "bool" else "str")
        needs = tool["needs"]
        assert needs[0]["verb"] == "ai.chat.untrusted"
        assert needs[0]["scope"] == {"kind": "wild"}
        if name in ("summarize", "triage"):
            assert needs[1]["verb"] == "memory.write"
            assert needs[1]["scope"] == {
                "kind": "fixed", "scope": {"kind": "self-ref", "value": "mail-ai"},
            }
        else:
            assert len(needs) == 1
    source = (HERE / "main.py").read_text() + (HERE / "server.py").read_text() + (HERE / "native_host.py").read_text()
    for obsolete in ("argparse", "def run(", "cmd_", "_args_to_argv", "serve_manifest_operations",
                     "_on_call_tool", "_resolve_manifest_arguments", "_call_context"):
        assert obsolete not in source


# Injection lives only in the test driver. Both production transports run in a
# separate isolated interpreter with their real framing and public SDK binding.
DRIVER = r"""
import json, os, pathlib, runpy, sys
root, mode, text, failure = sys.argv[1:]
host = runpy.run_path(str(pathlib.Path(root) / "native_host.py"), run_name="mail_native_fixture")
mail = host["mail_ai"]
effects = []
def require(verb, **kwargs):
    effects.append(["policy", verb, kwargs])
    if failure == "policy":
        raise mail.policy.PermissionDenied({"summary": "not granted", "verb": verb})
    if failure == "unavailable-policy":
        raise mail.policy.PolicyUnavailable("offline")
def chat(**kwargs):
    effects.append(["ai", kwargs])
    errors = {
        "budget": mail.ai.AiBudgetExceeded({"error": "over"}),
        "safety": mail.ai.AiSafetyViolation({"error": "unsafe"}),
        "consent": mail.ai.AiDenied({"error": "no consent"}),
        "unavailable": mail.ai.AiUnavailable("offline"),
        "ai-error": mail.ai.AiError("failed"),
        "unexpected": RuntimeError("SECRET EMAIL BODY"),
    }
    if failure in errors:
        raise errors[failure]
    return mail.ai.AiResponse(
        text=text, model="fake-model", provider="fake",
        usage=mail.ai.Usage(input_tokens=10, output_tokens=20, units=30),
        budget=mail.ai.Budget(period="2024-01", units_used=100, units_cap=500000),
        review=mail.ai.Review(safety="strict", prompt_redacted=False),
    )
def remember(**kwargs):
    effects.append(["memory", kwargs])
mail.policy.require, mail.ai.chat, mail.memory.remember = require, chat, remember
if mode == "native":
    status = host["main"]()
elif mode == "probe":
    sys.argv = [str(pathlib.Path(root) / "native_host.py"), "--probe"]
    try:
        runpy.run_path(sys.argv[0], run_name="__main__")
    except SystemExit as exc:
        status = exc.code
else:
    os.environ["COS_APP_MANIFEST"] = str(pathlib.Path(root) / "app.json")
    server = runpy.run_path(str(pathlib.Path(root) / "server.py"))
    server["create_app"]().serve()
    status = 0
print("TEST_EFFECTS=" + json.dumps(effects), file=sys.stderr)
sys.exit(status)
"""


def frame(value):
    raw = json.dumps(value, ensure_ascii=False).encode("utf-8")
    return struct.pack("<I", len(raw)) + raw


def native_request(verb, args, rid="request"):
    return {"id": rid, "verb": verb, "args": args}


def mcp_request(verb, args, rid="request"):
    # Test fixture for the authenticated metadata normally attached by core.
    # Production Native Messaging never constructs this metadata.
    return {
        "jsonrpc": "2.0", "id": rid, "method": "tools/call",
        "params": {
            "name": f"mail-ai.{verb}", "arguments": args,
            "_meta": {"claw-os.dev/call-context": {
                "wire_version": 1, "call_id": str(rid), "trace_id": "test-trace",
                "session_id": "test-session", "task_id": "test-task",
                "caller": {"kind": "system-agent", "id": "test-agent", "owner_uid": 1000},
            }},
        },
    }


def drive(mode, requests=(), *, text="Hello", failure="", raw=None):
    if raw is None:
        if mode == "native":
            raw = b"".join(frame(request) for request in requests)
        else:
            raw = b"".join((json.dumps(request) + "\n").encode() for request in requests)
    process = subprocess.run(
        [sys.executable, "-I", "-c", DRIVER, str(HERE.resolve()), mode, text, failure],
        input=raw, capture_output=True, timeout=20, cwd=HERE,
        env={key: value for key, value in os.environ.items() if key not in ("COS_APP_MANIFEST", "COS_SESSION")},
    )
    stderr = process.stderr.decode()
    effect_line = next(line for line in stderr.splitlines() if line.startswith("TEST_EFFECTS="))
    effects = json.loads(effect_line.removeprefix("TEST_EFFECTS="))
    diagnostics = "\n".join(line for line in stderr.splitlines() if not line.startswith("TEST_EFFECTS="))
    replies = []
    if mode == "native":
        stream = io.BytesIO(process.stdout)
        while header := stream.read(4):
            assert len(header) == 4
            length, = struct.unpack("<I", header)
            body = stream.read(length)
            assert len(body) == length
            replies.append(json.loads(body))
    else:
        replies = [json.loads(line) for line in process.stdout.splitlines()]
    return process.returncode, replies, effects, diagnostics


@pytest.mark.parametrize("verb,args,text,units", CASES)
def test_real_native_and_sdk_mcp_roundtrip_match(verb, args, text, units):
    native = drive("native", [native_request(verb, args)], text=text)
    mcp = drive("mcp", [mcp_request(verb, args)], text=text)
    assert native[0] == mcp[0] == 0
    assert native[3] == mcp[3] == ""
    assert native[1][0]["ok"] is True
    assert native[1][0]["result"] == mcp[1][0]["result"]["structuredContent"]
    assert native[2] == mcp[2]
    assert native[2][0] == ["policy", "ai.chat.untrusted", {"wild": True}]
    assert native[2][1][1]["max_units"] == units
    assert native[2][1][1]["origin"] == "external-content"
    assert len(native[2]) == (3 if verb in ("summarize", "triage") else 2)


@pytest.mark.parametrize("mode", ["native", "mcp"])
def test_all_invalid_requests_rejected_before_any_effect(mode):
    builder = native_request if mode == "native" else mcp_request
    cases = list(invalid_cases())
    # Stay below the SDK's bounded pending queue; the business validation is
    # still exercised for every declared argument and both boolean values.
    for start in range(0, len(cases), 16):
        batch = cases[start:start + 16]
        status, replies, effects, diagnostics = drive(
            mode, [builder(verb, args, str(i)) for i, (verb, args) in enumerate(batch)],
        )
        assert status == 0
        assert diagnostics == ""
        assert effects == []
        assert len(replies) == len(batch)
        for reply in replies:
            if mode == "native":
                assert reply["ok"] is False
                assert "error" in reply
            else:
                result = reply["result"]
                assert result["isError"] is True


@pytest.mark.parametrize("failure", [
    "policy", "unavailable-policy", "budget", "safety", "consent", "unavailable", "ai-error",
])
def test_real_ingress_error_shapes_match(failure):
    args = {"text": "Bonjour", "target": "English"}
    native = drive("native", [native_request("translate", args)], failure=failure)
    mcp = drive("mcp", [mcp_request("translate", args)], failure=failure)
    assert native[0] == mcp[0] == 0
    assert native[1][0]["ok"] is False
    assert native[1][0]["detail"] == mcp[1][0]["result"]["structuredContent"]
    assert mcp[1][0]["result"]["isError"] is True
    assert native[2] == mcp[2]
    assert len(native[2]) == (1 if failure in ("policy", "unavailable-policy") else 2)


def test_native_bad_envelopes_keep_host_healthy():
    malformed = [
        None, [], "text", 42, {}, {"id": "a", "verb": "triage"},
        {"id": False, "verb": "triage", "args": {}},
        {"id": "", "verb": "triage", "args": {}},
        {"id": "a", "verb": False, "args": {}},
        {"id": "a", "verb": " ", "args": {}},
        {"id": "a", "verb": "triage", "args": [], "extra": True},
        *[native_request("triage", value) for value in (None, False, 1, [], "")],
        native_request("unknown", {}),
    ]
    valid = native_request("translate", {"text": "Bonjour", "target": "English"}, "last")
    status, replies, effects, diagnostics = drive("native", [*malformed, valid])
    assert status == 0
    assert diagnostics == ""
    assert len(replies) == len(malformed) + 1
    assert all(reply["ok"] is False for reply in replies[:-1])
    assert replies[-1]["ok"] is True and replies[-1]["id"] == "last"
    assert len(effects) == 2


@pytest.mark.parametrize("raw,message", [
    (b"\x01", "header"),
    (struct.pack("<I", 0), "length"),
    (struct.pack("<I", 8 * 1024 * 1024 + 1), "length"),
    (struct.pack("<I", 10) + b"{}", "body"),
    (struct.pack("<I", 1) + b"\xff", "UTF-8"),
    (struct.pack("<I", 1) + b"{", "JSON"),
    (struct.pack("<I", 3) + b"NaN", "JSON"),
])
def test_real_native_malformed_frames_are_not_eof(raw, message):
    status, replies, effects, diagnostics = drive("native", raw=raw)
    assert status == 1
    assert replies == effects == []
    assert message in diagnostics
    assert "Traceback" not in diagnostics


def test_native_eof_and_probe_are_side_effect_free():
    assert drive("native", raw=b"") == (0, [], [], "")
    status, replies, effects, diagnostics = drive("probe")
    assert status == 0
    assert effects == [] and diagnostics == ""
    assert replies == [{"ok": True, "verbs": sorted(main.HANDLERS)}]
    process = subprocess.run(
        [sys.executable, "-I", str(HERE / "native_host.py"), "--probe"],
        capture_output=True, timeout=10,
        env={**os.environ, "PYTHONPATH": "/nonexistent-path"},
    )
    assert process.returncode == 0, process.stderr
    assert json.loads(process.stdout)["verbs"] == sorted(main.HANDLERS)


def test_native_unexpected_error_does_not_leak_email_or_traceback():
    status, replies, effects, diagnostics = drive(
        "native", [native_request("translate", {"text": "Bonjour", "target": "English"})],
        failure="unexpected",
    )
    assert status == 0 and replies[0]["ok"] is False
    assert len(effects) == 2
    for output in (json.dumps(replies), diagnostics):
        assert "SECRET EMAIL BODY" not in output
        assert "Traceback" not in output


def test_sdk_requires_authenticated_mcp_metadata():
    request = mcp_request("translate", {"text": "Hi", "target": "French"})
    del request["params"]["_meta"]
    status, replies, effects, _ = drive("mcp", [request])
    assert status == 0
    assert effects == []
    assert "authenticated" in replies[0]["error"]["message"]


def test_sdk_tools_list_matches_business_arguments():
    status, replies, effects, diagnostics = drive(
        "mcp", [{"jsonrpc": "2.0", "id": "list", "method": "tools/list", "params": {}}],
    )
    assert status == 0
    assert effects == [] and diagnostics == ""
    tools = replies[0]["result"]["tools"]
    assert len(tools) == 6
    for tool in tools:
        verb = tool["name"].split(".")[1]
        schema = tool["inputSchema"]
        assert schema["additionalProperties"] is False
        assert set(schema["properties"]) == set(inspect.signature(main.HANDLERS[verb]).parameters)
