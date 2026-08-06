/**
 * Token bootstrap modal. Mirrors the old single-file SPA's first-launch
 * flow: read `cos.token` from localStorage; if missing or rejected by
 * `/api/meta`, prompt the user to paste the bootstrap secret printed by
 * `cos agent serve`. The bootstrap secret is exchanged for a one-hour signed
 * access token; only the short-lived token is stored in localStorage.
 *
 * Also reads `?t=<token>` from the initial URL so the user can just
 * click the link printed by the daemon.
 */

import { useEffect, useState } from "react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { api, exchangeBootstrapToken, getToken } from "@/lib/api";

type Status = "checking" | "ok" | "needs-token";

export function TokenGate({ children, onMeta }: { children: React.ReactNode; onMeta: (m: any) => void }) {
  const [status, setStatus] = useState<Status>("checking");
  const [pasted, setPasted] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void bootstrap();
    const requireAuth = () => setStatus("needs-token");
    window.addEventListener("cos:auth-required", requireAuth);
    return () => window.removeEventListener("cos:auth-required", requireAuth);
  }, []);

  async function bootstrap() {
    const url = new URL(window.location.href);
    const t = url.searchParams.get("t");
    if (t) {
      url.searchParams.delete("t");
      window.history.replaceState({}, "", url.pathname + url.hash);
      try {
        await exchangeBootstrapToken(t);
      } catch (e: any) {
        setErr(e?.message || "Invalid bootstrap token");
        setStatus("needs-token");
        return;
      }
    }
    await verify();
  }

  async function verify() {
    setStatus("checking");
    if (!getToken()) {
      setStatus("needs-token");
      return;
    }
    try {
      const meta = await api.get("/api/meta");
      onMeta(meta);
      setStatus("ok");
    } catch (e: any) {
      if (e?.status === 401) {
        setStatus("needs-token");
      } else {
        // Server error — still let the UI render; the user can retry.
        setStatus("ok");
      }
    }
  }

  async function submit() {
    setErr(null);
    setBusy(true);
    try {
      await exchangeBootstrapToken(pasted.trim());
      const meta = await api.get("/api/meta");
      onMeta(meta);
      setStatus("ok");
    } catch (e: any) {
      setErr(e?.message || "Invalid token");
    } finally {
      setBusy(false);
    }
  }

  if (status === "checking") {
    return (
      <div className="grid min-h-svh place-items-center text-sm text-muted-foreground">
        Connecting…
      </div>
    );
  }

  if (status === "needs-token") {
    return (
      <Dialog open>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>Paste your access token</DialogTitle>
            <DialogDescription>
              Run <code className="font-mono text-xs">cos agent serve --status</code> on
              the host to see the bootstrap URL, or paste the bootstrap token manually.
            </DialogDescription>
          </DialogHeader>
          <div className="flex flex-col gap-3">
            <Input
              type="password"
              autoFocus
              placeholder="64-character bootstrap token"
              value={pasted}
              onChange={(e) => setPasted(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") submit();
              }}
            />
            {err && <p className="text-xs text-destructive">{err}</p>}
            <div className="flex justify-end gap-2">
              <Button disabled={busy || !pasted.trim()} onClick={submit}>
                {busy ? "Verifying…" : "Connect"}
              </Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>
    );
  }

  return <>{children}</>;
}
