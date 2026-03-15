"""aos web — Full browser with JavaScript rendering, powered by Jina Reader."""

import json
import os
import re
import urllib.error
import urllib.parse
import urllib.request

TIMEOUT = 30
USER_AGENT = "aos/1.0.0"
READER_URL = os.environ.get("JINA_READER_URL", "http://localhost:3000")
DATA_DIR = os.environ.get("AOS_DATA_DIR", "/var/lib/aos")
DEFAULT_MAX_LENGTH = 50000

# Cached Reader availability (None = not checked yet)
_reader_available = None


# ---------------------------------------------------------------------------
# Reader availability check
# ---------------------------------------------------------------------------

def _is_reader_available():
    """Check if Jina Reader is reachable. Result is cached for process lifetime."""
    global _reader_available
    if _reader_available is not None:
        return _reader_available
    try:
        req = urllib.request.Request(READER_URL, method="HEAD")
        urllib.request.urlopen(req, timeout=5)
        _reader_available = True
    except Exception:
        _reader_available = False
    return _reader_available


# ---------------------------------------------------------------------------
# urllib helpers (used for fallback and submit)
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


# ---------------------------------------------------------------------------
# Fallback HTML-to-text helpers (when Reader is not available)
# ---------------------------------------------------------------------------

def _collapse_whitespace(text):
    """Collapse runs of whitespace into single spaces and strip."""
    text = re.sub(r"[ \t]+", " ", text)
    text = re.sub(r"\n{3,}", "\n\n", text)
    lines = [line.strip() for line in text.splitlines()]
    return "\n".join(lines).strip()


def _extract_title(html):
    """Pull text from the first <title> tag."""
    m = re.search(r"<title[^>]*>(.*?)</title>", html, re.DOTALL | re.IGNORECASE)
    if m:
        title = re.sub(r"<[^>]+>", "", m.group(1))
        return _collapse_whitespace(title)
    return ""


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


def _html_to_text(html):
    """Convert HTML to clean readable text."""
    # Remove noise sections
    for tag in ("script", "style", "nav", "footer", "header", "noscript"):
        html = re.sub(
            rf"<{tag}[\s>].*?</{tag}>", " ", html,
            flags=re.DOTALL | re.IGNORECASE,
        )

    html = re.sub(r"<br\s*/?>", "\n", html, flags=re.IGNORECASE)
    html = re.sub(
        r"</(p|div|li|tr|h[1-6]|blockquote|section|article)>",
        "\n", html, flags=re.IGNORECASE,
    )
    html = re.sub(r"<[^>]+>", " ", html)

    # Decode common HTML entities
    html = re.sub(r"&#(\d+);", lambda m: chr(int(m.group(1))), html)
    html = re.sub(r"&#x([0-9a-fA-F]+);", lambda m: chr(int(m.group(1), 16)), html)
    for entity, char in (("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">"),
                         ("&quot;", '"'), ("&apos;", "'"), ("&nbsp;", " ")):
        html = html.replace(entity, char)

    return _collapse_whitespace(html)


# ---------------------------------------------------------------------------
# Arg parsing helper
# ---------------------------------------------------------------------------

def _parse_args(args, flags):
    """Parse --flag value pairs from args list.

    *flags* is a dict mapping flag names (without --) to their default values.
    Returns (positional_args, parsed_flags_dict).
    """
    positional = []
    result = dict(flags)
    i = 0
    while i < len(args):
        key = args[i].lstrip("-")
        if args[i].startswith("--") and key in flags:
            if i + 1 < len(args):
                result[key] = args[i + 1]
                i += 2
            else:
                i += 1
        else:
            positional.append(args[i])
            i += 1
    return positional, result


def _sanitize_filename(url):
    """Create a filesystem-safe filename from a URL."""
    name = re.sub(r"[^a-zA-Z0-9]", "_", url)
    return name[:120]


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def _cmd_read(args):
    """Fetch a URL and return clean Markdown content."""
    if not args:
        return {"error": "usage: aos web read <url> [--selector CSS] "
                "[--remove CSS] [--wait CSS] [--max-length N]"}

    positional, flags = _parse_args(args, {
        "selector": None,
        "remove": None,
        "wait": None,
        "max-length": str(DEFAULT_MAX_LENGTH),
    })

    if not positional:
        return {"error": "usage: aos web read <url>"}

    url = positional[0]
    if not url.startswith(("http://", "https://")):
        url = "https://" + url

    max_length = DEFAULT_MAX_LENGTH
    try:
        max_length = int(flags["max-length"])
    except (ValueError, TypeError):
        pass

    # --- Try Jina Reader first ---
    if _is_reader_available():
        return _read_via_reader(url, flags, max_length)

    # --- Fallback to urllib ---
    return _read_via_urllib(url, max_length)


def _read_via_reader(url, flags, max_length):
    """Fetch URL through the local Jina Reader service."""
    reader_target = f"{READER_URL}/{url}"
    headers = {
        "Accept": "application/json",
        "X-Timeout": "30000",
    }
    if flags.get("selector"):
        headers["X-Target-Selector"] = flags["selector"]
    if flags.get("remove"):
        headers["X-Remove-Selector"] = flags["remove"]
    if flags.get("wait"):
        headers["X-Wait-For-Selector"] = flags["wait"]

    try:
        req = urllib.request.Request(reader_target)
        req.add_header("User-Agent", USER_AGENT)
        for k, v in headers.items():
            req.add_header(k, v)
        resp = urllib.request.urlopen(req, timeout=TIMEOUT)
        body = resp.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as e:
        return {"error": f"Reader HTTP {e.code}: {e.reason}", "url": url}
    except ConnectionRefusedError:
        # Reader went away mid-process — fall back
        return _read_via_urllib(url, max_length)
    except urllib.error.URLError as e:
        if "Connection refused" in str(e.reason):
            return _read_via_urllib(url, max_length)
        return {"error": f"Reader error: {e.reason}", "url": url}
    except Exception as e:
        return {"error": str(e), "url": url}

    try:
        data = json.loads(body)
    except json.JSONDecodeError:
        return {"error": "invalid JSON from Reader", "url": url}

    content = data.get("content", "")
    if len(content) > max_length:
        content = content[:max_length] + "\n\n[truncated]"

    return {
        "url": data.get("url", url),
        "title": data.get("title", ""),
        "content": content,
        "links": data.get("links", {}),
        "engine": "reader",
    }


def _read_via_urllib(url, max_length):
    """Fallback: fetch URL with urllib and convert HTML to text."""
    try:
        req = _build_request(url)
        resp = urllib.request.urlopen(req, timeout=TIMEOUT)
        html = resp.read().decode("utf-8", errors="replace")
        final_url = resp.url
    except urllib.error.HTTPError as e:
        return {"error": f"HTTP {e.code}: {e.reason}", "url": url}
    except urllib.error.URLError as e:
        return {"error": f"could not fetch: {e.reason}", "url": url}
    except Exception as e:
        return {"error": str(e), "url": url}

    title = _extract_title(html)
    links = _extract_links(html)
    content = _html_to_text(html)

    if len(content) > max_length:
        content = content[:max_length] + "\n\n[truncated]"

    return {
        "url": final_url,
        "title": title,
        "content": content,
        "links": links,
        "engine": "urllib",
        "warning": "Jina Reader not running, using basic HTTP fetch "
                   "(no JS rendering)",
    }


def _cmd_screenshot(args):
    """Capture a screenshot of a URL via Jina Reader."""
    if not args:
        return {"error": "usage: aos web screenshot <url> [--wait CSS] "
                "[--full-page]"}

    positional, flags = _parse_args(args, {
        "wait": None,
        "full-page": None,
    })

    if not positional:
        return {"error": "usage: aos web screenshot <url>"}

    url = positional[0]
    if not url.startswith(("http://", "https://")):
        url = "https://" + url

    if not _is_reader_available():
        return {"error": "screenshot requires Jina Reader service"}

    reader_target = f"{READER_URL}/{url}"
    headers = {
        "X-Return-Format": "screenshot",
        "X-Timeout": "30000",
    }
    if flags.get("wait"):
        headers["X-Wait-For-Selector"] = flags["wait"]

    try:
        req = urllib.request.Request(reader_target)
        req.add_header("User-Agent", USER_AGENT)
        for k, v in headers.items():
            req.add_header(k, v)
        resp = urllib.request.urlopen(req, timeout=TIMEOUT)
        image_data = resp.read()
    except urllib.error.HTTPError as e:
        return {"error": f"Reader HTTP {e.code}: {e.reason}", "url": url}
    except urllib.error.URLError as e:
        return {"error": f"Reader error: {e.reason}", "url": url}
    except Exception as e:
        return {"error": str(e), "url": url}

    screenshots_dir = os.path.join(DATA_DIR, "screenshots")
    os.makedirs(screenshots_dir, exist_ok=True)

    filename = _sanitize_filename(url) + ".png"
    filepath = os.path.join(screenshots_dir, filename)

    try:
        with open(filepath, "wb") as f:
            f.write(image_data)
    except Exception as e:
        return {"error": f"failed to save screenshot: {e}", "url": url}

    return {
        "url": url,
        "path": filepath,
        "size": len(image_data),
    }


def _cmd_submit(args):
    """POST form data to a URL (uses urllib directly, not Jina Reader)."""
    if not args:
        return {"error": "usage: aos web submit <url> [--data JSON]"}

    url = args[0]
    if not url.startswith(("http://", "https://")):
        url = "https://" + url

    data = None
    headers = {}

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
    "screenshot": _cmd_screenshot,
    "submit": _cmd_submit,
}


def run(command, args):
    """Main entry point called by the aos router."""
    handler = _COMMANDS.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    return handler(args)
