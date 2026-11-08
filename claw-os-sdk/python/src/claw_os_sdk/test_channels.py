import json
from unittest.mock import patch

import pytest

from claw_os_sdk import channels


def test_channel_reply_transports_only_event_text_and_explicit_confirmation():
    cancelled = lambda: None
    with patch.object(channels, "call_json_with_stdin_binary", return_value={"submitted": True}) as call:
        result = channels.control(
            "reply", event_id="channel-event-synthetic", text="Synthetic reply",
            confirm=True, deadline_unix_ms=123, check_cancelled=cancelled, binary="/fixture/cos",
        )
    assert result == {"submitted": True}
    assert call.call_args.args[:2] == ("/fixture/cos", ["__channel", "control"])
    assert json.loads(call.call_args.args[2]) == {
        "action": "reply", "event_id": "channel-event-synthetic", "text": "Synthetic reply", "confirm": True,
    }
    assert call.call_args.kwargs == {"deadline_unix_ms": 123, "check_cancelled": cancelled}


def test_oversized_channel_requests_never_reach_transport():
    with patch.object(channels, "call_json_with_stdin_binary") as call, pytest.raises(ValueError):
        channels.control("reply", event_id="event", text="x" * (20 * 1024), confirm=True)
    call.assert_not_called()
