"""search — Web and image search for Claw OS.

Google Custom Search with Brave fallback.  Uses only stdlib (urllib).
"""

import json
import os
import urllib.error
import urllib.parse
import urllib.request

VERSION = os.environ.get("COS_VERSION", "0.1.0")
USER_AGENT = "cos/" + VERSION
TIMEOUT = 15
MAX_RESULTS_DEFAULT = 5
MAX_RESULTS_LIMIT = 10

# ---------------------------------------------------------------------------
# Credential helpers
# ---------------------------------------------------------------------------

def _google_credentials():
    """Return (api_key, engine_id) or (None, None)."""
    api_key = os.environ.get("GOOGLE_SEARCH_API_KEY")
    engine_id = os.environ.get("GOOGLE_SEARCH_ENGINE_ID")
    if api_key and engine_id:
        return api_key, engine_id
    return None, None


def _brave_credential():
    """Return api_key or None."""
    return os.environ.get("BRAVE_SEARCH_API_KEY") or None


def _pick_provider(preferred=None):
    """Choose a search provider.  Returns (provider, config) or (None, error_dict)."""
    google_key, google_cx = _google_credentials()
    brave_key = _brave_credential()

    if preferred == "google":
        if google_key:
            return "google", {"key": google_key, "cx": google_cx}
        return None, {"error": "Google credentials not configured",
                      "hint": ("Store API credentials: "
                               "cos credential store GOOGLE_SEARCH_API_KEY <key> --tier 1 && "
                               "cos credential store GOOGLE_SEARCH_ENGINE_ID <id> --tier 1")}

    if preferred == "brave":
        if brave_key:
            return "brave", {"key": brave_key}
        return None, {"error": "Brave credentials not configured",
                      "hint": ("Store API credentials: "
                               "cos credential store BRAVE_SEARCH_API_KEY <key> --tier 1")}

    # No preference — Google first, then Brave
    if google_key:
        return "google", {"key": google_key, "cx": google_cx}
    if brave_key:
        return "brave", {"key": brave_key}

    return None, {
        "error": "No search provider configured",
        "hint": ("Store API credentials: "
                 "cos credential store GOOGLE_SEARCH_API_KEY <key> --tier 1 && "
                 "cos credential store GOOGLE_SEARCH_ENGINE_ID <id> --tier 1. "
                 "Or: cos credential store BRAVE_SEARCH_API_KEY <key> --tier 1"),
    }


# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

def _parse_args(args):
    """Parse [query_words...] [--max-results N] [--provider google|brave].

    Returns (query, max_results, provider) or (None, None, error_dict).
    """
    query_parts = []
    max_results = MAX_RESULTS_DEFAULT
    provider = None
    i = 0
    while i < len(args):
        if args[i] == "--max-results":
            if i + 1 >= len(args):
                return None, None, None, {"error": "--max-results requires a value"}
            try:
                max_results = int(args[i + 1])
            except ValueError:
                return None, None, None, {"error": f"invalid --max-results value: {args[i + 1]}"}
            if max_results < 1:
                max_results = 1
            elif max_results > MAX_RESULTS_LIMIT:
                max_results = MAX_RESULTS_LIMIT
            i += 2
        elif args[i] == "--provider":
            if i + 1 >= len(args):
                return None, None, None, {"error": "--provider requires a value (google|brave)"}
            provider = args[i + 1]
            if provider not in ("google", "brave"):
                return None, None, None, {"error": f"unknown provider: {provider} (choose google or brave)"}
            i += 2
        else:
            query_parts.append(args[i])
            i += 1

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

    if provider == "google":
        result = _google_web(query, max_results, config)
        # Auto-fallback to Brave if Google fails
        if "error" in result and _brave_credential():
            brave_key = _brave_credential()
            result = _brave_web(query, max_results, {"key": brave_key})
        return result
    else:
        return _brave_web(query, max_results, config)


def cmd_image(args):
    """Search for images."""
    query, max_results, preferred, parse_err = _parse_args(args)
    if parse_err:
        return parse_err

    provider, config = _pick_provider(preferred)
    if provider is None:
        return config  # config is the error dict

    if provider == "google":
        result = _google_image(query, max_results, config)
        # Auto-fallback to Brave if Google fails
        if "error" in result and _brave_credential():
            brave_key = _brave_credential()
            result = _brave_image(query, max_results, {"key": brave_key})
        return result
    else:
        return _brave_image(query, max_results, config)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def _schema():
    return {
        "web": {
            "description": "Search the web for information (Google Custom Search with Brave fallback)",
            "parameters": [
                {"name": "query", "type": "string", "required": True, "description": "Search query words", "kind": "positional"},
                {"name": "--max-results", "type": "integer", "required": False, "description": "Maximum results to return (1-10)", "kind": "flag", "default": 5},
                {"name": "--provider", "type": "string", "required": False, "description": "Search provider: google or brave", "kind": "flag"},
            ],
            "example": "cos app search web 'rust programming language' --max-results 5",
        },
        "image": {
            "description": "Search for images (Google Custom Search with Brave fallback)",
            "parameters": [
                {"name": "query", "type": "string", "required": True, "description": "Image search query words", "kind": "positional"},
                {"name": "--max-results", "type": "integer", "required": False, "description": "Maximum results to return (1-10)", "kind": "flag", "default": 5},
                {"name": "--provider", "type": "string", "required": False, "description": "Search provider: google or brave", "kind": "flag"},
            ],
            "example": "cos app search image 'cute cats' --max-results 3",
        },
    }


def run(command, args):
    """Called by cos router."""
    if command == "__schema__":
        return _schema()
    commands = {
        "web": cmd_web,
        "image": cmd_image,
    }
    handler = commands.get(command)
    if not handler:
        return {"error": f"unknown command: {command}"}
    return handler(args)
