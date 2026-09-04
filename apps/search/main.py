"""Explicit-provider web and image search for Claw OS."""

from __future__ import annotations

import json
import os
import shlex
import sys
import urllib.error
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.credentials import load_credential  # noqa: E402
from _shared.safe_http import open_url  # noqa: E402
from cos_runtime import memory, policy  # noqa: E402


VERSION = os.environ.get("COS_VERSION", "0.1.0")
USER_AGENT = "cos/" + VERSION
TIMEOUT = 15
MAX_RESULTS_DEFAULT = 5
MAX_RESULTS_LIMIT = 10
MAX_QUERY_CHARS = 2048
MAX_RESPONSE_BYTES = 5 * 1024 * 1024

GOOGLE_HOST = "www.googleapis.com"
BRAVE_HOST = "api.search.brave.com"
PROVIDERS = frozenset({"google", "brave"})


def _validate_request(
    provider: object,
    query: object,
    max_results: object,
) -> tuple[str, str, int]:
    if not isinstance(provider, str) or provider not in PROVIDERS:
        raise ValueError("provider must be google or brave")
    if (
        not isinstance(query, str)
        or not query.strip()
        or len(query) > MAX_QUERY_CHARS
    ):
        raise ValueError(
            f"query must be a non-empty string of at most {MAX_QUERY_CHARS} characters"
        )
    if (
        type(max_results) is not int
        or not 1 <= max_results <= MAX_RESULTS_LIMIT
    ):
        raise ValueError(f"max_results must be between 1 and {MAX_RESULTS_LIMIT}")
    return provider, query, max_results


def _credential(name: str) -> str:
    value, error = load_credential(name)
    if error is not None or value is None:
        raise RuntimeError(error or f"credential default/{name} has no value")
    return value


def _provider_config(provider: str) -> dict[str, str]:
    if provider == "google":
        return {
            "key": _credential("GOOGLE_SEARCH_API_KEY"),
            "cx": _credential("GOOGLE_SEARCH_ENGINE_ID"),
        }
    if provider == "brave":
        return {"key": _credential("BRAVE_SEARCH_API_KEY")}
    raise ValueError("provider must be google or brave")


def _read_json_response(response) -> dict[str, object]:
    body = response.read(MAX_RESPONSE_BYTES + 1)
    if len(body) > MAX_RESPONSE_BYTES:
        raise RuntimeError(
            f"search provider response exceeds {MAX_RESPONSE_BYTES} bytes"
        )
    try:
        payload = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise RuntimeError("search provider returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise RuntimeError("search provider returned a non-object response")
    return payload


def _request_json(
    url: str,
    headers: dict[str, str] | None = None,
) -> dict[str, object]:
    request_headers = {"User-Agent": USER_AGENT}
    if headers is not None:
        request_headers.update(headers)
    request = urllib.request.Request(url, headers=request_headers)
    try:
        with open_url(request, timeout=TIMEOUT)[0] as response:
            return _read_json_response(response)
    except urllib.error.HTTPError as exc:
        exc.close()
        raise RuntimeError(
            f"search provider returned HTTP {exc.code}"
        ) from None
    except urllib.error.URLError as exc:
        raise RuntimeError(f"search provider request failed: {exc.reason}") from exc
    except TimeoutError as exc:
        raise TimeoutError(f"search provider exceeded {TIMEOUT}s") from exc


def _items(payload: dict[str, object], key: str) -> list[dict[str, object]]:
    value = payload.get(key, [])
    if not isinstance(value, list) or not all(
        isinstance(item, dict) for item in value
    ):
        raise RuntimeError(f"search provider returned invalid `{key}` results")
    return value


def _text(item: dict[str, object], key: str) -> str:
    value = item.get(key, "")
    if not isinstance(value, str):
        raise RuntimeError(f"search provider returned non-string `{key}`")
    return value


def _total_results(value: object) -> int:
    if value is None:
        return 0
    if isinstance(value, bool):
        raise RuntimeError("search provider returned an invalid total result count")
    try:
        total = int(value)
    except (TypeError, ValueError) as exc:
        raise RuntimeError(
            "search provider returned an invalid total result count"
        ) from exc
    if total < 0:
        raise RuntimeError("search provider returned an invalid total result count")
    return total


def _dimension(item: dict[str, object], key: str) -> int:
    value = item.get(key, 0)
    if type(value) is not int or value < 0:
        raise RuntimeError(f"search provider returned invalid `{key}`")
    return value


def _google_web(
    query: str,
    max_results: int,
    config: dict[str, str],
) -> dict[str, object]:
    params = urllib.parse.urlencode(
        {
            "q": query,
            "key": config["key"],
            "cx": config["cx"],
            "num": max_results,
        }
    )
    payload = _request_json(f"https://{GOOGLE_HOST}/customsearch/v1?{params}")
    results = [
        {
            "title": _text(item, "title"),
            "url": _text(item, "link"),
            "snippet": _text(item, "snippet"),
        }
        for item in _items(payload, "items")[:max_results]
    ]
    search_information = payload.get("searchInformation", {})
    if not isinstance(search_information, dict):
        raise RuntimeError("Google returned invalid search information")
    return {
        "query": query,
        "provider": "google",
        "results": results,
        "count": len(results),
        "total_results": _total_results(search_information.get("totalResults")),
    }


def _google_image(
    query: str,
    max_results: int,
    config: dict[str, str],
) -> dict[str, object]:
    params = urllib.parse.urlencode(
        {
            "q": query,
            "key": config["key"],
            "cx": config["cx"],
            "searchType": "image",
            "num": max_results,
        }
    )
    payload = _request_json(f"https://{GOOGLE_HOST}/customsearch/v1?{params}")
    results = []
    for item in _items(payload, "items")[:max_results]:
        image = item.get("image", {})
        if not isinstance(image, dict):
            raise RuntimeError("Google returned invalid image metadata")
        results.append(
            {
                "title": _text(item, "title"),
                "url": _text(item, "link"),
                "thumbnail": _text(image, "thumbnailLink"),
                "width": _dimension(image, "width"),
                "height": _dimension(image, "height"),
                "source": _text(item, "displayLink"),
            }
        )
    return {
        "query": query,
        "provider": "google",
        "results": results,
        "count": len(results),
    }


def _brave_web(
    query: str,
    max_results: int,
    config: dict[str, str],
) -> dict[str, object]:
    params = urllib.parse.urlencode({"q": query, "count": max_results})
    payload = _request_json(
        f"https://{BRAVE_HOST}/res/v1/web/search?{params}",
        headers={
            "Accept": "application/json",
            "X-Subscription-Token": config["key"],
        },
    )
    web = payload.get("web", {})
    if not isinstance(web, dict):
        raise RuntimeError("Brave returned invalid web search metadata")
    results = [
        {
            "title": _text(item, "title"),
            "url": _text(item, "url"),
            "snippet": _text(item, "description"),
        }
        for item in _items(web, "results")[:max_results]
    ]
    return {
        "query": query,
        "provider": "brave",
        "results": results,
        "count": len(results),
        "total_results": _total_results(web.get("totalResults")),
    }


def _brave_image(
    query: str,
    max_results: int,
    config: dict[str, str],
) -> dict[str, object]:
    params = urllib.parse.urlencode({"q": query, "count": max_results})
    payload = _request_json(
        f"https://{BRAVE_HOST}/res/v1/images/search?{params}",
        headers={
            "Accept": "application/json",
            "X-Subscription-Token": config["key"],
        },
    )
    results = []
    for item in _items(payload, "results")[:max_results]:
        properties = item.get("properties", {})
        thumbnail = item.get("thumbnail", {})
        if not isinstance(properties, dict) or not isinstance(thumbnail, dict):
            raise RuntimeError("Brave returned invalid image metadata")
        results.append(
            {
                "title": _text(item, "title"),
                "url": _text(item, "url"),
                "thumbnail": _text(thumbnail, "src"),
                "width": _dimension(properties, "width"),
                "height": _dimension(properties, "height"),
                "source": _text(item, "source"),
            }
        )
    return {
        "query": query,
        "provider": "brave",
        "results": results,
        "count": len(results),
    }


def _remember_search(
    kind: str,
    query: str,
    provider: str,
    result: dict[str, object],
) -> None:
    count = result["count"]
    memory.remember(
        source="search",
        text=(
            f"{kind.capitalize()} search {query!r} via {provider} "
            f"returned {count} result(s)"
        ),
        kind="event",
        tags=["search", kind, provider],
        link=(
            f"cos app search {kind} --provider {provider} "
            f"{shlex.quote(query)}"
        ),
    )


def _search(
    kind: str,
    provider: str,
    query: str,
    max_results: int,
) -> dict[str, object]:
    provider, query, max_results = _validate_request(
        provider,
        query,
        max_results,
    )
    config = _provider_config(provider)
    host = GOOGLE_HOST if provider == "google" else BRAVE_HOST
    policy.require("net.dial", host=host)

    if (kind, provider) == ("web", "google"):
        result = _google_web(query, max_results, config)
    elif (kind, provider) == ("web", "brave"):
        result = _brave_web(query, max_results, config)
    elif (kind, provider) == ("image", "google"):
        result = _google_image(query, max_results, config)
    elif (kind, provider) == ("image", "brave"):
        result = _brave_image(query, max_results, config)
    else:
        raise ValueError("search kind must be web or image")

    _remember_search(kind, query, provider, result)
    return result


def web(
    provider: str,
    query: str,
    max_results: int = MAX_RESULTS_DEFAULT,
) -> dict[str, object]:
    return _search("web", provider, query, max_results)


def image(
    provider: str,
    query: str,
    max_results: int = MAX_RESULTS_DEFAULT,
) -> dict[str, object]:
    return _search("image", provider, query, max_results)
