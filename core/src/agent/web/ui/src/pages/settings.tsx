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

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  Loader2,
  RotateCcw,
  XCircle,
} from "lucide-react";

import { api } from "@/lib/api";
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
  const [oauth, setOauth] = useState<{
    user_code?: string;
    verification_uri?: string;
    status: "polling" | "done" | "error";
  } | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setMsg(null);
    try {
      const [st, pv] = await Promise.all([
        api.get(`/api/setup/status/${modality}`),
        api.get(`/api/setup/providers/${modality}`),
      ]);
      setStatus(st);
      const list = extractProviders(pv);
      setProviders(list);
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
      setMsg({ kind: "err", text: e?.message || "Load failed" });
    } finally {
      setLoading(false);
    }
  }, [modality]);

  useEffect(() => {
    load();
  }, [load]);

  // Load model list whenever provider changes. The /api/setup/models
  // endpoint only supports `copilot`; for everything else we fall back
  // to the `models` list returned by /api/setup/providers/{modality}.
  useEffect(() => {
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
    api
      .get(`/api/setup/models/${modality}/${provider}`)
      .then((r) => {
        if (cancelled) return;
        setModels(extractModelIds(r));
      })
      .catch(() => {
        if (!cancelled) setModels([]);
      });
    return () => {
      cancelled = true;
    };
  }, [modality, provider, providers, status]);

  const providerEntry = useMemo(
    () => providers.find((p) => providerId(p) === provider),
    [providers, provider],
  );

  async function apply(verify: boolean) {
    setBusy("apply");
    setMsg(null);
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
      setMsg({ kind: "ok", text: verify ? "Applied and verified." : "Applied." });
      setApiKey("");
      await load();
    } catch (e: any) {
      setMsg({ kind: "err", text: e?.message || "Apply failed" });
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
      const r = await api.post(`/api/setup/test/${modality}`);
      setMsg({ kind: "ok", text: (r as any)?.message || "Verified." });
    } catch (e: any) {
      setMsg({ kind: "err", text: e?.message || "Verify failed" });
    } finally {
      setBusy(null);
    }
  }

  async function oauthStart() {
    setBusy("oauth");
    setMsg(null);
    setOauth(null);
    try {
      const r: any = await api.post("/api/setup/oauth/start", { provider, modality });
      const userCode = r?.user_code || r?.verification_code;
      const url = r?.verification_uri || r?.verification_url;
      const devCode = r?.device_code;
      if (!devCode) throw new Error("No device_code returned by server");
      setOauth({ user_code: userCode, verification_uri: url, status: "polling" });
      const deadline = Date.now() + 10 * 60_000;
      const interval = Number(r?.interval) || 5;
      while (Date.now() < deadline) {
        await new Promise((res) => setTimeout(res, interval * 1000));
        try {
          const p: any = await api.post("/api/setup/oauth/poll", {
            provider,
            modality,
            device_code: devCode,
          });
          // `status === "ok"` is the canonical success. Other statuses
          // ("pending" | "slow_down" | "expired" | "denied") loop back
          // or fall through to the timeout branch.
          if (p?.status === "ok") {
            await finalizeOauth();
            return;
          }
          if (p?.status === "expired" || p?.status === "denied") {
            setOauth({ status: "error" });
            setMsg({
              kind: "err",
              text: p.status === "denied" ? "Sign-in denied." : "Code expired. Try again.",
            });
            return;
          }
        } catch {
          // server reports authorization_pending as a 4xx — keep polling
        }
      }
      setOauth({ status: "error" });
      setMsg({ kind: "err", text: "OAuth timed out" });
    } catch (e: any) {
      setOauth({ status: "error" });
      setMsg({ kind: "err", text: e?.message || "OAuth failed" });
    } finally {
      setBusy(null);
    }
  }

  // After OAuth success, the GitHub token has been stored as a credential
  // but the active text provider in config.json hasn't been switched. The
  // CLI wizard's terminal flow handles this in one shot: fetch live
  // models, let the user pick one, then `apply` with the credential. The
  // web port mirrors that — auto-pick the first model and apply so the
  // user lands on a fully-configured "ready" state instead of "signed in
  // but not configured".
  async function finalizeOauth() {
    setBusy("oauth-finalize");
    setOauth({ status: "done" });
    setMsg({ kind: "ok", text: "Signed in. Configuring Copilot…" });
    try {
      // Fetch the live model list now that we have a token.
      const m: any = await api.get(`/api/setup/models/${modality}/${provider}`);
      const ids = extractModelIds(m);
      setModels(ids);
      // Prefer the provider's declared default if it's in the live list,
      // else the first live entry.
      const def = providerEntry?.default_model || "";
      const pick = (def && ids.includes(def) ? def : ids[0]) || def;
      if (!pick) {
        setMsg({
          kind: "err",
          text: "Signed in, but Copilot returned no models. Pick one manually and click Apply.",
        });
        await load();
        return;
      }
      setModel(pick);
      // Auto-apply. Copilot's `apply` resolves the credential by provider
      // name (COPILOT_GITHUB_TOKEN_CREDENTIAL) so we don't pass api_key.
      await api.post("/api/setup/apply", {
        modality,
        provider,
        model: pick,
        verify: true,
      });
      setMsg({ kind: "ok", text: `Copilot configured with ${pick}.` });
      await load();
    } catch (e: any) {
      setMsg({
        kind: "err",
        text: `Signed in, but auto-configure failed: ${e?.message || e}. Pick a model and click Apply.`,
      });
      // Even on failure, refresh status so the UI reflects the stored token.
      await load();
    } finally {
      setBusy(null);
    }
  }

  const { configured, ready, reason } = readSetupStatus(status);
  const oauthOnly = isOauthOnly(providerEntry);
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

      {loading ? (
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
              busy={busy === "oauth"}
              oauth={oauth}
              ready={ready}
              onStart={oauthStart}
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

          <div className="grid gap-1.5">
            <Label>Model</Label>
            {models.length > 0 ? (
              <Select value={model} onValueChange={setModel}>
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
                  oauthOnly && !ready
                    ? "Sign in first to fetch the model list"
                    : "model id"
                }
                value={model}
                onChange={(e) => setModel(e.target.value)}
              />
            )}
          </div>

          {msg && (
            <p
              className={cn(
                "text-sm",
                msg.kind === "ok" ? "text-emerald-500" : "text-destructive",
              )}
            >
              {msg.text}
            </p>
          )}

          <div className="flex flex-wrap gap-2">
            <Button
              disabled={busy != null || !provider || (oauthOnly && !ready)}
              onClick={() => apply(true)}
            >
              {busy === "apply" ? (
                <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
              ) : null}
              Apply & verify
            </Button>
            <Button
              variant="outline"
              disabled={busy != null || !provider || (oauthOnly && !ready)}
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
}: {
  providerLabel: string;
  busy: boolean;
  oauth: { user_code?: string; verification_uri?: string; status: string } | null;
  ready: boolean;
  onStart: () => void;
}) {
  return (
    <div className="grid gap-3 rounded-md border border-dashed border-border bg-muted/30 p-4">
      <div className="grid gap-1">
        <p className="text-sm font-medium">{providerLabel} sign-in</p>
        <p className="text-xs text-muted-foreground">
          {providerLabel} uses GitHub device-flow authorization — no API key.
          {ready ? " Already signed in." : ""}
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
      <Button onClick={onStart} disabled={busy} className="w-fit">
        {busy ? <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" /> : null}
        {ready ? "Sign in again" : `Sign in with GitHub`}
      </Button>
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
