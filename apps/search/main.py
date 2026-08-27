"""search — Web and image search for Claw OS.

Google Custom Search with Brave fallback.  Uses only stdlib (urllib).
"""

import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.credentials import load_credential  # noqa: E402
from cos_runtime import memory, policy  # noqa: E402

VERSION = os.environ.get("COS_VERSION", "0.1.0")
USER_AGENT = "cos/" + VERSION
TIMEOUT = 15
MAX_RESULTS_DEFAULT = 5
MAX_RESULTS_LIMIT = 10

GOOGLE_HOST = "www.googleapis.com"
BRAVE_HOST = "api.search.brave.com"

# ---------------------------------------------------------------------------
# Credential helpers
# ---------------------------------------------------------------------------

def _google_credentials():
    """Return (api_key, engine_id) or (None, None)."""
    api_key = os.environ.get("GOOGLE_SEARCH_API_KEY")
    if not api_key:
        api_key, _ = load_credential("GOOGLE_SEARCH_API_KEY")
    engine_id = os.environ.get("GOOGLE_SEARCH_ENGINE_ID")
    if not engine_id:
        engine_id, _ = load_credential("GOOGLE_SEARCH_ENGINE_ID")
    if api_key and engine_id:
        return api_key, engine_id
    return None, None


def _brave_credential():
    """Return api_key or None."""
    value = os.environ.get("BRAVE_SEARCH_API_KEY")
    if value:
        return value
    return load_credential("BRAVE_SEARCH_API_KEY")[0]


def _pick_provider(preferred=None):
    """Choose a search provider.  Returns (provider, config) or (None, error_dict)."""
    if preferred not in {None, "google", "brave"}:
        return None, {
            "error": "invalid search provider; expected google or brave",
            "retryable": False,
        }
    if preferred == "google":
        google_key, google_cx = _google_credentials()
        if google_key and google_cx:
            return "google", {"key": google_key, "cx": google_cx}
        return None, {
            "error": "Google Search credentials are not configured",
            "auth_required": True,
            "retryable": False,
            "required_credentials": [
                "GOOGLE_SEARCH_API_KEY",
                "GOOGLE_SEARCH_ENGINE_ID",
            ],
        }

    if preferred == "brave":
        brave_key = _brave_credential()
        if brave_key:
            return "brave", {"key": brave_key}
        return None, {
            "error": "Brave Search credentials are not configured",
            "auth_required": True,
            "retryable": False,
            "required_credentials": ["BRAVE_SEARCH_API_KEY"],
        }

    # No preference — Google first, then Brave
    google_key, google_cx = _google_credentials()
    if google_key and google_cx:
        return "google", {"key": google_key, "cx": google_cx}
    brave_key = _brave_credential()
    if brave_key:
        return "brave", {"key": brave_key}

    return None, {
        "error": "No search provider configured",
        "auth_required": True,
        "retryable": False,
        "providers": {
            "google": ["GOOGLE_SEARCH_API_KEY", "GOOGLE_SEARCH_ENGINE_ID"],
            "brave": ["BRAVE_SEARCH_API_KEY"],
        },
    }


# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

def _parse_args(args):
    """Parse [query_words...] [--max-results N] [--provider google|brave].

    Returns (query, max_results, provider) or (None, None, error_dict).
    """
    from canonical_argv import parse_canonical_argv
    try:
        query_parts, options = parse_canonical_argv(
            args, value_flags={"max-results", "provider"}
        )
    except ValueError as error:
        return None, None, None, {"error": str(error)}

    provider = options.get("provider")
    if provider is not None and provider not in ("google", "brave"):
        return None, None, None, {
            "error": f"unknown provider: {provider} (choose google or brave)"
        }
    try:
        max_results = int(options.get("max_results", MAX_RESULTS_DEFAULT))
    except ValueError:
        return None, None, None, {
            "error": f"invalid --max-results value: {options['max_results']}"
        }
    max_results = min(MAX_RESULTS_LIMIT, max(1, max_results))

    query = " ".join(query_parts)
    if not query:
        return None, None, None, {"error": "missing search query"}

    return query, max_results, provider, None


# ---------------------------------------------------------------------------
# HTTP helper
# ---------------------------------------------------------------------------

def _request_json(url, headers=None):
    """GET *url* and return parsed JSON, or an error dict."""
    hdrs = {"User-Agent": USER_AGENT}
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(url, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT) as resp:
            body = resp.read().decode("utf-8", errors="replace")
            return json.loads(body), None
    except urllib.error.HTTPError as e:
        detail = ""
        try:
            detail = e.read().decode("utf-8", errors="replace")
        except Exception:
            pass
        return None, {"error": f"HTTP {e.code}: {detail or str(e)}", "status": e.code}
    except urllib.error.URLError as e:
        return None, {"error": str(e.reason)}
    except Exception as e:
        return None, {"error": str(e)}


# ---------------------------------------------------------------------------
# Google Custom Search
# ---------------------------------------------------------------------------

def _google_web(query, max_results, config):
    params = urllib.parse.urlencode({
        "q": query,
        "key": config["key"],
        "cx": config["cx"],
        "num": max_results,
    })
    url = f"https://www.googleapis.com/customsearch/v1?{params}"
    data, err = _request_json(url)
    if err:
        return err

    results = []
    for item in data.get("items", []):
        results.append({
            "title": item.get("title", ""),
            "url": item.get("link", ""),
            "snippet": item.get("snippet", ""),
        })

    total_str = data.get("searchInformation", {}).get("totalResults", "0")
    try:
        total = int(total_str)
    except (ValueError, TypeError):
        total = 0

    return {
        "query": query,
        "provider": "google",
        "results": results,
        "count": len(results),
        "total_results": total,
    }


def _google_image(query, max_results, config):
    params = urllib.parse.urlencode({
        "q": query,
        "key": config["key"],
        "cx": config["cx"],
        "searchType": "image",
        "num": max_results,
    })
    url = f"https://www.googleapis.com/customsearch/v1?{params}"
    data, err = _request_json(url)
    if err:
        return err

    results = []
    for item in data.get("items", []):
        img = item.get("image", {})
        results.append({
            "title": item.get("title", ""),
            "url": item.get("link", ""),
            "thumbnail": item.get("image", {}).get("thumbnailLink", ""),
            "width": img.get("width", 0),
            "height": img.get("height", 0),
            "source": item.get("displayLink", ""),
        })

    return {
        "query": query,
        "provider": "google",
        "results": results,
        "count": len(results),
    }


# ---------------------------------------------------------------------------
# Brave Search
# ---------------------------------------------------------------------------

def _brave_web(query, max_results, config):
    params = urllib.parse.urlencode({"q": query, "count": max_results})
    url = f"https://api.search.brave.com/res/v1/web/search?{params}"
    headers = {
        "Accept": "application/json",
        "X-Subscription-Token": config["key"],
    }
    data, err = _request_json(url, headers=headers)
    if err:
        return err

    results = []
    for item in data.get("web", {}).get("results", []):
        results.append({
            "title": item.get("title", ""),
            "url": item.get("url", ""),
            "snippet": item.get("description", ""),
        })

    total = data.get("web", {}).get("totalResults", 0)
    if isinstance(total, str):
        try:
            total = int(total)
        except (ValueError, TypeError):
            total = 0

    return {
        "query": query,
        "provider": "brave",
        "results": results,
        "count": len(results),
        "total_results": total,
    }


def _brave_image(query, max_results, config):
    params = urllib.parse.urlencode({"q": query, "count": max_results})
    url = f"https://api.search.brave.com/res/v1/images/search?{params}"
    headers = {
        "Accept": "application/json",
        "X-Subscription-Token": config["key"],
    }
    data, err = _request_json(url, headers=headers)
    if err:
        return err

    results = []
    for item in data.get("results", []):
        props = item.get("properties", {})
        results.append({
            "title": item.get("title", ""),
            "url": item.get("url", ""),
            "thumbnail": item.get("thumbnail", {}).get("src", ""),
            "width": props.get("width", 0),
            "height": props.get("height", 0),
            "source": item.get("source", ""),
        })

    return {
        "query": query,
        "provider": "brave",
        "results": results,
        "count": len(results),
    }


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def cmd_web(args):
    """Search the web for information."""
    query, max_results, preferred, parse_err = _parse_args(args)
    if parse_err:
        return parse_err

    provider, config = _pick_provider(preferred)
    if provider is None:
        return config  # config is the error dict

    host = GOOGLE_HOST if provider == "google" else BRAVE_HOST
    policy.require("net.dial", host=host)

    if provider == "google":
        result = _google_web(query, max_results, config)
    else:
        result = _brave_web(query, max_results, config)
    if isinstance(result, dict):
        result.setdefault("provider", provider)
    _remember_search("web", query, result)
    return result


def cmd_image(args):
    """Search for images."""
    query, max_results, preferred, parse_err = _parse_args(args)
    if parse_err:
        return parse_err

    provider, config = _pick_provider(preferred)
    if provider is None:
        return config  # config is the error dict

    host = GOOGLE_HOST if provider == "google" else BRAVE_HOST
    policy.require("net.dial", host=host)

    if provider == "google":
        result = _google_image(query, max_results, config)
    else:
        result = _brave_image(query, max_results, config)
    if isinstance(result, dict):
        result.setdefault("provider", provider)
    _remember_search("image", query, result)
    return result


def _remember_search(kind, query, result):
    try:
        if not isinstance(result, dict) or "error" in result:
            return
        provider = result.get("provider") or "unknown"
        count = result.get("count") or len(result.get("results") or [])
        memory.remember(
            source="search",
            text=f"{kind.capitalize()} search '{query}' via {provider} → {count} result(s)",
            kind="event",
            tags=["search", kind, provider],
            link=f"cos app search {kind} --query \"{query}\"",
        )
    except memory.MemoryError:
        pass


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def run(command, args):
    """Called by cos router."""
    commands = {
        "web": cmd_web,
        "image": cmd_image,
    }
    handler = commands.get(command)
    if not handler:
        return {"error": f"unknown command: {command}"}
    try:
        return handler(args)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}
