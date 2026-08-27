"""cos web — Browser via cos-browser engine (vendored from Obscura).

Subprocesses the `cos-browser` binary for read / scrape / screenshot. Falls
back to a pure-stdlib urllib path only for the simplest read case when the
binary is unavailable.

Output shapes (deliberately *not* the legacy Reader/Markdown shape):

    cos app web read URL                 -> {url, title, text, links, engine}
    cos app web read URL --html          -> {url, title, html, engine}
    cos app web read URL --eval EXPR     -> {url, result}
    cos app web scrape URL...            -> {results: [...], total_time_ms, ...}
    cos app web screenshot URL --output P -> {ok, output, bytes, ...}
    cos app web submit URL --data ...    -> {url, status, body}  (urllib POST)
"""

import json
import os
import re
import shutil
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

# Shared env scrubbing — drop OPENAI_API_KEY / GITHUB_TOKEN / etc. out
# of the cos-browser child's environment.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.env_scrub import scrub_env  # noqa: E402
from _shared.safe_http import open_url  # noqa: E402

from claw_os_sdk import ai  # noqa: E402
from cos_runtime import memory, policy  # noqa: E402

TIMEOUT = int(os.environ.get("COS_WEB_TIMEOUT", "30"))
DEFAULT_MAX_LENGTH = int(os.environ.get("COS_WEB_MAX_CONTENT_LENGTH", "50000"))
COS_BROWSER_BIN = os.environ.get("COS_BROWSER_BIN", "cos-browser")
USER_AGENT = "cos/" + os.environ.get("COS_VERSION", "0.1.0")


# ---------------------------------------------------------------------------
# JS payload — extract {title, text, links} from rendered DOM in one pass.
# Escape \t/\n/\s as raw backslash sequences so V8 sees JS escapes, not Python.
# ---------------------------------------------------------------------------

_EXTRACT_JS = r"""(function(){
  function blockText(el){
    if(!el) return '';
    if(el.nodeType===3) return el.textContent||'';
    if(el.nodeType!==1) return '';
    var tag=(el.tagName||'').toLowerCase();
    if(tag==='script'||tag==='style'||tag==='noscript'||tag==='template') return '';
    var s='';
    var cn=el.childNodes||[];
    for(var i=0;i<cn.length;i++) s+=blockText(cn[i]);
    var blocks=['p','div','section','article','header','footer','nav','main','aside','h1','h2','h3','h4','h5','h6','li','tr','blockquote','pre','br','hr'];
    if(blocks.indexOf(tag)!==-1) s+='\n';
    return s;
  }
  var body=document.body||document.documentElement;
  var raw=blockText(body);
  var text=raw.replace(/[\t ]+/g,' ').replace(/\n{3,}/g,'\n\n').replace(/^\s+|\s+$/g,'');
  var anchors=document.querySelectorAll?document.querySelectorAll('a[href]'):[];
  var links=[];
  for(var i=0;i<anchors.length&&i<500;i++){
    var a=anchors[i];
    var href=a.getAttribute('href')||'';
    var t=(a.textContent||'').replace(/\s+/g,' ').replace(/^ +| +$/g,'');
    if(href) links.push({href:href,text:t});
  }
  return {title:document.title||'',text:text,links:links};
})()"""


# ---------------------------------------------------------------------------
# Subprocess helpers
# ---------------------------------------------------------------------------

def _has_cos_browser():
    if os.path.isabs(COS_BROWSER_BIN):
        return os.path.isfile(COS_BROWSER_BIN) and os.access(COS_BROWSER_BIN, os.X_OK)
    return shutil.which(COS_BROWSER_BIN) is not None


def _run_cos_browser(argv, timeout_secs=None):
    """Run `cos-browser <argv...>` and return (stdout, stderr, returncode, error).

    On binary-not-found returns (None, None, None, "<reason>").
    """
    if not _has_cos_browser():
        return None, None, None, (
            f"cos-browser binary not found ({COS_BROWSER_BIN}). "
            "Install Claw OS or set $COS_BROWSER_BIN."
        )
    timeout_secs = timeout_secs if timeout_secs is not None else TIMEOUT
    try:
        # ``stdin=DEVNULL`` so cos-browser can't block waiting for input
        # if it ever tries to prompt; ``env=scrub_env()`` keeps provider
        # API keys / OAuth tokens out of the browser engine.
        proc = subprocess.run(
            [COS_BROWSER_BIN] + argv,
            capture_output=True,
            text=True,
            timeout=timeout_secs + 10,
            stdin=subprocess.DEVNULL,
            env=scrub_env(),
            check=False,
        )
        return proc.stdout, proc.stderr, proc.returncode, None
    except subprocess.TimeoutExpired:
        return None, None, None, f"cos-browser exceeded {timeout_secs}s"


# ---------------------------------------------------------------------------
# Arg parsing
# ---------------------------------------------------------------------------

def _parse_args(args, flags):
    """Parse --flag value (and --bool-flag) from args. flags: {name: default}.

    A default of True/False marks the flag as boolean (no value consumed).
    Returns (positional_args, parsed_flags).
    """
    positional = []
    result = dict(flags)
    i = 0
    while i < len(args):
        cur = args[i]
        if cur.startswith("--"):
            key = cur[2:]
            if key in flags:
                if isinstance(flags[key], bool):
                    result[key] = True
                    i += 1
                    continue
                if i + 1 < len(args):
                    result[key] = args[i + 1]
                    i += 2
                    continue
                i += 1
                continue
        positional.append(cur)
        i += 1
    return positional, result


def _normalize_url(url):
    if not url.startswith(("http://", "https://")):
        url = "https://" + url
    return url


def _host_of(url):
    """Extract host[:port] from a normalized URL, or None if invalid."""
    try:
        parsed = urllib.parse.urlparse(url)
    except ValueError:
        return None
    if not parsed.hostname:
        return None
    if parsed.port:
        if ":" in parsed.hostname:
            return f"[{parsed.hostname}]:{parsed.port}"
        return f"{parsed.hostname}:{parsed.port}"
    return parsed.hostname


def _truncate(s, n):
    if isinstance(s, str) and len(s) > n:
        return s[:n] + "\n\n[truncated]"
    return s


# ---------------------------------------------------------------------------
# urllib fallback (used only if cos-browser is missing)
# ---------------------------------------------------------------------------

def _urllib_fallback(url, max_length):
    try:
        req = urllib.request.Request(url, method="GET")
        req.add_header("User-Agent", USER_AGENT)
        resp = open_url(req, timeout=TIMEOUT, initial_authorized=True)[0]
        html = resp.read().decode("utf-8", errors="replace")
        final_url = resp.url
    except urllib.error.HTTPError as e:
        return {"error": f"HTTP {e.code}: {e.reason}", "url": url}
    except urllib.error.URLError as e:
        return {"error": f"could not fetch: {e.reason}", "url": url}
    except policy.PolicyError:
        raise
    except Exception as e:
        return {"error": str(e), "url": url}

    title = ""
    m = re.search(r"<title[^>]*>(.*?)</title>", html, re.DOTALL | re.IGNORECASE)
    if m:
        title = re.sub(r"<[^>]+>", "", m.group(1)).strip()

    for tag in ("script", "style", "nav", "footer", "header", "noscript"):
        html = re.sub(rf"<{tag}[\s>].*?</{tag}>", " ", html,
                      flags=re.DOTALL | re.IGNORECASE)
    html = re.sub(r"<br\s*/?>", "\n", html, flags=re.IGNORECASE)
    html = re.sub(r"</(p|div|li|tr|h[1-6]|blockquote|section|article)>",
                  "\n", html, flags=re.IGNORECASE)
    text = re.sub(r"<[^>]+>", " ", html)
    for entity, char in (("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">"),
                         ("&quot;", '"'), ("&apos;", "'"), ("&nbsp;", " ")):
        text = text.replace(entity, char)
    text = re.sub(r"[ \t]+", " ", text)
    text = re.sub(r"\n{3,}", "\n\n", text).strip()

    return {
        "url": final_url,
        "title": title,
        "text": _truncate(text, max_length),
        "links": [],
        "engine": "urllib-fallback",
        "warning": "cos-browser unavailable; using stdlib urllib (no JS rendering)",
    }


# ---------------------------------------------------------------------------
# AI summarisation prompt
# ---------------------------------------------------------------------------

_SUMMARIZE_SYSTEM = (
    "You are a concise summariser. Read the user's text and reply with "
    "exactly 3 short bullet lines, no preamble."
)


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def _cmd_read(args):
    """Fetch a URL via cos-browser and return clean text + links + title."""
    if not args:
        return {"error": "usage: cos web read <url> [--selector CSS] "
                "[--wait CSS] [--timeout SEC] [--max-length N] [--html] [--eval JS]"}

    positional, flags = _parse_args(args, {
        "selector": None,
        "wait": None,
        "timeout": str(TIMEOUT),
        "max-length": str(DEFAULT_MAX_LENGTH),
        "html": False,
        "eval": None,
        "user-agent": None,
    })

    if not positional:
        return {"error": "usage: cos web read <url>"}

    url = _normalize_url(positional[0])
    host = _host_of(url)
    if host is None:
        return {"error": f"invalid URL: {url}"}
    policy.require("net.dial", host=host)
    try:
        timeout_secs = int(flags["timeout"])
    except (TypeError, ValueError):
        timeout_secs = TIMEOUT
    try:
        max_length = int(flags["max-length"])
    except (TypeError, ValueError):
        max_length = DEFAULT_MAX_LENGTH

    # cos-browser missing? graceful degrade for plain reads only.
    if not _has_cos_browser():
        if flags["html"] or flags["eval"]:
            return {"error": "cos-browser is required for --html / --eval", "url": url}
        return _urllib_fallback(url, max_length)

    cb_args = ["fetch", url, "--quiet", "--timeout", str(timeout_secs)]
    if flags["selector"]:
        cb_args += ["--selector", flags["selector"]]
    if flags["wait"]:
        cb_args += ["--selector", flags["wait"]]  # alias
    if flags["user-agent"]:
        cb_args += ["--user-agent", flags["user-agent"]]

    if flags["html"]:
        cb_args += ["--dump", "html"]
        out, err, rc, error = _run_cos_browser(cb_args, timeout_secs)
        if error:
            return {"error": error, "url": url}
        if rc != 0:
            return {"error": (err or "").strip() or f"cos-browser exit {rc}", "url": url}
        return {
            "url": url,
            "title": "",
            "html": _truncate(out or "", max_length),
            "engine": "cos-browser",
        }

    if flags["eval"]:
        cb_args += ["--eval", flags["eval"]]
        out, err, rc, error = _run_cos_browser(cb_args, timeout_secs)
        if error:
            return {"error": error, "url": url}
        if rc != 0:
            return {"error": (err or "").strip() or f"cos-browser exit {rc}", "url": url}
        body = (out or "").strip()
        # Try JSON first; if not JSON, return as-is.
        try:
            result = json.loads(body)
        except (ValueError, json.JSONDecodeError):
            result = body
        return {"url": url, "result": result, "engine": "cos-browser"}

    # Default: extract {title, text, links} via single eval pass.
    cb_args += ["--eval", _EXTRACT_JS]
    out, err, rc, error = _run_cos_browser(cb_args, timeout_secs)
    if error:
        return {"error": error, "url": url}
    if rc != 0:
        return {"error": (err or "").strip() or f"cos-browser exit {rc}", "url": url}

    body = (out or "").strip()
    try:
        data = json.loads(body)
    except (ValueError, json.JSONDecodeError):
        return {
            "error": "cos-browser returned non-JSON output",
            "url": url,
            "stdout_head": body[:500],
        }

    return {
        "url": url,
        "title": data.get("title", ""),
        "text": _truncate(data.get("text", ""), max_length),
        "links": data.get("links", []),
        "engine": "cos-browser",
    }


def _cmd_summarize(args):
    """Fetch a URL and pipe its extracted text through the AI gate."""
    if not args:
        return {"error": "usage: cos web summarize <url>"}

    # Coarse-grained capability check — fail fast before fetching the
    # page if the agent doesn't actually have AI access.
    policy.require("ai.chat.untrusted", wild=True)

    read_result = _cmd_read(args)
    if isinstance(read_result, dict) and "error" in read_result:
        return read_result

    text = read_result.get("text", "") if isinstance(read_result, dict) else ""
    url = read_result.get("url", args[0]) if isinstance(read_result, dict) else args[0]
    title = read_result.get("title", "") if isinstance(read_result, dict) else ""
    if not text.strip():
        return {"error": "page produced no extractable text", "url": url}

    try:
        response = ai.chat(
            prompt=text,
            origin="external-content",
            system=_SUMMARIZE_SYSTEM,
            max_units=4000,
        )
    except ai.AiBudgetExceeded as exc:
        return {"error": "AI budget exceeded for this app", "detail": exc.payload}
    except ai.AiSafetyViolation as exc:
        return {"error": "safety violation", "detail": exc.payload}
    except ai.AiDenied as exc:
        return {"error": "AI call denied", "detail": exc.payload}
    except ai.AiUnavailable as exc:
        return {"error": f"AI unavailable: {exc}"}
    except ai.AiError as exc:
        return {"error": str(exc)}

    out = {
        "url": url,
        "title": title,
        "summary": response.text,
        "source_chars": len(text),
        "model": response.model,
        "provider": response.provider,
        "usage": {
            "input_tokens": response.usage.input_tokens,
            "output_tokens": response.usage.output_tokens,
            "units": response.usage.units,
        },
        "budget": {
            "period": response.budget.period,
            "units_used": response.budget.units_used,
            "units_cap": response.budget.units_cap,
        },
        "review": {
            "safety": response.review.safety,
            "prompt_redacted": response.review.prompt_redacted,
        },
    }
    _remember_summary(url, title, response.text)
    return out


def _remember_summary(url, title, summary):
    try:
        if not summary or not url:
            return
        first = summary.strip().splitlines()
        head = first[0] if first else ""
        if len(head) > 200:
            head = head[:197] + "..."
        label = title or url
        memory.remember(
            source="web",
            text=f"Summarised page {label}: {head}",
            kind="note",
            entity_id=url,
            tags=["web", "summary"],
            link=f"cos app web read {url}",
        )
    except memory.MemoryError:
        pass


def _cmd_screenshot(args):
    """Capture a screenshot via cos-browser (which shells out to chromium)."""
    if not args:
        return {"error": "usage: cos web screenshot <url> --output PATH "
                "[--width W] [--height H] [--full-page] [--timeout SEC]"}

    positional, flags = _parse_args(args, {
        "output": None,
        "width": "1280",
        "height": "720",
        "full-page": False,
        "timeout": "60",
    })

    if not positional:
        return {"error": "usage: cos web screenshot <url> --output PATH"}
    url = _normalize_url(positional[0])
    output = flags["output"]
    if not output:
        return {"error": "missing --output PATH"}

    host = _host_of(url)
    if host is None:
        return {"error": f"invalid URL: {url}"}
    output_abs = os.path.realpath(output)
    policy.require("net.dial", host=host)
    policy.require("fs.write", path=output_abs)

    try:
        width = int(flags["width"])
        height = int(flags["height"])
        timeout_secs = int(flags["timeout"])
    except (TypeError, ValueError):
        return {"error": "width/height/timeout must be integers"}

    cb_args = [
        "screenshot", url,
        "--output", output,
        "--width", str(width),
        "--height", str(height),
        "--timeout", str(timeout_secs),
    ]
    if flags["full-page"]:
        cb_args.append("--full-page")

    out, err, rc, error = _run_cos_browser(cb_args, timeout_secs)
    if error:
        return {"error": error, "url": url}
    if rc != 0:
        return {"error": (err or "").strip() or f"cos-browser exit {rc}", "url": url}

    body = (out or "").strip()
    try:
        return json.loads(body)
    except (ValueError, json.JSONDecodeError):
        return {"ok": True, "url": url, "output": output, "raw": body}


def _cmd_scrape(args):
    """Scrape multiple URLs in parallel via cos-browser."""
    if not args:
        return {"error": "usage: cos web scrape <url> [<url>...] "
                "[--eval JS] [--concurrency N] [--timeout SEC]"}

    positional, flags = _parse_args(args, {
        "eval": _EXTRACT_JS,
        "concurrency": "10",
        "timeout": "60",
        "format": "json",
    })

    urls = [_normalize_url(u) for u in positional]
    if not urls:
        return {"error": "no URLs given"}

    for u in urls:
        host = _host_of(u)
        if host is None:
            return {"error": f"invalid URL: {u}"}
        policy.require("net.dial", host=host)

    try:
        concurrency = int(flags["concurrency"])
        timeout_secs = int(flags["timeout"])
    except (TypeError, ValueError):
        return {"error": "concurrency/timeout must be integers"}

    cb_args = [
        "scrape",
        "--concurrency", str(concurrency),
        "--timeout", str(timeout_secs),
        "--format", flags["format"],
    ]
    if flags["eval"]:
        cb_args += ["--eval", flags["eval"]]
    cb_args += urls

    out, err, rc, error = _run_cos_browser(cb_args, timeout_secs * max(1, len(urls) // concurrency + 1))
    if error:
        return {"error": error}
    if rc != 0:
        return {"error": (err or "").strip() or f"cos-browser exit {rc}"}

    body = (out or "").strip()
    try:
        return json.loads(body)
    except (ValueError, json.JSONDecodeError):
        return {"raw": body}


def _cmd_submit(args):
    """POST form data via stdlib urllib (no JS render needed for typical forms)."""
    if not args:
        return {"error": "usage: cos web submit <url> --data '{...}' [--method POST]"}

    positional, flags = _parse_args(args, {
        "data": None,
        "method": "POST",
        "timeout": str(TIMEOUT),
    })
    if not positional:
        return {"error": "usage: cos web submit <url> --data '{...}'"}

    url = _normalize_url(positional[0])
    host = _host_of(url)
    if host is None:
        return {"error": f"invalid URL: {url}"}

    raw = flags["data"] or ""
    method = (flags["method"] or "POST").upper()
    try:
        timeout_secs = int(flags["timeout"])
    except (TypeError, ValueError):
        timeout_secs = TIMEOUT

    payload = None
    content_type = None
    if raw:
        try:
            obj = json.loads(raw)
            payload = urllib.parse.urlencode(obj).encode("utf-8")
            content_type = "application/x-www-form-urlencoded"
        except (ValueError, json.JSONDecodeError):
            payload = raw.encode("utf-8")
            content_type = "application/x-www-form-urlencoded"

    try:
        req = urllib.request.Request(url, data=payload, method=method)
        req.add_header("User-Agent", USER_AGENT)
        if content_type:
            req.add_header("Content-Type", content_type)
        resp = open_url(req, timeout=timeout_secs)[0]
        body = resp.read().decode("utf-8", errors="replace")
        return {"url": resp.url, "status": resp.status, "body": _truncate(body, DEFAULT_MAX_LENGTH)}
    except urllib.error.HTTPError as e:
        return {"error": f"HTTP {e.code}: {e.reason}", "url": url}
    except urllib.error.URLError as e:
        return {"error": f"could not submit: {e.reason}", "url": url}
    except policy.PolicyError:
        raise
    except Exception as e:
        return {"error": str(e), "url": url}


# ---------------------------------------------------------------------------
# Schema (advertised to agents via `cos app web --schema`)
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def run(command, args):
    """Entry point called by cos."""
    from canonical_argv import normalize_canonical_argv
    args = normalize_canonical_argv(args, bool_flags={"html", "full-page"})
    handlers = {
        "read": _cmd_read,
        "scrape": _cmd_scrape,
        "screenshot": _cmd_screenshot,
        "submit": _cmd_submit,
        "summarize": _cmd_summarize,
    }
    handler = handlers.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    try:
        return handler(args)
    except policy.PermissionDenied as denied:
        return {"error": str(denied), "denial": denied.denial}
    except policy.PolicyUnavailable as exc:
        return {"error": f"capability check failed: {exc}"}


def main():
    argv = sys.argv[1:]

    if not argv:
        print(json.dumps({
            "error": "usage: cos app web <read|scrape|screenshot|submit|summarize> [args...]",
            "commands": ["read", "scrape", "screenshot", "submit", "summarize"],
        }))
        return

    cmd, rest = argv[0], argv[1:]
    result = run(cmd, rest)
    print(json.dumps(result, indent=2, ensure_ascii=False))
    if isinstance(result, dict) and "error" in result:
        sys.exit(1)


if __name__ == "__main__":
    main()
