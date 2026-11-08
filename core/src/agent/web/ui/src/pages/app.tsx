import { useEffect, useRef, useState } from "react";
import { ArrowLeft, Loader2, ShieldCheck } from "lucide-react";

import { api } from "@/lib/api";
import { appMessage, readAppReview, readAppView, type AppReview, type AppView } from "@/lib/apps";
import { navigate, useRoute } from "@/lib/router";
import { useTheme } from "@/lib/theme";
import { AppReviewDialog } from "@/components/app-review";
import { Button } from "@/components/ui/button";

export function AppPage() {
  const route = useRoute();
  const appId = route.slice("/apps/".length);
  const [view, setView] = useState<AppView | null>(null);
  const [review, setReview] = useState<AppReview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const frame = useRef<HTMLIFrameElement | null>(null);
  const theme = useTheme();

  useEffect(() => {
    const controller = new AbortController();
    let opened: string | null = null;
    setView(null); setReview(null); setError(null);
    void api.post(`/api/apps/${encodeURIComponent(appId)}/open`, undefined, { signal: controller.signal })
      .then((value) => {
        if (controller.signal.aborted) return;
        if (value?.status === "review_required") setReview(readAppReview(value.review));
        else { const next = readAppView(value, appId); opened = next.view_id; setView(next); }
      }).catch((failure) => {
        if (!controller.signal.aborted) setError(failure instanceof Error ? failure.message : "App could not open.");
      });
    return () => {
      controller.abort();
      if (opened) void api.post(`/api/app-views/${opened}/close`).catch(() => {});
    };
  }, [appId, reload]);

  useEffect(() => {
    if (!view) return;
    const handled = new Set<string>();
    let active = true;
    const initialize = () => frame.current?.contentWindow?.postMessage({
      protocol: "claw.web-app/v1", type: "init", nonce: view.nonce, app: view.app,
      theme: document.documentElement.classList.contains("dark") ? "dark" : "light",
    }, "*");
    const receive = (event: MessageEvent) => {
      if (event.source !== frame.current?.contentWindow || event.origin !== "null") return;
      if (event.data?.protocol === "claw.web-app/v1" && event.data?.type === "ready") {
        initialize(); return;
      }
      const message = appMessage(event.data, view.nonce, view.app);
      if (!message || handled.has(message.id) || handled.size >= 1024) return;
      handled.add(message.id);
      void api.post(`/api/apps/${encodeURIComponent(appId)}/call`, {
        view_id: view.view_id, tool: message.tool, arguments: message.arguments,
      }, { signal: AbortSignal.timeout(120_000) }).then((value) => {
        if (!active) return;
        frame.current?.contentWindow?.postMessage({
          protocol: "claw.web-app/v1", type: "result", nonce: view.nonce,
          id: message.id, ok: true, result: value.result,
        }, "*");
      }).catch((failure) => {
        if (!active) return;
        const messageText = failure instanceof Error ? failure.message : "App call outcome is not confirmed.";
        setError(messageText);
        frame.current?.contentWindow?.postMessage({
          protocol: "claw.web-app/v1", type: "result", nonce: view.nonce,
          id: message.id, ok: false, error: messageText,
        }, "*");
      });
    };
    window.addEventListener("message", receive);
    return () => { active = false; window.removeEventListener("message", receive); };
  }, [view, appId]);

  useEffect(() => {
    if (!view) return;
    frame.current?.contentWindow?.postMessage({
      protocol: "claw.web-app/v1", type: "theme", nonce: view.nonce,
      theme: document.documentElement.classList.contains("dark") ? "dark" : "light",
    }, "*");
  }, [theme, view]);

  return <div className="flex h-full min-h-0 flex-col">
    <div className="flex shrink-0 flex-wrap items-center gap-3 border-b px-4 py-3">
      <Button size="sm" variant="ghost" onClick={() => navigate("/apps")}><ArrowLeft className="mr-1 h-4 w-4" /> Apps</Button>
      <div><h1 className="text-sm font-semibold">{view?.app.name || appId}</h1>
        <p className="text-[11px] text-muted-foreground">{appId} · {view?.app.version}</p></div>
      <span className="ml-auto flex items-center gap-1 text-[11px] text-muted-foreground">
        <ShieldCheck className="h-3.5 w-3.5" /> Isolated product UI · normal App permissions
      </span>
    </div>
    {error && <div role="alert" className="flex shrink-0 items-center justify-between gap-3 border-b bg-destructive/5 px-4 py-2 text-xs">
      <p className="break-words">{error}</p><div className="flex shrink-0 gap-1">
        <Button variant="ghost" size="sm" onClick={() => navigate("/approvals")}>Approvals</Button>
        <Button variant="ghost" size="sm" onClick={() => setReload((value) => value + 1)}>Reopen App</Button>
      </div></div>}
    {!view && !error && !review && <p className="flex items-center gap-2 p-6 text-sm text-muted-foreground">
      <Loader2 className="h-4 w-4 animate-spin" /> Verifying the installed App interface...</p>}
    {view && <iframe ref={frame} title={`${view.app.name} product interface`}
      src={view.src} sandbox="allow-scripts" referrerPolicy="no-referrer"
      onLoad={() => frame.current?.contentWindow?.postMessage({
        protocol: "claw.web-app/v1", type: "init", nonce: view.nonce, app: view.app,
        theme: document.documentElement.classList.contains("dark") ? "dark" : "light",
      }, "*")}
      className="min-h-0 w-full flex-1 border-0" />}
    {review && <AppReviewDialog review={review} onConfirmed={() => {
      setReview(null); setReload((value) => value + 1);
    }} onCancel={() => { setReview(null); navigate("/apps"); }} />}
  </div>;
}
