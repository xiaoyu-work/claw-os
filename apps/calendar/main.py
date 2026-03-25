"""Calendar — local events with optional Google/Outlook sync.

Local SQLite calendar as default (zero config), plus Google Calendar and
Outlook providers when credentials are configured.
"""

import json
import os
import random
import sqlite3
import string
import time
import urllib.parse
import urllib.request
from datetime import datetime, timedelta, timezone

DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _db_path():
    """Return the full path to the local calendar database."""
    return os.path.join(DATA_DIR, "calendar", "events.db")


def _init_db():
    """Open (and create if needed) the local events database."""
    path = _db_path()
    os.makedirs(os.path.dirname(path), exist_ok=True)
    conn = sqlite3.connect(path)
    conn.execute("""
        CREATE TABLE IF NOT EXISTS events (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            start_time TEXT NOT NULL,
            end_time TEXT,
            description TEXT DEFAULT '',
            location TEXT DEFAULT '',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )
    """)
    conn.commit()
    return conn


def _new_id():
    """Generate a unique event ID like ``evt-<timestamp>-<random>``."""
    ts = int(time.time())
    suffix = "".join(random.choices(string.ascii_lowercase + string.digits, k=6))
    return f"evt-{ts}-{suffix}"


def _parse_args(args):
    """Parse ``--key value`` pairs from an argument list into a dict."""
    result = {}
    i = 0
    while i < len(args):
        if args[i].startswith("--") and i + 1 < len(args):
            key = args[i][2:].replace("-", "_")
            result[key] = args[i + 1]
            i += 2
        else:
            i += 1
    return result


def _detect_provider(explicit=None):
    """Return the calendar provider to use.

    If *explicit* is given use that; otherwise sniff environment variables
    for Google / Outlook tokens, falling back to ``"local"``.
    """
    if explicit:
        return explicit
    if os.environ.get("GOOGLE_CALENDAR_TOKEN") or os.environ.get("GOOGLE_OAUTH_TOKEN"):
        return "google"
    if os.environ.get("MICROSOFT_ACCESS_TOKEN") or os.environ.get("MICROSOFT_OAUTH_TOKEN"):
        return "outlook"
    return "local"


def _now_iso():
    """Return the current UTC time as an ISO-8601 string."""
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _default_end(start_iso):
    """Return *start + 1 hour* as an ISO-8601 string."""
    try:
        # Handle both 'Z' suffix and '+00:00' offset
        cleaned = start_iso.replace("Z", "+00:00")
        dt = datetime.fromisoformat(cleaned)
        return (dt + timedelta(hours=1)).strftime("%Y-%m-%dT%H:%M:%SZ")
    except (ValueError, TypeError):
        return start_iso


# ---------------------------------------------------------------------------
# Google Calendar helpers
# ---------------------------------------------------------------------------

def _google_token():
    return os.environ.get("GOOGLE_CALENDAR_TOKEN") or os.environ.get("GOOGLE_OAUTH_TOKEN")


def _google_request(method, url, body=None, token=None):
    """Make an authenticated request to Google Calendar API."""
    headers = {"Authorization": f"Bearer {token}"}
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req) as resp:
        if resp.status == 204:
            return {}
        return json.loads(resp.read().decode())


def _google_event_to_dict(item):
    """Normalise a Google Calendar event into our common format."""
    start = item.get("start", {})
    end = item.get("end", {})
    return {
        "id": item.get("id", ""),
        "title": item.get("summary", ""),
        "start": start.get("dateTime", start.get("date", "")),
        "end": end.get("dateTime", end.get("date", "")),
        "description": item.get("description", ""),
        "location": item.get("location", ""),
    }


# ---------------------------------------------------------------------------
# Outlook helpers
# ---------------------------------------------------------------------------

def _outlook_token():
    return os.environ.get("MICROSOFT_ACCESS_TOKEN") or os.environ.get("MICROSOFT_OAUTH_TOKEN")


def _outlook_request(method, url, body=None, token=None):
    """Make an authenticated request to Microsoft Graph API."""
    headers = {"Authorization": f"Bearer {token}"}
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req) as resp:
        if resp.status == 204:
            return {}
        return json.loads(resp.read().decode())


def _outlook_event_to_dict(item):
    """Normalise an Outlook event into our common format."""
    start = item.get("start", {})
    end = item.get("end", {})
    return {
        "id": item.get("id", ""),
        "title": item.get("subject", ""),
        "start": start.get("dateTime", ""),
        "end": end.get("dateTime", ""),
        "description": item.get("bodyPreview", ""),
        "location": (item.get("location") or {}).get("displayName", ""),
    }


# ---------------------------------------------------------------------------
# list command
# ---------------------------------------------------------------------------

def _list_local(from_time, to_time):
    conn = _init_db()
    try:
        rows = conn.execute(
            "SELECT id, title, start_time, end_time, description, location "
            "FROM events WHERE start_time >= ? AND start_time < ? ORDER BY start_time",
            (from_time, to_time),
        ).fetchall()
    finally:
        conn.close()
    return [
        {"id": r[0], "title": r[1], "start": r[2], "end": r[3],
         "description": r[4], "location": r[5]}
        for r in rows
    ]


def _list_google(from_time, to_time):
    token = _google_token()
    if not token:
        return None  # sentinel — caller returns error
    qs = urllib.parse.urlencode({
        "timeMin": from_time,
        "timeMax": to_time,
        "singleEvents": "true",
        "orderBy": "startTime",
    })
    url = f"https://www.googleapis.com/calendar/v3/calendars/primary/events?{qs}"
    data = _google_request("GET", url, token=token)
    return [_google_event_to_dict(item) for item in data.get("items", [])]


def _list_outlook(from_time, to_time):
    token = _outlook_token()
    if not token:
        return None
    qs = urllib.parse.urlencode({
        "startDateTime": from_time,
        "endDateTime": to_time,
        "$orderby": "start/dateTime",
    })
    url = f"https://graph.microsoft.com/v1.0/me/calendarview?{qs}"
    data = _outlook_request("GET", url, token=token)
    return [_outlook_event_to_dict(item) for item in data.get("value", [])]


def cmd_list(args):
    """List events in a time range."""
    parsed = _parse_args(args)
    from_time = parsed.get("from")
    to_time = parsed.get("to")
    if not from_time or not to_time:
        return {"error": "usage: calendar list --from <datetime> --to <datetime> [--provider local|google|outlook]"}

    provider = _detect_provider(parsed.get("provider"))

    try:
        if provider == "google":
            events = _list_google(from_time, to_time)
            if events is None:
                return {
                    "error": "Google Calendar token not configured",
                    "hint": "Set GOOGLE_CALENDAR_TOKEN or GOOGLE_OAUTH_TOKEN credential",
                }
        elif provider == "outlook":
            events = _list_outlook(from_time, to_time)
            if events is None:
                return {
                    "error": "Outlook token not configured",
                    "hint": "Set MICROSOFT_ACCESS_TOKEN or MICROSOFT_OAUTH_TOKEN credential",
                }
        else:
            events = _list_local(from_time, to_time)
    except urllib.error.HTTPError as exc:
        return {"error": f"API request failed ({exc.code})", "detail": exc.reason}
    except urllib.error.URLError as exc:
        return {"error": f"API request failed: {exc.reason}"}

    return {
        "provider": provider,
        "from": from_time,
        "to": to_time,
        "events": events,
        "count": len(events),
    }


# ---------------------------------------------------------------------------
# create command
# ---------------------------------------------------------------------------

def _create_local(title, start, end, description, location):
    event_id = _new_id()
    now = _now_iso()
    conn = _init_db()
    try:
        conn.execute(
            "INSERT INTO events (id, title, start_time, end_time, description, location, created_at, updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            (event_id, title, start, end, description, location, now, now),
        )
        conn.commit()
    finally:
        conn.close()
    return {
        "id": event_id,
        "title": title,
        "start": start,
        "end": end,
        "description": description,
        "location": location,
    }


def _create_google(title, start, end, description, location):
    token = _google_token()
    if not token:
        return None
    body = {
        "summary": title,
        "start": {"dateTime": start},
        "end": {"dateTime": end},
        "description": description,
        "location": location,
    }
    url = "https://www.googleapis.com/calendar/v3/calendars/primary/events"
    data = _google_request("POST", url, body=body, token=token)
    return _google_event_to_dict(data)


def _create_outlook(title, start, end, description, location):
    token = _outlook_token()
    if not token:
        return None
    body = {
        "subject": title,
        "start": {"dateTime": start, "timeZone": "UTC"},
        "end": {"dateTime": end, "timeZone": "UTC"},
        "body": {"contentType": "text", "content": description},
        "location": {"displayName": location},
    }
    url = "https://graph.microsoft.com/v1.0/me/events"
    data = _outlook_request("POST", url, body=body, token=token)
    return _outlook_event_to_dict(data)


def cmd_create(args):
    """Create an event."""
    parsed = _parse_args(args)
    title = parsed.get("title")
    start = parsed.get("start")
    if not title:
        return {"error": "usage: calendar create --title <title> --start <datetime> [--end <datetime>] [--description <text>] [--location <text>] [--provider local|google|outlook]"}
    if not start:
        return {"error": "usage: calendar create --title <title> --start <datetime> [--end <datetime>]"}

    end = parsed.get("end") or _default_end(start)
    description = parsed.get("description", "")
    location = parsed.get("location", "")
    provider = _detect_provider(parsed.get("provider"))

    try:
        if provider == "google":
            event = _create_google(title, start, end, description, location)
            if event is None:
                return {
                    "error": "Google Calendar token not configured",
                    "hint": "Set GOOGLE_CALENDAR_TOKEN or GOOGLE_OAUTH_TOKEN credential",
                }
        elif provider == "outlook":
            event = _create_outlook(title, start, end, description, location)
            if event is None:
                return {
                    "error": "Outlook token not configured",
                    "hint": "Set MICROSOFT_ACCESS_TOKEN or MICROSOFT_OAUTH_TOKEN credential",
                }
        else:
            event = _create_local(title, start, end, description, location)
    except urllib.error.HTTPError as exc:
        return {"error": f"API request failed ({exc.code})", "detail": exc.reason}
    except urllib.error.URLError as exc:
        return {"error": f"API request failed: {exc.reason}"}

    return {"created": True, "provider": provider, "event": event}


# ---------------------------------------------------------------------------
# update command
# ---------------------------------------------------------------------------

def _update_local(event_id, fields):
    conn = _init_db()
    try:
        row = conn.execute("SELECT id FROM events WHERE id = ?", (event_id,)).fetchone()
        if row is None:
            return None
        col_map = {
            "title": "title",
            "start": "start_time",
            "end": "end_time",
            "description": "description",
            "location": "location",
        }
        sets = []
        values = []
        for key, col in col_map.items():
            if key in fields:
                sets.append(f"{col} = ?")
                values.append(fields[key])
        if not sets:
            return {"id": event_id}
        sets.append("updated_at = ?")
        values.append(_now_iso())
        values.append(event_id)
        conn.execute(f"UPDATE events SET {', '.join(sets)} WHERE id = ?", values)
        conn.commit()
        updated = conn.execute(
            "SELECT id, title, start_time, end_time, description, location FROM events WHERE id = ?",
            (event_id,),
        ).fetchone()
    finally:
        conn.close()
    return {
        "id": updated[0], "title": updated[1], "start": updated[2],
        "end": updated[3], "description": updated[4], "location": updated[5],
    }


def _update_google(event_id, fields):
    token = _google_token()
    if not token:
        return None
    body = {}
    if "title" in fields:
        body["summary"] = fields["title"]
    if "start" in fields:
        body["start"] = {"dateTime": fields["start"]}
    if "end" in fields:
        body["end"] = {"dateTime": fields["end"]}
    if "description" in fields:
        body["description"] = fields["description"]
    if "location" in fields:
        body["location"] = fields["location"]
    url = f"https://www.googleapis.com/calendar/v3/calendars/primary/events/{urllib.parse.quote(event_id)}"
    data = _google_request("PATCH", url, body=body, token=token)
    return _google_event_to_dict(data)


def _update_outlook(event_id, fields):
    token = _outlook_token()
    if not token:
        return None
    body = {}
    if "title" in fields:
        body["subject"] = fields["title"]
    if "start" in fields:
        body["start"] = {"dateTime": fields["start"], "timeZone": "UTC"}
    if "end" in fields:
        body["end"] = {"dateTime": fields["end"], "timeZone": "UTC"}
    if "description" in fields:
        body["body"] = {"contentType": "text", "content": fields["description"]}
    if "location" in fields:
        body["location"] = {"displayName": fields["location"]}
    url = f"https://graph.microsoft.com/v1.0/me/events/{urllib.parse.quote(event_id)}"
    data = _outlook_request("PATCH", url, body=body, token=token)
    return _outlook_event_to_dict(data)


def cmd_update(args):
    """Update an event."""
    parsed = _parse_args(args)
    event_id = parsed.get("id")
    if not event_id:
        return {"error": "usage: calendar update --id <event-id> [--title <title>] [--start <datetime>] [--end <datetime>] [--description <text>] [--provider local|google|outlook]"}

    provider = _detect_provider(parsed.get("provider"))
    fields = {k: v for k, v in parsed.items() if k in ("title", "start", "end", "description", "location")}

    try:
        if provider == "google":
            token = _google_token()
            if not token:
                return {
                    "error": "Google Calendar token not configured",
                    "hint": "Set GOOGLE_CALENDAR_TOKEN or GOOGLE_OAUTH_TOKEN credential",
                }
            event = _update_google(event_id, fields)
        elif provider == "outlook":
            token = _outlook_token()
            if not token:
                return {
                    "error": "Outlook token not configured",
                    "hint": "Set MICROSOFT_ACCESS_TOKEN or MICROSOFT_OAUTH_TOKEN credential",
                }
            event = _update_outlook(event_id, fields)
        else:
            event = _update_local(event_id, fields)
            if event is None:
                return {"error": f"event not found: {event_id}"}
    except urllib.error.HTTPError as exc:
        return {"error": f"API request failed ({exc.code})", "detail": exc.reason}
    except urllib.error.URLError as exc:
        return {"error": f"API request failed: {exc.reason}"}

    return {"updated": True, "provider": provider, "event": event}


# ---------------------------------------------------------------------------
# delete command
# ---------------------------------------------------------------------------

def _delete_local(event_id):
    conn = _init_db()
    try:
        row = conn.execute("SELECT id FROM events WHERE id = ?", (event_id,)).fetchone()
        if row is None:
            return False
        conn.execute("DELETE FROM events WHERE id = ?", (event_id,))
        conn.commit()
    finally:
        conn.close()
    return True


def _delete_google(event_id):
    token = _google_token()
    if not token:
        return None
    url = f"https://www.googleapis.com/calendar/v3/calendars/primary/events/{urllib.parse.quote(event_id)}"
    _google_request("DELETE", url, token=token)
    return True


def _delete_outlook(event_id):
    token = _outlook_token()
    if not token:
        return None
    url = f"https://graph.microsoft.com/v1.0/me/events/{urllib.parse.quote(event_id)}"
    _outlook_request("DELETE", url, token=token)
    return True


def cmd_delete(args):
    """Delete an event."""
    parsed = _parse_args(args)
    event_id = parsed.get("id")
    if not event_id:
        return {"error": "usage: calendar delete --id <event-id> [--provider local|google|outlook]"}

    provider = _detect_provider(parsed.get("provider"))

    try:
        if provider == "google":
            result = _delete_google(event_id)
            if result is None:
                return {
                    "error": "Google Calendar token not configured",
                    "hint": "Set GOOGLE_CALENDAR_TOKEN or GOOGLE_OAUTH_TOKEN credential",
                }
        elif provider == "outlook":
            result = _delete_outlook(event_id)
            if result is None:
                return {
                    "error": "Outlook token not configured",
                    "hint": "Set MICROSOFT_ACCESS_TOKEN or MICROSOFT_OAUTH_TOKEN credential",
                }
        else:
            result = _delete_local(event_id)
            if not result:
                return {"error": f"event not found: {event_id}"}
    except urllib.error.HTTPError as exc:
        return {"error": f"API request failed ({exc.code})", "detail": exc.reason}
    except urllib.error.URLError as exc:
        return {"error": f"API request failed: {exc.reason}"}

    return {"deleted": True, "provider": provider, "id": event_id}


# ---------------------------------------------------------------------------
# today command
# ---------------------------------------------------------------------------

def cmd_today(args):
    """Show today's events (midnight-to-midnight UTC)."""
    now = datetime.now(timezone.utc)
    today_start = now.replace(hour=0, minute=0, second=0, microsecond=0)
    tomorrow_start = today_start + timedelta(days=1)
    new_args = [
        "--from", today_start.strftime("%Y-%m-%dT%H:%M:%SZ"),
        "--to", tomorrow_start.strftime("%Y-%m-%dT%H:%M:%SZ"),
    ] + args
    return cmd_list(new_args)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

COMMANDS = {
    "list": cmd_list,
    "create": cmd_create,
    "update": cmd_update,
    "delete": cmd_delete,
    "today": cmd_today,
}


def _schema():
    return {
        "list": {
            "description": "List events in a time range",
            "parameters": [
                {"name": "--from", "type": "string", "required": True, "description": "Start datetime in ISO-8601 format", "kind": "flag"},
                {"name": "--to", "type": "string", "required": True, "description": "End datetime in ISO-8601 format", "kind": "flag"},
                {"name": "--provider", "type": "string", "required": False, "description": "Calendar provider: local, google, or outlook (auto-detected if omitted)", "kind": "flag"},
            ],
            "example": "cos app calendar list --from 2025-01-01T00:00:00Z --to 2025-01-02T00:00:00Z",
        },
        "create": {
            "description": "Create a new calendar event",
            "parameters": [
                {"name": "--title", "type": "string", "required": True, "description": "Event title", "kind": "flag"},
                {"name": "--start", "type": "string", "required": True, "description": "Start datetime in ISO-8601 format", "kind": "flag"},
                {"name": "--end", "type": "string", "required": False, "description": "End datetime in ISO-8601 format (defaults to start + 1 hour)", "kind": "flag"},
                {"name": "--description", "type": "string", "required": False, "description": "Event description", "kind": "flag", "default": ""},
                {"name": "--location", "type": "string", "required": False, "description": "Event location", "kind": "flag", "default": ""},
                {"name": "--provider", "type": "string", "required": False, "description": "Calendar provider: local, google, or outlook", "kind": "flag"},
            ],
            "example": "cos app calendar create --title 'Team Meeting' --start 2025-01-15T10:00:00Z --end 2025-01-15T11:00:00Z",
        },
        "update": {
            "description": "Update an existing calendar event",
            "parameters": [
                {"name": "--id", "type": "string", "required": True, "description": "Event ID to update", "kind": "flag"},
                {"name": "--title", "type": "string", "required": False, "description": "New event title", "kind": "flag"},
                {"name": "--start", "type": "string", "required": False, "description": "New start datetime", "kind": "flag"},
                {"name": "--end", "type": "string", "required": False, "description": "New end datetime", "kind": "flag"},
                {"name": "--description", "type": "string", "required": False, "description": "New event description", "kind": "flag"},
                {"name": "--location", "type": "string", "required": False, "description": "New event location", "kind": "flag"},
                {"name": "--provider", "type": "string", "required": False, "description": "Calendar provider: local, google, or outlook", "kind": "flag"},
            ],
            "example": "cos app calendar update --id evt-123-abc --title 'Updated Meeting'",
        },
        "delete": {
            "description": "Delete a calendar event",
            "parameters": [
                {"name": "--id", "type": "string", "required": True, "description": "Event ID to delete", "kind": "flag"},
                {"name": "--provider", "type": "string", "required": False, "description": "Calendar provider: local, google, or outlook", "kind": "flag"},
            ],
            "example": "cos app calendar delete --id evt-123-abc",
        },
        "today": {
            "description": "Show today's events (midnight-to-midnight UTC)",
            "parameters": [
                {"name": "--provider", "type": "string", "required": False, "description": "Calendar provider: local, google, or outlook", "kind": "flag"},
            ],
            "example": "cos app calendar today",
        },
    }


def run(command, args):
    """Entry point called by cos."""
    if command == "__schema__":
        return _schema()

    # Re-read DATA_DIR in case COS_DATA_DIR changed (e.g. in tests).
    global DATA_DIR
    DATA_DIR = os.environ.get("COS_DATA_DIR", "/var/lib/cos")

    handler = COMMANDS.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    return handler(args)
