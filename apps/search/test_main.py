"""Tests for the search app."""

import json
import os
import sys
import urllib.error
import urllib.request
from unittest import mock

# Ensure the app directory is importable.
sys.path.insert(0, os.path.dirname(__file__))

from main import run  # noqa: E402


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _clear_credentials():
    """Remove all search credentials from env."""
    for key in [
        "GOOGLE_SEARCH_API_KEY",
        "GOOGLE_SEARCH_ENGINE_ID",
        "BRAVE_SEARCH_API_KEY",
    ]:
        os.environ.pop(key, None)


def _set_google_credentials():
    os.environ["GOOGLE_SEARCH_API_KEY"] = "fake-google-key"
    os.environ["GOOGLE_SEARCH_ENGINE_ID"] = "fake-cx"


def _set_brave_credentials():
    os.environ["BRAVE_SEARCH_API_KEY"] = "fake-brave-key"


# ---------------------------------------------------------------------------
# Error cases — no credentials
# ---------------------------------------------------------------------------

def test_web_no_credentials():
    """Without API keys, should return helpful error."""
    _clear_credentials()
    result = run("web", ["test query"])
    assert "error" in result
    assert "hint" in result
    assert "No search provider configured" in result["error"]


def test_image_no_credentials():
    """Without API keys, image search returns helpful error."""
    _clear_credentials()
    result = run("image", ["test query"])
    assert "error" in result
    assert "hint" in result


# ---------------------------------------------------------------------------
# Error cases — missing query
# ---------------------------------------------------------------------------

def test_web_missing_query():
    result = run("web", [])
    assert "error" in result
    assert "missing" in result["error"].lower() or "query" in result["error"].lower()


def test_image_missing_query():
    result = run("image", [])
    assert "error" in result


# ---------------------------------------------------------------------------
# Unknown command
# ---------------------------------------------------------------------------

def test_unknown_command():
    result = run("bogus", [])
    assert "error" in result
    assert "unknown command" in result["error"]


# ---------------------------------------------------------------------------
# Argument parsing edge cases
# ---------------------------------------------------------------------------

def test_max_results_missing_value():
    _clear_credentials()
    _set_google_credentials()
    result = run("web", ["query", "--max-results"])
    assert "error" in result


def test_max_results_invalid_value():
    _clear_credentials()
    _set_google_credentials()
    result = run("web", ["query", "--max-results", "abc"])
    assert "error" in result


def test_provider_missing_value():
    _clear_credentials()
    _set_google_credentials()
    result = run("web", ["query", "--provider"])
    assert "error" in result


def test_provider_unknown_value():
    _clear_credentials()
    _set_google_credentials()
    result = run("web", ["query", "--provider", "bing"])
    assert "error" in result
    assert "unknown provider" in result["error"]


def test_provider_explicit_not_configured():
    """Requesting a provider that isn't configured returns a clear error."""
    _clear_credentials()
    result = run("web", ["query", "--provider", "brave"])
    assert "error" in result
    assert "Brave" in result["error"]


# ---------------------------------------------------------------------------
# Successful Google web search (mocked)
# ---------------------------------------------------------------------------

def _mock_google_web_response():
    """Return a bytes payload mimicking Google Custom Search JSON."""
    return json.dumps({
        "searchInformation": {"totalResults": "12345"},
        "items": [
            {
                "title": "Example Result",
                "link": "https://example.com",
                "snippet": "An example snippet.",
            }
        ],
    }).encode()


def test_web_google_success():
    _clear_credentials()
    _set_google_credentials()

    fake_resp = mock.MagicMock()
    fake_resp.read.return_value = _mock_google_web_response()
    fake_resp.__enter__ = mock.MagicMock(return_value=fake_resp)
    fake_resp.__exit__ = mock.MagicMock(return_value=False)

    with mock.patch("urllib.request.urlopen", return_value=fake_resp):
        result = run("web", ["example query"])

    assert "error" not in result
    assert result["provider"] == "google"
    assert result["query"] == "example query"
    assert result["count"] == 1
    assert result["total_results"] == 12345
    assert result["results"][0]["title"] == "Example Result"
    assert result["results"][0]["url"] == "https://example.com"


# ---------------------------------------------------------------------------
# Successful Brave web search (mocked)
# ---------------------------------------------------------------------------

def _mock_brave_web_response():
    return json.dumps({
        "web": {
            "totalResults": 999,
            "results": [
                {
                    "title": "Brave Result",
                    "url": "https://brave.com",
                    "description": "A brave snippet.",
                }
            ],
        }
    }).encode()


def test_web_brave_success():
    _clear_credentials()
    _set_brave_credentials()

    fake_resp = mock.MagicMock()
    fake_resp.read.return_value = _mock_brave_web_response()
    fake_resp.__enter__ = mock.MagicMock(return_value=fake_resp)
    fake_resp.__exit__ = mock.MagicMock(return_value=False)

    with mock.patch("urllib.request.urlopen", return_value=fake_resp):
        result = run("web", ["brave query"])

    assert "error" not in result
    assert result["provider"] == "brave"
    assert result["query"] == "brave query"
    assert result["results"][0]["url"] == "https://brave.com"


# ---------------------------------------------------------------------------
# Google image search (mocked)
# ---------------------------------------------------------------------------

def _mock_google_image_response():
    return json.dumps({
        "items": [
            {
                "title": "Cat Photo",
                "link": "https://example.com/cat.jpg",
                "displayLink": "example.com",
                "image": {
                    "thumbnailLink": "https://example.com/cat_thumb.jpg",
                    "width": 1920,
                    "height": 1080,
                },
            }
        ],
    }).encode()


def test_image_google_success():
    _clear_credentials()
    _set_google_credentials()

    fake_resp = mock.MagicMock()
    fake_resp.read.return_value = _mock_google_image_response()
    fake_resp.__enter__ = mock.MagicMock(return_value=fake_resp)
    fake_resp.__exit__ = mock.MagicMock(return_value=False)

    with mock.patch("urllib.request.urlopen", return_value=fake_resp):
        result = run("image", ["cute cats"])

    assert "error" not in result
    assert result["provider"] == "google"
    assert result["count"] == 1
    assert result["results"][0]["width"] == 1920
    assert result["results"][0]["source"] == "example.com"


# ---------------------------------------------------------------------------
# Google fails → Brave fallback (mocked)
# ---------------------------------------------------------------------------

def test_web_google_fails_brave_fallback():
    """When Google returns an HTTP error and Brave is configured, fall back."""
    _clear_credentials()
    _set_google_credentials()
    _set_brave_credentials()

    call_count = {"n": 0}

    def _side_effect(req, timeout=None):
        call_count["n"] += 1
        if call_count["n"] == 1:
            # First call (Google) → fail
            raise urllib.error.HTTPError(
                url=req.full_url, code=403, msg="Forbidden",
                hdrs={}, fp=mock.MagicMock(read=lambda: b"forbidden"),
            )
        # Second call (Brave) → succeed
        fake_resp = mock.MagicMock()
        fake_resp.read.return_value = _mock_brave_web_response()
        fake_resp.__enter__ = mock.MagicMock(return_value=fake_resp)
        fake_resp.__exit__ = mock.MagicMock(return_value=False)
        return fake_resp

    with mock.patch("urllib.request.urlopen", side_effect=_side_effect):
        result = run("web", ["fallback test"])

    assert "error" not in result
    assert result["provider"] == "brave"


# ---------------------------------------------------------------------------
# Multi-word query parsing
# ---------------------------------------------------------------------------

def test_multiword_query():
    _clear_credentials()
    _set_google_credentials()

    fake_resp = mock.MagicMock()
    fake_resp.read.return_value = _mock_google_web_response()
    fake_resp.__enter__ = mock.MagicMock(return_value=fake_resp)
    fake_resp.__exit__ = mock.MagicMock(return_value=False)

    with mock.patch("urllib.request.urlopen", return_value=fake_resp):
        result = run("web", ["rust", "async", "runtime", "--max-results", "3"])

    assert result["query"] == "rust async runtime"


# ---------------------------------------------------------------------------
# Max results clamping
# ---------------------------------------------------------------------------

def test_max_results_clamped_to_limit():
    _clear_credentials()
    _set_google_credentials()

    fake_resp = mock.MagicMock()
    fake_resp.read.return_value = _mock_google_web_response()
    fake_resp.__enter__ = mock.MagicMock(return_value=fake_resp)
    fake_resp.__exit__ = mock.MagicMock(return_value=False)

    with mock.patch("urllib.request.urlopen", return_value=fake_resp) as mock_open:
        run("web", ["test", "--max-results", "50"])
        # The URL should have num=10, not 50
        called_req = mock_open.call_args[0][0]
        assert "num=10" in called_req.full_url
