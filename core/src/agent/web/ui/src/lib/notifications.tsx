import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

import { api, streamSse } from "@/lib/api";
import { navigate } from "@/lib/router";

export type NotificationSeverity = "info" | "warning" | "error" | "critical";
export type NotificationState = "unread" | "read" | "acknowledged" | "dismissed";

export type NotificationRecord = {
  schema: number;
  sequence: number;
  id: string;
  owner_uid: number;
  source: string;
  kind: string;
  severity: NotificationSeverity;
  title: string;
  body: string;
  delivery_policy: "activity" | "immediate";
  task_id?: string;
  session_id?: string;
  job_id?: string;
  state: NotificationState;
  occurrences: number;
  created_at_ms: number;
  updated_at_ms: number;
  actions: Array<{ id: string; label: string; uri: string }>;
  deliveries: Array<{
    channel: "web" | "desktop" | "ntfy";
    state: "queued" | "delivering" | "delivered" | "failed" | "suppressed";
  }>;
};

export type NotificationPreferences = {
  web_enabled: boolean;
  desktop_enabled: boolean;
  ntfy_enabled: boolean;
  web_min_severity: NotificationSeverity;
  desktop_min_severity: NotificationSeverity;
  ntfy_min_severity: NotificationSeverity;
  muted_kinds: string[];
  dnd_start_minute_utc?: number;
  dnd_end_minute_utc?: number;
  critical_bypasses_dnd: boolean;
  retention_days: number;
  ntfy_server: string;
  ntfy_topic?: string;
};

type NotificationContextValue = {
  notifications: NotificationRecord[];
  unreadCount: number;
  connected: boolean;
  error: string | null;
  preferences: NotificationPreferences | null;
  browserEnabled: boolean;
  enableBrowserNotifications: () => Promise<void>;
  disableBrowserNotifications: () => void;
  refresh: () => Promise<void>;
  markRead: (id: string) => Promise<void>;
  acknowledge: (id: string) => Promise<void>;
  dismiss: (id: string) => Promise<void>;
  savePreferences: (preferences: NotificationPreferences) => Promise<void>;
};

const NotificationContext = createContext<NotificationContextValue | null>(null);
const BROWSER_NOTIFICATION_KEY = "cos.browser-notifications.enabled";

type DeliveryClaim = {
  notification: NotificationRecord;
};

function browserNotificationsEnabled(): boolean {
  if (typeof window === "undefined" || !("Notification" in window)) return false;
  try {
    return (
      localStorage.getItem(BROWSER_NOTIFICATION_KEY) === "true" &&
      window.Notification.permission === "granted"
    );
  } catch {
    return false;
  }
}

export function mergeNotificationRecords(
  current: NotificationRecord[],
  incoming: NotificationRecord,
): NotificationRecord[] {
  const next = current.filter((item) => item.id !== incoming.id);
  if (incoming.state !== "dismissed") next.push(incoming);
  next.sort((a, b) => b.updated_at_ms - a.updated_at_ms || b.sequence - a.sequence);
  return next;
}

export function NotificationProvider({ children }: { children: ReactNode }) {
  const [notifications, setNotifications] = useState<NotificationRecord[]>([]);
  const [preferences, setPreferences] = useState<NotificationPreferences | null>(null);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [browserEnabled, setBrowserEnabled] = useState(browserNotificationsEnabled);
  const cursorRef = useRef(0);

  const refresh = useCallback(async () => {
    const [list, prefs] = await Promise.all([
      api.get<{ cursor: number; notifications: NotificationRecord[] }>("/api/notifications"),
      api.get<NotificationPreferences>("/api/notifications/preferences"),
    ]);
    cursorRef.current = list.cursor || 0;
    setNotifications(list.notifications || []);
    setPreferences(prefs);
    setError(null);
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    let stopped = false;

    const deliverPending = async () => {
      const envelope = await api.post<{ deliveries?: DeliveryClaim[] }>(
        "/api/notifications/delivery/claim",
      );
      for (const claim of envelope.deliveries || []) {
        const notification = claim.notification;
        setNotifications((current) =>
          mergeNotificationRecords(current, notification),
        );
        if (
          browserEnabled &&
          notification.delivery_policy === "immediate" &&
          notification.state === "unread" &&
          document.visibilityState !== "visible" &&
          "Notification" in window &&
          window.Notification.permission === "granted"
        ) {
          const popup = new window.Notification(notification.title, {
            body: notification.body,
            tag: notification.id,
          });
          popup.onclick = () => {
            window.focus();
            navigate("/notifications");
          };
        }
        await api.post(
          `/api/notifications/${encodeURIComponent(notification.id)}/delivered`,
        );
      }
    };

    const run = async () => {
      try {
        await refresh();
        await deliverPending();
      } catch (cause: any) {
        setError(cause?.message || "Failed to load notifications");
      }
      while (!stopped) {
        try {
          await streamSse(
            "/api/notifications/stream",
            { cursor: cursorRef.current },
            (event, data) => {
              if (event === "error") {
                setError(data?.error || "Notification stream failed");
                return;
              }
              if (event !== "notification" || !data?.notification) return;
              const notification = data.notification as NotificationRecord;
              const isNewDelivery =
                data.change === "published" || data.change === "updated";
              cursorRef.current = Math.max(cursorRef.current, Number(data.cursor) || 0);
              setNotifications((current) =>
                mergeNotificationRecords(current, notification),
              );
              setConnected(true);
              setError(null);
              window.dispatchEvent(
                new CustomEvent("cos:notifications-changed", {
                  detail: notification,
                }),
              );
              if (isNewDelivery) {
                void deliverPending().catch((cause) => {
                  console.error("notification delivery failed", cause);
                });
              }
            },
            controller.signal,
          );
        } catch (cause: any) {
          if (controller.signal.aborted) return;
          setConnected(false);
          setError(cause?.message || "Notification stream disconnected");
          await new Promise((resolve) => window.setTimeout(resolve, 1_000));
        }
      }
    };
    void run();
    return () => {
      stopped = true;
      controller.abort();
    };
  }, [browserEnabled, refresh]);

  const mutate = useCallback(async (id: string, action: string) => {
    const updated = await api.post<NotificationRecord>(
      `/api/notifications/${encodeURIComponent(id)}/${action}`,
    );
    setNotifications((current) => mergeNotificationRecords(current, updated));
  }, []);

  const value = useMemo<NotificationContextValue>(
    () => ({
      notifications,
      unreadCount: notifications.filter(
        (item) =>
          item.state === "unread" &&
          item.delivery_policy === "immediate" &&
          item.deliveries?.some(
            (delivery) =>
              delivery.channel === "web" && delivery.state !== "suppressed",
          ),
      ).length,
      connected,
      error,
      preferences,
      browserEnabled,
      enableBrowserNotifications: async () => {
        if (!("Notification" in window)) {
          throw new Error("Browser notifications are not supported");
        }
        const permission = await window.Notification.requestPermission();
        if (permission !== "granted") {
          throw new Error("Browser notification permission was not granted");
        }
        try {
          localStorage.setItem(BROWSER_NOTIFICATION_KEY, "true");
        } catch {
          throw new Error("Browser notification preference could not be saved");
        }
        setBrowserEnabled(true);
      },
      disableBrowserNotifications: () => {
        try {
          localStorage.removeItem(BROWSER_NOTIFICATION_KEY);
        } catch {
          // Permission remains browser-owned; disabling still applies in memory.
        }
        setBrowserEnabled(false);
      },
      refresh,
      markRead: (id) => mutate(id, "read"),
      acknowledge: (id) => mutate(id, "acknowledge"),
      dismiss: (id) => mutate(id, "dismiss"),
      savePreferences: async (next) => {
        const saved = await api.post<NotificationPreferences>(
          "/api/notifications/preferences",
          next,
        );
        setPreferences(saved);
        await refresh();
      },
    }),
    [browserEnabled, connected, error, mutate, notifications, preferences, refresh],
  );

  return (
    <NotificationContext.Provider value={value}>
      {children}
    </NotificationContext.Provider>
  );
}

export function useNotifications(): NotificationContextValue {
  const value = useContext(NotificationContext);
  if (!value) {
    throw new Error("useNotifications must be used inside NotificationProvider");
  }
  return value;
}
