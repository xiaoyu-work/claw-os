/**
 * Token bootstrap modal. Mirrors the old single-file SPA's first-launch
 * flow: read `cos.token` from localStorage; if missing or rejected by
 * `/api/meta`, prompt the user to paste the token printed by
 * `cos agent serve`. The token is the 32-byte hex value persisted to
 * `$COS_DATA_DIR/agent/web/serve.token`.
 *
 * Also reads `?t=<token>` from the initial URL so the user can just
 * click the link printed by the daemon.
 */

import { useEffect, useState } from "react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { api, getToken, setToken } from "@/lib/api";

type Status = "checking" | "ok" | "needs-token";

export function TokenGate({ children, onMeta }: { children: React.ReactNode; onMeta: (m: any) => void }) {
  const [status, setStatus] = useState<Status>("checking");
  const [pasted, setPasted] = useState("");
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const url = new URL(window.location.href);
    const t = url.searchParams.get("t");
    if (t && !getToken()) {
      setToken(t);
      url.searchParams.delete("t");
      window.history.replaceState({}, "", url.pathname + url.hash);
    }
    verify();
  }, []);

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
    setToken(pasted.trim());
    try {
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
              the host to see the URL with the token, or paste it manually.
            </DialogDescription>
          </DialogHeader>
          <div className="flex flex-col gap-3">
            <Input
              type="password"
              autoFocus
              placeholder="32-byte hex token"
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
