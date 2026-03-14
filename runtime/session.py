"""Agent OS session manager.

Manages isolated agent sessions inside the rootfs using chroot or containers.
Each session gets its own workspace directory and resource tracking.
"""

import json
import os
import subprocess
import time
import uuid


SESSIONS_DIR = "/var/lib/aos/sessions"
WORKSPACE_BASE = "/workspace"


def _session_path(session_id):
    return os.path.join(SESSIONS_DIR, session_id)


def _save_session(session_id, data):
    path = _session_path(session_id)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        json.dump(data, f)


def _load_session(session_id):
    path = _session_path(session_id)
    if not os.path.exists(path):
        return None
    with open(path, "r") as f:
        return json.load(f)


def create(name=None):
    """Create a new agent session."""
    session_id = uuid.uuid4().hex[:12]
    workspace = os.path.join(WORKSPACE_BASE, session_id)
    os.makedirs(workspace, exist_ok=True)

    session = {
        "id": session_id,
        "name": name or session_id,
        "workspace": workspace,
        "created_at": time.time(),
        "status": "active",
        "pid": None,
    }
    _save_session(session_id, session)
    return session


def get(session_id):
    """Get session info."""
    return _load_session(session_id)


def list_sessions():
    """List all sessions."""
    if not os.path.isdir(SESSIONS_DIR):
        return []
    sessions = []
    for name in sorted(os.listdir(SESSIONS_DIR)):
        data = _load_session(name)
        if data:
            sessions.append(data)
    return sessions


def destroy(session_id):
    """Destroy a session and clean up its workspace."""
    session = _load_session(session_id)
    if session is None:
        return False
    # Clean up workspace
    workspace = session.get("workspace", "")
    if workspace and os.path.isdir(workspace):
        subprocess.run(["rm", "-rf", workspace], check=False)
    # Remove session file
    path = _session_path(session_id)
    if os.path.exists(path):
        os.remove(path)
    return True


def exec_in_session(session_id, command):
    """Execute a command in a session's workspace."""
    session = _load_session(session_id)
    if session is None:
        return {"error": f"session not found: {session_id}"}

    try:
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            cwd=session["workspace"],
            timeout=300,
        )
        return {
            "session": session_id,
            "command": command,
            "exit_code": result.returncode,
            "stdout": result.stdout,
            "stderr": result.stderr,
        }
    except Exception as e:
        return {"error": str(e)}
