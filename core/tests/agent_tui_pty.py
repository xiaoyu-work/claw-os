#!/usr/bin/env python3
"""Exercise the Claw-owned Agent TUI through a Linux PTY."""

import argparse
import contextlib
from collections import Counter
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import pwd
import re
import select
import signal
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time
import uuid


SESSION_ID = "ses_001953abcdef0_123456789abc"
FORK_SESSION_ID = "ses_001953abcdef1_abcdef123456"
PRESENTATION_ID = "01234567-89ab-8cde-8123-456789abcdef"
ANSWER = "CLAW_TUI_PTY_COMPLETED_9F43"
RUNNING = "CLAW_TUI_PTY_RUNNING_3A91"
PENDING_APPROVAL_ID = "approval-center-pending"
RECENT_APPROVAL_ID = "approval-center-recent"
ACTIVITY_ID = "00000000-0000-4000-8000-000000000001"


class FixtureBroker:
    def __init__(self, path, case):
        self.path = path
        self.case = case
        self.requests = []
        self.errors = []
        self.cancelled = threading.Event()
        self.stopped = threading.Event()
        self.task_id = str(uuid.uuid4())
        self.queued_task_id = str(uuid.uuid4())
        self.queue_submitted = threading.Event()
        self.history_running_id = "task-history-running"
        self.history_failed_id = "task-history-failed"
        self.history_retry_id = "task-history-retry"
        self.history_cancelled = False
        self.notification_state = "unread"
        self.agent_hooks = {
            "logging": False,
            "audit": False,
            "checkpoint": True,
        }
        self.copilot_credential_present = True
        self.notification_preferences = {
            "web_enabled": True,
            "desktop_enabled": True,
            "ntfy_enabled": False,
            "web_min_severity": "info",
            "desktop_min_severity": "info",
            "ntfy_min_severity": "warning",
            "muted_kinds": [],
            "dnd_start_minute_utc": None,
            "dnd_end_minute_utc": None,
            "critical_bypasses_dnd": True,
            "retention_days": 30,
            "ntfy_server": "https://ntfy.sh",
            "ntfy_topic": None,
        }
        self.activity_state = "active"
        self.activity_completion_note = None
        self.activity_task_id = "task-activity-fixture"
        self.execution_limits = {
            "activity_id": ACTIVITY_ID,
            "owner_uid": os.geteuid(),
            "revision": 1,
            "enabled": True,
            "limits": {
                "max_attempts": 10,
                "max_turns_per_attempt": 5,
                "expires_at": "2035-01-01T00:00:00Z",
            },
            "used_attempts": 2,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }
        self.monetary_budget = {
            "activity_id": ACTIVITY_ID,
            "owner_uid": os.geteuid(),
            "revision": 1,
            "enabled": True,
            "spent_microusd": 100,
            "reserved_microusd": 50,
            "budget": {
                "currency": "USD",
                "max_total_microusd": 5000000,
                "input_microusd_per_million_tokens": 250000,
                "output_microusd_per_million_tokens": 1000000,
                "max_output_tokens_per_turn": 4096,
            },
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }
        self.scheduling_policy = {
            "activity_id": ACTIVITY_ID,
            "owner_uid": os.geteuid(),
            "revision": 1,
            "priority": "foreground",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }
        self.capability_policy = {
            "activity_id": ACTIVITY_ID,
            "owner_uid": os.geteuid(),
            "revision": 1,
            "enabled": True,
            "rules": [],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }
        self.cursors = {}
        self.requested_model = None
        self.requested_reasoning_effort = None
        self.requested_plan_only = False
        self.requested_workspace = None
        self.stream_disconnects = 0
        self.slow_progress_waiting = threading.Event()
        self.slow_progress_release = threading.Event()
        self.approval_state = "pending"
        self.approval_decided = threading.Event()
        self.home = Path(pwd.getpwuid(os.geteuid()).pw_dir)
        self.workspace = str(self.home / "project")
        self.session_id = SESSION_ID
        self.parent_session_id = SESSION_ID
        self.title = "Terminal integration fixture"
        self.archived = False
        self.backtrack_forked = False
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(str(path))
        self.listener.listen(8)
        self.listener.settimeout(0.2)
        self.thread = threading.Thread(target=self.serve)

    def conversation(self):
        jobs = [self.job("running")] if self.case == "resume-running" else []
        messages = []
        if self.case == "backtrack":
            messages = [
                {"role": "user", "text": "First retained prompt"},
                {"role": "assistant", "text": "First retained answer"},
                {"role": "user", "text": "Second editable prompt"},
                {"role": "assistant", "text": "Second retained answer"},
            ]
            if self.backtrack_forked:
                messages = messages[:2]
        return {
            "id": self.session_id,
            "presentation_id": PRESENTATION_ID,
            "title": self.title,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "archived": self.archived,
            "deleted": False,
            "parent_id": None,
            "messages": messages,
            "message_count": len(messages),
            "messages_truncated": False,
            "jobs": jobs,
            "job_count": len(jobs),
            "jobs_truncated": False,
        }

    def job(self, status, task_id=None, prompt=None, after_task_id=None):
        return {
            "id": task_id or self.task_id,
            "session_id": self.session_id,
            "workspace": self.requested_workspace or str(self.home),
            "after_task_id": after_task_id,
            "prompt": prompt or (
                "First line\nSecond line"
                if self.case == "multiline"
                else "Run the terminal integration fixture"
            ),
            "status": status,
            "created_at": "2026-01-01T00:00:00Z",
            "started_at": "2026-01-01T00:00:00Z",
            "finished_at": None if status == "running" else "2026-01-01T00:00:01Z",
            "provider": "ollama",
            "model": "tui-fixture",
            "requested_model": self.requested_model,
            "requested_reasoning_effort": self.requested_reasoning_effort,
            "plan_only": self.requested_plan_only,
            "turns_used": 2,
            "response": ANSWER if status == "ok" else None,
            "error": None,
            "waiting_on": [],
        }

    def history_job(self, task_id, status):
        return {
            "id": task_id,
            "session_id": self.session_id,
            "workspace": str(self.home),
            "prompt": f"Historical {status} task",
            "title": f"Historical {status} task",
            "status": status,
            "created_at": "2025-12-31T23:59:00Z",
            "started_at": "2025-12-31T23:59:01Z",
            "finished_at": (
                None
                if status in ("pending", "running", "waiting_approval")
                else "2025-12-31T23:59:02Z"
            ),
            "provider": "ollama",
            "model": "tui-fixture",
            "requested_model": "tui-fixture",
            "turns_used": 1,
            "response": None,
            "error": "fixture failure" if status == "error" else None,
            "waiting_on": [],
            "cancel_requested": False,
        }

    def notification(self):
        return {
            "schema": 1,
            "sequence": 1,
            "id": "notification-inbox-fixture",
            "owner_uid": os.geteuid(),
            "source": "agent",
            "kind": "task.completed",
            "severity": "info",
            "title": "Fixture task completed",
            "body": "The durable fixture task completed.",
            "delivery_policy": "immediate",
            "task_id": self.history_failed_id,
            "session_id": self.session_id,
            "state": self.notification_state,
            "occurrences": 1,
            "created_at_ms": 1767225600000,
            "updated_at_ms": 1767225600000,
            "actions": [{
                "id": "open-task",
                "label": "Open task",
                "uri": f"clawos://task/{self.history_failed_id}",
            }],
            "deliveries": [{
                "channel": "web",
                "state": "delivered",
                "attempts": 1,
            }],
        }

    def activity(self):
        return {
            "id": ACTIVITY_ID,
            "owner_uid": os.geteuid(),
            "title": "Fixture Activity",
            "goal": "Complete the fixture goal",
            "completion_criteria": "",
            "boundaries": "",
            "resources": [],
            "state": self.activity_state,
            "completion_note": self.activity_completion_note,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:01Z",
        }

    def activity_job(self):
        value = self.history_job(self.activity_task_id, "pending")
        value["activity_id"] = ACTIVITY_ID
        return value

    def hook_settings(self):
        return {
            "applies_to": "future_tasks",
            "hooks": [
                {"kind": kind, "enabled": self.agent_hooks[kind]}
                for kind in ("logging", "audit", "checkpoint")
            ],
        }

    def dispatch(self, method, params):
        if method == "daemon.status":
            return {"daemon": "clawd", "status": "running"}
        if method == "daemon.health":
            return {
                "daemon": "clawd",
                "status": "ok",
                "started_at": "2026-01-01T00:00:00Z",
                "uptime_ms": 1000,
            }
        if method == "agent.conversation.create":
            return {"conversation": self.conversation()}
        if method == "agent.conversation.get":
            if params["id"] == self.parent_session_id:
                self.session_id = self.parent_session_id
                self.title = "Terminal integration fixture"
                self.archived = False
            elif params["id"] not in (self.session_id, PRESENTATION_ID):
                raise AssertionError("terminal did not preserve the native session identity")
            return {"conversation": self.conversation()}
        if method == "agent.conversation.list":
            return {
                "conversations": [self.conversation()],
                "conversation_count": 1,
                "conversations_truncated": False,
            }
        if method == "agent.conversation.update":
            if params["id"] != self.session_id:
                raise AssertionError("conversation update addressed another session")
            if "title" in params:
                self.title = params["title"]
            if "archived" in params:
                self.archived = params["archived"]
            return {"conversation": self.conversation()}
        if method == "agent.conversation.fork":
            if params["id"] != self.session_id:
                raise AssertionError("conversation fork addressed another session")
            if self.case == "backtrack":
                if params.get("before_user_turn") != 1:
                    raise AssertionError("backtrack fork used the wrong retained prefix")
                self.backtrack_forked = True
            elif "before_user_turn" in params:
                raise AssertionError("ordinary fork unexpectedly truncated history")
            self.session_id = FORK_SESSION_ID
            self.title = "Forked terminal fixture"
            return {"conversation": self.conversation()}
        if method == "agent.conversation.revert":
            if params["id"] != self.session_id or params.get("user_turns") != 1:
                raise AssertionError("conversation rewind changed identity or count")
            return {"conversation": self.conversation()}
        if method == "memory.sessions":
            return {
                "n": 1 if self.case in ("platform", "memory-center") else 0,
                "sessions": [],
            }
        if method == "memory.reset":
            if self.case != "memory-center" or params != {"confirm": True}:
                raise AssertionError("unexpected learned-memory reset")
            return {
                "notes_deleted": 2,
                "app_memories_deleted": 3,
                "semantic_rows_deleted": 4,
                "conversations_preserved": True,
            }
        if method == "agent.hooks.get":
            if self.case != "hooks-center":
                raise AssertionError("unexpected Agent hooks query")
            return self.hook_settings()
        if method == "agent.hooks.set":
            if self.case != "hooks-center":
                raise AssertionError("unexpected Agent hooks mutation")
            kind = params.get("kind")
            enabled = params.get("enabled")
            if kind not in self.agent_hooks or not isinstance(enabled, bool):
                raise AssertionError("Agent hooks mutation was not closed and typed")
            changed = self.agent_hooks[kind] != enabled
            self.agent_hooks[kind] = enabled
            value = self.hook_settings()
            value.update({"updated_kind": kind, "changed": changed})
            return value
        if method == "agent.account.get":
            if self.case != "account-center":
                raise AssertionError("unexpected Agent account query")
            return {
                "provider": "copilot",
                "credential_present": self.copilot_credential_present,
            }
        if method == "agent.account.logout":
            if self.case != "account-center" or params != {"confirm": True}:
                raise AssertionError("unexpected Agent account logout")
            was_present = self.copilot_credential_present
            self.copilot_credential_present = False
            return {
                "provider": "copilot",
                "credential_present": False,
                "was_present": was_present,
            }
        if method == "agent.usage":
            return {
                "scope": "overall",
                "total": {
                    "calls": 2,
                    "success": 2,
                    "error": 0,
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "cache_read_tokens": 3,
                    "cache_write_tokens": 1,
                    "total_duration_ms": 50,
                    "finish_reasons": {"stop": 2},
                    "errors": 0,
                },
                "by_provider": {
                    "fixture": {
                        "calls": 2,
                        "success": 2,
                        "error": 0,
                        "input_tokens": 10,
                        "output_tokens": 5,
                        "cache_read_tokens": 3,
                        "cache_write_tokens": 1,
                        "total_duration_ms": 50,
                        "finish_reasons": {"stop": 2},
                        "errors": 0,
                    },
                },
                "by_model": {},
                "parse_errors": 0,
                "log_lines": 2,
                "log_bytes": 256,
                "breakdown_truncated": False,
            }
        if method == "memory.history":
            return {"session_id": SESSION_ID, "n": 0, "messages": []}
        if method == "permission.pending" and self.case in ("approval-center", "approval-choice"):
            metadata = self.case == "approval-choice"
            return {
                "requests": [{
                    "id": PENDING_APPROVAL_ID,
                    "verb": "fs.meta" if metadata else "fs.write",
                    "scope": {
                        "kind": "path",
                        "value": "/**" if metadata else "/home/claw/report.md",
                    },
                    "session": self.session_id,
                    "reason": (
                        "Inspect file details: path:/**"
                        if metadata
                        else "Write the requested report"
                    ),
                    "requested_at": 1767225600,
                    "risk": "low" if metadata else "high",
                    "requester": "Claw Agent",
                }]
            }
        if method == "permission.recent" and self.case == "approval-center":
            return {
                "requests": [{
                    "id": RECENT_APPROVAL_ID,
                    "verb": "net.dial",
                    "scope": {"kind": "host", "value": "example.com"},
                    "session": self.session_id,
                    "reason": "Fetch public release metadata",
                    "requested_at": 1767225500,
                    "risk": "medium",
                    "requester": "Claw Agent",
                    "decision": {
                        "outcome": "approved",
                        "decided_at": 1767225550,
                        "duration": "once",
                    },
                }]
            }
        if method in ("permission.pending", "permission.recent"):
            return {"requests": []}
        if method == "permission.status":
            return {
                "statuses": [
                    {
                        "id": approval_id,
                        "status": (
                            self.approval_state
                            if approval_id == PENDING_APPROVAL_ID
                            else "consumed"
                        ),
                    }
                    for approval_id in params["ids"]
                ]
            }
        if method == "permission.decide":
            if (
                self.case != "approval-choice"
                or params != {
                    "id": PENDING_APPROVAL_ID,
                    "decision": "approve",
                    "duration": "once",
                }
            ):
                raise AssertionError("permission approval was not exact and owner-bound")
            self.approval_state = "approved"
            self.approval_decided.set()
            return {
                "id": PENDING_APPROVAL_ID,
                "decision": "approved",
            }
        if method == "notification.list":
            return {
                "schema": 1,
                "cursor": 1,
                "unread": 1 if self.notification_state == "unread" else 0,
                "notifications": [self.notification()],
            }
        if method in (
            "notification.read",
            "notification.acknowledge",
            "notification.dismiss",
        ):
            if params["id"] != "notification-inbox-fixture":
                raise AssertionError("notification mutation addressed another record")
            self.notification_state = {
                "notification.read": "read",
                "notification.acknowledge": "acknowledged",
                "notification.dismiss": "dismissed",
            }[method]
            return self.notification()
        if method == "notification.preferences.get":
            return self.notification_preferences
        if method == "notification.preferences.set":
            self.notification_preferences = dict(params)
            return self.notification_preferences
        if method == "activity.create":
            if (
                params.get("title") != "Fixture Activity"
                or params.get("goal") != "Complete the fixture goal"
            ):
                raise AssertionError("Activity create changed its title or goal")
            self.activity_state = "active"
            self.activity_completion_note = None
            return self.activity()
        if method == "activity.list":
            return {"schema": 1, "activities": [self.activity()]}
        if method == "activity.get":
            if params["id"] != ACTIVITY_ID:
                raise AssertionError("Activity get addressed another goal")
            return {
                "schema": 1,
                "activity": self.activity(),
                "jobs": [],
                "sessions": [],
                "job_limit": 100,
            }
        if method == "activity.transition":
            if params["id"] != ACTIVITY_ID:
                raise AssertionError("Activity transition addressed another goal")
            self.activity_state = params["state"]
            self.activity_completion_note = params.get("completion_note")
            return self.activity()
        if method == "activity.run":
            if params["id"] != ACTIVITY_ID or params.get("workspace") != str(self.home):
                raise AssertionError("Activity run changed its goal or workspace")
            return self.activity_job()
        if method == "activity.attention":
            if params["id"] != ACTIVITY_ID:
                raise AssertionError("Activity attention addressed another goal")
            return {
                "schema": 1,
                "activity_id": ACTIVITY_ID,
                "activity_state": self.activity_state,
                "limit": 100,
                "counts": {
                    "queued": 1,
                    "running": 0,
                    "waiting": 0,
                    "completed": 0,
                    "failed": 0,
                    "cancelled": 0,
                    "indeterminate": 0,
                    "pending_decisions": 0,
                    "unavailable_decisions": 0,
                    "unread_notifications": 0,
                },
                "decisions": [],
                "issues": [],
                "totals": {"decisions": 0, "issues": 0, "notifications": 0},
                "has_more": {
                    "decisions": False,
                    "issues": False,
                    "notifications": False,
                },
                "notifications": [],
            }
        if method == "activity.objects":
            if params["id"] != ACTIVITY_ID:
                raise AssertionError("Activity objects addressed another goal")
            return {
                "schema": 1,
                "activity_id": ACTIVITY_ID,
                "objects": [{
                    "label": "Config proposal",
                    "reference": (
                        "app://fs/change-plan?id=%2Ftmp%2Fconfig"
                        "&revision=00000000-0000-4000-8000-000000000002"
                    ),
                    "status": "declared",
                    "description": {
                        "app_id": "fs",
                        "object_type": "change-plan",
                        "invocation": {
                            "operation": "plan_show",
                            "args": ["/tmp/config"],
                        },
                    },
                    "error": None,
                }],
            }
        if method == "activity.receipts":
            if params["id"] != ACTIVITY_ID:
                raise AssertionError("Activity receipts addressed another goal")
            return {
                "schema": 1,
                "activity_id": ACTIVITY_ID,
                "receipts": [{
                    "id": "00000000-0000-4000-8000-000000000003",
                    "activity_id": ACTIVITY_ID,
                    "received_at": "2026-01-01T00:00:02Z",
                    "source": "caller_reported",
                    "report": {
                        "app_id": "fs",
                        "operation": "plan_write",
                        "outcome": "returned",
                        "result": {
                            "preview": (
                                "App-reported file change plan; not authorization "
                                "or OS-confirmed effects.\n--- before\n+++ after"
                            ),
                        },
                    },
                }],
            }
        if method == "activity.object_state.list":
            if params["id"] != ACTIVITY_ID:
                raise AssertionError("Activity object state addressed another goal")
            return {
                "schema": 1,
                "activity_id": ACTIVITY_ID,
                "entries": [{
                    "id": "00000000-0000-4000-8000-000000000004",
                    "reference": "app://fs/file?id=%2Ftmp%2Fconfig",
                    "content": {
                        "kind": "user_statement",
                        "text": "Ready for review",
                    },
                    "source": "caller_reported",
                    "recorded_at": "2026-01-01T00:00:03Z",
                }],
            }
        if method == "activity.operation.preview":
            if (
                params["id"] != ACTIVITY_ID
                or params["app_id"] != "fs"
                or params["operation"] != "stat"
            ):
                raise AssertionError("operation preview changed its identity")
            return {
                "schema": 1,
                "app_id": "fs",
                "app_name": "Files",
                "app_version": "1",
                "package_digest": "sha256:" + "a" * 64,
                "operation": "stat",
                "operation_label": "Inspect file",
                "effects_declared": True,
                "effects": [{
                    "kind": "read",
                    "label": "Read file metadata",
                    "requested_targets": [],
                    "target_state": "unresolved",
                }],
                "unresolved_arguments": ["path"],
                "authorization_checked": False,
                "executed": False,
                "effects_confirmed": False,
                "notes": ["Preview only"],
            }
        if method == "activity.execution_limits.get":
            return {
                "schema": 1,
                "activity_id": ACTIVITY_ID,
                "execution_limits": self.execution_limits,
            }
        if method == "activity.execution_limits.set":
            if params.get("expected_revision") != self.execution_limits["revision"]:
                raise AssertionError("execution limit edit revision was not exact")
            self.execution_limits["revision"] += 1
            self.execution_limits["limits"] = params["limits"]
            return self.execution_limits
        if method == "activity.execution_limits.enabled":
            if params["expected_revision"] != self.execution_limits["revision"]:
                raise AssertionError("execution limit revision was not exact")
            self.execution_limits["revision"] += 1
            self.execution_limits["enabled"] = params["enabled"]
            return self.execution_limits
        if method == "activity.monetary_budget.get":
            return {
                "schema": 1,
                "activity_id": ACTIVITY_ID,
                "monetary_budget": self.monetary_budget,
            }
        if method == "activity.monetary_budget.set":
            if params.get("expected_revision") != self.monetary_budget["revision"]:
                raise AssertionError("monetary budget edit revision was not exact")
            self.monetary_budget["revision"] += 1
            self.monetary_budget["budget"] = params["budget"]
            return self.monetary_budget
        if method == "activity.monetary_budget.enabled":
            if params["expected_revision"] != self.monetary_budget["revision"]:
                raise AssertionError("monetary budget revision was not exact")
            self.monetary_budget["revision"] += 1
            self.monetary_budget["enabled"] = params["enabled"]
            return self.monetary_budget
        if method == "activity.scheduling_policy.get":
            return {
                "schema": 1,
                "activity_id": ACTIVITY_ID,
                "scheduling_policy": self.scheduling_policy,
            }
        if method == "activity.scheduling_policy.set":
            if params["expected_revision"] != self.scheduling_policy["revision"]:
                raise AssertionError("scheduling revision was not exact")
            self.scheduling_policy["revision"] += 1
            self.scheduling_policy["priority"] = params["priority"]
            return self.scheduling_policy
        if method == "activity.capability_policy.get":
            return {
                "schema": 1,
                "activity_id": ACTIVITY_ID,
                "capability_policy": self.capability_policy,
            }
        if method == "activity.capability_policy.set":
            if params.get("expected_revision") != self.capability_policy["revision"]:
                raise AssertionError("capability policy edit revision was not exact")
            self.capability_policy["revision"] += 1
            self.capability_policy["rules"] = params["policy"]["rules"]
            return self.capability_policy
        if method == "activity.capability_policy.enabled":
            if params["expected_revision"] != self.capability_policy["revision"]:
                raise AssertionError("capability policy revision was not exact")
            self.capability_policy["revision"] += 1
            self.capability_policy["enabled"] = params["enabled"]
            return self.capability_policy
        if method == "task.list":
            if self.case == "task-center":
                return {
                    "jobs": [
                        self.history_job(
                            self.history_running_id,
                            "cancelled" if self.history_cancelled else "running",
                        ),
                        self.history_job(self.history_failed_id, "error"),
                    ]
                }
            return {"jobs": []}
        if method == "task.workspace.resolve":
            requested = params.get("path")
            if requested not in (None, "project", self.workspace):
                raise AssertionError("workspace resolver received an unexpected path")
            return {"workspace": str(self.home if requested is None else self.home / "project")}
        if method == "task.submit":
            if params.get("session_id") != self.session_id:
                raise AssertionError("task was not submitted under the canonical conversation")
            if self.case == "durable-queue":
                after_task_id = params.get("after_task_id")
                if after_task_id is None:
                    expected_prompt = "Run the terminal integration fixture"
                    task_id = self.task_id
                    status = "running"
                else:
                    if after_task_id != self.task_id:
                        raise AssertionError("durable queue changed its predecessor")
                    expected_prompt = "Queued follow up"
                    task_id = self.queued_task_id
                    status = "pending"
                    self.queue_submitted.set()
                if params.get("prompt") != expected_prompt:
                    raise AssertionError("durable queue changed prompt text")
                self.requested_model = params.get("model")
                self.requested_workspace = params.get("workspace")
                if not self.requested_workspace:
                    raise AssertionError("durable queue omitted its broker workspace")
                return self.job(
                    status,
                    task_id=task_id,
                    prompt=expected_prompt,
                    after_task_id=after_task_id,
                )
            expected_prompt = (
                "First line\nSecond line"
                if self.case in ("multiline", "multiline-key")
                else "Inspect @notes.txt"
                if self.case == "file-mentions"
                else "Run the terminal integration fixture"
            )
            if params.get("prompt") != expected_prompt:
                raise AssertionError("terminal input was changed or dropped")
            if self.case == "attachments":
                attachments = params.get("attachments")
                if not isinstance(attachments, list) or len(attachments) != 1:
                    raise AssertionError("terminal omitted the selected image attachment")
                attachment = attachments[0]
                if (
                    attachment.get("name") != "fixture.png"
                    or attachment.get("media_type") != "image/png"
                    or "path" in attachment
                ):
                    raise AssertionError("terminal changed attachment identity or leaked its path")
                if not isinstance(attachment.get("data"), str):
                    raise AssertionError("terminal attachment omitted bounded image bytes")
            elif params.get("attachments"):
                raise AssertionError("terminal attached an image to the wrong task")
            if self.case == "task-controls":
                if (
                    params.get("use_memory") is not False
                    or params.get("max_turns") != 8
                    or params.get("reasoning_effort") != "minimal"
                    or params.get("plan_only") is not True
                ):
                    raise AssertionError("terminal task controls were not bound to submission")
            if self.case == "memory-center" and params.get("use_memory") is not False:
                raise AssertionError("memory center toggle was not bound to submission")
            self.requested_model = params.get("model")
            self.requested_reasoning_effort = params.get("reasoning_effort")
            self.requested_plan_only = params.get("plan_only", False)
            self.requested_workspace = params.get("workspace")
            if not self.requested_workspace:
                raise AssertionError("task submission omitted its broker workspace")
            return self.job("running")
        if method == "task.cancel":
            if params["id"] == self.history_running_id:
                self.history_cancelled = True
                value = self.history_job(self.history_running_id, "cancelled")
                value["cancelled"] = True
                return value
            if params["id"] == self.task_id:
                self.cancelled.set()
                value = self.job("cancelled")
                value["cancelled"] = True
                return value
            raise AssertionError("cancellation addressed the wrong task")
        if method in ("task.get", "task.status"):
            if params["id"] == self.history_running_id:
                return self.history_job(
                    self.history_running_id,
                    "cancelled" if self.history_cancelled else "running",
                )
            if params["id"] == self.history_failed_id:
                return self.history_job(self.history_failed_id, "error")
            if params["id"] == self.history_retry_id:
                return self.history_job(self.history_retry_id, "pending")
            if params["id"] == self.queued_task_id:
                return self.job(
                    "pending",
                    task_id=self.queued_task_id,
                    prompt="Queued follow up",
                    after_task_id=self.task_id,
                )
            return self.job("cancelled" if self.cancelled.is_set() else "running")
        if method == "task.retry":
            if params["id"] != self.history_failed_id:
                raise AssertionError("retry addressed the wrong task")
            return self.history_job(self.history_retry_id, "pending")
        if method == "task.stream":
            task_id = params["id"]
            if task_id not in (self.task_id, self.queued_task_id):
                raise AssertionError("stream addressed the wrong task")
            if self.case == "reconnect" and self.stream_disconnects < 2:
                self.stream_disconnects += 1
                raise ConnectionAbortedError("fixture broker restart")
            events = []
            if params.get("cursor", 0) == 0:
                tool_name = "cos_delegate" if self.case == "agents" else "cos_sysinfo"
                events = [
                    {"event": {"kind": "text_delta", "text": RUNNING + "\n"}},
                    {"event": {"kind": "tool_use_start", "id": "tool-1", "name": tool_name}},
                    {"event": {
                        "kind": "tool_use", "id": "tool-1",
                        "name": tool_name, "input": None,
                    }},
                    {"event": {
                        "kind": "done", "finish": "tool_use",
                        "usage": {
                            "input_tokens": 12, "output_tokens": 4,
                            "cache_read_tokens": 0, "cache_write_tokens": 0,
                        },
                    }},
                    {"progress": {"kind": "tool_start", "id": "tool-1", "name": tool_name}},
                ]
                if self.case == "approval-choice":
                    events.append({
                        "progress": {
                            "kind": "waiting_approval",
                            "request_ids": [PENDING_APPROVAL_ID],
                        },
                    })
                if self.case in (
                    "complete",
                    "resume",
                    "commands",
                    "confirmations",
                    "multiline",
                    "multiline-key",
                    "approval-center",
                    "attachments",
                    "appearance",
                    "agents",
                    "side",
                    "platform",
                    "memory-center",
                    "hooks-center",
                    "mcp-center",
                    "extensions-center",
                    "usage-center",
                    "debug-center",
                    "account-center",
                    "voice-settings",
                    "copy-export",
                    "raw-scrollback",
                    "vim",
                    "file-mentions",
                    "activity-lifecycle",
                    "activity-controls",
                    "activity-evidence",
                    "activity-review",
                    "notification-inbox",
                    "workspace",
                    "task-center",
                    "task-controls",
                    "reconnect",
                    "startup-reconnect",
                ) or (self.case == "durable-queue" and task_id == self.queued_task_id):
                    events.extend([
                        {"progress": {
                            "kind": "tool_result", "id": "tool-1", "name": tool_name,
                            "ok": True, "latency_ms": 42,
                        }},
                        {"event": {"kind": "text_delta", "text": "\n## Result\n\n" + ANSWER}},
                        {"event": {
                            "kind": "done", "finish": "stop",
                            "usage": {
                                "input_tokens": 20, "output_tokens": 10,
                                "cache_read_tokens": 0, "cache_write_tokens": 0,
                            },
                        }},
                    ])
                self.cursors[task_id] = len(events)
            elif self.case == "slow-progress":
                self.slow_progress_waiting.set()
                if not self.slow_progress_release.wait(timeout=10):
                    raise AssertionError("slow progress fixture was not released")
                events = [
                    {"progress": {
                        "kind": "tool_result", "id": "tool-1", "name": "cos_sysinfo",
                        "ok": True, "latency_ms": 1234,
                    }},
                    {"event": {"kind": "text_delta", "text": "\n## Result\n\n" + ANSWER}},
                    {"event": {
                        "kind": "done", "finish": "stop",
                        "usage": {
                            "input_tokens": 20, "output_tokens": 10,
                            "cache_read_tokens": 0, "cache_write_tokens": 0,
                        },
                    }},
                ]
                self.cursors[task_id] = params["cursor"] + len(events)
            terminal = self.case in (
                "complete",
                "resume",
                "commands",
                "confirmations",
                "multiline",
                "multiline-key",
                "approval-center",
                "attachments",
                "appearance",
                "agents",
                "side",
                "platform",
                "memory-center",
                "hooks-center",
                "mcp-center",
                "extensions-center",
                "usage-center",
                "debug-center",
                "account-center",
                "voice-settings",
                "copy-export",
                "raw-scrollback",
                "vim",
                "file-mentions",
                "activity-lifecycle",
                "activity-controls",
                "activity-evidence",
                "activity-review",
                "notification-inbox",
                "workspace",
                "task-center",
                "task-controls",
                "reconnect",
                "startup-reconnect",
            ) or (self.case == "slow-progress" and self.slow_progress_release.is_set()) or (
                self.case == "approval-choice" and self.approval_decided.is_set()
            ) or (
                self.case == "durable-queue"
                and (task_id == self.queued_task_id or self.queue_submitted.is_set())
            ) or self.cancelled.is_set()
            if not terminal:
                time.sleep(0.05)
            status = "cancelled" if self.cancelled.is_set() else "ok" if terminal else "running"
            prompt = (
                "Queued follow up"
                if task_id == self.queued_task_id
                else "Run the terminal integration fixture"
            )
            after_task_id = self.task_id if task_id == self.queued_task_id else None
            return {
                "cursor": self.cursors.get(task_id, params.get("cursor", 0)),
                "events": events,
                "terminal": terminal,
                "job": self.job(
                    status,
                    task_id=task_id,
                    prompt=prompt,
                    after_task_id=after_task_id,
                ),
            }
        raise AssertionError(f"unexpected broker method: {method}")

    @staticmethod
    def read_exact(connection, length):
        chunks = bytearray()
        while len(chunks) < length:
            chunk = connection.recv(length - len(chunks))
            if not chunk:
                raise EOFError("truncated broker request")
            chunks.extend(chunk)
        return bytes(chunks)

    def serve(self):
        while not self.stopped.is_set():
            try:
                connection, _ = self.listener.accept()
            except socket.timeout:
                continue
            with connection:
                connection.settimeout(3)
                try:
                    header = self.read_exact(connection, 10)
                    magic, kind, reserved, length = struct.unpack(">4sBBI", header)
                    if (magic, kind, reserved) != (b"CBK1", 1, 0) or length > 1024 * 1024:
                        raise AssertionError("invalid broker framing")
                    request = json.loads(self.read_exact(connection, length))
                    self.requests.append(request)
                    try:
                        result = self.dispatch(request["command"], request.get("params", {}))
                        response = {
                            "v": 2, "id": request["id"], "ok": True,
                            "result": result, "error": None,
                        }
                    except (AssertionError, KeyError) as error:
                        self.errors.append(str(error))
                        response = {
                            "v": 2, "id": request["id"], "ok": False, "result": None,
                            "error": {"code": "execution_failed", "message": str(error)},
                        }
                    encoded = json.dumps(response).encode()
                    connection.sendall(struct.pack(">4sBBI", b"CBK1", 2, 0, len(encoded)) + encoded)
                except ConnectionAbortedError:
                    pass
                except BrokenPipeError as error:
                    if self.case != "resume-running":
                        self.errors.append(str(error))
                except (OSError, EOFError, ValueError, AssertionError) as error:
                    self.errors.append(str(error))

    def close(self):
        self.stopped.set()
        self.slow_progress_release.set()
        self.thread.join(timeout=5)
        self.listener.close()
        if self.thread.is_alive():
            raise AssertionError("fixture broker did not stop")


def read_terminal(master, output, deadline, predicate, exit_process=None):
    pending = b""
    while time.monotonic() < deadline:
        if predicate(output):
            return
        readable, _, _ = select.select([master], [], [], 0.1)
        if not readable:
            continue
        try:
            chunk = os.read(master, 65536)
        except OSError as error:
            if error.errno == errno.EIO:
                if exit_process is not None:
                    exit_process.wait(timeout=max(0.1, deadline - time.monotonic()))
                break
            raise
        if not chunk:
            if exit_process is not None:
                exit_process.wait(timeout=max(0.1, deadline - time.monotonic()))
            break
        output.extend(chunk)
        pending += chunk
        for query, response in (
            (b"\x1b[6n", b"\x1b[1;1R"),
            (b"\x1b[c", b"\x1b[?1;2c"),
            (b"\x1b[>c", b"\x1b[>0;0;0c"),
            (b"\x1b[?u", b"\x1b[?0u"),
            (b"\x1b]10;?\x1b\\", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
            (b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
        ):
            if query in pending:
                os.write(master, response)
                pending = pending.replace(query, b"")
        pending = pending[-16:]
    if not predicate(output):
        errors = re.findall(r"Error: ([^\r\n]+)", output.decode(errors="replace"))
        detail = errors[-1] if errors else "no completed interaction before timeout"
        raise AssertionError(f"terminal interaction did not reach its expected state: {detail}")


def terminal_child():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


def send_prompt(master, output, text):
    os.write(master, text.encode())
    settled = time.monotonic() + 0.4
    read_terminal(master, output, settled + 2, lambda _data: time.monotonic() >= settled)
    os.write(master, b"\r")


def process_diagnostics(pid):
    records = []
    pending = [pid]
    while pending and len(records) < 16:
        current = pending.pop()
        root = Path("/proc") / str(current)
        try:
            status = root.joinpath("status").read_text().splitlines()
            records.append({
                "pid": current,
                "comm": root.joinpath("comm").read_text().strip(),
                "status": [line for line in status if line.startswith(("State:", "PPid:", "Threads:"))],
                "wait": root.joinpath("wchan").read_text().strip(),
            })
            children = root.joinpath("task", str(current), "children").read_text()
            pending.extend(int(child) for child in children.split())
        except OSError as error:
            records.append({"pid": current, "inspection_error": str(error)})
    return records


def run(cos, case, transcript, original_namespace, trace):
    if os.geteuid() == 0:
        raise RuntimeError("run this fixture as a non-root account, like ordinary Agent chat")
    output = bytearray()
    with tempfile.TemporaryDirectory(prefix="claw-tui-pty-") as temporary, contextlib.ExitStack() as cleanup:
        root = Path(temporary)
        if (
            not re.fullmatch(r"mnt:\[\d+\]", original_namespace)
            or os.readlink("/proc/self/ns/mnt") == original_namespace
        ):
            raise RuntimeError("launch through unshare --user --map-current-user --keep-caps --mount --net")
        home = Path(pwd.getpwuid(os.geteuid()).pw_dir)
        shadow_home = root / "home"
        shadow_home.mkdir(mode=0o700)
        (shadow_home / "project").mkdir(mode=0o700)
        if case == "attachments":
            (shadow_home / "project" / "fixture.png").write_bytes(
                b"\x89PNG\r\n\x1a\nfixture"
            )
        if case == "file-mentions":
            (shadow_home / "notes.txt").write_text("mention path only")
        subprocess.run(["mount", "--bind", str(shadow_home), str(home)], check=True)
        cleanup.callback(subprocess.run, ["umount", str(home)], check=True)
        config = root / "config.json"
        agent_config = {
            "provider": "copilot" if case in ("task-controls", "account-center") else "ollama",
            "model": "tui-fixture",
            "base_url": "http://127.0.0.1:1",
        }
        if case in (
            "platform",
            "memory-center",
            "hooks-center",
            "mcp-center",
            "extensions-center",
            "usage-center",
            "debug-center",
            "account-center",
            "voice-settings",
        ):
            agent_config.update({
                "mcp_servers": [{
                    "name": "fixture_mcp",
                    "command": "false",
                    "enabled": True,
                }],
                "agent_api_discovery_enabled": False,
            })
            (root / "apps").mkdir()
            (root / "extensions").mkdir()
        config_document = {"agent": agent_config}
        if case == "voice-settings":
            config_document.update({
                "stt": {
                    "provider": "none",
                    "model": "system-stt",
                },
                "tts": {
                    "provider": "none",
                    "model": "system-tts",
                    "default_voice": "claw",
                    "default_format": "wav",
                },
            })
        config.write_text(json.dumps(config_document))
        broker_path = root / "clawd.sock"
        broker = None if case == "startup-reconnect" else FixtureBroker(broker_path, case)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 110, 0, 0))
        environment = {
            **os.environ,
            "TERM": "xterm-256color",
            "CLAWD_SOCKET": str(broker_path),
            "COS_CONFIG_PATH": str(config),
            "COS_DATA_DIR": str(root / "data"),
            "COS_USER_DATA_DIR": str(home / ".local" / "share" / "cos"),
            "COS_APPS_DIR": str(root / "apps"),
            "COS_AGENT_EXTENSIONS_DIR": str(root / "extensions"),
        }
        command = [str(cos), "agent", "chat", "--tui"]
        if case == "plain":
            command = [str(cos), "agent", "chat", "--plain", "--no-memory"]
        elif case in ("resume", "resume-running"):
            command.extend(["--session", PRESENTATION_ID])
        if trace:
            command = ["strace", "-f", "-tt", "-o", str(trace), *command]
        process = subprocess.Popen(
            command,
            stdin=slave, stdout=slave, stderr=slave, cwd=root, env=environment,
            preexec_fn=terminal_child,
        )
        os.close(slave)
        if broker is None:
            read_terminal(
                master,
                output,
                time.monotonic() + 10,
                lambda data: b"Claw broker is unavailable; retrying" in data,
            )
            broker = FixtureBroker(broker_path, case)
        broker.thread.start()
        try:
            if case == "plain":
                read_terminal(
                    master, output, time.monotonic() + 30,
                    lambda data: b"you> " in data,
                )
                send_prompt(master, output, "/quit")
                read_terminal(
                    master, output, time.monotonic() + 15,
                    lambda _data: process.poll() is not None,
                    exit_process=process,
                )
                if process.wait(timeout=5) != 0 or broker.requests:
                    raise AssertionError("plain chat did not preserve its independent line interface")
                print(json.dumps({"case": case, "task_submissions": 0, "completed": True}))
                return
            ready_at = None

            def ready(data):
                nonlocal ready_at
                opened = any(
                    request["command"] in (
                        "agent.conversation.create",
                        "agent.conversation.get",
                    )
                    for request in broker.requests
                )
                if opened and b"CLAW" in data:
                    if ready_at is None:
                        ready_at = time.monotonic()
                    return time.monotonic() - ready_at >= 0.5
                return False

            read_terminal(
                master, output, time.monotonic() + 45,
                ready,
            )
            if case == "backtrack":
                os.write(master, b"\x1b")
                time.sleep(0.2)
                os.write(master, b"\x1b")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Backtrack conversation" in data,
                )
                os.write(master, b"\x1b[B\r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.fork"
                        and request["params"].get("before_user_turn") == 1
                        for request in broker.requests
                    ),
                )
                os.write(master, b"\x1b")
                settled = time.monotonic() + 0.2
                read_terminal(
                    master,
                    output,
                    settled + 2,
                    lambda _data: time.monotonic() >= settled,
                )
                send_prompt(master, output, "/quit")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: process.poll() is not None,
                    exit_process=process,
                )
                if process.wait(timeout=5) != 0:
                    raise AssertionError(f"TUI exited with {process.returncode}")
                if broker.errors:
                    raise AssertionError("; ".join(broker.errors))
                submissions = sum(
                    request["command"] == "task.submit"
                    for request in broker.requests
                )
                if submissions != 0:
                    raise AssertionError("backtrack submitted the edited prompt automatically")
                print(json.dumps({
                    "case": case,
                    "task_submissions": submissions,
                    "completed": True,
                }))
                return
            if case == "attachments":
                send_prompt(master, output, "/attach project/fixture.png")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Attached image fixture.png" in data,
                )
            if case == "appearance":
                send_prompt(master, output, "/appearance")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Appearance" in data
                    and b"Local terminal presentation only" in data,
                )
                os.write(master, b"ths\x1b")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Claw - Terminal integration fixture" in data,
                )
                settled = time.monotonic() + 0.4
                read_terminal(
                    master,
                    output,
                    settled + 2,
                    lambda _data: time.monotonic() >= settled,
                )
            if case == "task-controls":
                os.write(master, b"\x14")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Task model controls" in data,
                )
                os.write(master, b"prmt\x1b")
                time.sleep(0.4)
            if case == "memory-center":
                send_prompt(master, output, "/platform")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Verified read-only inventory" in data,
                )
                os.write(master, b"m")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Memory Center" in data
                    and b"Use and record memory for future tasks: on" in data,
                )
                os.write(master, b"mr")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Reset learned memory?" in data,
                )
                if any(
                    request["command"] == "memory.reset"
                    for request in broker.requests
                ):
                    raise AssertionError("learned memory reset ran before confirmation")
                os.write(master, b"y")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Reset learned memory:" in data
                    and any(
                        request["command"] == "memory.reset"
                        and request["params"] == {"confirm": True}
                        for request in broker.requests
                    ),
                )
            if case == "hooks-center":
                send_prompt(master, output, "/platform")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Verified read-only inventory" in data,
                )
                os.write(master, b"h")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Hooks Center" in data
                    and b"Built-in lifecycle hooks" in data
                    and any(
                        request["command"] == "agent.hooks.get"
                        for request in broker.requests
                    ),
                )
                os.write(master, b"la")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"logging: on" in data
                    and b"audit: on" in data
                    and sum(
                        request["command"] == "agent.hooks.set"
                        for request in broker.requests
                    )
                    == 2,
                )
                os.write(master, b"\x1b")
                time.sleep(0.4)
                os.write(master, b"\x1b")
                time.sleep(0.4)
            if case == "mcp-center":
                send_prompt(master, output, "/platform")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Verified read-only inventory" in data,
                )
                os.write(master, b"c")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"MCP Center" in data
                    and b"fixture_mcp" in data
                    and b"operator config" in data,
                )
                os.write(master, b"\x1b")
                time.sleep(0.4)
                os.write(master, b"\x1b")
                time.sleep(0.4)
            if case == "extensions-center":
                send_prompt(master, output, "/platform")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Verified read-only inventory" in data,
                )
                os.write(master, b"e")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Extensions Center" in data
                    and b"One authenticated Claw extension model" in data,
                )
                os.write(master, b"\x1b")
                time.sleep(0.4)
                os.write(master, b"\x1b")
                time.sleep(0.4)
            if case == "usage-center":
                send_prompt(master, output, "/platform")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Verified read-only inventory" in data,
                )
                os.write(master, b"u")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Usage Center" in data
                    and sum(
                        request["command"] == "agent.usage"
                        for request in broker.requests
                    )
                    >= 2,
                )
                os.write(master, b"d")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Usage Center" in data
                    and any(
                        request["command"] == "agent.usage"
                        and "--since" in request["params"].get("args", [])
                        for request in broker.requests
                    ),
                )
                os.write(master, b"\x1b")
                time.sleep(0.4)
                os.write(master, b"\x1b")
                time.sleep(0.4)
            if case == "debug-center":
                send_prompt(master, output, "/platform")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Verified read-only inventory" in data,
                )
                os.write(master, b"d")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Debug Center" in data
                    and any(
                        request["command"] == "daemon.health"
                        for request in broker.requests
                    ),
                )
                os.write(master, b"\x1b")
                time.sleep(0.4)
                os.write(master, b"\x1b")
                time.sleep(0.4)
            if case == "account-center":
                send_prompt(master, output, "/platform")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Verified read-only inventory" in data,
                )
                os.write(master, b"a")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Account Center" in data
                    and any(
                        request["command"] == "agent.account.get"
                        for request in broker.requests
                    ),
                )
                os.write(master, b"l")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Log out of GitHub Copilot?" in data,
                )
                if any(
                    request["command"] == "agent.account.logout"
                    for request in broker.requests
                ):
                    raise AssertionError("Agent account logged out before confirmation")
                os.write(master, b"y")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Copilot credential revoked" in data
                    and any(
                        request["command"] == "agent.account.logout"
                        and request["params"] == {"confirm": True}
                        for request in broker.requests
                    ),
                )
            if case == "voice-settings":
                os.write(master, b"\x1b[97;10u")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Voice Center" in data
                    and b"system-stt" in data
                    and b"system-tts" in data,
                )
                os.write(master, b"\x1b")
                time.sleep(0.4)
            if case == "file-mentions":
                os.write(master, b"Inspect \x06")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Mention workspace file" in data,
                )
                os.write(master, b"notes\r")
                time.sleep(0.4)
                os.write(master, b"\r")
            if case == "commands":
                send_prompt(master, output, "/resume")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.list"
                        for request in broker.requests
                    ),
                )
                os.write(master, b"\r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.get"
                        for request in broker.requests
                    ),
                )
                send_prompt(master, output, "/rename Command fixture")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.update"
                        and request["params"].get("title") == "Command fixture"
                        for request in broker.requests
                    ),
                )
                send_prompt(master, output, "/fork")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.fork"
                        for request in broker.requests
                    ),
                )
            if case == "confirmations":
                send_prompt(master, output, "/archive")
                settled = time.monotonic() + 0.5
                read_terminal(
                    master,
                    output,
                    settled + 2,
                    lambda _data: time.monotonic() >= settled,
                )
                if any(
                    request["command"] == "agent.conversation.update"
                    for request in broker.requests
                ):
                    raise AssertionError("archive mutated before confirmation")
                os.write(master, b"n")
                time.sleep(0.2)
                send_prompt(master, output, "/archive")
                time.sleep(0.5)
                os.write(master, b"y")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.update"
                        and request["params"].get("archived") is True
                        for request in broker.requests
                    ),
                )
                send_prompt(master, output, "/unarchive")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.update"
                        and request["params"].get("archived") is False
                        for request in broker.requests
                    ),
                )
                send_prompt(master, output, "/rewind 1")
                time.sleep(0.5)
                os.write(master, b"y")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.revert"
                        for request in broker.requests
                    ),
                )
            if case == "task-center":
                send_prompt(master, output, "/tasks")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "task.list"
                        for request in broker.requests
                    ),
                )
                os.write(master, b"\r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "task.get"
                        and request["params"].get("id") == broker.history_running_id
                        for request in broker.requests
                    ),
                )
                os.write(master, b"c")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "task.cancel"
                        and request["params"].get("id") == broker.history_running_id
                        for request in broker.requests
                    )
                    and sum(
                        request["command"] == "task.get"
                        and request["params"].get("id") == broker.history_running_id
                        for request in broker.requests
                    )
                    >= 2,
                )
                time.sleep(0.2)
                os.write(master, b"\x1b")
                time.sleep(0.2)
                send_prompt(master, output, "/tasks")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: sum(
                        request["command"] == "task.list"
                        for request in broker.requests
                    )
                    >= 2,
                )
                os.write(master, b"\x1b[B\r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "task.get"
                        and request["params"].get("id") == broker.history_failed_id
                        for request in broker.requests
                    ),
                )
                os.write(master, b"r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: any(
                        request["command"] == "task.retry"
                        and request["params"].get("id") == broker.history_failed_id
                        for request in broker.requests
                    )
                    and broker.history_retry_id.encode() in data,
                )
                os.write(master, b"\x1b")
                time.sleep(0.2)
            if case == "approval-center":
                send_prompt(master, output, "/approvals")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: all(
                        any(request["command"] == command for request in broker.requests)
                        for command in (
                            "permission.pending",
                            "permission.recent",
                            "permission.status",
                        )
                    )
                    and b"Approval center" in data,
                )
                os.write(master, b"\r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: PENDING_APPROVAL_ID.encode() in data,
                )
                os.write(master, b"\x1b")
                time.sleep(0.2)
                send_prompt(master, output, "/approvals")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: sum(
                        request["command"] == "permission.recent"
                        for request in broker.requests
                    )
                    >= 2,
                )
                time.sleep(0.3)
                os.write(master, b"\x1b[B\r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: RECENT_APPROVAL_ID.encode() in data,
                )
                os.write(master, b"a")
                time.sleep(0.2)
                os.write(master, b"\x1b")
                time.sleep(0.3)
            if case == "notification-inbox":
                send_prompt(master, output, "/inbox")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: any(
                        request["command"] == "notification.list"
                        for request in broker.requests
                    )
                    and b"Notification Inbox" in data,
                )
                os.write(master, b"\r")
                time.sleep(0.2)
                for key, command, state in [
                    (b"m", "notification.read", b"READ"),
                    (b"a", "notification.acknowledge", b"ACKNOWLEDGED"),
                    (b"d", "notification.dismiss", b"DISMISSED"),
                ]:
                    os.write(master, key)
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data, expected=command: any(
                            request["command"] == expected
                            for request in broker.requests
                        ),
                    )
                    time.sleep(0.4)
                os.write(master, b"\x1b")
                time.sleep(0.3)
                send_prompt(master, output, "/notify-settings")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: any(
                        request["command"] == "notification.preferences.get"
                        for request in broker.requests
                    )
                    and b"Notification delivery settings" in data,
                )
                os.write(master, b"w")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "notification.preferences.set"
                        and request["params"].get("web_enabled") is False
                        for request in broker.requests
                    ),
                )
                os.write(master, b"q")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "notification.preferences.set"
                        and request["params"].get("dnd_start_minute_utc") == 0
                        and request["params"].get("dnd_end_minute_utc") == 0
                        for request in broker.requests
                    ),
                )
                os.write(master, b"\x1b")
                time.sleep(0.3)
            if case == "activity-lifecycle":
                send_prompt(master, output, "/activities")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: any(
                        request["command"] == "activity.list"
                        for request in broker.requests
                    )
                    and b"Activities" in data,
                )
                os.write(master, b"\r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: any(
                        request["command"] == "activity.get"
                        for request in broker.requests
                    )
                    and ACTIVITY_ID.encode() in data,
                )
                os.write(master, b"\x1b")
                time.sleep(0.3)
                send_prompt(
                    master,
                    output,
                    "/activity-create Fixture Activity | Complete the fixture goal",
                )
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "activity.create"
                        for request in broker.requests
                    )
                    and sum(
                        request["command"] == "activity.get"
                        for request in broker.requests
                    )
                    >= 2,
                )
                time.sleep(0.3)
                os.write(master, b"p")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "activity.transition"
                        and request["params"].get("state") == "paused"
                        for request in broker.requests
                    ),
                )
                time.sleep(0.3)
                os.write(master, b"u")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "activity.transition"
                        and request["params"].get("state") == "active"
                        for request in broker.requests
                    ),
                )
                time.sleep(0.3)
                os.write(master, b"r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: any(
                        request["command"] == "activity.run"
                        for request in broker.requests
                    )
                    and broker.activity_task_id.encode() in data,
                )
                os.write(master, b"\x1b")
                time.sleep(0.3)
                send_prompt(master, output, f"/activity {ACTIVITY_ID}")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: sum(
                        request["command"] == "activity.get"
                        for request in broker.requests
                    )
                    >= 2,
                )
                time.sleep(0.3)
                os.write(master, b"a")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: any(
                        request["command"] == "activity.attention"
                        for request in broker.requests
                    )
                    and b"Activity attention" in data,
                )
                os.write(master, b"\x1b")
                time.sleep(0.3)
                send_prompt(
                    master,
                    output,
                    f"/activity-complete {ACTIVITY_ID} | User confirmed completion",
                )
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Complete Activity goal?" in data,
                )
                os.write(master, b"y")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "activity.transition"
                        and request["params"].get("state") == "completed"
                        and request["params"].get("completion_note")
                        == "User confirmed completion"
                        for request in broker.requests
                    ),
                )
                time.sleep(0.3)
                send_prompt(master, output, f"/activity-resume {ACTIVITY_ID}")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: sum(
                        request["command"] == "activity.transition"
                        and request["params"].get("state") == "active"
                        for request in broker.requests
                    )
                    >= 2,
                )
                time.sleep(0.3)
                send_prompt(master, output, f"/activity-cancel {ACTIVITY_ID}")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Cancel Activity goal?" in data,
                )
                os.write(master, b"y")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "activity.transition"
                        and request["params"].get("state") == "cancelled"
                        for request in broker.requests
                    ),
                )
            if case == "activity-controls":
                send_prompt(master, output, f"/activity-controls {ACTIVITY_ID}")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: all(
                        any(request["command"] == command for request in broker.requests)
                        for command in (
                            "activity.execution_limits.get",
                            "activity.monetary_budget.get",
                            "activity.scheduling_policy.get",
                            "activity.capability_policy.get",
                        )
                    )
                    and b"Activity controls" in data,
                )
                time.sleep(0.4)
                for refresh, key, command in [
                    (2, b"l\r", "activity.execution_limits.enabled"),
                    (3, b"b\r", "activity.monetary_budget.enabled"),
                    (4, b"p\r", "activity.scheduling_policy.set"),
                    (5, b"c\r", "activity.capability_policy.enabled"),
                ]:
                    os.write(master, key)
                    read_terminal(
                        master,
                        output,
                        time.monotonic() + 15,
                        lambda _data, expected=command, count=refresh: any(
                            request["command"] == expected
                            for request in broker.requests
                        )
                        and sum(
                            request["command"] == "activity.execution_limits.get"
                            for request in broker.requests
                        )
                        >= count,
                    )
                    time.sleep(0.4)
                os.write(master, b"\x1b")
                time.sleep(0.3)
                send_prompt(
                    master,
                    output,
                    (
                        f"/activity-limits-set {ACTIVITY_ID} 2 | "
                        '{"max_attempts":12,"max_turns_per_attempt":4,'
                        '"expires_at":"2036-01-01T00:00:00Z"}'
                    ),
                )
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "activity.execution_limits.set"
                        for request in broker.requests
                    )
                    and sum(
                        request["command"] == "activity.execution_limits.get"
                        for request in broker.requests
                    )
                    >= 6,
                )
                time.sleep(0.4)
                os.write(master, b"\x1b")
                time.sleep(0.3)
            if case == "activity-evidence":
                send_prompt(master, output, f"/activity-evidence {ACTIVITY_ID}")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: all(
                        any(request["command"] == command for request in broker.requests)
                        for command in (
                            "activity.objects",
                            "activity.receipts",
                            "activity.object_state.list",
                        )
                    ),
                )
                os.write(master, b"p")
                time.sleep(0.2)
                send_prompt(master, output, "fs stat | []")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: any(
                        request["command"] == "activity.operation.preview"
                        for request in broker.requests
                    )
                    and b"Activity operation preview" in data,
                )
                os.write(master, b"e")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: sum(
                        request["command"] == "activity.objects"
                        for request in broker.requests
                    )
                    >= 2,
                )
                os.write(master, b"\x1b")
                time.sleep(0.4)
            if case == "activity-review":
                send_prompt(master, output, f"/review {ACTIVITY_ID}")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Staged file review" in data
                    and b"staged_file_plans" in data
                    and all(
                        any(request["command"] == command for request in broker.requests)
                        for command in ("activity.objects", "activity.receipts")
                    ),
                )
                os.write(master, b"\x1b")
                time.sleep(0.4)
            if case == "workspace":
                send_prompt(master, output, "/workspace project")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "task.workspace.resolve"
                        for request in broker.requests
                    ),
                )
            if case == "durable-queue":
                send_prompt(master, output, "Run the terminal integration fixture")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: RUNNING.encode() in data
                    and sum(
                        request["command"] == "task.submit"
                        for request in broker.requests
                    )
                    == 1,
                )
                send_prompt(master, output, "Queued follow up")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "task.submit"
                        and request["params"].get("after_task_id") == broker.task_id
                        and request["params"].get("prompt") == "Queued follow up"
                        for request in broker.requests
                    ),
                )
            if case == "multiline":
                os.write(master, b"\x1b[200~First line\nSecond line\x1b[201~")
                settled = time.monotonic() + 0.4
                read_terminal(
                    master,
                    output,
                    settled + 2,
                    lambda _data: time.monotonic() >= settled,
                )
                os.write(master, b"\r")
            elif case == "multiline-key":
                os.write(master, b"First line\x0aSecond line")
                settled = time.monotonic() + 0.4
                read_terminal(
                    master,
                    output,
                    settled + 2,
                    lambda _data: time.monotonic() >= settled,
                )
                os.write(master, b"\r")
            elif case == "vim":
                send_prompt(master, output, "/vim")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"VIM NORMAL" in data,
                )
                os.write(master, b"iRun the terminal integration fixture\r")
            elif case not in ("durable-queue", "resume-running", "file-mentions"):
                send_prompt(master, output, "Run the terminal integration fixture")
            marker = (
                ANSWER
                if case in (
                    "agents",
                    "complete",
                    "durable-queue",
                    "copy-export",
                    "raw-scrollback",
                    "vim",
                )
                else RUNNING
            )
            if case == "slow-progress":
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: broker.slow_progress_waiting.is_set()
                    and b"Running" in data
                    and b"cos_sysinfo" in data,
                )
                animated_at = len(output)
                first_tick_due = time.monotonic() + 0.35
                read_terminal(
                    master,
                    output,
                    first_tick_due + 2,
                    lambda data: time.monotonic() >= first_tick_due
                    and len(data) > animated_at,
                )
                first_tick_at = len(output)
                second_tick_due = time.monotonic() + 0.75
                read_terminal(
                    master,
                    output,
                    second_tick_due + 2,
                    lambda data: time.monotonic() >= second_tick_due
                    and len(data) > first_tick_at,
                )
                broker.slow_progress_release.set()
                marker = ANSWER
            if case == "reconnect":
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"RECONNECTING" in data,
                )
                marker = ANSWER
            read_terminal(
                master, output, time.monotonic() + 30,
                lambda data: marker.encode() in data,
            )
            if case == "copy-export":
                send_prompt(master, output, "/copy")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"\x1b]52;c;" in data,
                )
                send_prompt(master, output, "/export conversation.md")
                exported = shadow_home / "conversation.md"
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: exported.is_file(),
                )
                if ANSWER not in exported.read_text():
                    raise AssertionError("conversation export omitted the assistant answer")
            if case == "raw-scrollback":
                send_prompt(master, output, "/raw")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"--- Claw transcript snapshot ---" in data
                    and ANSWER.encode() in data
                    and b"--- end Claw transcript snapshot ---" in data,
                )
            if case == "agents":
                send_prompt(master, output, "/agents")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Scoped delegate calls" in data
                    and b"tool-1" in data,
                )
                os.write(master, b"\x1b")
                time.sleep(0.3)
            if case == "side":
                send_prompt(master, output, "/side")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.fork"
                        for request in broker.requests
                    ),
                )
                send_prompt(master, output, "/side return")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: any(
                        request["command"] == "agent.conversation.update"
                        and request["params"].get("archived") is True
                        for request in broker.requests
                    )
                    and any(
                        request["command"] == "agent.conversation.get"
                        and request["params"].get("id") == broker.parent_session_id
                        for request in broker.requests
                    ),
                )
            if case == "platform":
                send_prompt(master, output, "/platform")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Verified read-only inventory" in data
                    and b"fixture_mcp" in data
                    and any(
                        request["command"] == "agent.usage"
                        for request in broker.requests
                    )
                    and any(
                        request["command"] == "memory.sessions"
                        for request in broker.requests
                    ),
                )
                os.write(master, b"\x1b")
                time.sleep(0.3)
            if case == "approval-choice":
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda data: b"Authorize once" in data
                    and b"Inspect file details" in data
                    and b"All files and folders" in data
                    and b"without reading the contents" in data
                    and b"Left/Right choose" in data
                    and any(
                        request["command"] == "permission.pending"
                        for request in broker.requests
                    ),
                )
                os.write(master, b"\x1b[97;1:2u")
                time.sleep(0.2)
                if any(
                    request["command"] == "permission.decide"
                    for request in broker.requests
                ):
                    raise AssertionError("repeated approval shortcut was accepted")
                os.write(master, b"\r")
                read_terminal(
                    master,
                    output,
                    time.monotonic() + 15,
                    lambda _data: broker.approval_decided.is_set(),
                )
                settled = time.monotonic() + 0.5
                read_terminal(
                    master,
                    output,
                    settled + 2,
                    lambda _data: time.monotonic() >= settled,
                )
                if b"Claw OS authorization" in output:
                    raise AssertionError("TUI approval opened the password prompt")
            if case == "cancel":
                os.write(master, b"\x1b")
                read_terminal(
                    master, output, time.monotonic() + 15,
                    lambda _data: broker.cancelled.is_set(),
                )
            send_prompt(master, output, "/quit")
            read_terminal(
                master, output, time.monotonic() + 15,
                lambda _data: process.poll() is not None,
                exit_process=process,
            )
            if process.wait(timeout=5) != 0:
                raise AssertionError(f"TUI exited with {process.returncode}")
            if broker.errors:
                raise AssertionError("; ".join(broker.errors))
            submissions = sum(request["command"] == "task.submit" for request in broker.requests)
            expected_submissions = (
                2 if case == "durable-queue"
                else 0 if case == "resume-running"
                else 1
            )
            if submissions != expected_submissions:
                raise AssertionError(
                    f"expected {expected_submissions} actual task submission(s), got {submissions}"
                )
            if case == "workspace" and not any(
                request["command"] == "task.submit"
                and request["params"].get("workspace") == broker.workspace
                for request in broker.requests
            ):
                raise AssertionError("task submission did not retain the resolved workspace")
            if case == "resume" and any(
                request["command"] == "agent.conversation.create"
                for request in broker.requests
            ):
                raise AssertionError("resume created a replacement conversation")
            if case == "startup-reconnect":
                creates = sum(
                    request["command"] == "agent.conversation.create"
                    for request in broker.requests
                )
                if creates != 1:
                    raise AssertionError(
                        f"startup reconnect created {creates} conversations"
                    )
            print(json.dumps({"case": case, "task_submissions": submissions, "completed": True}))
        except AssertionError:
            if transcript:
                transcript.with_suffix(".diagnostics.json").write_text(json.dumps({
                    "requests": list(dict.fromkeys(request["command"] for request in broker.requests)),
                    "request_counts": dict(Counter(request["command"] for request in broker.requests)),
                    "broker_errors": broker.errors,
                    "processes": process_diagnostics(process.pid),
                }, indent=2))
            raise
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
            os.close(master)
            broker.close()
            if transcript:
                transcript.write_bytes(output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cos", type=Path, required=True)
    parser.add_argument(
        "--case",
        choices=(
            "complete",
            "slow-progress",
            "cancel",
            "commands",
            "confirmations",
            "durable-queue",
            "multiline",
            "multiline-key",
            "approval-center",
            "approval-choice",
            "attachments",
            "appearance",
            "agents",
            "side",
            "platform",
            "memory-center",
            "hooks-center",
            "mcp-center",
            "extensions-center",
            "usage-center",
            "debug-center",
            "account-center",
            "voice-settings",
            "copy-export",
            "raw-scrollback",
            "vim",
            "file-mentions",
            "backtrack",
            "activity-lifecycle",
            "activity-controls",
            "activity-evidence",
            "activity-review",
            "notification-inbox",
            "plain",
            "reconnect",
            "resume",
            "resume-running",
            "startup-reconnect",
            "task-center",
            "task-controls",
            "workspace",
        ),
        default="complete",
    )
    parser.add_argument("--transcript", type=Path)
    parser.add_argument("--original-mount-namespace", required=True)
    parser.add_argument("--trace", type=Path)
    arguments = parser.parse_args()
    run(
        arguments.cos.resolve(strict=True),
        arguments.case,
        arguments.transcript,
        arguments.original_mount_namespace,
        arguments.trace.resolve() if arguments.trace else None,
    )
