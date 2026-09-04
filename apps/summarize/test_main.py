from pathlib import Path
from unittest import mock

import pytest

from test_support import load_local_module


main = load_local_module(
    Path(__file__).with_name("main.py"),
    "claw_test_summarize_main",
)


def _response(text: str = "- First\n- Second\n- Third") -> object:
    return main.ai.AiResponse(
        text=text,
        model="test-model",
        provider="test-provider",
        usage=main.ai.Usage(input_tokens=10, output_tokens=20, units=30),
        budget=main.ai.Budget(
            period="2026-09",
            units_used=40,
            units_cap=100_000,
        ),
        review=main.ai.Review(safety="strict", prompt_redacted=False),
    )


@pytest.mark.parametrize("text", [None, 7, "", " \n\t "])
def test_validates_text_before_policy(monkeypatch, text):
    require = mock.Mock()
    chat = mock.Mock()
    remember = mock.Mock()
    monkeypatch.setattr(main.policy, "require", require)
    monkeypatch.setattr(main.ai, "chat", chat)
    monkeypatch.setattr(main.memory, "remember", remember)

    with pytest.raises(ValueError, match="non-empty string"):
        main.summarize(text)

    require.assert_not_called()
    chat.assert_not_called()
    remember.assert_not_called()


def test_calls_ai_chat_exactly_after_policy(monkeypatch):
    order = []
    require = mock.Mock(side_effect=lambda *args, **kwargs: order.append("policy"))
    chat = mock.Mock(
        side_effect=lambda **kwargs: (order.append("ai"), _response())[1]
    )
    monkeypatch.setattr(main.policy, "require", require)
    monkeypatch.setattr(main.ai, "chat", chat)
    monkeypatch.setattr(main.memory, "remember", mock.Mock())

    main.summarize("Text to summarize")

    assert order == ["policy", "ai"]
    require.assert_called_once_with("ai.chat.untrusted", wild=True)
    chat.assert_called_once_with(
        prompt="Text to summarize",
        origin="external-content",
        system=main.SYSTEM_PROMPT,
        max_units=4000,
    )


def test_returns_structured_result_and_records_memory(monkeypatch):
    head = "a" * 205
    summary = f"{head}\n- Second line"
    remember = mock.Mock()
    monkeypatch.setattr(main.policy, "require", mock.Mock())
    monkeypatch.setattr(main.ai, "chat", mock.Mock(return_value=_response(summary)))
    monkeypatch.setattr(main.memory, "remember", remember)

    result = main.summarize("Explicit input")

    assert result == {
        "summary": summary,
        "source": "<input>",
        "model": "test-model",
        "provider": "test-provider",
        "usage": {
            "input_tokens": 10,
            "output_tokens": 20,
            "units": 30,
        },
        "budget": {
            "period": "2026-09",
            "units_used": 40,
            "units_cap": 100_000,
        },
        "review": {
            "safety": "strict",
            "prompt_redacted": False,
        },
    }
    remember.assert_called_once_with(
        source="summarize",
        text=f"Summarised <input>: {'a' * 197}...",
        kind="note",
        tags=["summarize"],
    )


@pytest.mark.parametrize("summary", ["", " \n\t "])
def test_empty_model_output_raises(monkeypatch, summary):
    remember = mock.Mock()
    monkeypatch.setattr(main.policy, "require", mock.Mock())
    monkeypatch.setattr(main.ai, "chat", mock.Mock(return_value=_response(summary)))
    monkeypatch.setattr(main.memory, "remember", remember)

    with pytest.raises(RuntimeError, match="empty summary"):
        main.summarize("Explicit input")

    remember.assert_not_called()


def test_ai_errors_propagate(monkeypatch):
    error = main.ai.AiUnavailable("AI unavailable")
    monkeypatch.setattr(main.policy, "require", mock.Mock())
    monkeypatch.setattr(main.ai, "chat", mock.Mock(side_effect=error))
    remember = mock.Mock()
    monkeypatch.setattr(main.memory, "remember", remember)

    with pytest.raises(main.ai.AiUnavailable) as raised:
        main.summarize("Explicit input")

    assert raised.value is error
    remember.assert_not_called()


def test_policy_errors_propagate_before_ai(monkeypatch):
    error = main.policy.PolicyUnavailable("policy unavailable")
    monkeypatch.setattr(main.policy, "require", mock.Mock(side_effect=error))
    chat = mock.Mock()
    remember = mock.Mock()
    monkeypatch.setattr(main.ai, "chat", chat)
    monkeypatch.setattr(main.memory, "remember", remember)

    with pytest.raises(main.policy.PolicyUnavailable) as raised:
        main.summarize("Explicit input")

    assert raised.value is error
    chat.assert_not_called()
    remember.assert_not_called()


def test_memory_errors_propagate(monkeypatch):
    error = main.memory.MemoryError("memory unavailable")
    monkeypatch.setattr(main.policy, "require", mock.Mock())
    monkeypatch.setattr(main.ai, "chat", mock.Mock(return_value=_response()))
    monkeypatch.setattr(main.memory, "remember", mock.Mock(side_effect=error))

    with pytest.raises(main.memory.MemoryError) as raised:
        main.summarize("Explicit input")

    assert raised.value is error
