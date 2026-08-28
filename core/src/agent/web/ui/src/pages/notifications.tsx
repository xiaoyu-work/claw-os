import { Bell, BellOff, Check, CircleAlert, Loader2, X } from "lucide-react";
import { useEffect, useState } from "react";

import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  type NotificationPreferences,
  type NotificationRecord,
  useNotifications,
} from "@/lib/notifications";

export function NotificationsPage() {
  const notifications = useNotifications();
  const [draft, setDraft] = useState<NotificationPreferences | null>(null);
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    if (notifications.preferences) setDraft(notifications.preferences);
  }, [notifications.preferences]);

  async function save() {
    if (!draft) return;
    setSaving(true);
    setMessage(null);
    try {
      await notifications.savePreferences(draft);
      setMessage("Notification preferences saved.");
    } catch (cause: any) {
      setMessage(cause?.message || "Failed to save notification preferences");
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="flex h-full flex-col gap-4 overflow-y-auto p-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">Notifications</h1>
          <p className="text-xs text-muted-foreground">
            Live Agent, scheduled-task, reminder, and system alerts.
          </p>
        </div>
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <span
            className={`h-2 w-2 rounded-full ${
              notifications.connected ? "bg-emerald-500" : "bg-amber-500"
            }`}
          />
          {notifications.connected ? "Live" : "Reconnecting"}
        </div>
      </div>

      <Card className="grid gap-3 p-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div>
            <p className="text-sm font-medium">Browser notifications</p>
            <p className="text-xs text-muted-foreground">
              Show alerts while this page is in the background.
            </p>
          </div>
          {notifications.browserEnabled ? (
            <Button variant="outline" size="sm" onClick={notifications.disableBrowserNotifications}>
              <BellOff className="mr-2 h-3.5 w-3.5" />
              Disable
            </Button>
          ) : (
            <Button
              size="sm"
              onClick={() =>
                void notifications
                  .enableBrowserNotifications()
                  .catch((error) => setMessage(error.message))
              }
            >
              <Bell className="mr-2 h-3.5 w-3.5" />
              Enable
            </Button>
          )}
        </div>

        {draft && (
          <>
            <div className="grid gap-3 sm:grid-cols-3">
              <Toggle
                label="Web activity"
                checked={draft.web_enabled}
                onChange={(checked) => setDraft({ ...draft, web_enabled: checked })}
              />
              <Toggle
                label="Desktop popups"
                checked={draft.desktop_enabled}
                onChange={(checked) => setDraft({ ...draft, desktop_enabled: checked })}
              />
              <Toggle
                label="ntfy push"
                checked={draft.ntfy_enabled}
                onChange={(checked) => setDraft({ ...draft, ntfy_enabled: checked })}
              />
            </div>
            <div className="grid gap-3 sm:grid-cols-3">
              <SeveritySelect
                label="Web minimum severity"
                value={draft.web_min_severity}
                onChange={(value) => setDraft({ ...draft, web_min_severity: value })}
              />
              <SeveritySelect
                label="Desktop minimum severity"
                value={draft.desktop_min_severity}
                onChange={(value) =>
                  setDraft({ ...draft, desktop_min_severity: value })
                }
              />
              <SeveritySelect
                label="ntfy minimum severity"
                value={draft.ntfy_min_severity}
                onChange={(value) => setDraft({ ...draft, ntfy_min_severity: value })}
              />
            </div>
            {draft.ntfy_enabled && (
              <label className="grid gap-1 text-xs">
                <span className="font-medium">ntfy topic</span>
                <input
                  className="h-9 rounded-md border bg-background px-3"
                  value={draft.ntfy_topic || ""}
                  onChange={(event) =>
                    setDraft({ ...draft, ntfy_topic: event.target.value || undefined })
                  }
                  placeholder="my-claw-alerts"
                />
              </label>
            )}
            <div className="grid gap-3 sm:grid-cols-3">
              <MinuteInput
                label="DND start (UTC minute)"
                value={draft.dnd_start_minute_utc}
                onChange={(value) =>
                  setDraft({ ...draft, dnd_start_minute_utc: value })
                }
              />
              <MinuteInput
                label="DND end (UTC minute)"
                value={draft.dnd_end_minute_utc}
                onChange={(value) =>
                  setDraft({ ...draft, dnd_end_minute_utc: value })
                }
              />
              <Toggle
                label="Critical alerts bypass DND"
                checked={draft.critical_bypasses_dnd}
                onChange={(checked) =>
                  setDraft({ ...draft, critical_bypasses_dnd: checked })
                }
              />
            </div>
            <div className="grid gap-3 sm:grid-cols-2">
              <label className="grid gap-1 text-xs">
                <span className="font-medium">Muted event kinds</span>
                <input
                  className="h-9 rounded-md border bg-background px-3"
                  value={draft.muted_kinds.join(", ")}
                  onChange={(event) =>
                    setDraft({
                      ...draft,
                      muted_kinds: event.target.value
                        .split(",")
                        .map((value) => value.trim())
                        .filter(Boolean),
                    })
                  }
                  placeholder="cron.completed, agent.completed"
                />
              </label>
              <label className="grid gap-1 text-xs">
                <span className="font-medium">History retention (days)</span>
                <input
                  type="number"
                  min={1}
                  max={365}
                  className="h-9 rounded-md border bg-background px-3"
                  value={draft.retention_days}
                  onChange={(event) =>
                    setDraft({ ...draft, retention_days: Number(event.target.value) })
                  }
                />
              </label>
            </div>
            <div className="flex justify-end">
              <Button size="sm" onClick={() => void save()} disabled={saving}>
                {saving && <Loader2 className="mr-2 h-3.5 w-3.5 animate-spin" />}
                Save delivery preferences
              </Button>
            </div>
          </>
        )}
        {message && <p className="text-xs text-muted-foreground">{message}</p>}
      </Card>

      {notifications.error && (
        <Card className="border-destructive/40 p-3 text-sm text-destructive">
          {notifications.error}
        </Card>
      )}

      <div className="grid gap-2">
        {notifications.notifications.length === 0 ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            No notifications.
          </p>
        ) : (
          notifications.notifications.map((notification) => (
            <NotificationCard key={notification.id} notification={notification} />
          ))
        )}
      </div>
    </div>
  );
}

function SeveritySelect({
  label,
  value,
  onChange,
}: {
  label: string;
  value: NotificationRecord["severity"];
  onChange: (value: NotificationRecord["severity"]) => void;
}) {
  return (
    <label className="grid gap-1 text-xs">
      <span className="font-medium">{label}</span>
      <select
        className="h-9 rounded-md border bg-background px-3"
        value={value}
        onChange={(event) =>
          onChange(event.target.value as NotificationRecord["severity"])
        }
      >
        <option value="info">Info</option>
        <option value="warning">Warning</option>
        <option value="error">Error</option>
        <option value="critical">Critical</option>
      </select>
    </label>
  );
}

function MinuteInput({
  label,
  value,
  onChange,
}: {
  label: string;
  value?: number;
  onChange: (value?: number) => void;
}) {
  return (
    <label className="grid gap-1 text-xs">
      <span className="font-medium">{label}</span>
      <input
        type="number"
        min={0}
        max={1439}
        className="h-9 rounded-md border bg-background px-3"
        value={value ?? ""}
        onChange={(event) =>
          onChange(event.target.value === "" ? undefined : Number(event.target.value))
        }
        placeholder="Disabled"
      />
    </label>
  );
}

function Toggle({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="flex items-center gap-2 rounded-md border px-3 py-2 text-sm">
      <input
        type="checkbox"
        checked={checked}
        onChange={(event) => onChange(event.target.checked)}
      />
      {label}
    </label>
  );
}

function NotificationCard({ notification }: { notification: NotificationRecord }) {
  const actions = useNotifications();
  const critical = notification.severity === "critical" || notification.severity === "error";
  return (
    <Card
      className={`grid gap-2 p-4 ${
        notification.state === "unread" ? "border-primary/50" : "opacity-80"
      }`}
    >
      <div className="flex items-start justify-between gap-4">
        <div className="flex min-w-0 gap-3">
          <CircleAlert
            className={`mt-0.5 h-4 w-4 shrink-0 ${
              critical ? "text-destructive" : "text-amber-500"
            }`}
          />
          <div className="min-w-0">
            <p className="text-sm font-medium">{notification.title}</p>
            <p className="mt-1 text-sm text-muted-foreground">{notification.body}</p>
            <p className="mt-2 text-[11px] text-muted-foreground">
              {notification.source} · {notification.kind} ·{" "}
              {new Date(notification.updated_at_ms).toLocaleString()}
              {notification.occurrences > 1
                ? ` · repeated ${notification.occurrences} times`
                : ""}
            </p>
          </div>
        </div>
        <div className="flex shrink-0 gap-1">
          {notification.state === "unread" && (
            <Button
              size="sm"
              variant="ghost"
              title="Mark read"
              onClick={() => void actions.markRead(notification.id)}
            >
              <Check className="h-3.5 w-3.5" />
            </Button>
          )}
          <Button
            size="sm"
            variant="ghost"
            title="Acknowledge"
            onClick={() => void actions.acknowledge(notification.id)}
          >
            <BellOff className="h-3.5 w-3.5" />
          </Button>
          <Button
            size="sm"
            variant="ghost"
            title="Dismiss"
            onClick={() => void actions.dismiss(notification.id)}
          >
            <X className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>
    </Card>
  );
}
