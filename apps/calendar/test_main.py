"""Tests for the calendar app."""

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(__file__))

from main import run


def _setup_local():
    """Point to a fresh temp database for testing."""
    tmp = tempfile.mkdtemp()
    os.environ["COS_DATA_DIR"] = tmp
    # Clear any provider tokens so we default to local
    for var in (
        "GOOGLE_CALENDAR_TOKEN", "GOOGLE_OAUTH_TOKEN",
        "MICROSOFT_ACCESS_TOKEN", "MICROSOFT_OAUTH_TOKEN",
    ):
        os.environ.pop(var, None)
    return tmp


class TestCreateAndList(unittest.TestCase):
    def setUp(self):
        _setup_local()

    def test_create_returns_event(self):
        result = run("create", ["--title", "Test Event", "--start", "2026-03-25T10:00:00Z"])
        self.assertTrue(result["created"])
        self.assertEqual(result["provider"], "local")
        self.assertTrue(result["event"]["id"].startswith("evt-"))
        self.assertEqual(result["event"]["title"], "Test Event")
        self.assertEqual(result["event"]["start"], "2026-03-25T10:00:00Z")

    def test_create_default_end_is_one_hour_later(self):
        result = run("create", ["--title", "Quick", "--start", "2026-03-25T10:00:00Z"])
        self.assertEqual(result["event"]["end"], "2026-03-25T11:00:00Z")

    def test_create_explicit_end(self):
        result = run("create", [
            "--title", "Long", "--start", "2026-03-25T10:00:00Z",
            "--end", "2026-03-25T14:00:00Z",
        ])
        self.assertEqual(result["event"]["end"], "2026-03-25T14:00:00Z")

    def test_create_with_description_and_location(self):
        result = run("create", [
            "--title", "Offsite", "--start", "2026-03-25T10:00:00Z",
            "--description", "Annual planning", "--location", "Room 42",
        ])
        self.assertEqual(result["event"]["description"], "Annual planning")
        self.assertEqual(result["event"]["location"], "Room 42")

    def test_list_returns_created_events(self):
        run("create", ["--title", "Event A", "--start", "2026-03-25T09:00:00Z"])
        run("create", ["--title", "Event B", "--start", "2026-03-25T14:00:00Z"])
        result = run("list", ["--from", "2026-03-25T00:00:00Z", "--to", "2026-03-26T00:00:00Z"])
        self.assertEqual(result["provider"], "local")
        self.assertEqual(result["count"], 2)
        titles = [e["title"] for e in result["events"]]
        self.assertIn("Event A", titles)
        self.assertIn("Event B", titles)

    def test_list_filters_by_range(self):
        run("create", ["--title", "In range", "--start", "2026-03-25T09:00:00Z"])
        run("create", ["--title", "Out of range", "--start", "2026-03-26T09:00:00Z"])
        result = run("list", ["--from", "2026-03-25T00:00:00Z", "--to", "2026-03-26T00:00:00Z"])
        self.assertEqual(result["count"], 1)
        self.assertEqual(result["events"][0]["title"], "In range")

    def test_list_sorted_by_start(self):
        run("create", ["--title", "Later", "--start", "2026-03-25T15:00:00Z"])
        run("create", ["--title", "Earlier", "--start", "2026-03-25T08:00:00Z"])
        result = run("list", ["--from", "2026-03-25T00:00:00Z", "--to", "2026-03-26T00:00:00Z"])
        self.assertEqual(result["events"][0]["title"], "Earlier")
        self.assertEqual(result["events"][1]["title"], "Later")


class TestCreateValidation(unittest.TestCase):
    def setUp(self):
        _setup_local()

    def test_create_missing_title(self):
        result = run("create", ["--start", "2026-03-25T10:00:00Z"])
        self.assertIn("error", result)

    def test_create_missing_start(self):
        result = run("create", ["--title", "No start"])
        self.assertIn("error", result)

    def test_create_missing_both(self):
        result = run("create", [])
        self.assertIn("error", result)


class TestListValidation(unittest.TestCase):
    def setUp(self):
        _setup_local()

    def test_list_missing_from(self):
        result = run("list", ["--to", "2026-03-26T00:00:00Z"])
        self.assertIn("error", result)

    def test_list_missing_to(self):
        result = run("list", ["--from", "2026-03-25T00:00:00Z"])
        self.assertIn("error", result)

    def test_list_missing_both(self):
        result = run("list", [])
        self.assertIn("error", result)


class TestDelete(unittest.TestCase):
    def setUp(self):
        _setup_local()

    def test_delete_existing(self):
        result = run("create", ["--title", "Delete me", "--start", "2026-03-25T10:00:00Z"])
        event_id = result["event"]["id"]
        result = run("delete", ["--id", event_id])
        self.assertTrue(result["deleted"])
        self.assertEqual(result["id"], event_id)
        # Verify it's gone
        result = run("list", ["--from", "2026-03-25T00:00:00Z", "--to", "2026-03-26T00:00:00Z"])
        self.assertEqual(result["count"], 0)

    def test_delete_nonexistent(self):
        result = run("delete", ["--id", "evt-0-missing"])
        self.assertIn("error", result)

    def test_delete_missing_id(self):
        result = run("delete", [])
        self.assertIn("error", result)


class TestUpdate(unittest.TestCase):
    def setUp(self):
        _setup_local()

    def test_update_title(self):
        result = run("create", ["--title", "Old title", "--start", "2026-03-25T10:00:00Z"])
        event_id = result["event"]["id"]
        result = run("update", ["--id", event_id, "--title", "New title"])
        self.assertTrue(result["updated"])
        self.assertEqual(result["event"]["title"], "New title")
        # Original start should be preserved
        self.assertEqual(result["event"]["start"], "2026-03-25T10:00:00Z")

    def test_update_multiple_fields(self):
        result = run("create", ["--title", "Meeting", "--start", "2026-03-25T10:00:00Z"])
        event_id = result["event"]["id"]
        result = run("update", [
            "--id", event_id,
            "--title", "Updated Meeting",
            "--description", "With notes",
            "--location", "Room 5",
        ])
        self.assertTrue(result["updated"])
        self.assertEqual(result["event"]["title"], "Updated Meeting")
        self.assertEqual(result["event"]["description"], "With notes")
        self.assertEqual(result["event"]["location"], "Room 5")

    def test_update_nonexistent(self):
        result = run("update", ["--id", "evt-0-missing", "--title", "Nope"])
        self.assertIn("error", result)

    def test_update_missing_id(self):
        result = run("update", ["--title", "No ID"])
        self.assertIn("error", result)


class TestToday(unittest.TestCase):
    def setUp(self):
        _setup_local()

    def test_today_returns_events_and_count(self):
        result = run("today", [])
        self.assertIn("events", result)
        self.assertIn("count", result)
        self.assertEqual(result["provider"], "local")

    def test_today_with_provider_arg(self):
        result = run("today", ["--provider", "local"])
        self.assertIn("events", result)
        self.assertEqual(result["provider"], "local")


class TestUnknownCommand(unittest.TestCase):
    def test_unknown_command(self):
        result = run("bogus", [])
        self.assertIn("error", result)

    def test_another_unknown(self):
        result = run("sync", [])
        self.assertIn("error", result)


class TestProviderErrors(unittest.TestCase):
    def setUp(self):
        _setup_local()

    def test_google_no_token_list(self):
        result = run("list", [
            "--provider", "google",
            "--from", "2026-03-25T00:00:00Z",
            "--to", "2026-03-26T00:00:00Z",
        ])
        self.assertIn("error", result)
        error_lower = result.get("error", "").lower()
        hint_lower = result.get("hint", "").lower()
        self.assertTrue(
            "token" in error_lower or "credential" in hint_lower,
            f"Expected token/credential hint, got: {result}",
        )

    def test_google_no_token_create(self):
        result = run("create", [
            "--provider", "google",
            "--title", "Test",
            "--start", "2026-03-25T10:00:00Z",
        ])
        self.assertIn("error", result)

    def test_google_no_token_update(self):
        result = run("update", [
            "--provider", "google",
            "--id", "some-id",
            "--title", "New",
        ])
        self.assertIn("error", result)

    def test_google_no_token_delete(self):
        result = run("delete", ["--provider", "google", "--id", "some-id"])
        self.assertIn("error", result)

    def test_outlook_no_token_list(self):
        result = run("list", [
            "--provider", "outlook",
            "--from", "2026-03-25T00:00:00Z",
            "--to", "2026-03-26T00:00:00Z",
        ])
        self.assertIn("error", result)
        error_lower = result.get("error", "").lower()
        hint_lower = result.get("hint", "").lower()
        self.assertTrue(
            "token" in error_lower or "credential" in hint_lower,
            f"Expected token/credential hint, got: {result}",
        )

    def test_outlook_no_token_create(self):
        result = run("create", [
            "--provider", "outlook",
            "--title", "Test",
            "--start", "2026-03-25T10:00:00Z",
        ])
        self.assertIn("error", result)

    def test_outlook_no_token_delete(self):
        result = run("delete", ["--provider", "outlook", "--id", "some-id"])
        self.assertIn("error", result)


class TestProviderDetection(unittest.TestCase):
    def setUp(self):
        _setup_local()

    def test_defaults_to_local(self):
        from main import _detect_provider
        self.assertEqual(_detect_provider(), "local")

    def test_explicit_overrides(self):
        from main import _detect_provider
        self.assertEqual(_detect_provider("google"), "google")
        self.assertEqual(_detect_provider("outlook"), "outlook")
        self.assertEqual(_detect_provider("local"), "local")

    def test_detects_google_token(self):
        from main import _detect_provider
        os.environ["GOOGLE_CALENDAR_TOKEN"] = "fake"
        try:
            self.assertEqual(_detect_provider(), "google")
        finally:
            del os.environ["GOOGLE_CALENDAR_TOKEN"]

    def test_detects_outlook_token(self):
        from main import _detect_provider
        os.environ["MICROSOFT_ACCESS_TOKEN"] = "fake"
        try:
            self.assertEqual(_detect_provider(), "outlook")
        finally:
            del os.environ["MICROSOFT_ACCESS_TOKEN"]


if __name__ == "__main__":
    unittest.main()
