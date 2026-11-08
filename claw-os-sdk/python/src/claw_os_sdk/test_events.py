import json
import time
from unittest.mock import patch

import pytest

from claw_os_sdk import events


def test_event_transport_passes_only_business_data_and_preserves_explicit_deadline():
    with patch.object(events, "call_json_with_stdin_binary", return_value={"accepted": True}) as call:
        result = events.publish(
            "message.received", "stable-key",
            payload={"object": "app://email/gmail-message?id=message"},
            entity_id="message", deadline_unix_ms=123, binary="/fixture/cos",
        )
    assert result == {"accepted": True}
    assert call.call_args.args[:2] == ("/fixture/cos", ["__app_event", "publish"])
    assert json.loads(call.call_args.args[2]) == {
        "event_type": "message.received", "key": "stable-key", "entity_id": "message",
        "payload": {"object": "app://email/gmail-message?id=message"},
    }
    assert call.call_args.kwargs["deadline_unix_ms"] == 123


def test_oversized_events_are_rejected_before_transport():
    with patch.object(events, "call_json_with_stdin_binary") as call, pytest.raises(ValueError):
        events.publish("changed", "key", payload={"text": "x" * (20 * 1024)})
    call.assert_not_called()
