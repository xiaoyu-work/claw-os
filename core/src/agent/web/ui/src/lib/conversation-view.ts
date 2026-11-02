import { useCallback, useEffect, useRef } from "react";
import { api } from "./api";

type NotificationViewer = { id: string; revision: number; visible: boolean };
let viewerId: string | undefined;
let revision = 0;

function isVisible(): boolean {
  return document.visibilityState === "visible" && document.hasFocus();
}

export function notificationViewer(allowed = true): NotificationViewer {
  viewerId ||= crypto.randomUUID();
  return { id: viewerId, revision: ++revision, visible: allowed && isVisible() };
}

export function useConversationView(sessionId: string, ready = true): void {
  const session = useRef(sessionId);
  const readyRef = useRef(ready);
  const failed = useRef(false);
  const publish = useCallback((visible = isVisible(), keepalive = false) => {
    const viewer = notificationViewer(visible && !!session.current && readyRef.current);
    void api.post("/api/notifications/view", {
      viewer,
      ...(session.current ? { session_id: session.current } : {}),
    }, { keepalive, signal: AbortSignal.timeout(3_000) }).then(() => {
      failed.current = false;
    }).catch((error: unknown) => {
      if (!failed.current) {
        console.warn("Conversation visibility could not be reported; background alerts remain enabled.", error);
        failed.current = true;
      }
    });
  }, []);

  useEffect(() => {
    session.current = sessionId;
    readyRef.current = ready;
    publish();
  }, [sessionId, ready, publish]);

  useEffect(() => {
    const sync = () => publish();
    const detach = () => publish(false, true);
    document.addEventListener("visibilitychange", sync);
    window.addEventListener("focus", sync);
    window.addEventListener("blur", sync);
    window.addEventListener("pageshow", sync);
    window.addEventListener("pagehide", detach);
    const timer = window.setInterval(() => {
      if (session.current && readyRef.current && isVisible()) publish();
    }, 5_000);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", sync);
      window.removeEventListener("focus", sync);
      window.removeEventListener("blur", sync);
      window.removeEventListener("pageshow", sync);
      window.removeEventListener("pagehide", detach);
      detach();
    };
  }, [publish]);
}
