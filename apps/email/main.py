"""email — send, search, and manage email via SMTP or Gmail/Outlook providers.

This is the **AI-agent** surface for email. Users who want a GUI inbox
open Thunderbird (preinstalled, default ``x-scheme-handler/mailto``
handler). The two surfaces are intentionally independent:

* GUI side  — Thunderbird, IMAP/SMTP/JMAP. Driven by the user.
* Agent side — this app, Gmail / Outlook HTTPS APIs or plain SMTP.
  Driven by the AI through ``apps/email``'s operations
  (send / search / list / read / draft).

Same mailbox, two clients; neither needs to know about the other.
"""

import argparse
import base64
import json
import os
import smtplib
import sys
import urllib.error
import urllib.request
from email.mime.multipart import MIMEMultipart
from email.mime.text import MIMEText

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _shared.credentials import load_credential  # noqa: E402
from claw_os_sdk import ai  # noqa: E402
from cos_runtime import memory, policy  # noqa: E402


# ---------------------------------------------------------------------------
# Provider host map — used for fine-grained net.dial scoping
# ---------------------------------------------------------------------------

GMAIL_API_HOST = "gmail.googleapis.com"
OUTLOOK_API_HOST = "graph.microsoft.com"
GOOGLE_ACCESS_TOKEN = "GOOGLE_ACCESS_TOKEN"
MICROSOFT_ACCESS_TOKEN = "MICROSOFT_ACCESS_TOKEN"


def _credential_value(store_name, *legacy_env_names):
    value = os.environ.get(store_name)
    if value:
        return value, None
    for name in legacy_env_names:
        value = os.environ.get(name)
        if value:
            return value, None
    return load_credential(store_name)


def _gmail_auth_error(detail, status=None):
    result = {
        "error": "Gmail authorization is required or has expired",
        "provider": "gmail",
        "auth_required": True,
        "retryable": False,
        "credential": f"default/{GOOGLE_ACCESS_TOKEN}",
        "detail": detail,
        "setup": {
            "interactive_oauth_available": True,
            "agent_action": {
                "tool": "cos_oauth_login",
                "input": {"provider": "google"},
            },
            "login_command": "cos credential oauth-login google",
            "message": (
                "The system Agent should start the trusted browser authorization "
                "with setup.agent_action. The login command is a terminal fallback."
            ),
        },
    }
    if status is not None:
        result["status"] = status
    return result


def _outlook_auth_error(detail, status=None):
    result = {
        "error": "Outlook authorization is required or has expired",
        "provider": "outlook",
        "auth_required": True,
        "retryable": False,
        "credential": f"default/{MICROSOFT_ACCESS_TOKEN}",
        "detail": detail,
        "setup": {
            "interactive_oauth_available": True,
            "agent_action": {
                "tool": "cos_oauth_login",
                "input": {"provider": "microsoft"},
            },
            "login_command": "cos credential oauth-login microsoft",
            "message": (
                "The system Agent should start the trusted browser authorization "
                "with setup.agent_action. The login command is a terminal fallback."
            ),
        },
    }
    if status is not None:
        result["status"] = status
    return result


# ---------------------------------------------------------------------------
# Provider detection
# ---------------------------------------------------------------------------

def _detect_provider():
    """Detect which email provider is configured, in priority order."""
    if (
        os.environ.get(GOOGLE_ACCESS_TOKEN)
        or os.environ.get("GMAIL_ACCESS_TOKEN")
        or os.environ.get("GOOGLE_OAUTH_TOKEN")
    ):
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
    password, _ = _credential_value("SMTP_PASSWORD")
    password = password or ""
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
    return _credential_value(
        GOOGLE_ACCESS_TOKEN,
        "GMAIL_ACCESS_TOKEN",
        "GOOGLE_OAUTH_TOKEN",
    )


def _gmail_request(url, method="GET", data=None):
    """Make an authenticated request to the Gmail API."""
    token, credential_error = _gmail_token()
    if not token:
        return _gmail_auth_error(
            credential_error or f"missing default/{GOOGLE_ACCESS_TOKEN}"
        )
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
        finally:
            e.close()
        if e.code == 401:
            return _gmail_auth_error(err_body or str(e), status=e.code)
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
            if detail.get("auth_required") or detail.get("retryable") is False:
                return detail
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
            if detail.get("auth_required") or detail.get("retryable") is False:
                return detail
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
    return _credential_value(MICROSOFT_ACCESS_TOKEN, "MICROSOFT_OAUTH_TOKEN")


def _outlook_request(url, method="GET", data=None):
    """Make an authenticated request to the Microsoft Graph API."""
    token, credential_error = _outlook_token()
    if not token:
        return _outlook_auth_error(
            credential_error or f"missing default/{MICROSOFT_ACCESS_TOKEN}"
        )
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
        finally:
            e.close()
        if e.code == 401:
            return _outlook_auth_error(err_body or str(e), status=e.code)
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

def _remember_sent(result, to, subject, body, cc=None):
    """Push a one-line summary of a sent email into the agent's memory."""
    try:
        provider = result.get("provider") or "smtp"
        message_id = result.get("id") or ""
        snippet = (body or "").strip().splitlines()
        first = snippet[0] if snippet else ""
        if len(first) > 200:
            first = first[:197] + "..."
        text = f"Sent email to {to}: {subject}".strip()
        if first:
            text += f" — {first}"
        tags = ["email", "sent", provider]
        if cc:
            tags.append("cc")
        memory.remember(
            source="email",
            text=text,
            kind="event",
            entity_id=message_id or None,
            tags=tags,
            link=f"cos app email read --id {message_id}" if message_id else None,
        )
    except memory.MemoryError:
        pass


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
                "Set SMTP_HOST for SMTP, GOOGLE_ACCESS_TOKEN for Gmail, "
                "or MICROSOFT_ACCESS_TOKEN for Outlook"
            ),
        }

    if provider == "smtp":
        smtp_host = os.environ.get("SMTP_HOST", "localhost")
        policy.require("secret.read", name="default/SMTP_PASSWORD")
        policy.require("net.dial", host=smtp_host)
        result = _send_smtp(opts.to, opts.subject, opts.body, cc=opts.cc)
    elif provider == "gmail":
        policy.require("secret.read", name="default/GOOGLE_ACCESS_TOKEN")
        policy.require("net.dial", host=GMAIL_API_HOST)
        result = _send_gmail(opts.to, opts.subject, opts.body, cc=opts.cc)
    elif provider == "outlook":
        policy.require("secret.read", name="default/MICROSOFT_ACCESS_TOKEN")
        policy.require("net.dial", host=OUTLOOK_API_HOST)
        result = _send_outlook(opts.to, opts.subject, opts.body, cc=opts.cc)
    else:
        return {"error": f"unknown provider: {provider}"}

    if isinstance(result, dict) and result.get("sent"):
        _remember_sent(result, opts.to, opts.subject, opts.body, cc=opts.cc)
    return result


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
                "Set GOOGLE_ACCESS_TOKEN for Gmail "
                "or MICROSOFT_ACCESS_TOKEN for Outlook"
            ),
        }
    if provider == "smtp":
        return {"error": "search requires gmail or outlook provider"}

    if provider == "gmail":
        policy.require("secret.read", name="default/GOOGLE_ACCESS_TOKEN")
        policy.require("net.dial", host=GMAIL_API_HOST)
        return _search_gmail(opts.query, opts.max_results)
    elif provider == "outlook":
        policy.require("secret.read", name="default/MICROSOFT_ACCESS_TOKEN")
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
                "Set GOOGLE_ACCESS_TOKEN for Gmail "
                "or MICROSOFT_ACCESS_TOKEN for Outlook"
            ),
        }
    if provider == "smtp":
        return {"error": "list requires gmail or outlook provider"}

    if provider == "gmail":
        policy.require("secret.read", name="default/GOOGLE_ACCESS_TOKEN")
        policy.require("net.dial", host=GMAIL_API_HOST)
        return _list_gmail(opts.max_results, opts.unread)
    elif provider == "outlook":
        policy.require("secret.read", name="default/MICROSOFT_ACCESS_TOKEN")
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
                "Set GOOGLE_ACCESS_TOKEN for Gmail "
                "or MICROSOFT_ACCESS_TOKEN for Outlook"
            ),
        }
    if provider == "smtp":
        return {"error": "read requires gmail or outlook provider"}

    if provider == "gmail":
        policy.require("secret.read", name="default/GOOGLE_ACCESS_TOKEN")
        policy.require("net.dial", host=GMAIL_API_HOST)
        return _read_gmail(opts.message_id)
    elif provider == "outlook":
        policy.require("secret.read", name="default/MICROSOFT_ACCESS_TOKEN")
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
    policy.require("ai.chat", wild=True)

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

def run(command, args):
    """Entry point called by cos."""
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
