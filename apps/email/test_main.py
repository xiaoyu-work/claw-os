"""Tests for email app."""

import base64
import json
import os
import sys
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, os.path.dirname(__file__))

from main import (
    _detect_provider,
    _parse_gmail_message,
    _parse_outlook_message,
    _resolve_provider,
    run,
)

# ---------------------------------------------------------------------------
# Helpers — clear all provider env vars between tests
# ---------------------------------------------------------------------------

PROVIDER_ENV_KEYS = [
    "GMAIL_ACCESS_TOKEN",
    "GOOGLE_OAUTH_TOKEN",
    "MICROSOFT_ACCESS_TOKEN",
    "MICROSOFT_OAUTH_TOKEN",
    "SMTP_HOST",
    "SMTP_PORT",
    "SMTP_USER",
    "SMTP_PASSWORD",
    "SMTP_FROM",
]


def _clear_provider_env():
    for key in PROVIDER_ENV_KEYS:
        os.environ.pop(key, None)


# ---------------------------------------------------------------------------
# Provider detection
# ---------------------------------------------------------------------------


class TestDetectProvider(unittest.TestCase):
    def setUp(self):
        _clear_provider_env()

    def tearDown(self):
        _clear_provider_env()

    def test_no_env_returns_none(self):
        self.assertIsNone(_detect_provider())

    def test_gmail_access_token(self):
        os.environ["GMAIL_ACCESS_TOKEN"] = "tok"
        self.assertEqual(_detect_provider(), "gmail")

    def test_google_oauth_token(self):
        os.environ["GOOGLE_OAUTH_TOKEN"] = "tok"
        self.assertEqual(_detect_provider(), "gmail")

    def test_microsoft_access_token(self):
        os.environ["MICROSOFT_ACCESS_TOKEN"] = "tok"
        self.assertEqual(_detect_provider(), "outlook")

    def test_microsoft_oauth_token(self):
        os.environ["MICROSOFT_OAUTH_TOKEN"] = "tok"
        self.assertEqual(_detect_provider(), "outlook")

    def test_smtp_host(self):
        os.environ["SMTP_HOST"] = "mail.example.com"
        self.assertEqual(_detect_provider(), "smtp")

    def test_gmail_takes_priority_over_outlook(self):
        os.environ["GMAIL_ACCESS_TOKEN"] = "tok"
        os.environ["MICROSOFT_ACCESS_TOKEN"] = "tok"
        self.assertEqual(_detect_provider(), "gmail")

    def test_gmail_takes_priority_over_smtp(self):
        os.environ["GMAIL_ACCESS_TOKEN"] = "tok"
        os.environ["SMTP_HOST"] = "localhost"
        self.assertEqual(_detect_provider(), "gmail")

    def test_outlook_takes_priority_over_smtp(self):
        os.environ["MICROSOFT_ACCESS_TOKEN"] = "tok"
        os.environ["SMTP_HOST"] = "localhost"
        self.assertEqual(_detect_provider(), "outlook")


class TestResolveProvider(unittest.TestCase):
    def setUp(self):
        _clear_provider_env()

    def tearDown(self):
        _clear_provider_env()

    def test_explicit_provider_returned_directly(self):
        self.assertEqual(_resolve_provider("gmail"), "gmail")

    def test_none_when_no_env(self):
        self.assertIsNone(_resolve_provider(None))

    def test_falls_back_to_detected(self):
        os.environ["SMTP_HOST"] = "localhost"
        self.assertEqual(_resolve_provider(None), "smtp")


# ---------------------------------------------------------------------------
# Unknown command
# ---------------------------------------------------------------------------


class TestUnknownCommand(unittest.TestCase):
    def test_unknown_command(self):
        result = run("bogus", [])
        self.assertIn("error", result)
        self.assertIn("unknown command", result["error"])


# ---------------------------------------------------------------------------
# Send command
# ---------------------------------------------------------------------------


class TestSendCommand(unittest.TestCase):
    def setUp(self):
        _clear_provider_env()

    def tearDown(self):
        _clear_provider_env()

    def test_send_no_provider(self):
        result = run("send", ["--to", "x@y.com", "--subject", "hi", "--body", "hello"])
        self.assertIn("error", result)
        self.assertIn("hint", result)
        self.assertIn("no email provider configured", result["error"])

    def test_send_missing_to(self):
        os.environ["SMTP_HOST"] = "localhost"
        result = run("send", ["--subject", "hi", "--body", "hello"])
        self.assertIn("error", result)

    def test_send_missing_subject(self):
        os.environ["SMTP_HOST"] = "localhost"
        result = run("send", ["--to", "x@y.com", "--body", "hello"])
        self.assertIn("error", result)

    def test_send_missing_body(self):
        os.environ["SMTP_HOST"] = "localhost"
        result = run("send", ["--to", "x@y.com", "--subject", "hi"])
        self.assertIn("error", result)

    @patch("main.smtplib.SMTP")
    def test_send_smtp_success(self, mock_smtp_cls):
        os.environ["SMTP_HOST"] = "mail.example.com"
        os.environ["SMTP_PORT"] = "587"
        os.environ["SMTP_USER"] = "user"
        os.environ["SMTP_PASSWORD"] = "pass"
        os.environ["SMTP_FROM"] = "me@example.com"

        mock_server = MagicMock()
        mock_smtp_cls.return_value.__enter__ = MagicMock(return_value=mock_server)
        mock_smtp_cls.return_value.__exit__ = MagicMock(return_value=False)

        result = run(
            "send",
            ["--to", "x@y.com", "--subject", "hi", "--body", "hello"],
        )
        self.assertTrue(result.get("sent"))
        self.assertEqual(result["to"], "x@y.com")
        self.assertEqual(result["provider"], "smtp")
        mock_server.starttls.assert_called_once()
        mock_server.login.assert_called_once_with("user", "pass")
        mock_server.send_message.assert_called_once()

    @patch("main.smtplib.SMTP")
    def test_send_smtp_with_cc(self, mock_smtp_cls):
        os.environ["SMTP_HOST"] = "localhost"
        os.environ["SMTP_PORT"] = "25"

        mock_server = MagicMock()
        mock_smtp_cls.return_value.__enter__ = MagicMock(return_value=mock_server)
        mock_smtp_cls.return_value.__exit__ = MagicMock(return_value=False)

        result = run(
            "send",
            [
                "--to", "x@y.com",
                "--subject", "hi",
                "--body", "hello",
                "--cc", "z@y.com",
            ],
        )
        self.assertTrue(result.get("sent"))
        # Verify the message was sent (cc handled in MIME message)
        mock_server.send_message.assert_called_once()
        msg = mock_server.send_message.call_args[0][0]
        self.assertEqual(msg["Cc"], "z@y.com")

    @patch("main.smtplib.SMTP")
    def test_send_smtp_no_starttls_on_port_25(self, mock_smtp_cls):
        os.environ["SMTP_HOST"] = "localhost"
        os.environ["SMTP_PORT"] = "25"

        mock_server = MagicMock()
        mock_smtp_cls.return_value.__enter__ = MagicMock(return_value=mock_server)
        mock_smtp_cls.return_value.__exit__ = MagicMock(return_value=False)

        result = run(
            "send",
            ["--to", "x@y.com", "--subject", "hi", "--body", "hello"],
        )
        self.assertTrue(result.get("sent"))
        mock_server.starttls.assert_not_called()

    @patch("main.smtplib.SMTP")
    def test_send_smtp_no_login_without_credentials(self, mock_smtp_cls):
        os.environ["SMTP_HOST"] = "localhost"
        os.environ["SMTP_PORT"] = "25"

        mock_server = MagicMock()
        mock_smtp_cls.return_value.__enter__ = MagicMock(return_value=mock_server)
        mock_smtp_cls.return_value.__exit__ = MagicMock(return_value=False)

        result = run(
            "send",
            ["--to", "x@y.com", "--subject", "hi", "--body", "hello"],
        )
        self.assertTrue(result.get("sent"))
        mock_server.login.assert_not_called()

    @patch("main._gmail_request")
    def test_send_gmail_success(self, mock_req):
        os.environ["GMAIL_ACCESS_TOKEN"] = "test-token"
        mock_req.return_value = {"id": "msg123", "threadId": "t1", "labelIds": ["SENT"]}

        result = run(
            "send",
            [
                "--to", "x@y.com",
                "--subject", "hi",
                "--body", "hello",
                "--provider", "gmail",
            ],
        )
        self.assertTrue(result.get("sent"))
        self.assertEqual(result["provider"], "gmail")
        self.assertEqual(result["id"], "msg123")
        mock_req.assert_called_once()

    @patch("main._outlook_request")
    def test_send_outlook_success(self, mock_req):
        os.environ["MICROSOFT_ACCESS_TOKEN"] = "test-token"
        mock_req.return_value = {}

        result = run(
            "send",
            [
                "--to", "x@y.com",
                "--subject", "hi",
                "--body", "hello",
                "--provider", "outlook",
            ],
        )
        self.assertTrue(result.get("sent"))
        self.assertEqual(result["provider"], "outlook")
        mock_req.assert_called_once()

    @patch("main._outlook_request")
    def test_send_outlook_with_cc(self, mock_req):
        os.environ["MICROSOFT_ACCESS_TOKEN"] = "test-token"
        mock_req.return_value = {}

        result = run(
            "send",
            [
                "--to", "x@y.com",
                "--subject", "hi",
                "--body", "hello",
                "--cc", "z@y.com",
                "--provider", "outlook",
            ],
        )
        self.assertTrue(result.get("sent"))
        # Verify cc was included in the request payload
        call_kwargs = mock_req.call_args
        payload = call_kwargs[1]["data"] if "data" in (call_kwargs[1] or {}) else call_kwargs[0][2]
        self.assertIn("ccRecipients", payload["message"])


# ---------------------------------------------------------------------------
# Search command
# ---------------------------------------------------------------------------


class TestSearchCommand(unittest.TestCase):
    def setUp(self):
        _clear_provider_env()

    def tearDown(self):
        _clear_provider_env()

    def test_search_no_provider(self):
        result = run("search", ["--query", "test"])
        self.assertIn("error", result)
        self.assertIn("hint", result)

    def test_search_smtp_only(self):
        os.environ["SMTP_HOST"] = "localhost"
        result = run("search", ["--query", "test"])
        self.assertIn("error", result)
        self.assertIn("search requires gmail or outlook provider", result["error"])

    @patch("main._gmail_request")
    def test_search_gmail(self, mock_req):
        os.environ["GMAIL_ACCESS_TOKEN"] = "tok"
        # First call: list messages, subsequent calls: get each message
        mock_req.side_effect = [
            {"messages": [{"id": "m1"}, {"id": "m2"}]},
            {
                "id": "m1",
                "snippet": "Hello",
                "labelIds": ["UNREAD"],
                "payload": {
                    "headers": [
                        {"name": "From", "value": "a@b.com"},
                        {"name": "Subject", "value": "Test"},
                        {"name": "Date", "value": "2026-03-25"},
                    ],
                    "parts": [],
                },
            },
            {
                "id": "m2",
                "snippet": "World",
                "labelIds": [],
                "payload": {
                    "headers": [
                        {"name": "From", "value": "c@d.com"},
                        {"name": "Subject", "value": "Re: Test"},
                        {"name": "Date", "value": "2026-03-26"},
                    ],
                    "parts": [],
                },
            },
        ]

        result = run("search", ["--query", "test", "--provider", "gmail"])
        self.assertEqual(result["provider"], "gmail")
        self.assertEqual(result["count"], 2)
        self.assertEqual(result["emails"][0]["id"], "m1")
        self.assertTrue(result["emails"][0]["unread"])
        self.assertFalse(result["emails"][1]["unread"])

    @patch("main._outlook_request")
    def test_search_outlook(self, mock_req):
        os.environ["MICROSOFT_ACCESS_TOKEN"] = "tok"
        mock_req.return_value = {
            "value": [
                {
                    "id": "o1",
                    "from": {"emailAddress": {"address": "a@b.com"}},
                    "subject": "Test",
                    "bodyPreview": "Hello",
                    "receivedDateTime": "2026-03-25T10:00:00Z",
                    "isRead": False,
                    "toRecipients": [],
                },
            ],
        }

        result = run("search", ["--query", "test", "--provider", "outlook"])
        self.assertEqual(result["provider"], "outlook")
        self.assertEqual(result["count"], 1)
        self.assertEqual(result["emails"][0]["id"], "o1")
        self.assertTrue(result["emails"][0]["unread"])


# ---------------------------------------------------------------------------
# List command
# ---------------------------------------------------------------------------


class TestListCommand(unittest.TestCase):
    def setUp(self):
        _clear_provider_env()

    def tearDown(self):
        _clear_provider_env()

    def test_list_no_provider(self):
        result = run("list", [])
        self.assertIn("error", result)
        self.assertIn("hint", result)

    def test_list_smtp_only(self):
        os.environ["SMTP_HOST"] = "localhost"
        result = run("list", [])
        self.assertIn("error", result)
        self.assertIn("list requires gmail or outlook provider", result["error"])

    @patch("main._gmail_request")
    def test_list_gmail(self, mock_req):
        os.environ["GMAIL_ACCESS_TOKEN"] = "tok"
        mock_req.side_effect = [
            {"messages": [{"id": "m1"}]},
            {
                "id": "m1",
                "snippet": "Recent",
                "labelIds": [],
                "payload": {
                    "headers": [
                        {"name": "From", "value": "x@y.com"},
                        {"name": "Subject", "value": "Hello"},
                        {"name": "Date", "value": "2026-03-25"},
                    ],
                    "parts": [],
                },
            },
        ]
        result = run("list", ["--max-results", "5", "--provider", "gmail"])
        self.assertEqual(result["provider"], "gmail")
        self.assertEqual(result["count"], 1)

    @patch("main._outlook_request")
    def test_list_outlook_unread(self, mock_req):
        os.environ["MICROSOFT_ACCESS_TOKEN"] = "tok"
        mock_req.return_value = {"value": []}

        result = run("list", ["--unread", "--provider", "outlook"])
        self.assertEqual(result["provider"], "outlook")
        self.assertEqual(result["count"], 0)
        # Verify the filter was applied in the URL
        call_url = mock_req.call_args[0][0]
        self.assertIn("isRead", call_url)


# ---------------------------------------------------------------------------
# Read command
# ---------------------------------------------------------------------------


class TestReadCommand(unittest.TestCase):
    def setUp(self):
        _clear_provider_env()

    def tearDown(self):
        _clear_provider_env()

    def test_read_no_provider(self):
        result = run("read", ["--id", "msg123"])
        self.assertIn("error", result)
        self.assertIn("hint", result)

    def test_read_smtp_only(self):
        os.environ["SMTP_HOST"] = "localhost"
        result = run("read", ["--id", "msg123"])
        self.assertIn("error", result)
        self.assertIn("read requires gmail or outlook provider", result["error"])

    @patch("main._gmail_request")
    def test_read_gmail(self, mock_req):
        os.environ["GMAIL_ACCESS_TOKEN"] = "tok"
        body_b64 = base64.urlsafe_b64encode(b"Hello world").decode()
        mock_req.return_value = {
            "id": "msg123",
            "snippet": "Hello world",
            "labelIds": ["UNREAD", "INBOX"],
            "payload": {
                "headers": [
                    {"name": "From", "value": "boss@co.com"},
                    {"name": "To", "value": "me@co.com"},
                    {"name": "Subject", "value": "Urgent"},
                    {"name": "Date", "value": "2026-03-25T10:00:00Z"},
                ],
                "body": {"data": body_b64},
                "parts": [
                    {
                        "mimeType": "application/pdf",
                        "filename": "doc.pdf",
                        "body": {"size": 52400},
                    },
                ],
            },
        }

        result = run("read", ["--id", "msg123", "--provider", "gmail"])
        self.assertEqual(result["id"], "msg123")
        self.assertEqual(result["from"], "boss@co.com")
        self.assertEqual(result["to"], ["me@co.com"])
        self.assertEqual(result["subject"], "Urgent")
        self.assertEqual(result["body"], "Hello world")
        self.assertTrue(result["unread"])
        self.assertEqual(len(result["attachments"]), 1)
        self.assertEqual(result["attachments"][0]["name"], "doc.pdf")

    @patch("main._outlook_request")
    def test_read_outlook(self, mock_req):
        os.environ["MICROSOFT_ACCESS_TOKEN"] = "tok"
        mock_req.return_value = {
            "id": "o123",
            "from": {"emailAddress": {"address": "boss@co.com"}},
            "toRecipients": [{"emailAddress": {"address": "me@co.com"}}],
            "subject": "Urgent",
            "body": {"contentType": "Text", "content": "Please review"},
            "bodyPreview": "Please review",
            "receivedDateTime": "2026-03-25T10:00:00Z",
            "isRead": False,
            "attachments": [{"name": "doc.pdf", "size": 52400}],
        }

        result = run("read", ["--id", "o123", "--provider", "outlook"])
        self.assertEqual(result["id"], "o123")
        self.assertEqual(result["from"], "boss@co.com")
        self.assertEqual(result["to"], ["me@co.com"])
        self.assertEqual(result["subject"], "Urgent")
        self.assertEqual(result["body"], "Please review")
        self.assertTrue(result["unread"])
        self.assertEqual(len(result["attachments"]), 1)


# ---------------------------------------------------------------------------
# Parser edge-cases
# ---------------------------------------------------------------------------


class TestParseGmailMessage(unittest.TestCase):
    def test_empty_payload(self):
        result = _parse_gmail_message({"id": "x", "payload": {}, "labelIds": []})
        self.assertEqual(result["id"], "x")
        self.assertEqual(result["body"], "")
        self.assertEqual(result["attachments"], [])

    def test_body_from_parts(self):
        body_b64 = base64.urlsafe_b64encode(b"Part body").decode()
        msg = {
            "id": "x",
            "snippet": "snip",
            "labelIds": [],
            "payload": {
                "headers": [],
                "body": {},
                "parts": [
                    {"mimeType": "text/html", "body": {"data": "ignored"}},
                    {"mimeType": "text/plain", "body": {"data": body_b64}},
                ],
            },
        }
        result = _parse_gmail_message(msg)
        self.assertEqual(result["body"], "Part body")


class TestParseOutlookMessage(unittest.TestCase):
    def test_minimal_message(self):
        result = _parse_outlook_message({"id": "o1"})
        self.assertEqual(result["id"], "o1")
        self.assertEqual(result["from"], "")
        self.assertEqual(result["to"], [])
        self.assertEqual(result["body"], "")
        self.assertFalse(result["unread"])


# ---------------------------------------------------------------------------
# API error propagation
# ---------------------------------------------------------------------------


class TestApiErrors(unittest.TestCase):
    def setUp(self):
        _clear_provider_env()

    def tearDown(self):
        _clear_provider_env()

    @patch("main._gmail_request")
    def test_gmail_send_api_error(self, mock_req):
        os.environ["GMAIL_ACCESS_TOKEN"] = "tok"
        mock_req.return_value = {"error": "Invalid token", "status": 401}

        result = run(
            "send",
            ["--to", "x@y.com", "--subject", "hi", "--body", "test", "--provider", "gmail"],
        )
        self.assertIn("error", result)
        self.assertEqual(result["status"], 401)

    @patch("main._outlook_request")
    def test_outlook_search_api_error(self, mock_req):
        os.environ["MICROSOFT_ACCESS_TOKEN"] = "tok"
        mock_req.return_value = {"error": "Forbidden", "status": 403}

        result = run("search", ["--query", "test", "--provider", "outlook"])
        self.assertIn("error", result)
        self.assertEqual(result["status"], 403)


if __name__ == "__main__":
    unittest.main()
