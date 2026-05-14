"""email — send, search, and manage email via SMTP or Gmail/Outlook providers."""

import argparse
import base64
import json
import os
import smtplib
import urllib.error
import urllib.request
from email.mime.multipart import MIMEMultipart
from email.mime.text import MIMEText

from _lib import ai, policy


# ---------------------------------------------------------------------------
# Provider host map — used for fine-grained net.dial scoping
# ---------------------------------------------------------------------------

GMAIL_API_HOST = "gmail.googleapis.com"
OUTLOOK_API_HOST = "graph.microsoft.com"


# ---------------------------------------------------------------------------
# Provider detection
# ---------------------------------------------------------------------------

def _detect_provider():
    """Detect which email provider is configured, in priority order."""
    if os.environ.get("GMAIL_ACCESS_TOKEN") or os.environ.get("GOOGLE_OAUTH_TOKEN"):
        return "gmail"
    if os.environ.get("MICROSOFT_ACCESS_TOKEN") or os.environ.get("MICROSOFT_OAUTH_TOKEN"):
        return "outlook"
    if os.environ.get("SMTP_HOST"):
        return "smtp"
    return None


def _resolve_provider(requested):
    """Return the provider to use, or an error dict if none available."""
    if requested:
        return requested
    detected = _detect_provider()
    if detected:
        return detected
    return None


# ---------------------------------------------------------------------------
# Argument parsers
# ---------------------------------------------------------------------------

def _build_send_parser():
    p = argparse.ArgumentParser(prog="cos email send", add_help=False)
    p.add_argument("--to", required=True)
    p.add_argument("--subject", required=True)
    p.add_argument("--body", required=True)
    p.add_argument("--cc", default=None)
    p.add_argument("--provider", default=None, choices=["smtp", "gmail", "outlook"])
    return p


def _build_search_parser():
    p = argparse.ArgumentParser(prog="cos email search", add_help=False)
    p.add_argument("--query", required=True)
    p.add_argument("--max-results", type=int, default=10)
    p.add_argument("--provider", default=None, choices=["gmail", "outlook"])
    return p


def _build_list_parser():
    p = argparse.ArgumentParser(prog="cos email list", add_help=False)
    p.add_argument("--max-results", type=int, default=10)
    p.add_argument("--unread", action="store_true")
    p.add_argument("--provider", default=None, choices=["gmail", "outlook"])
    return p


def _build_read_parser():
    p = argparse.ArgumentParser(prog="cos email read", add_help=False)
    p.add_argument("--id", required=True, dest="message_id")
    p.add_argument("--provider", default=None, choices=["gmail", "outlook"])
    return p


def _build_draft_parser():
    p = argparse.ArgumentParser(prog="cos email draft", add_help=False)
    p.add_argument("--context", required=True)
    p.add_argument(
        "--style",
        default="formal",
        choices=["formal", "casual", "short"],
    )
    return p


_DRAFT_SYSTEMS = {
    "formal": (
        "Draft a polite, professional email reply based on the user's context. "
        "Use a courteous tone, complete sentences, and a clear sign-off. "
        "Return only the email body — no preamble, no subject line."
    ),
    "casual": (
        "Draft a friendly, conversational email reply based on the user's context. "
        "Keep it warm and approachable but still clear. "
        "Return only the email body — no preamble, no subject line."
    ),
    "short": (
        "Draft a very short, to-the-point email reply based on the user's context. "
        "Aim for 2-3 sentences maximum. "
        "Return only the email body — no preamble, no subject line."
    ),
}


# ---------------------------------------------------------------------------
# SMTP send
# ---------------------------------------------------------------------------

def _send_smtp(to, subject, body, cc=None):
    """Send an email via SMTP."""
    host = os.environ.get("SMTP_HOST", "localhost")
    port = int(os.environ.get("SMTP_PORT", "587"))
    user = os.environ.get("SMTP_USER", "")
    password = os.environ.get("SMTP_PASSWORD", "")
    from_addr = os.environ.get("SMTP_FROM", user)

    msg = MIMEMultipart()
    msg["From"] = from_addr
    msg["To"] = to
    msg["Subject"] = subject
    if cc:
        msg["Cc"] = cc
    msg.attach(MIMEText(body, "plain"))

    with smtplib.SMTP(host, port) as server:
        if port == 587:
            server.starttls()
        if user and password:
            server.login(user, password)
        server.send_message(msg)

    return {"sent": True, "to": to, "subject": subject, "provider": "smtp"}


# ---------------------------------------------------------------------------
# Gmail helpers
# ---------------------------------------------------------------------------

def _gmail_token():
    return os.environ.get("GMAIL_ACCESS_TOKEN") or os.environ.get("GOOGLE_OAUTH_TOKEN")


def _gmail_request(url, method="GET", data=None):
    """Make an authenticated request to the Gmail API."""
    token = _gmail_token()
    headers = {"Authorization": f"Bearer {token}"}
    if data is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(data).encode("utf-8") if isinstance(data, dict) else data
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read().decode("utf-8", errors="replace")
            return json.loads(raw) if raw.strip() else {}
    except urllib.error.HTTPError as e:
        err_body = ""
        try:
            err_body = e.read().decode("utf-8", errors="replace")
        except Exception:
            pass
        return {"error": err_body or str(e), "status": e.code}
    except urllib.error.URLError as e:
        return {"error": str(e.reason)}
    except Exception as e:
        return {"error": str(e)}


def _send_gmail(to, subject, body, cc=None):
    """Send an email via the Gmail API."""
    msg = MIMEMultipart()
    msg["To"] = to
    msg["Subject"] = subject
    if cc:
        msg["Cc"] = cc
    msg.attach(MIMEText(body, "plain"))

    raw = base64.urlsafe_b64encode(msg.as_bytes()).decode()
    result = _gmail_request(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/send",
        method="POST",
        data={"raw": raw},
    )
    if "error" in result:
        return result
    return {
        "sent": True,
        "to": to,
        "subject": subject,
        "provider": "gmail",
        "id": result.get("id", ""),
    }


def _parse_gmail_message(msg_data):
    """Extract structured fields from a Gmail API message resource."""
    headers = {}
    for h in msg_data.get("payload", {}).get("headers", []):
        headers[h["name"].lower()] = h["value"]

    snippet = msg_data.get("snippet", "")
    labels = msg_data.get("labelIds", [])
    unread = "UNREAD" in labels

    # Extract plain-text body from parts or payload body
    body = ""
    payload = msg_data.get("payload", {})
    if payload.get("body", {}).get("data"):
        body = base64.urlsafe_b64decode(payload["body"]["data"]).decode(
            "utf-8", errors="replace"
        )
    else:
        for part in payload.get("parts", []):
            if part.get("mimeType") == "text/plain" and part.get("body", {}).get("data"):
                body = base64.urlsafe_b64decode(part["body"]["data"]).decode(
                    "utf-8", errors="replace"
                )
                break

    # Attachments
    attachments = []
    for part in payload.get("parts", []):
        filename = part.get("filename")
        if filename:
            attachments.append({
                "name": filename,
                "size": part.get("body", {}).get("size", 0),
            })

    return {
        "id": msg_data.get("id", ""),
        "from": headers.get("from", ""),
        "to": [a.strip() for a in headers.get("to", "").split(",") if a.strip()],
        "subject": headers.get("subject", ""),
        "snippet": snippet,
        "body": body,
        "date": headers.get("date", ""),
        "unread": unread,
        "attachments": attachments,
    }


def _search_gmail(query, max_results):
    """Search emails via the Gmail API."""
    url = (
        f"https://gmail.googleapis.com/gmail/v1/users/me/messages"
        f"?q={urllib.request.quote(query)}&maxResults={max_results}"
    )
    result = _gmail_request(url)
    if "error" in result:
        return result

    messages = result.get("messages", [])
    emails = []
    for m in messages:
        detail = _gmail_request(
            f"https://gmail.googleapis.com/gmail/v1/users/me/messages/{m['id']}"
            f"?format=metadata&metadataHeaders=From&metadataHeaders=Subject&metadataHeaders=Date"
        )
        if "error" in detail:
            continue
        parsed = _parse_gmail_message(detail)
        emails.append({
            "id": parsed["id"],
            "from": parsed["from"],
            "subject": parsed["subject"],
            "snippet": parsed["snippet"],
            "date": parsed["date"],
            "unread": parsed["unread"],
        })

    return {"query": query, "provider": "gmail", "emails": emails, "count": len(emails)}


def _list_gmail(max_results, unread):
    """List recent emails via the Gmail API."""
    query = "is:unread" if unread else ""
    url = (
        f"https://gmail.googleapis.com/gmail/v1/users/me/messages"
        f"?maxResults={max_results}"
    )
    if query:
        url += f"&q={urllib.request.quote(query)}"
    result = _gmail_request(url)
    if "error" in result:
        return result

    messages = result.get("messages", [])
    emails = []
    for m in messages:
        detail = _gmail_request(
            f"https://gmail.googleapis.com/gmail/v1/users/me/messages/{m['id']}"
            f"?format=metadata&metadataHeaders=From&metadataHeaders=Subject&metadataHeaders=Date"
        )
        if "error" in detail:
            continue
        parsed = _parse_gmail_message(detail)
        emails.append({
            "id": parsed["id"],
            "from": parsed["from"],
            "subject": parsed["subject"],
            "snippet": parsed["snippet"],
            "date": parsed["date"],
            "unread": parsed["unread"],
        })

    return {"provider": "gmail", "emails": emails, "count": len(emails)}


def _read_gmail(message_id):
    """Read a specific email via the Gmail API."""
    url = (
        f"https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}"
        f"?format=full"
    )
    result = _gmail_request(url)
    if "error" in result:
        return result
    return _parse_gmail_message(result)


# ---------------------------------------------------------------------------
# Outlook helpers
# ---------------------------------------------------------------------------

def _outlook_token():
    return os.environ.get("MICROSOFT_ACCESS_TOKEN") or os.environ.get("MICROSOFT_OAUTH_TOKEN")


def _outlook_request(url, method="GET", data=None):
    """Make an authenticated request to the Microsoft Graph API."""
    token = _outlook_token()
    headers = {"Authorization": f"Bearer {token}"}
    if data is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(data).encode("utf-8") if isinstance(data, dict) else data
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            raw = resp.read().decode("utf-8", errors="replace")
            return json.loads(raw) if raw.strip() else {}
    except urllib.error.HTTPError as e:
        err_body = ""
        try:
            err_body = e.read().decode("utf-8", errors="replace")
        except Exception:
            pass
        return {"error": err_body or str(e), "status": e.code}
    except urllib.error.URLError as e:
        return {"error": str(e.reason)}
    except Exception as e:
        return {"error": str(e)}


def _send_outlook(to, subject, body, cc=None):
    """Send an email via the Microsoft Graph API."""
    payload = {
        "message": {
            "subject": subject,
            "body": {"contentType": "Text", "content": body},
            "toRecipients": [{"emailAddress": {"address": to}}],
        }
    }
    if cc:
        payload["message"]["ccRecipients"] = [{"emailAddress": {"address": cc}}]

    result = _outlook_request(
        "https://graph.microsoft.com/v1.0/me/sendMail",
        method="POST",
        data=payload,
    )
    if "error" in result:
        return result
    return {"sent": True, "to": to, "subject": subject, "provider": "outlook"}


def _parse_outlook_message(msg_data):
    """Extract structured fields from an Outlook Graph API message resource."""
    from_field = msg_data.get("from", {}).get("emailAddress", {})
    to_list = [
        r.get("emailAddress", {}).get("address", "")
        for r in msg_data.get("toRecipients", [])
    ]
    attachments = [
        {"name": a.get("name", ""), "size": a.get("size", 0)}
        for a in msg_data.get("attachments", [])
    ]
    return {
        "id": msg_data.get("id", ""),
        "from": from_field.get("address", ""),
        "to": to_list,
        "subject": msg_data.get("subject", ""),
        "snippet": msg_data.get("bodyPreview", ""),
        "body": msg_data.get("body", {}).get("content", ""),
        "date": msg_data.get("receivedDateTime", ""),
        "unread": not msg_data.get("isRead", True),
        "attachments": attachments,
    }


def _search_outlook(query, max_results):
    """Search emails via the Microsoft Graph API."""
    encoded_query = urllib.request.quote(query)
    url = (
        f"https://graph.microsoft.com/v1.0/me/messages"
        f"?$search=%22{encoded_query}%22&$top={max_results}"
    )
    result = _outlook_request(url)
    if "error" in result:
        return result

    messages = result.get("value", [])
    emails = []
    for m in messages:
        parsed = _parse_outlook_message(m)
        emails.append({
            "id": parsed["id"],
            "from": parsed["from"],
            "subject": parsed["subject"],
            "snippet": parsed["snippet"],
            "date": parsed["date"],
            "unread": parsed["unread"],
        })

    return {"query": query, "provider": "outlook", "emails": emails, "count": len(emails)}


def _list_outlook(max_results, unread):
    """List recent emails via the Microsoft Graph API."""
    url = f"https://graph.microsoft.com/v1.0/me/messages?$top={max_results}"
    if unread:
        url += "&$filter=isRead%20eq%20false"
    url += "&$orderby=receivedDateTime%20desc"
    result = _outlook_request(url)
    if "error" in result:
        return result

    messages = result.get("value", [])
    emails = []
    for m in messages:
        parsed = _parse_outlook_message(m)
        emails.append({
            "id": parsed["id"],
            "from": parsed["from"],
            "subject": parsed["subject"],
            "snippet": parsed["snippet"],
            "date": parsed["date"],
            "unread": parsed["unread"],
        })

    return {"provider": "outlook", "emails": emails, "count": len(emails)}


def _read_outlook(message_id):
    """Read a specific email via the Microsoft Graph API."""
    url = f"https://graph.microsoft.com/v1.0/me/messages/{message_id}"
    result = _outlook_request(url)
    if "error" in result:
        return result
    return _parse_outlook_message(result)


# ---------------------------------------------------------------------------
# Command handlers
# ---------------------------------------------------------------------------

def cmd_send(args):
    parser = _build_send_parser()
    try:
        opts = parser.parse_args(args)
    except SystemExit:
        return {"error": "missing required arguments: --to, --subject, --body"}

    provider = _resolve_provider(opts.provider)
    if provider is None:
        return {
            "error": "no email provider configured",
            "hint": (
                "Set SMTP_HOST for SMTP, GMAIL_ACCESS_TOKEN for Gmail, "
                "or MICROSOFT_ACCESS_TOKEN for Outlook"
            ),
        }

    if provider == "smtp":
        smtp_host = os.environ.get("SMTP_HOST", "localhost")
        policy.require("secret.read", name="SMTP_PASSWORD")
        policy.require("net.dial", host=smtp_host)
        return _send_smtp(opts.to, opts.subject, opts.body, cc=opts.cc)
    elif provider == "gmail":
        policy.require("secret.read", name="GMAIL_ACCESS_TOKEN")
        policy.require("net.dial", host=GMAIL_API_HOST)
        return _send_gmail(opts.to, opts.subject, opts.body, cc=opts.cc)
    elif provider == "outlook":
        policy.require("secret.read", name="MICROSOFT_ACCESS_TOKEN")
        policy.require("net.dial", host=OUTLOOK_API_HOST)
        return _send_outlook(opts.to, opts.subject, opts.body, cc=opts.cc)
    else:
        return {"error": f"unknown provider: {provider}"}


def cmd_search(args):
    parser = _build_search_parser()
    try:
        opts = parser.parse_args(args)
    except SystemExit:
        return {"error": "missing required argument: --query"}

    provider = _resolve_provider(opts.provider)
    if provider is None:
        return {
            "error": "no email provider configured",
            "hint": (
                "Set GMAIL_ACCESS_TOKEN for Gmail "
                "or MICROSOFT_ACCESS_TOKEN for Outlook"
            ),
        }
    if provider == "smtp":
        return {"error": "search requires gmail or outlook provider"}

    if provider == "gmail":
        policy.require("secret.read", name="GMAIL_ACCESS_TOKEN")
        policy.require("net.dial", host=GMAIL_API_HOST)
        return _search_gmail(opts.query, opts.max_results)
    elif provider == "outlook":
        policy.require("secret.read", name="MICROSOFT_ACCESS_TOKEN")
        policy.require("net.dial", host=OUTLOOK_API_HOST)
        return _search_outlook(opts.query, opts.max_results)
    else:
        return {"error": f"unknown provider: {provider}"}


def cmd_list(args):
    parser = _build_list_parser()
    try:
        opts = parser.parse_args(args)
    except SystemExit:
        return {"error": "invalid arguments for list command"}

    provider = _resolve_provider(opts.provider)
    if provider is None:
        return {
            "error": "no email provider configured",
            "hint": (
                "Set GMAIL_ACCESS_TOKEN for Gmail "
                "or MICROSOFT_ACCESS_TOKEN for Outlook"
            ),
        }
    if provider == "smtp":
        return {"error": "list requires gmail or outlook provider"}

    if provider == "gmail":
        policy.require("secret.read", name="GMAIL_ACCESS_TOKEN")
        policy.require("net.dial", host=GMAIL_API_HOST)
        return _list_gmail(opts.max_results, opts.unread)
    elif provider == "outlook":
        policy.require("secret.read", name="MICROSOFT_ACCESS_TOKEN")
        policy.require("net.dial", host=OUTLOOK_API_HOST)
        return _list_outlook(opts.max_results, opts.unread)
    else:
        return {"error": f"unknown provider: {provider}"}


def cmd_read(args):
    parser = _build_read_parser()
    try:
        opts = parser.parse_args(args)
    except SystemExit:
        return {"error": "missing required argument: --id"}

    provider = _resolve_provider(opts.provider)
    if provider is None:
        return {
            "error": "no email provider configured",
            "hint": (
                "Set GMAIL_ACCESS_TOKEN for Gmail "
                "or MICROSOFT_ACCESS_TOKEN for Outlook"
            ),
        }
    if provider == "smtp":
        return {"error": "read requires gmail or outlook provider"}

    if provider == "gmail":
        policy.require("secret.read", name="GMAIL_ACCESS_TOKEN")
        policy.require("net.dial", host=GMAIL_API_HOST)
        return _read_gmail(opts.message_id)
    elif provider == "outlook":
        policy.require("secret.read", name="MICROSOFT_ACCESS_TOKEN")
        policy.require("net.dial", host=OUTLOOK_API_HOST)
        return _read_outlook(opts.message_id)
    else:
        return {"error": f"unknown provider: {provider}"}


def cmd_draft(args):
    """Draft an email reply via the AI gate. Pure text — does NOT send."""
    parser = _build_draft_parser()
    try:
        opts = parser.parse_args(args)
    except SystemExit:
        return {"error": "usage: cos email draft --context <text> [--style formal|casual|short]"}

    if not opts.context.strip():
        return {"error": "--context must be non-empty"}

    # Coarse-grained capability check — fail fast on a denied agent.
    policy.require("ai.chat", name="claude-*")

    system_prompt = _DRAFT_SYSTEMS.get(opts.style, _DRAFT_SYSTEMS["formal"])

    try:
        response = ai.chat(
            prompt=opts.context,
            origin="trusted",
            system=system_prompt,
            max_units=3000,
        )
    except ai.AiBudgetExceeded as exc:
        return {"error": "AI budget exceeded for this app", "detail": exc.payload}
    except ai.AiModelNotAllowed as exc:
        return {"error": "model not allowed", "detail": exc.payload}
    except ai.AiSafetyViolation as exc:
        return {"error": "safety violation", "detail": exc.payload}
    except ai.AiDenied as exc:
        return {"error": "AI call denied", "detail": exc.payload}
    except ai.AiUnavailable as exc:
        return {"error": f"AI unavailable: {exc}"}
    except ai.AiError as exc:
        return {"error": str(exc)}

    return {
        "draft": response.text,
        "style": opts.style,
        "model": response.model,
        "provider": response.provider,
        "usage": {
            "input_tokens": response.usage.input_tokens,
            "output_tokens": response.usage.output_tokens,
            "units": response.usage.units,
            "usd": response.usage.usd,
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


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def _schema():
    return {
        "send": {
            "description": "Send an email via SMTP, Gmail, or Outlook",
            "parameters": [
                {"name": "--to", "type": "string", "required": True, "description": "Recipient email address", "kind": "flag"},
                {"name": "--subject", "type": "string", "required": True, "description": "Email subject line", "kind": "flag"},
                {"name": "--body", "type": "string", "required": True, "description": "Email body text", "kind": "flag"},
                {"name": "--cc", "type": "string", "required": False, "description": "CC recipient email address", "kind": "flag"},
                {"name": "--provider", "type": "string", "required": False, "description": "Email provider: smtp, gmail, or outlook (auto-detected if omitted)", "kind": "flag"},
            ],
            "example": "cos app email send --to user@example.com --subject 'Hello' --body 'Hi there'",
        },
        "search": {
            "description": "Search emails by query (requires Gmail or Outlook provider)",
            "parameters": [
                {"name": "--query", "type": "string", "required": True, "description": "Search query string", "kind": "flag"},
                {"name": "--max-results", "type": "integer", "required": False, "description": "Maximum results to return", "kind": "flag", "default": 10},
                {"name": "--provider", "type": "string", "required": False, "description": "Email provider: gmail or outlook", "kind": "flag"},
            ],
            "example": "cos app email search --query 'from:boss@example.com' --max-results 5",
        },
        "list": {
            "description": "List recent emails (requires Gmail or Outlook provider)",
            "parameters": [
                {"name": "--max-results", "type": "integer", "required": False, "description": "Maximum emails to return", "kind": "flag", "default": 10},
                {"name": "--unread", "type": "boolean", "required": False, "description": "Show only unread emails", "kind": "flag", "default": False},
                {"name": "--provider", "type": "string", "required": False, "description": "Email provider: gmail or outlook", "kind": "flag"},
            ],
            "example": "cos app email list --max-results 20 --unread",
        },
        "read": {
            "description": "Read a specific email by message ID (requires Gmail or Outlook provider)",
            "parameters": [
                {"name": "--id", "type": "string", "required": True, "description": "Message ID to read", "kind": "flag"},
                {"name": "--provider", "type": "string", "required": False, "description": "Email provider: gmail or outlook", "kind": "flag"},
            ],
            "example": "cos app email read --id abc123def",
        },
        "draft": {
            "description": "Draft an email reply via the AI gate. Pure text generation — does NOT send.",
            "parameters": [
                {"name": "--context", "type": "string", "required": True, "description": "The email thread or instruction to base the draft on", "kind": "flag"},
                {"name": "--style", "type": "string", "required": False, "description": "Tone of the reply: formal, casual, or short", "kind": "flag", "default": "formal"},
            ],
            "example": "cos app email draft --context 'Thanks for the update — can you confirm the deadline?' --style formal",
        },
    }


def run(command, args):
    """Entry point called by cos."""
    if command == "__schema__":
        return _schema()
    handlers = {
        "send": cmd_send,
        "search": cmd_search,
        "list": cmd_list,
        "read": cmd_read,
        "draft": cmd_draft,
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
