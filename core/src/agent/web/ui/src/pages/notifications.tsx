import { Bell, BellOff, ChevronDown, CircleAlert, Inbox as InboxIcon, Loader2, MailOpen, MoreHorizontal, Settings2, TriangleAlert, X } from "lucide-react";
import { useState } from "react";

import { NotificationSettings } from "@/components/notification-settings";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { type NotificationRecord, notificationActionRoute, useNotifications } from "@/lib/notifications";
import { navigate } from "@/lib/router";

export function NotificationsPage() {
  const notifications = useNotifications();
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [browserBusy, setBrowserBusy] = useState(false);
  const [filter, setFilter] = useState<"all" | "unread">("all");
  const [feedback, setFeedback] = useState<{ text: string; error: boolean } | null>(null);
  const unread = notifications.notifications.filter((notification) => notification.state === "unread").length;
  const records = filter === "unread"
    ? notifications.notifications.filter((notification) => notification.state === "unread")
    : notifications.notifications;
  const browserPaused = notifications.browserEnabled && notifications.preferences?.web_enabled === false;

  const enableBrowser = async () => {
    setBrowserBusy(true);
    setFeedback(null);
    try {
      await notifications.enableBrowserNotifications();
    } catch (cause: unknown) {
      setFeedback({ text: cause instanceof Error ? cause.message : "Browser notifications could not be enabled.", error: true });
    } finally {
      setBrowserBusy(false);
    }
  };

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto w-full max-w-4xl space-y-6 px-4 py-6 sm:px-6" aria-label="Notification Inbox">
        <header className="flex items-center justify-between gap-4">
          <div className="flex items-center gap-3">
            <h1 className="text-xl font-semibold tracking-tight">Inbox</h1>
            {unread > 0 && <span className="rounded-full bg-muted px-2 py-0.5 text-[11px] text-muted-foreground">{unread} unread</span>}
          </div>
          <div className="flex shrink-0 items-center gap-3">
            <span className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
              <span className={`h-1.5 w-1.5 rounded-full ${notifications.connected ? "bg-emerald-500" : "bg-amber-500"}`} />
              {notifications.connected ? "Live" : "Connecting"}
            </span>
            <Button size="icon-sm" variant="ghost" aria-label="Notification settings" title="Notification settings"
              disabled={!notifications.preferences} onClick={() => setSettingsOpen(true)}>
              <Settings2 className="h-4 w-4" />
            </Button>
          </div>
        </header>

        <section className="flex items-center justify-between gap-4 rounded-xl border bg-muted/20 px-4 py-3" aria-label="Browser notifications">
          <div className="flex min-w-0 items-center gap-3">
            <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-background">
              <Bell className="h-4 w-4 text-muted-foreground" aria-hidden="true" />
            </span>
            <div>
              <h2 className="text-sm font-medium">Browser notifications</h2>
              <p className="mt-0.5 text-xs text-muted-foreground">
                {browserPaused ? "Web alerts are off in delivery settings." : "Get an alert when you're away."}
              </p>
            </div>
          </div>
          {notifications.browserEnabled ? (
            <div className="flex shrink-0 items-center gap-2">
              <span className={`text-xs ${browserPaused ? "text-muted-foreground" : "text-emerald-600 dark:text-emerald-400"}`}>
                {browserPaused ? "Paused" : "On"}
              </span>
              <Button size="sm" variant="ghost" onClick={notifications.disableBrowserNotifications}>Disable</Button>
            </div>
          ) : (
            <Button size="sm" variant="outline" disabled={browserBusy} onClick={() => void enableBrowser()}>
              {browserBusy && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
              Enable
            </Button>
          )}
        </section>

        {feedback && (
          <div role={feedback.error ? "alert" : "status"} className={`flex items-start justify-between gap-3 rounded-lg border px-3 py-2 text-xs ${feedback.error ? "border-destructive/30 text-destructive" : "text-muted-foreground"}`}>
            <p>{feedback.text}</p>
            <button type="button" aria-label="Dismiss message" onClick={() => setFeedback(null)}><X className="h-3.5 w-3.5" /></button>
          </div>
        )}
        {notifications.error && <p role="alert" className="text-xs text-destructive">{notifications.error}</p>}

        <section className="space-y-3" aria-label="Notifications">
          <div className="inline-flex rounded-lg bg-muted/60 p-1" role="group" aria-label="Notification filter">
            {(["all", "unread"] as const).map((value) => (
              <button key={value} type="button" aria-pressed={filter === value} onClick={() => setFilter(value)}
                className={`rounded-md px-3 py-1 text-xs font-medium transition-colors ${filter === value ? "bg-background text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground"}`}>
                {value === "all" ? "All" : "Unread"}
              </button>
            ))}
          </div>
          {records.length === 0 ? (
            <div className="grid justify-items-center rounded-xl border border-dashed px-6 py-16 text-center">
              <span className="mb-4 flex h-12 w-12 items-center justify-center rounded-2xl bg-muted/50">
                <InboxIcon className="h-5 w-5 text-muted-foreground" aria-hidden="true" />
              </span>
              <h2 className="text-sm font-medium">{filter === "unread" ? "Nothing unread" : "You're all caught up"}</h2>
              <p className="mt-1 text-xs text-muted-foreground">New alerts will appear here.</p>
            </div>
          ) : (
            <div className="divide-y overflow-hidden rounded-xl border bg-card/30">
              {records.map((notification) => <NotificationRow key={notification.id} notification={notification}
                onError={(text) => setFeedback({ text, error: true })} />)}
            </div>
          )}
        </section>

        {settingsOpen && notifications.preferences && (
          <NotificationSettings preferences={notifications.preferences} onSave={notifications.savePreferences}
            onClose={() => setSettingsOpen(false)}
            onSaved={() => { setSettingsOpen(false); setFeedback({ text: "Notification settings saved.", error: false }); }} />
        )}
      </div>
    </div>
  );
}

function NotificationRow({ notification, onError }: { notification: NotificationRecord; onError: (message: string) => void }) {
  const actions = useNotifications();
  const [busy, setBusy] = useState(false);
  const critical = notification.severity === "critical" || notification.severity === "error";
  const Icon = critical ? CircleAlert : notification.severity === "warning" ? TriangleAlert : Bell;
  const iconClass = critical ? "bg-destructive/10 text-destructive" :
    notification.severity === "warning" ? "bg-amber-500/10 text-amber-600 dark:text-amber-400" : "bg-muted text-muted-foreground";
  const act = async (action: "read" | "acknowledge" | "dismiss", route?: string) => {
    setBusy(true);
    try {
      const mutate = action === "read" ? actions.markRead : action === "acknowledge" ? actions.acknowledge : actions.dismiss;
      await mutate(notification.id);
      if (route) navigate(route);
    } catch (cause: unknown) {
      onError(cause instanceof Error ? cause.message : "The notification could not be updated.");
    } finally {
      setBusy(false);
    }
  };
  return (
    <article className="group px-4 py-4 transition-colors hover:bg-muted/20" data-notification-id={notification.id}>
      <div className="flex items-start gap-3">
        <span className={`mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg ${iconClass}`}>
          <Icon className="h-4 w-4" aria-hidden="true" />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex items-start justify-between gap-3">
            <div className="flex min-w-0 items-start gap-2">
              {notification.state === "unread" && <span className="mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full bg-primary" aria-label="Unread" />}
              <h3 className={`min-w-0 break-words text-sm ${notification.state === "unread" ? "font-semibold" : "font-medium text-muted-foreground"}`}>{notification.title}</h3>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <time className="text-[11px] text-muted-foreground" dateTime={new Date(notification.updated_at_ms).toISOString()}
                title={new Date(notification.updated_at_ms).toLocaleString()}>{relativeTime(notification.updated_at_ms)}</time>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button size="icon-sm" variant="ghost" disabled={busy} aria-label={`Actions for ${notification.title}`}
                    className="sm:opacity-0 sm:group-hover:opacity-100 sm:group-focus-within:opacity-100">
                    {busy ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <MoreHorizontal className="h-4 w-4" />}
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  {notification.state === "unread" && <DropdownMenuItem onSelect={() => void act("read")}><MailOpen />Mark as read</DropdownMenuItem>}
                  {notification.state !== "acknowledged" && <DropdownMenuItem onSelect={() => void act("acknowledge")}><BellOff />Acknowledge</DropdownMenuItem>}
                  <DropdownMenuSeparator />
                  <DropdownMenuItem onSelect={() => void act("dismiss")}><X />Dismiss</DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          </div>
          <details className="group mt-1 text-xs" open={critical}>
            <summary className="cursor-pointer list-none [&::-webkit-details-marker]:hidden">
              <span className="block line-clamp-2 whitespace-pre-wrap break-words text-sm leading-relaxed text-muted-foreground group-open:line-clamp-none">{notification.body}</span>
              <span className="mt-2 inline-flex items-center gap-1 text-[11px] text-muted-foreground">
                Details <ChevronDown className="h-3 w-3 transition-transform group-open:rotate-180" aria-hidden="true" />
              </span>
            </summary>
            <p className="mt-2 break-words text-[11px] text-muted-foreground">
              {notification.source} · {notification.kind}
              {notification.occurrences > 1 ? ` · ${notification.occurrences} updates` : ""}
            </p>
          </details>
          {notification.actions.length > 0 && <div className="mt-3 flex flex-wrap gap-2">
            {notification.actions.map((action) => {
              const route = notificationActionRoute(action.uri, notification.task_id);
              return route && <Button key={action.id} size="sm" variant="outline" disabled={busy}
                onClick={() => void act("read", route)}>{action.label}</Button>;
            })}
          </div>}
        </div>
      </div>
    </article>
  );
}

function relativeTime(timestamp: number): string {
  const minutes = Math.max(0, Math.floor((Date.now() - timestamp) / 60_000));
  if (minutes < 1) return "Just now";
  if (minutes < 60) return `${minutes}m`;
  if (minutes < 1440) return `${Math.floor(minutes / 60)}h`;
  return new Date(timestamp).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
