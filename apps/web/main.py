"""cos web — Full browser with JavaScript rendering, powered by Jina Reader."""

import json
import os
import re
import urllib.error
import urllib.parse
import urllib.request

TIMEOUT = int(os.environ.get("COS_WEB_TIMEOUT", "30"))
USER_AGENT = "cos/0.3.0"
READER_URL = os.environ.get("COS_WEB_READER_URL", os.environ.get("COS_BROWSER_URL", "http://localhost:3000"))
DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")
DEFAULT_MAX_LENGTH = int(os.environ.get("COS_WEB_MAX_CONTENT_LENGTH", "50000"))

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
        return {"error": "usage: cos web read <url> [--selector CSS] "
                "[--remove CSS] [--wait CSS] [--max-length N]"}

    positional, flags = _parse_args(args, {
        "selector": None,
        "remove": None,
        "wait": None,
        "max-length": str(DEFAULT_MAX_LENGTH),
    })

    if not positional:
        return {"error": "usage: cos web read <url>"}

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

    # --- Fallback to urllib (with structured extraction) ---
    return _read_via_urllib_with_extraction(url, max_length)


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
        return {"error": "usage: cos web screenshot <url> [--wait CSS] "
                "[--full-page]"}

    positional, flags = _parse_args(args, {
        "wait": None,
        "full-page": None,
    })

    if not positional:
        return {"error": "usage: cos web screenshot <url>"}

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
        return {"error": "usage: cos web submit <url> [--data JSON]"}

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
# Structured extractors for common sites
# ---------------------------------------------------------------------------

# URL patterns → extractor functions
_EXTRACTORS = {}


def _register_extractor(pattern):
    """Decorator to register a structured extractor for a URL pattern."""
    def decorator(fn):
        _EXTRACTORS[pattern] = fn
        return fn
    return decorator


def _find_extractor(url):
    """Find a matching extractor for the given URL."""
    for pattern, fn in _EXTRACTORS.items():
        if pattern in url:
            return fn
    return None


@_register_extractor("github.com")
def _extract_github(html, url):
    """Extract structured data from GitHub pages."""
    data = {}

    # Repository page
    if re.search(r'github\.com/[^/]+/[^/]+/?$', url):
        data["type"] = "github_repo"

        # Stars
        m = re.search(r'aria-label="(\d[\d,]*)\s+stars?"', html, re.IGNORECASE)
        if m:
            data["stars"] = int(m.group(1).replace(",", ""))

        # Forks
        m = re.search(r'aria-label="(\d[\d,]*)\s+forks?"', html, re.IGNORECASE)
        if m:
            data["forks"] = int(m.group(1).replace(",", ""))

        # Language
        m = re.search(r'itemprop="programmingLanguage">([^<]+)<', html)
        if m:
            data["language"] = m.group(1).strip()

        # Description
        m = re.search(r'<p[^>]*class="[^"]*f4[^"]*"[^>]*>([^<]+)<', html)
        if m:
            data["description"] = m.group(1).strip()

        # Topics
        topics = re.findall(r'data-octo-click="topic_click"[^>]*>([^<]+)<', html)
        if topics:
            data["topics"] = [t.strip() for t in topics]

    # Issues/PR list page
    elif "/issues" in url or "/pull" in url:
        data["type"] = "github_issues"
        items = []
        for m in re.finditer(
            r'data-hovercard-type="issue"[^>]*>([^<]+)</a>',
            html, re.IGNORECASE
        ):
            items.append(m.group(1).strip())
        data["items"] = items[:20]

    return data if data else None


@_register_extractor("stackoverflow.com")
def _extract_stackoverflow(html, url):
    """Extract structured data from StackOverflow."""
    data = {"type": "stackoverflow"}

    # Question title
    m = re.search(r'<h1[^>]*itemprop="name"[^>]*>.*?<a[^>]*>([^<]+)</a>', html, re.DOTALL)
    if m:
        data["title"] = m.group(1).strip()

    # Vote count
    m = re.search(r'itemprop="upvoteCount"\s+content="(\d+)"', html)
    if m:
        data["votes"] = int(m.group(1))

    # Answer count
    m = re.search(r'data-answercount="(\d+)"', html)
    if m:
        data["answers"] = int(m.group(1))

    # Accepted answer exists?
    data["has_accepted"] = 'accepted-answer' in html

    return data if len(data) > 1 else None


@_register_extractor("docs.python.org")
def _extract_python_docs(html, url):
    """Extract structured data from Python documentation."""
    data = {"type": "python_docs"}

    # Module/function name from breadcrumb
    m = re.search(r'<h1>([^<]+)<', html)
    if m:
        data["title"] = m.group(1).strip()

    # Version
    m = re.search(r'Python\s+([\d.]+)\s+documentation', html)
    if m:
        data["python_version"] = m.group(1)

    return data if len(data) > 1 else None


def _apply_structured_extraction(result, url):
    """Try to add structured data to a web read result."""
    # Only for reader/urllib results that have content
    if "error" in result or "content" not in result:
        return result

    # For reader results, we don't have raw HTML — skip extraction
    # For urllib results, the content is already text, but we have
    # the original HTML in the pipeline. We add extraction there.
    return result


def _read_via_urllib_with_extraction(url, max_length):
    """Fallback: fetch URL with urllib, apply structured extraction."""
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

    result = {
        "url": final_url,
        "title": title,
        "content": content,
        "links": links,
        "engine": "urllib",
        "warning": "Jina Reader not running, using basic HTTP fetch "
                   "(no JS rendering)",
    }

    # Try structured extraction
    extractor = _find_extractor(final_url)
    if extractor:
        structured = extractor(html, final_url)
        if structured:
            result["structured"] = structured

    return result


# ---------------------------------------------------------------------------
# Forms discovery
# ---------------------------------------------------------------------------

def _discover_forms(html):
    """Discover forms on a page and return their schema."""
    forms = []
    for m in re.finditer(
        r'<form([^>]*)>(.*?)</form>',
        html, re.DOTALL | re.IGNORECASE
    ):
        attrs = m.group(1)
        body = m.group(2)

        form = {}

        # Action
        am = re.search(r'action=["\']([^"\']+)["\']', attrs)
        if am:
            form["action"] = am.group(1)

        # Method
        mm = re.search(r'method=["\']([^"\']+)["\']', attrs, re.IGNORECASE)
        form["method"] = mm.group(1).upper() if mm else "GET"

        # ID
        im = re.search(r'id=["\']([^"\']+)["\']', attrs)
        if im:
            form["id"] = im.group(1)

        # Fields
        fields = []
        for fm in re.finditer(
            r'<input([^>]*)>',
            body, re.IGNORECASE
        ):
            field_attrs = fm.group(1)
            field = {}
            for fa in ["name", "type", "placeholder", "value", "id"]:
                fam = re.search(rf'{fa}=["\']([^"\']*)["\']', field_attrs, re.IGNORECASE)
                if fam:
                    field[fa] = fam.group(1)
            if field.get("name"):
                fields.append(field)

        # Textareas
        for tm in re.finditer(
            r'<textarea([^>]*)>',
            body, re.IGNORECASE
        ):
            ta_attrs = tm.group(1)
            field = {"type": "textarea"}
            for fa in ["name", "placeholder", "id"]:
                fam = re.search(rf'{fa}=["\']([^"\']*)["\']', ta_attrs, re.IGNORECASE)
                if fam:
                    field[fa] = fam.group(1)
            if field.get("name"):
                fields.append(field)

        # Select
        for sm in re.finditer(
            r'<select([^>]*)>(.*?)</select>',
            body, re.DOTALL | re.IGNORECASE
        ):
            s_attrs = sm.group(1)
            s_body = sm.group(2)
            field = {"type": "select"}
            nm = re.search(r'name=["\']([^"\']+)["\']', s_attrs)
            if nm:
                field["name"] = nm.group(1)
            options = re.findall(r'<option[^>]*value=["\']([^"\']*)["\'][^>]*>([^<]*)<', s_body)
            field["options"] = [{"value": v, "label": l.strip()} for v, l in options]
            if field.get("name"):
                fields.append(field)

        if fields:
            form["fields"] = fields
            forms.append(form)

    return forms if forms else None


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

_COMMANDS = {
    "read": _cmd_read,
    "screenshot": _cmd_screenshot,
    "submit": _cmd_submit,
}


def run(command, args):
    """Main entry point called by the cos router."""
    handler = _COMMANDS.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    return handler(args)
