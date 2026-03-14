"""aos web — Agent-native browser returning structured content, not HTML."""

import json
import re
import shutil
import urllib.error
import urllib.parse
import urllib.request

TIMEOUT = 30
USER_AGENT = "aos/0.3.0"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _build_request(url, method="GET", data=None, headers=None):
    """Build a urllib Request with standard headers."""
    if not url.startswith(("http://", "https://")):
        url = "https://" + url
    req = urllib.request.Request(url, method=method)
    req.add_header("User-Agent", USER_AGENT)
    if headers:
        for k, v in headers.items():
            req.add_header(k, v)
    if data is not None:
        if isinstance(data, str):
            req.data = data.encode("utf-8")
        elif isinstance(data, bytes):
            req.data = data
    return req


def _fetch(url, method="GET", data=None, headers=None):
    """Fetch a URL and return (response_body, final_url, status)."""
    req = _build_request(url, method=method, data=data, headers=headers)
    resp = urllib.request.urlopen(req, timeout=TIMEOUT)
    body = resp.read().decode("utf-8", errors="replace")
    return body, resp.url, resp.status


# ---------------------------------------------------------------------------
# HTML-to-text converter
# ---------------------------------------------------------------------------

def _extract_links(html):
    """Extract <a href="...">text</a> links from raw HTML."""
    links = []
    for m in re.finditer(r'<a\s[^>]*href=["\']([^"\']+)["\'][^>]*>(.*?)</a>',
                         html, re.DOTALL | re.IGNORECASE):
        href = m.group(1).strip()
        text = re.sub(r"<[^>]+>", "", m.group(2))
        text = _collapse_whitespace(text)
        if href and text:
            links.append({"text": text, "href": href})
    return links


def _extract_title(html):
    """Pull text from the first <title> tag."""
    m = re.search(r"<title[^>]*>(.*?)</title>", html, re.DOTALL | re.IGNORECASE)
    if m:
        title = re.sub(r"<[^>]+>", "", m.group(1))
        return _collapse_whitespace(title)
    return ""


def _strip_tags_with_content(html, tags):
    """Remove specified tags *and* everything between them."""
    for tag in tags:
        html = re.sub(
            rf"<{tag}[\s>].*?</{tag}>",
            " ",
            html,
            flags=re.DOTALL | re.IGNORECASE,
        )
    return html


def _collapse_whitespace(text):
    """Collapse runs of whitespace into single spaces and strip."""
    text = re.sub(r"[ \t]+", " ", text)
    text = re.sub(r"\n{3,}", "\n\n", text)
    lines = [line.strip() for line in text.splitlines()]
    return "\n".join(lines).strip()


def _html_to_text(html):
    """Convert HTML to clean readable text."""
    # Remove noise sections
    cleaned = _strip_tags_with_content(
        html, ["script", "style", "nav", "footer", "header", "noscript"]
    )

    # Convert block elements to newlines for readability
    cleaned = re.sub(r"<br\s*/?>", "\n", cleaned, flags=re.IGNORECASE)
    cleaned = re.sub(
        r"</(p|div|li|tr|h[1-6]|blockquote|section|article)>",
        "\n",
        cleaned,
        flags=re.IGNORECASE,
    )

    # Strip all remaining tags
    cleaned = re.sub(r"<[^>]+>", " ", cleaned)

    # Decode HTML entities
    cleaned = re.sub(r"&#(\d+);", lambda m: chr(int(m.group(1))), cleaned)
    cleaned = re.sub(r"&#x([0-9a-fA-F]+);", lambda m: chr(int(m.group(1), 16)), cleaned)
    cleaned = cleaned.replace("&amp;", "&")
    cleaned = cleaned.replace("&lt;", "<")
    cleaned = cleaned.replace("&gt;", ">")
    cleaned = cleaned.replace("&quot;", '"')
    cleaned = cleaned.replace("&apos;", "'")
    cleaned = cleaned.replace("&nbsp;", " ")

    return _collapse_whitespace(cleaned)


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def _cmd_read(args):
    """Fetch a URL and return structured readable content."""
    if not args:
        return {"error": "usage: aos web read <url>"}

    url = args[0]
    try:
        html, final_url, _status = _fetch(url)
    except urllib.error.HTTPError as e:
        return {"error": f"HTTP {e.code}: {e.reason}", "url": url}
    except urllib.error.URLError as e:
        return {"error": f"could not fetch: {e.reason}", "url": url}
    except Exception as e:
        return {"error": str(e), "url": url}

    title = _extract_title(html)
    links = _extract_links(html)
    content = _html_to_text(html)

    return {
        "url": final_url,
        "title": title,
        "content": content,
        "links": links,
    }


def _cmd_search(args):
    """Web search — not yet configured."""
    return {
        "error": "not configured",
        "hint": "set search API key via: aos kv set web:search_engine google|ddg",
    }


def _cmd_screenshot(args):
    """Capture a screenshot of a URL."""
    if not args:
        return {"error": "usage: aos web screenshot <url>"}

    # Check if chromium / playwright is available
    if shutil.which("chromium") or shutil.which("chromium-browser"):
        return {
            "url": args[0],
            "status": "placeholder",
            "hint": "chromium found but screenshot capture not yet implemented",
        }

    try:
        import playwright  # noqa: F401
        return {
            "url": args[0],
            "status": "placeholder",
            "hint": "playwright found but screenshot capture not yet implemented",
        }
    except ImportError:
        pass

    return {
        "error": "chromium not installed",
        "hint": "install with: aos pkg need chromium",
    }


def _cmd_submit(args):
    """POST form data to a URL."""
    if not args:
        return {"error": "usage: aos web submit <url> [--data JSON]"}

    url = args[0]
    data = None
    headers = {}

    # Parse --data flag
    rest = args[1:]
    i = 0
    while i < len(rest):
        if rest[i] == "--data" and i + 1 < len(rest):
            raw = rest[i + 1]
            try:
                parsed = json.loads(raw)
            except json.JSONDecodeError:
                return {"error": f"invalid JSON for --data: {raw}"}
            data = urllib.parse.urlencode(parsed).encode("utf-8")
            headers["Content-Type"] = "application/x-www-form-urlencoded"
            i += 2
        else:
            i += 1

    try:
        req = _build_request(url, method="POST", data=data, headers=headers)
        resp = urllib.request.urlopen(req, timeout=TIMEOUT)
        body = resp.read().decode("utf-8", errors="replace")
        return {"url": resp.url, "status": resp.status, "body": body}
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace") if e.fp else ""
        return {"url": url, "status": e.code, "body": body}
    except urllib.error.URLError as e:
        return {"error": f"could not connect: {e.reason}", "url": url}
    except Exception as e:
        return {"error": str(e), "url": url}


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

_COMMANDS = {
    "read": _cmd_read,
    "search": _cmd_search,
    "screenshot": _cmd_screenshot,
    "submit": _cmd_submit,
}


def run(command, args):
    """Main entry point called by the aos router."""
    handler = _COMMANDS.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    return handler(args)
