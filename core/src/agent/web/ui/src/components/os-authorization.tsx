import { Loader2, LockKeyhole } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { api, abortableDelay } from "@/lib/api";
import { APPROVAL_LABELS, readAuthorization, type AuthorizationView } from "@/lib/approvals";
import { Button } from "./ui/button";
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from "./ui/dialog";

const TERMINAL_PHASES = new Set(["finished", "failed", "cancelled", "expired"]);

export function OsAuthorizationDialog({ initial, onFinished }: {
  initial: AuthorizationView; onFinished: (error?: string) => void;
}) {
  const [current, setCurrent] = useState(initial);
  const [notice, setNotice] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const input = useRef<HTMLInputElement | null>(null);
  const finished = useRef(false);
  const callback = useRef(onFinished);
  callback.current = onFinished;

  function finish(next: AuthorizationView) {
    if (finished.current) return;
    finished.current = true;
    if (input.current) input.current.value = "";
    callback.current(next.phase === "finished" ? undefined :
      next.message || "OS authorization did not complete. Refresh the request.");
  }

  useEffect(() => {
    const controller = new AbortController();
    const deadline = AbortSignal.timeout(150_000);
    const signal = AbortSignal.any([controller.signal, deadline]);
    void (async () => {
      try {
        while (!signal.aborted) {
          const value: unknown = await api.get(`/api/authorizations/${initial.id}`, {
            signal: AbortSignal.any([signal, AbortSignal.timeout(10_000)]),
          });
          if (controller.signal.aborted) return;
          const next = readAuthorization(value, initial);
          setCurrent(next);
          if (TERMINAL_PHASES.has(next.phase)) {
            finish(next);
            return;
          }
          await abortableDelay(400, signal);
        }
      } catch {
        if (controller.signal.aborted || finished.current) return;
        finished.current = true;
        callback.current("The OS authorization outcome could not be confirmed. Refresh the request before trying again.");
      }
    })();
    return () => {
      controller.abort();
      if (input.current) input.current.value = "";
    };
  }, [initial.id]);

  async function respond() {
    if (!current.prompt || submitting || cancelling || !input.current) return;
    const request = api.post(`/api/authorizations/${initial.id}/response`, {
      prompt_id: current.prompt.id, response: input.current.value,
    }, { signal: AbortSignal.timeout(10_000) });
    input.current.value = "";
    setSubmitting(true);
    setNotice(null);
    try {
      const next = readAuthorization(await request, initial);
      if (!finished.current) setCurrent(next);
    } catch {
      if (!finished.current) {
        setNotice("The response outcome is not confirmed. Wait for the OS status; do not resend it automatically.");
      }
    } finally {
      setSubmitting(false);
    }
  }

  async function cancel() {
    if (cancelling || finished.current) return;
    if (input.current) input.current.value = "";
    setCancelling(true);
    try {
      const value: unknown = await api.post(`/api/authorizations/${initial.id}/cancel`, undefined, {
        signal: AbortSignal.timeout(10_000),
      });
      setCurrent(readAuthorization(value, initial));
    } catch {
      setNotice("Cancellation was not confirmed. Refresh the request to inspect its outcome.");
      setCancelling(false);
    }
  }

  return (
    <Dialog open onOpenChange={(open) => { if (!open) void cancel(); }}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <LockKeyhole className="h-4 w-4" /> OS authorization
          </DialogTitle>
          <DialogDescription>
            Verify with your Claw OS account password, not your GitHub password.
            It is sent only to OS authentication, never to the Agent or saved as a credential.
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-2 text-sm">
          <p className="font-medium">{current.label}</p>
          <p className="break-words"><strong>Target:</strong> {current.target}</p>
          <p className="text-xs text-muted-foreground">{current.choice !== null
            ? APPROVAL_LABELS[current.choice]
            : current.action === "confirm_global_app_stop"
            ? "Only confirms the reviewed all-App stop. Unsaved work may be lost. App delivery and Critical package authority remain separate; this is not package success."
            : current.action === "confirm_activation" ? "Confirm opening this App; no capability is granted."
            : "Confirm the reviewed App installation; runtime permissions remain separately enforced."}</p>
        </div>
        {current.phase === "prompt" && current.prompt && !cancelling ? (
          <form id={`os-auth-${initial.id}`} onSubmit={(event) => {
            event.preventDefault(); void respond();
          }} className="grid gap-2">
            <label htmlFor={`os-password-${initial.id}`} className="text-sm">
              {current.prompt.message || "OS password"}
            </label>
            <input ref={input} id={`os-password-${initial.id}`} type="password"
              autoComplete="off" autoFocus maxLength={4096}
              disabled={submitting || cancelling}
              className="rounded-md border bg-background px-3 py-2 text-sm" />
          </form>
        ) : (
          <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            {cancelling ? "Cancelling OS authentication..." :
              current.phase === "verifying" ? "Verifying with the OS..." : "Preparing OS authentication..."}
          </p>
        )}
        {notice && <p role="alert" className="text-xs text-destructive">{notice}</p>}
        <DialogFooter>
          <Button variant="outline" disabled={cancelling} onClick={() => void cancel()}>Cancel</Button>
          {current.phase === "prompt" && !cancelling && (
            <Button type="submit" form={`os-auth-${initial.id}`} disabled={submitting || cancelling}>
              {submitting ? "Verifying..." : "Authenticate"}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
