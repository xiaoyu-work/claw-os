/**
 * Settings page. Sub-nav for the five modalities (text/embed/tts/stt/imagegen)
 * plus an About panel.
 *
 * Each modality form is built dynamically from the response of
 * `GET /api/setup/providers/{modality}`, which returns the same shape
 * `cos agent setup providers <modality>` prints. Apply hits
 * `POST /api/setup/apply`. Verify hits `POST /api/setup/test/{modality}`.
 * OAuth flows for `text` use `oauth/start` + `oauth/poll`.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  Loader2,
  RotateCcw,
  XCircle,
} from "lucide-react";

import { abortableDelay, api } from "@/lib/api";
import { isActive, navigate, useRoute } from "@/lib/router";
import { readSetupStatus } from "@/lib/setup-status";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const MODALITIES = [
  { key: "text", label: "Text" },
  { key: "embed", label: "Embeddings" },
  { key: "tts", label: "Text → speech" },
  { key: "stt", label: "Speech → text" },
  { key: "imagegen", label: "Image gen" },
];

export function SettingsPage({ meta }: { meta: any }) {
  const route = useRoute();
  const modality = useMemo(() => {
    if (route === "/settings/about") return "about";
    const m = route.match(/^\/settings\/([^/]+)/);
    return m?.[1] === "llm" ? "text" : m?.[1] || "text";
  }, [route]);

  useEffect(() => {
    if (route === "/settings/llm") navigate("/settings/text");
  }, [route]);

  return (
    <div className="flex h-full overflow-hidden">
      <aside className="w-56 shrink-0 border-r p-3">
        <h2 className="px-2 pb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
          Settings
        </h2>
        <nav className="grid gap-0.5">
          {MODALITIES.map((m) => (
            <SettingsNavItem
              key={m.key}
              label={m.label}
              href={`/settings/${m.key}`}
              active={isActive(`/settings/${m.key}`, route) || (m.key === "text" && route === "/settings")}
            />
          ))}
          <div className="my-2 border-t" />
          <SettingsNavItem
            label="About"
            href="/settings/about"
            active={route === "/settings/about"}
          />
        </nav>
      </aside>
      <div className="flex-1 overflow-y-auto">
        {modality === "about" ? (
          <AboutPanel meta={meta} />
        ) : (
          <ModalityPanel key={modality} modality={modality} />
        )}
      </div>
    </div>
  );
}

function SettingsNavItem({
  label,
  href,
  active,
}: {
  label: string;
  href: string;
  active: boolean;
}) {
  return (
    <button
      type="button"
      onClick={() => navigate(href)}
      className={cn(
        "rounded-md px-2 py-1.5 text-left text-sm transition-colors",
        active
          ? "bg-sidebar-accent text-sidebar-accent-foreground"
          : "text-sidebar-foreground/80 hover:bg-sidebar-accent/60",
      )}
    >
      {label}
    </button>
  );
}

type ProviderField = {
  key: string;
  label?: string;
  help?: string;
  placeholder?: string;
  required?: boolean;
  secret?: boolean;
};

type ProviderEntry = {
  id?: string;
  key?: string;
  name?: string;
  label?: string;
  display?: string;
  models?: any[];
  auth_kind?: string;
  needs_credential?: boolean;
  default_env?: string;
  default_model?: string;
  extra_fields?: ProviderField[];
};

type OAuthState = {
  user_code?: string;
  verification_uri?: string;
  status: "polling" | "configuring" | "done" | "error";
  authorized?: boolean;
};

type OAuthStartResponse = {
  device_code: string;
  user_code: string;
  verification_uri: string;
  expires_in: number;
  interval: number;
};

type OAuthPollResponse = {
  status: "pending" | "slow_down" | "ok" | "expired" | "denied";
  interval?: number;
};

function setupRequestSignal(signal?: AbortSignal, timeout = 90_000): AbortSignal {
  const deadline = AbortSignal.timeout(timeout);
  return signal ? AbortSignal.any([signal, deadline]) : deadline;
}

function providerId(p: ProviderEntry): string {
  return String(p.id || p.key || p.name || p.label || "");
}

function providerLabel(p: ProviderEntry): string {
  const id = providerId(p);
  if (id === "copilot") return "GitHub Copilot";
  return String(p.label || p.display || p.name || id);
}

function isOauthOnly(p?: ProviderEntry): boolean {
  return p?.auth_kind === "oauth_device";
}

function ModalityPanel({ modality }: { modality: string }) {
  const [status, setStatus] = useState<any>(null);
  const [providers, setProviders] = useState<ProviderEntry[]>([]);
  const [provider, setProvider] = useState<string>("");
  const [model, setModel] = useState<string>("");
  const [apiKey, setApiKey] = useState<string>("");
  // Free-form extra fields driven by `provider.extra_fields[]` (e.g.
  // Azure's base_url + api_version). Keyed by field.key.
  const [extras, setExtras] = useState<Record<string, string>>({});
  const [models, setModels] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [msg, setMsg] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  // OAuth device-flow display state: shown after user clicks "Sign in"
  // and we have the user_code + verification_uri to display.
  const [oauth, setOauth] = useState<OAuthState | null>(null);
  const oauthRun = useRef<{ controller: AbortController; authorized: boolean } | null>(null);

  const load = useCallback(async (preserveSelection = false, signal?: AbortSignal) => {
    setLoading(true);
    try {
      const [st, pv] = await Promise.all([
        api.get(`/api/setup/status/${modality}`, { signal: setupRequestSignal(signal) }),
        api.get(`/api/setup/providers/${modality}`, { signal: setupRequestSignal(signal) }),
      ]);
      if (signal?.aborted) return;
      setStatus(st);
      const list = extractProviders(pv);
      setProviders(list);
      if (preserveSelection) return;
      const stObj = (st as any) || {};
      const curProvider = stObj.provider || stObj.current || providerId(list[0] || {});
      const curModel = stObj.model || stObj.current_model;
      setProvider(String(curProvider || ""));
      setModel(String(curModel || ""));
      const nextExtras: Record<string, string> = {};
      for (const k of ["base_url", "api_version", "endpoint"]) {
        if (typeof stObj[k] === "string") nextExtras[k] = stObj[k];
      }
      setExtras(nextExtras);
    } catch (e: any) {
      if (signal?.aborted) return;
      setMsg({ kind: "err", text: e?.message || "Load failed" });
    } finally {
      if (!signal?.aborted) setLoading(false);
    }
  }, [modality]);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    setBusy(null);
    return () => {
      oauthRun.current?.controller.abort();
      oauthRun.current = null;
    };
  }, [provider, modality]);

  // Load model list whenever provider changes. The /api/setup/models
  // endpoint only supports `copilot`; for everything else we fall back
  // to the `models` list returned by /api/setup/providers/{modality}.
  useEffect(() => {
    if (busy != null) return;
    if (!provider) return;
    const entry = providers.find((p) => providerId(p) === provider);
    if (entry?.models && entry.models.length > 0) {
      setModels(extractModelIds(entry.models));
      return;
    }
    if (
      isOauthOnly(entry) &&
      ((status as any)?.provider !== provider || (status as any)?.ready !== true)
    ) {
      setModels([]);
      return;
    }
    let cancelled = false;
    const controller = new AbortController();
    api
      .get(`/api/setup/models/${modality}/${provider}`, {
        signal: setupRequestSignal(controller.signal),
      })
      .then((r) => {
        if (cancelled) return;
        setModels(extractModelIds(r));
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        setModels([]);
        if (isOauthOnly(entry)) {
          setMsg({
            kind: "err",
            text: `Copilot model discovery failed: ${error instanceof Error ? error.message : "unknown error"}`,
          });
        }
      });
    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [modality, provider, providers, status, busy]);

  const providerEntry = useMemo(
    () => providers.find((p) => providerId(p) === provider),
    [providers, provider],
  );

  async function apply(verify: boolean) {
    setBusy("apply");
    setMsg(null);
    let saved = false;
    try {
      const body: any = { modality, provider, verify };
      if (model) body.model = model;
      if (apiKey) body.api_key = apiKey;
      // extras → known top-level fields (api endpoint accepts base_url
      // and api_version directly; future extra_fields would need server
      // support beyond what setup/apply currently exposes).
      if (extras.base_url) body.base_url = extras.base_url;
      if (extras.api_version) body.api_version = extras.api_version;
      await api.post("/api/setup/apply", body);
      saved = true;
      setApiKey("");
      const verified = verify ? await verifyProvider() : "";
      setMsg({ kind: "ok", text: verify ? `Settings saved. ${verified}` : "Applied." });
      await load();
    } catch (e: any) {
      setMsg({
        kind: "err",
        text: saved ? `Settings saved, but verification failed: ${e?.message || "unknown error"}`
          : e?.message || "Apply failed",
      });
    } finally {
      setBusy(null);
    }
  }

  async function reset() {
    if (!confirm(`Reset ${modality} configuration?`)) return;
    setBusy("reset");
    setMsg(null);
    try {
      await api.post(`/api/setup/reset/${modality}`);
      setMsg({ kind: "ok", text: "Reset." });
      setProvider("");
      setModel("");
      setApiKey("");
      setExtras({});
      setOauth(null);
      await load();
    } catch (e: any) {
      setMsg({ kind: "err", text: e?.message || "Reset failed" });
    } finally {
      setBusy(null);
    }
  }

  async function test() {
    setBusy("test");
    setMsg(null);
    try {
      setMsg({ kind: "ok", text: await verifyProvider() });
    } catch (e: any) {
      setMsg({ kind: "err", text: e?.message || "Verify failed" });
    } finally {
      setBusy(null);
    }
  }

  async function verifyProvider(): Promise<string> {
    const result = await api.post<{
      ok: boolean;
      attempted?: boolean;
      message?: string;
      kind?: string;
      reason?: unknown;
      probe?: { error_message?: string };
    }>(`/api/setup/test/${modality}`, undefined, { signal: setupRequestSignal() });
    if (result.ok !== true) {
      throw new Error(result.probe?.error_message ||
        readSetupStatus({ reason: result.reason }).reason || "Provider verification failed.");
    }
    return result.message || result.kind ||
      (result.attempted === false ? "Configured; no live model request was made." : "Verified.");
  }

  async function oauthStart() {
    if (oauthRun.current) return;
    const run = {
      controller: new AbortController(),
      authorized: oauth?.status === "error" && oauth.authorized === true,
    };
    oauthRun.current = run;
    const signal = run.controller.signal;
    setBusy("oauth");
    setMsg(null);
    try {
      if (run.authorized) {
        await finalizeOauth(signal);
        return;
      }
      setOauth(null);
      const start = await api.post<OAuthStartResponse>(
        "/api/setup/oauth/start", { provider, modality },
        { signal: setupRequestSignal(signal) },
      );
      signal.throwIfAborted();
      if (!start.device_code || !start.user_code || !start.verification_uri) {
        throw new Error("GitHub sign-in returned an incomplete device code.");
      }
      if (!Number.isSafeInteger(start.expires_in) || start.expires_in <= 0 ||
          !Number.isSafeInteger(start.interval) || start.interval <= 0) {
        throw new Error("GitHub sign-in returned invalid polling times.");
      }
      setOauth({
        user_code: start.user_code, verification_uri: start.verification_uri, status: "polling",
      });
      const deadline = Date.now() + start.expires_in * 1000;
      let interval = Math.max(start.interval, 5);
      while (Date.now() < deadline) {
        await abortableDelay(Math.min(interval * 1000, deadline - Date.now()), signal);
        const remaining = deadline - Date.now();
        if (remaining <= 0) break;
        const poll = await api.post<OAuthPollResponse>("/api/setup/oauth/poll", {
          provider, modality, device_code: start.device_code,
        }, { signal: setupRequestSignal(signal, Math.min(90_000, remaining)) });
        signal.throwIfAborted();
        switch (poll.status) {
          case "pending":
            continue;
          case "slow_down":
            if (poll.interval !== undefined &&
                (!Number.isSafeInteger(poll.interval) || poll.interval <= 0)) {
              throw new Error("GitHub sign-in returned an invalid polling interval.");
            }
            interval = Math.max(interval + 5, poll.interval ?? 0);
            continue;
          case "ok":
            run.authorized = true;
            await finalizeOauth(signal);
            return;
          case "expired":
            throw new Error("GitHub sign-in code expired. Start sign-in again.");
          case "denied":
            throw new Error("GitHub sign-in was denied.");
          default:
            throw new Error("GitHub sign-in returned an unexpected status.");
        }
      }
      throw new Error("GitHub sign-in code expired. Start sign-in again.");
    } catch (error: unknown) {
      if (signal.aborted) return;
      setOauth({ status: "error", authorized: run.authorized });
      const details = error instanceof Error ? error.message : "Sign-in failed.";
      setMsg({
        kind: "err",
        text: run.authorized
          ? `GitHub sign-in succeeded, but Copilot setup did not finish: ${details}`
          : `GitHub sign-in failed: ${details}`,
      });
      if (run.authorized) await load(true, signal);
    } finally {
      if (oauthRun.current === run) {
        oauthRun.current = null;
        setBusy(null);
      }
    }
  }

  function cancelOauth() {
    const authorized = oauthRun.current?.authorized === true;
    oauthRun.current?.controller.abort();
    oauthRun.current = null;
    setBusy(null);
    setLoading(false);
    setOauth(authorized ? { status: "error", authorized: true } : null);
    setMsg({ kind: "ok", text: "Stopped waiting. Any credentials already saved are kept." });
  }

  // After OAuth success, the GitHub token has been stored as a credential
  // but the active text provider in config.json hasn't been switched. The
  // CLI wizard's terminal flow handles this in one shot: fetch live
  // models, let the user pick one, then `apply` with the credential. The
  // web port mirrors that — auto-pick the first model and apply so the
  // user lands on a fully-configured "ready" state instead of "signed in
  // but not configured".
  async function finalizeOauth(signal: AbortSignal) {
    setBusy("oauth-finalize");
    setOauth({ status: "configuring", authorized: true });
    setMsg({ kind: "ok", text: "Signed in. Configuring Copilot…" });
    const modelsResponse: unknown = await api.get(`/api/setup/models/${modality}/${provider}`, {
      signal: setupRequestSignal(signal),
    });
    signal.throwIfAborted();
    const ids = extractModelIds(modelsResponse);
    setModels(ids);
    const preferred = providerEntry?.default_model || "";
    const pick = preferred && ids.includes(preferred) ? preferred : ids[0];
    if (!pick) {
      throw new Error("Copilot returned no selectable chat models. Check Copilot access or enter a model manually and click Apply.");
    }
    setModel(pick);
    await api.post("/api/setup/apply", {
      modality,
      provider,
      model: pick,
      verify: true,
    }, { signal: setupRequestSignal(signal) });
    signal.throwIfAborted();
    setOauth({ status: "done", authorized: true });
    setMsg({ kind: "ok", text: `Copilot configured with ${pick}.` });
    await load(false, signal);
  }

  const { configured, ready, reason } = readSetupStatus(status);
  const oauthOnly = isOauthOnly(providerEntry);
  const oauthAuthenticated = oauth?.authorized === true ||
    (status?.provider === provider && ready);
  // Provider-declared extra fields (e.g. Azure base_url + api_version).
  // We render these regardless of OAuth mode — Azure for example needs
  // its endpoint URL even though API key auth is the only path today.
  const extraFields = providerEntry?.extra_fields || [];

  return (
    <div className="mx-auto max-w-2xl p-6">
      <div className="mb-4 flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold capitalize">{modality}</h1>
          <p className="text-xs text-muted-foreground">
            Configure the {modality} provider for cos agent.
          </p>
        </div>
        <div className="flex items-center gap-2 text-xs">
          {ready ? (
            <span className="flex items-center gap-1 text-emerald-500">
              <CheckCircle2 className="h-3.5 w-3.5" /> configured
            </span>
          ) : (
            configured ? (
              <span className="flex items-center gap-1 text-amber-500">
                <AlertTriangle className="h-3.5 w-3.5" /> configured · needs attention
              </span>
            ) : (
              <span className="flex items-center gap-1 text-yellow-500">
                <XCircle className="h-3.5 w-3.5" /> not configured
              </span>
            )
          )}
        </div>
      </div>

      {loading && !status ? (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin" />
          Loading…
        </div>
      ) : (
        <Card className="grid gap-4 p-5">
          <div className="grid gap-1.5">
            <Label>Provider</Label>
            <Select
              value={provider}
              disabled={busy != null}
              onValueChange={(v) => {
                setProvider(v);
                setOauth(null);
                setMsg(null);
              }}
            >
              <SelectTrigger>
                <SelectValue placeholder="Choose…" />
              </SelectTrigger>
              <SelectContent>
                {providers.map((p) => {
                  const id = providerId(p);
                  return (
                    <SelectItem key={id} value={id}>
                      {providerLabel(p)}
                      {isOauthOnly(p) ? " · OAuth" : ""}
                    </SelectItem>
                  );
                })}
              </SelectContent>
            </Select>
          </div>

          {oauthOnly ? (
            <CopilotAuthBlock
              providerLabel={providerEntry ? providerLabel(providerEntry) : provider}
              busy={busy === "oauth" || busy === "oauth-finalize" ? busy : null}
              oauth={oauth}
              ready={status?.provider === provider && ready}
              onStart={oauthStart}
              onCancel={cancelOauth}
            />
          ) : (
            <>
              {extraFields.map((f) => (
                <div key={f.key} className="grid gap-1.5">
                  <Label>
                    {f.label || f.key}
                    {f.required ? <span className="text-destructive"> *</span> : null}
                  </Label>
                  <Input
                    type={f.secret ? "password" : "text"}
                    placeholder={f.placeholder || ""}
                    value={extras[f.key] || ""}
                    onChange={(e) =>
                      setExtras((x) => ({ ...x, [f.key]: e.target.value }))
                    }
                  />
                  {f.help && (
                    <p className="text-[11px] text-muted-foreground">{f.help}</p>
                  )}
                </div>
              ))}

              <div className="grid gap-1.5">
                <Label>API key</Label>
                <Input
                  type="password"
                  placeholder={
                    providerEntry?.default_env
                      ? `from env ${providerEntry.default_env}, or paste here`
                      : "Paste an API key"
                  }
                  value={apiKey}
                  onChange={(e) => setApiKey(e.target.value)}
                />
              </div>
            </>
          )}

          {configured && !ready && reason ? (
            <div className="rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-amber-700 dark:text-amber-300">
              {reason}
            </div>
          ) : null}

          {msg && (
            <p
              role={msg.kind === "err" ? "alert" : "status"}
              className={cn(
                "text-sm",
                msg.kind === "ok" ? "text-emerald-500" : "text-destructive",
              )}
            >
              {msg.text}
            </p>
          )}

          <div className="grid gap-1.5">
            <Label>Model</Label>
            {models.length > 0 ? (
              <Select value={model} onValueChange={setModel} disabled={busy != null}>
                <SelectTrigger>
                  <SelectValue placeholder="Choose model…" />
                </SelectTrigger>
                <SelectContent>
                  {models.map((m) => (
                    <SelectItem key={m} value={m}>
                      {m}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : (
              <Input
                placeholder={
                  oauthOnly && !oauthAuthenticated
                    ? "Sign in first to fetch the model list"
                    : "model id"
                }
                value={model}
                disabled={busy != null}
                onChange={(e) => setModel(e.target.value)}
              />
            )}
          </div>

          <div className="flex flex-wrap gap-2">
            <Button
              disabled={busy != null || !provider || (oauthOnly && !oauthAuthenticated)}
              onClick={() => apply(true)}
            >
              {busy === "apply" ? (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              ) : null}
              Apply & verify
            </Button>
            <Button
              variant="outline"
              disabled={busy != null || !provider || (oauthOnly && !oauthAuthenticated)}
              onClick={() => apply(false)}
            >
              Apply
            </Button>
            <Button variant="outline" disabled={busy != null || !ready} onClick={test}>
              Verify
            </Button>
            <div className="ml-auto" />
            <Button
              variant="ghost"
              className="text-destructive"
              disabled={busy != null}
              onClick={reset}
            >
              <RotateCcw className="mr-1 h-3.5 w-3.5" />
              Reset
            </Button>
          </div>
        </Card>
      )}
    </div>
  );
}

function CopilotAuthBlock({
  providerLabel,
  busy,
  oauth,
  ready,
  onStart,
  onCancel,
}: {
  providerLabel: string;
  busy: "oauth" | "oauth-finalize" | null;
  oauth: OAuthState | null;
  ready: boolean;
  onStart: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="grid gap-3 rounded-md border border-dashed border-border bg-muted/30 p-4">
      <div className="grid gap-1">
        <p className="text-sm font-medium">{providerLabel} sign-in</p>
        <p className="text-xs text-muted-foreground">
          {providerLabel} uses GitHub device-flow authorization — no API key.
          {ready || oauth?.authorized ? " A GitHub sign-in is saved." : ""}
        </p>
      </div>
      {oauth?.user_code && oauth.status === "polling" && (
        <div className="grid gap-2 rounded bg-background p-3">
          <p className="text-xs text-muted-foreground">
            Open this URL on any device, then enter the code:
          </p>
          {oauth.verification_uri && (
            <a
              href={oauth.verification_uri}
              target="_blank"
              rel="noopener noreferrer"
              className="break-all text-xs font-medium text-primary underline"
            >
              {oauth.verification_uri}
            </a>
          )}
          <div className="rounded bg-muted px-3 py-2 text-center text-lg font-mono font-semibold tracking-[0.3em]">
            {oauth.user_code}
          </div>
          <p className="flex items-center gap-2 text-xs text-muted-foreground">
            <Loader2 className="h-3 w-3 animate-spin" /> Waiting for you to approve…
          </p>
        </div>
      )}
      <div className="flex flex-wrap gap-2">
        <Button onClick={onStart} disabled={busy != null} className="w-fit">
          {busy ? <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" /> : null}
          {busy === "oauth-finalize" ? "Completing Copilot setup…"
            : busy === "oauth" ? (oauth?.status === "polling" ? "Waiting for GitHub approval…" : "Starting GitHub sign-in…")
            : oauth?.status === "error" && oauth.authorized ? "Retry Copilot setup"
            : ready ? "Sign in again" : "Sign in with GitHub"}
        </Button>
        {busy && <Button variant="outline" onClick={onCancel}>Stop waiting</Button>}
      </div>
    </div>
  );
}

function AboutPanel({ meta }: { meta: any }) {
  return (
    <div className="mx-auto max-w-2xl p-6">
      <h1 className="text-xl font-semibold">About</h1>
      <p className="mt-1 text-xs text-muted-foreground">
        cos agent web — running locally, served by{" "}
        <code className="font-mono">cos agent serve</code>.
      </p>
      <Card className="mt-4 p-5">
        <dl className="grid grid-cols-[120px_1fr] gap-y-2 text-sm">
          <dt className="text-muted-foreground">Version</dt>
          <dd className="font-mono">{meta?.version || "—"}</dd>
          <dt className="text-muted-foreground">Hostname</dt>
          <dd className="font-mono">{meta?.hostname || "—"}</dd>
          <dt className="text-muted-foreground">Provider</dt>
          <dd className="font-mono">{meta?.provider || "—"}</dd>
          <dt className="text-muted-foreground">Model</dt>
          <dd className="font-mono">{meta?.model || "—"}</dd>
        </dl>
      </Card>
    </div>
  );
}

function extractProviders(raw: any): ProviderEntry[] {
  if (!raw) return [];
  if (Array.isArray(raw)) return raw;
  if (Array.isArray(raw.providers)) return raw.providers;
  if (typeof raw === "object") {
    // Map-style { provider_id: meta }
    return Object.entries(raw).map(([k, v]) => ({
      id: k,
      ...(typeof v === "object" && v ? (v as object) : {}),
    }));
  }
  return [];
}

function extractModelIds(raw: any): string[] {
  if (!raw) return [];
  const list: any[] = Array.isArray(raw)
    ? raw
    : Array.isArray(raw.models)
      ? raw.models
      : [];
  return list.map((m) => (typeof m === "string" ? m : m?.id || m?.name || "")).filter(Boolean);
}
